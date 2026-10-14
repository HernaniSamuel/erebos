//! Type checking (ADR 0008): every type is written or fixed by the
//! construct, so this pass only checks; it never infers.
//!
//! Checking is bidirectional: an expression may receive the type expected
//! where it appears. That is how literals get their types (`let x: u8 = 1`)
//! and why `x + 1` works: the `1` is checked against the type of `x`.
//!
//! The result is a side table from [`ExprId`] to [`Ty`]; HIR plus this table
//! is the "typed HIR" that MIR building consumes.

use std::fmt::Write;

use crate::diag::{Diagnostic, Diagnostics};
use crate::hir::{
    self, BinOp, Block, Expr, ExprId, ExprKind, Fn, FnKind, Hir, IntTy, LocalId, Ptr, Stmt, Ty,
    UnOp,
};
use crate::span::Span;
use crate::target;

pub struct TypeckResults {
    expr_types: Vec<Ty>,
}

impl TypeckResults {
    pub fn expr_ty(&self, id: ExprId) -> Ty {
        self.expr_types[id.0 as usize]
    }
}

pub fn check(hir: &Hir, diags: &mut Diagnostics) -> TypeckResults {
    let mut checker = Checker {
        hir,
        // Overwritten for every expression; the placeholder is never read.
        expr_types: vec![Ty::Error; hir.expr_count as usize],
        diags,
        loops: Vec::new(),
    };
    for (_, f) in hir.fns() {
        checker.fn_(f);
    }
    TypeckResults {
        expr_types: checker.expr_types,
    }
}

/// Whether a value of type `from` may be used where `to` is expected.
/// `!` has no values, so it fits anywhere; `{error}` was already reported.
fn coerces(from: Ty, to: Ty) -> bool {
    from == to || matches!(from, Ty::Never | Ty::Error) || to == Ty::Error
}

/// Expressions that cannot be typed on their own: integer literals, and
/// operators applied only to them.
fn needs_expected_type(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Int(_) => true,
        ExprKind::Unary(_, operand) => needs_expected_type(operand),
        // A comparison is a `bool` whatever its operands are.
        ExprKind::Binary(op, _, _) if op.is_comparison() => false,
        ExprKind::Binary(op, lhs, _) if op.is_shift() => needs_expected_type(lhs),
        ExprKind::Binary(_, lhs, rhs) => needs_expected_type(lhs) && needs_expected_type(rhs),
        _ => false,
    }
}

/// The value of a constant shift amount, if the amount is a literal.
fn literal_value(expr: &Expr) -> Option<i128> {
    match &expr.kind {
        ExprKind::Int(value) => Some(*value as i128),
        ExprKind::Byte(value) => Some(*value as i128),
        ExprKind::Unary(UnOp::Neg, operand) => literal_value(operand).map(|v| -v),
        _ => None,
    }
}

struct Checker<'a> {
    hir: &'a Hir,
    expr_types: Vec<Ty>,
    diags: &'a mut Diagnostics,
    /// Enclosing `loop`/`while` expressions, innermost last.
    loops: Vec<LoopCtx>,
}

/// What the type checker knows about a place expression.
struct PlaceInfo {
    ty: Ty,
    /// Why the place cannot be written, if it cannot.
    immutable: Option<Immutable>,
    /// The last pointer on the path to the place, which decides whether
    /// accesses are volatile (ADR 0023).
    via: Option<Ptr>,
}

impl PlaceInfo {
    /// The place of an expression that failed to type; already reported.
    const ERROR: PlaceInfo = PlaceInfo {
        ty: Ty::Error,
        immutable: None,
        via: None,
    };

    /// The place a pointer points to; `pointer` is the pointer's expression.
    fn through(ptr: Ptr, pointer: Span) -> PlaceInfo {
        PlaceInfo {
            ty: *ptr.pointee,
            immutable: (!ptr.mutable).then_some(Immutable::ConstPointer(ptr, pointer)),
            via: Some(ptr),
        }
    }
}

enum Immutable {
    /// A local declared without `mut`.
    Local(LocalId),
    /// A place reached through a `*const` pointer, written at the span.
    ConstPointer(Ptr, Span),
}

struct LoopCtx {
    id: ExprId,
    /// The type expected of the loop's value, if any.
    expected: Option<Ty>,
    /// The type of the first `break`, which later ones must match.
    break_ty: Option<Ty>,
}

impl Checker<'_> {
    fn fn_(&mut self, f: &Fn) {
        let special = match f.kind {
            FnKind::Entry => Some((
                "ty::entry-returns",
                "entry",
                "there is nothing to return to on bare metal",
            )),
            FnKind::Panic => Some((
                "ty::panic-returns",
                "panic",
                "execution cannot continue after a panic (ADR 0014)",
            )),
            FnKind::Plain => None,
        };
        if let Some((code, what, why)) = special
            && f.ret != Ty::Never
        {
            self.diags.push(
                Diagnostic::error(code, format!("the {what} function must never return"))
                    .label(
                        f.ret_span.unwrap_or(f.name_span),
                        format!("return type is `{}`", f.ret),
                    )
                    .help(format!("{why}; declare it `-> !` and end it with a `loop`")),
            );
        }

        self.params(f);

        self.checked_block(f, &f.body, f.ret, f.ret_span);
    }

    /// Checks a block against an expected type. A tail expression reports its
    /// own mismatch; without a tail, the block itself is reported here.
    /// `because` points at what set the expectation, such as a return type.
    fn checked_block(&mut self, f: &Fn, block: &Block, expected: Ty, because: Option<Span>) -> Ty {
        let ty = self.block(f, block, Some(expected));
        if block.tail.is_none() && !coerces(ty, expected) {
            self.report_block_mismatch(block, expected, ty, because);
        }
        ty
    }

    /// Parameters carry values, so `()` and `!` make no sense there; the entry
    /// and panic functions are called by no one and take none.
    fn params(&mut self, f: &Fn) {
        if f.kind != FnKind::Plain
            && let Some(first) = f.params().first()
        {
            let what = if f.kind == FnKind::Entry {
                "entry"
            } else {
                "panic"
            };
            self.diags.push(
                Diagnostic::error(
                    "ty::special-fn-params",
                    format!("the {what} function takes no parameters"),
                )
                .label(first.span, "parameter here"),
            );
        }
        for param in f.params() {
            if matches!(param.ty, Ty::Unit | Ty::Never) {
                self.diags.push(
                    Diagnostic::error(
                        "ty::param-type",
                        format!("a parameter cannot have type `{}`", param.ty),
                    )
                    .label(param.span, "has no value to pass"),
                );
            }
        }
    }

    /// A block without a tail whose type is not the expected one.
    fn report_block_mismatch(
        &mut self,
        block: &Block,
        expected: Ty,
        found: Ty,
        because: Option<Span>,
    ) {
        let found_at = match block.stmts.last() {
            Some(Stmt::Expr { expr, .. }) => expr.span,
            _ => block.span,
        };
        let mut diag = Diagnostic::error("ty::mismatched-types", "mismatched types")
            .label(found_at, format!("expected `{expected}`, found `{found}`"));
        if let Some(because) = because {
            diag = diag.label(because, "expected because of this return type");
        }
        // ADR 0015: a value followed by `;` by habit.
        if let Some(Stmt::Expr {
            expr,
            semi: Some(semi),
        }) = block.stmts.last()
            && coerces(self.expr_types[expr.id.0 as usize], expected)
        {
            diag = diag
                .label(*semi, "remove this semicolon to return the value")
                .help("a block's value is its last expression without `;`");
        }
        self.diags.push(diag);
    }

    /// A block's type is its tail's, or `()` without one; a block that cannot
    /// reach its end has type `!`.
    fn block(&mut self, f: &Fn, block: &Block, expected: Option<Ty>) -> Ty {
        let mut diverges = false;
        for stmt in &block.stmts {
            let ty = match stmt {
                Stmt::Let { local, init } => self.check(f, init, Some(f.local(*local).ty)),
                // A statement without `;` (`if`, `loop`, ...) must not
                // silently drop a value (ADR 0015).
                Stmt::Expr { expr, semi: None } => self.check(f, expr, Some(Ty::Unit)),
                Stmt::Expr { expr, .. } => self.check(f, expr, None),
            };
            diverges |= ty == Ty::Never;
        }
        match &block.tail {
            Some(tail) => self.check(f, tail, expected),
            None if diverges => Ty::Never,
            None => Ty::Unit,
        }
    }

    /// Types `expr`, records the type, and reports a mismatch with
    /// `expected` if there is one. Returns the expression's own type.
    fn check(&mut self, f: &Fn, expr: &Expr, expected: Option<Ty>) -> Ty {
        let ty = self.expr_kind(f, expr, expected);
        self.expr_types[expr.id.0 as usize] = ty;
        // A block passes the expected type to its tail, which reports any
        // mismatch itself.
        let reported_inside = matches!(&expr.kind, ExprKind::Block(b) if b.tail.is_some());
        if let Some(expected) = expected
            && !coerces(ty, expected)
            && !reported_inside
        {
            match &expr.kind {
                ExprKind::Block(block) => self.report_block_mismatch(block, expected, ty, None),
                _ => self.mismatch(expr, expected, ty),
            }
        }
        ty
    }

    fn mismatch(&mut self, expr: &Expr, expected: Ty, found: Ty) {
        let mut diag = Diagnostic::error("ty::mismatched-types", "mismatched types")
            .label(expr.span, format!("expected `{expected}`, found `{found}`"));
        if matches!((expected, found), (Ty::Int(_), Ty::Int(_))) {
            diag = diag.help(format!(
                "integer types never convert implicitly; use `as {expected}` (ADR 0008)"
            ));
        }
        if expected == Ty::Unit && !matches!(found, Ty::Unit | Ty::Never | Ty::Error) {
            diag = diag.help("this value would be discarded; add `;` after it if that is intended");
        }
        if expected == Ty::Never && matches!(expr.kind, ExprKind::While { .. }) {
            diag =
                diag.help("a `while` loop can finish even with `true`; use `loop` to run forever");
        }
        self.diags.push(diag);
    }

    fn expr_kind(&mut self, f: &Fn, expr: &Expr, expected: Option<Ty>) -> Ty {
        match &expr.kind {
            ExprKind::Int(value) => self.int_literal(expr, *value, false, expected),
            ExprKind::Byte(_) => Ty::Int(IntTy::U8),
            ExprKind::Bool(_) => Ty::Bool,
            ExprKind::Unit => Ty::Unit,
            ExprKind::Local(local) => f.local(*local).ty,
            ExprKind::Call { callee, args } => self.call(f, expr, *callee, args),
            ExprKind::Unary(op, operand) => self.unary(f, expr, *op, operand, expected),
            ExprKind::Binary(op, lhs, rhs) if op.is_shift() => {
                let lhs_ty = self.check(f, lhs, expected);
                self.require(lhs, lhs_ty, op.symbol(), Operands::Int);
                self.shift_amount(f, rhs, lhs_ty, op.symbol());
                lhs_ty
            }
            ExprKind::Binary(op, lhs, rhs) if op.is_comparison() => {
                // Both sides have one type, never converted (ADR 0008).
                let ty = self.same_type_operands(f, lhs, rhs, None);
                let allowed = if matches!(op, BinOp::Eq | BinOp::Ne) {
                    Operands::Equatable
                } else {
                    Operands::Ordered
                };
                self.require(expr, ty, op.symbol(), allowed);
                Ty::Bool
            }
            ExprKind::Binary(op, lhs, rhs) => {
                let ty = self.same_type_operands(f, lhs, rhs, expected);
                let allowed = if op.is_bitwise() {
                    Operands::IntOrBool
                } else {
                    Operands::Int
                };
                self.require(expr, ty, op.symbol(), allowed);
                self.check_divisor(*op, rhs);
                ty
            }
            ExprKind::Logical(_, lhs, rhs) => {
                self.check(f, lhs, Some(Ty::Bool));
                self.check(f, rhs, Some(Ty::Bool));
                Ty::Bool
            }
            ExprKind::Cast(operand, target, target_span) => {
                self.cast(f, operand, *target, *target_span)
            }
            ExprKind::Deref(_) | ExprKind::Index(..) => {
                let info = self
                    .place_info(f, expr)
                    .expect("dereferences and indexing are places");
                self.access(&info, expr.span);
                info.ty
            }
            ExprKind::AddrOf { mutable, place } => self.addr_of(f, *mutable, place),
            ExprKind::Assign(place, value) => {
                let place_ty = self.place(f, place);
                self.check(f, value, Some(place_ty));
                Ty::Unit
            }
            ExprKind::CompoundAssign(op, place, value) => {
                let place_ty = self.place(f, place);
                let allowed = if op.is_bitwise() {
                    Operands::IntOrBool
                } else {
                    Operands::Int
                };
                self.require(place, place_ty, op.symbol(), allowed);
                if op.is_shift() {
                    self.shift_amount(f, value, place_ty, op.symbol());
                } else {
                    self.check(f, value, Some(place_ty));
                    self.check_divisor(*op, value);
                }
                Ty::Unit
            }
            ExprKind::Block(block) => self.block(f, block, expected),
            ExprKind::If { cond, then, else_ } => {
                self.check(f, cond, Some(Ty::Bool));
                let Some(else_) = else_ else {
                    // Without `else`, the missing branch has type `()`.
                    if let Some(want) = expected
                        && !coerces(Ty::Unit, want)
                    {
                        self.diags.push(
                            Diagnostic::error(
                                "ty::if-without-else",
                                format!("`if` without `else` has no `{want}` value"),
                            )
                            .label(expr.span, format!("expected `{want}`"))
                            .help("add an `else` branch for when the condition is false"),
                        );
                        self.block(f, then, Some(want));
                        return Ty::Error;
                    }
                    self.block(f, then, Some(Ty::Unit));
                    return Ty::Unit;
                };
                match expected {
                    Some(expected) => {
                        let then_ty = self.checked_block(f, then, expected, None);
                        let else_ty = self.check(f, else_, Some(expected));
                        // If neither branch finishes, neither does the `if`,
                        // so the code after it is unreachable.
                        if then_ty == Ty::Never && else_ty == Ty::Never {
                            Ty::Never
                        } else {
                            expected
                        }
                    }
                    None => {
                        // The first branch that can finish gives the type.
                        let then_ty = self.block(f, then, None);
                        let else_expected = (then_ty != Ty::Never).then_some(then_ty);
                        let else_ty = self.check(f, else_, else_expected);
                        if then_ty == Ty::Never {
                            else_ty
                        } else {
                            then_ty
                        }
                    }
                }
            }
            ExprKind::While { cond, body } => {
                self.check(f, cond, Some(Ty::Bool));
                self.in_loop(expr.id, None, |this| this.block(f, body, Some(Ty::Unit)));
                // The condition is not evaluated at compile time, so even
                // `while true` may finish.
                Ty::Unit
            }
            ExprKind::Loop(body) => {
                let ctx = self.in_loop(expr.id, expected, |this| {
                    this.block(f, body, Some(Ty::Unit))
                });
                // Without a `break`, a loop never finishes.
                ctx.break_ty.unwrap_or(Ty::Never)
            }
            ExprKind::Break { target, value } => {
                self.break_(f, expr, *target, value.as_deref());
                Ty::Never
            }
            ExprKind::Continue { .. } => Ty::Never,
            ExprKind::Return(value) => {
                match value {
                    Some(value) => {
                        self.check(f, value, Some(f.ret));
                    }
                    None if !coerces(Ty::Unit, f.ret) => self.mismatch(expr, f.ret, Ty::Unit),
                    None => {}
                }
                Ty::Never
            }
        }
    }

    /// Each argument is checked against its parameter's type.
    fn call(&mut self, f: &Fn, expr: &Expr, callee: hir::FnId, args: &[Expr]) -> Ty {
        let target = self.hir.fn_(callee);
        let params = target.params();
        if args.len() != params.len() {
            let plural = if params.len() == 1 { "" } else { "s" };
            self.diags.push(
                Diagnostic::error(
                    "ty::wrong-arg-count",
                    format!(
                        "`{}` takes {} argument{plural}, but {} {} given",
                        target.name,
                        params.len(),
                        args.len(),
                        if args.len() == 1 { "was" } else { "were" }
                    ),
                )
                .label(expr.span, "in this call")
                .label(target.name_span, "declared here"),
            );
        }
        for (i, arg) in args.iter().enumerate() {
            // Extra arguments were reported above; `{error}` keeps them quiet.
            let expected = params.get(i).map_or(Ty::Error, |param| param.ty);
            self.check(f, arg, Some(expected));
        }
        target.ret
    }

    /// Division by zero panics at run time (ADR 0007); a literal zero
    /// divisor is certainly a mistake, so it is rejected here.
    fn check_divisor(&mut self, op: BinOp, divisor: &Expr) {
        if matches!(op, BinOp::Div | BinOp::Rem) && literal_value(divisor) == Some(0) {
            self.diags.push(
                Diagnostic::error("ty::division-by-zero", "division by zero")
                    .label(divisor.span, "this divisor is zero")
                    .help("at run time this would call the panic function (ADR 0007)"),
            );
        }
    }

    fn in_loop(
        &mut self,
        id: ExprId,
        expected: Option<Ty>,
        body: impl FnOnce(&mut Self) -> Ty,
    ) -> LoopCtx {
        self.loops.push(LoopCtx {
            id,
            expected,
            break_ty: None,
        });
        body(self);
        self.loops.pop().expect("pushed above")
    }

    /// The value of a `break` must have the loop's expected type or, without
    /// one, the type of the loop's first `break`.
    fn break_(&mut self, f: &Fn, expr: &Expr, target: ExprId, value: Option<&Expr>) {
        let index = self
            .loops
            .iter()
            .rposition(|ctx| ctx.id == target)
            .expect("lowering resolved the loop");
        let ctx = &self.loops[index];
        let want = ctx.expected.or(ctx.break_ty);
        let ty = match value {
            Some(value) => self.check(f, value, want),
            None => {
                if let Some(want) = want
                    && !coerces(Ty::Unit, want)
                {
                    self.mismatch(expr, want, Ty::Unit);
                }
                Ty::Unit
            }
        };
        let ctx = &mut self.loops[index];
        if ctx.break_ty.is_none() {
            ctx.break_ty = Some(want.unwrap_or(ty));
        }
    }

    /// `negated` covers `-128` for `i8`, whose magnitude is one past the max.
    fn int_literal(&mut self, expr: &Expr, value: u64, negated: bool, expected: Option<Ty>) -> Ty {
        match expected {
            Some(Ty::Int(int)) => {
                let limit = if negated && int.is_signed() {
                    int.max() + 1
                } else {
                    int.max()
                };
                if value > limit {
                    let sign = if negated { "-" } else { "" };
                    self.diags.push(
                        Diagnostic::error(
                            "ty::literal-out-of-range",
                            format!("literal out of range for `{int}`"),
                        )
                        .label(
                            expr.span,
                            format!("`{sign}{value}` does not fit in `{int}`"),
                        ),
                    );
                }
                Ty::Int(int)
            }
            Some(Ty::Error) => Ty::Error,
            Some(other) => {
                self.diags.push(
                    Diagnostic::error("ty::mismatched-types", "mismatched types")
                        .label(expr.span, format!("expected `{other}`, found an integer")),
                );
                Ty::Error
            }
            None => {
                self.diags.push(
                    Diagnostic::error(
                        "ty::untyped-literal",
                        "cannot tell the type of this literal",
                    )
                    .label(expr.span, "no type is expected here")
                    .help("literals take the type expected where they appear; bind it first, e.g. `let x: u32 = ...` (ADR 0008)"),
                );
                Ty::Error
            }
        }
    }

    fn unary(&mut self, f: &Fn, expr: &Expr, op: UnOp, operand: &Expr, expected: Option<Ty>) -> Ty {
        let ty = match (op, &operand.kind) {
            (UnOp::Neg, ExprKind::Int(value)) => {
                let ty = self.int_literal(expr, *value, true, expected);
                self.expr_types[operand.id.0 as usize] = ty;
                ty
            }
            _ => self.check(f, operand, expected),
        };
        match (op, ty) {
            (UnOp::Not, ty) => {
                self.require(expr, ty, "!", Operands::IntOrBool);
                ty
            }
            (UnOp::Neg, Ty::Int(int)) if !int.is_signed() => {
                self.diags.push(
                    Diagnostic::error(
                        "ty::negate-unsigned",
                        format!("cannot negate a value of unsigned type `{int}`"),
                    )
                    .label(expr.span, "unsigned")
                    .help("for a wrapping negation, write `0 - x`"),
                );
                Ty::Error
            }
            (UnOp::Neg, ty) => {
                self.require(expr, ty, "-", Operands::Int);
                ty
            }
        }
    }

    /// Both operands must have the same integer type. Whichever side can be
    /// typed alone gives its type to the other (`1 + x` and `x + 1`).
    fn same_type_operands(&mut self, f: &Fn, lhs: &Expr, rhs: &Expr, expected: Option<Ty>) -> Ty {
        if expected.is_none() && needs_expected_type(lhs) && !needs_expected_type(rhs) {
            let rhs_ty = self.check(f, rhs, None);
            self.check(f, lhs, Some(rhs_ty));
            rhs_ty
        } else {
            let lhs_ty = self.check(f, lhs, expected);
            self.check(f, rhs, Some(lhs_ty));
            lhs_ty
        }
    }

    /// Shift amounts may be any integer type and are masked to the operand
    /// width (ADR 0007); a literal amount takes the shifted type, and a
    /// constant amount must be smaller than the width.
    fn shift_amount(&mut self, f: &Fn, amount: &Expr, shifted: Ty, op: &str) {
        if needs_expected_type(amount) {
            self.check(f, amount, Some(shifted));
        } else {
            let ty = self.check(f, amount, None);
            self.require(amount, ty, op, Operands::Int);
        }
        if let (Ty::Int(int), Some(value)) = (shifted, literal_value(amount))
            && !(0..int.bits() as i128).contains(&value)
        {
            self.diags.push(
                Diagnostic::error(
                    "ty::shift-out-of-range",
                    format!("shift amount out of range for `{int}`"),
                )
                .label(
                    amount.span,
                    format!("must be between 0 and {}", int.bits() - 1),
                )
                .help(format!(
                    "shifts mask their amount, so this would shift by {} (ADR 0007)",
                    value.rem_euclid(int.bits() as i128)
                )),
            );
        }
    }

    /// Integers and pointers convert to each other with `as` (ADR 0013). A
    /// literal operand takes the target type, or `usize` for a pointer
    /// target (ADR 0017).
    fn cast(&mut self, f: &Fn, operand: &Expr, target: Ty, target_span: Span) -> Ty {
        let literal_ty = match target {
            Ty::Int(_) => Some(target),
            Ty::Ptr(_) => Some(Ty::Int(IntTy::Usize)),
            _ => None,
        };
        let expected = if needs_expected_type(operand) {
            literal_ty
        } else {
            None
        };
        let from = self.check(f, operand, expected);
        let castable = |ty: Ty| matches!(ty, Ty::Int(_) | Ty::Ptr(_) | Ty::Error | Ty::Never);
        if from == Ty::Bool {
            // `true as u8 == 1`; a bool converts to integers only.
            if !matches!(target, Ty::Int(_) | Ty::Error) {
                self.diags.push(
                    Diagnostic::error(
                        "ty::invalid-cast",
                        format!("cannot cast `bool` to `{target}`"),
                    )
                    .label(target_span, "a `bool` converts only to integer types"),
                );
                return Ty::Error;
            }
            return target;
        }
        if !castable(from) {
            self.diags.push(
                Diagnostic::error("ty::invalid-cast", format!("cannot cast `{from}`")).label(
                    operand.span,
                    "only integers, pointers and `bool` can be cast",
                ),
            );
        }
        if target == Ty::Bool {
            self.diags.push(
                Diagnostic::error("ty::invalid-cast", "cannot cast to `bool`")
                    .label(target_span, "no conversion to `bool`")
                    .help("say what you mean with a comparison, such as `x != 0`"),
            );
            return Ty::Error;
        }
        if !castable(target) {
            self.diags.push(
                Diagnostic::error("ty::invalid-cast", format!("cannot cast to `{target}`"))
                    .label(target_span, "only integers and pointers can be cast to"),
            );
            return Ty::Error;
        }
        // Pointers convert only to and from `usize`, so any widening or
        // narrowing is a visible integer conversion (ADR 0019).
        let usize_ty = Ty::Int(IntTy::Usize);
        match (from, target) {
            (Ty::Int(_), Ty::Ptr(_)) if from != usize_ty => {
                self.diags.push(
                    Diagnostic::error(
                        "ty::pointer-cast-width",
                        format!("cannot cast `{from}` to a pointer"),
                    )
                    .label(operand.span, format!("has type `{from}`"))
                    .help(format!(
                        "only `usize` converts to pointers; write `x as usize as {target}` (ADR 0019)"
                    )),
                );
            }
            (Ty::Ptr(_), Ty::Int(_)) if target != usize_ty => {
                self.diags.push(
                    Diagnostic::error(
                        "ty::pointer-cast-width",
                        format!("cannot cast a pointer to `{target}`"),
                    )
                    .label(target_span, "not `usize`")
                    .help(format!(
                        "pointers convert only to `usize`; write `p as usize as {target}` (ADR 0019)"
                    )),
                );
            }
            _ => {}
        }
        target
    }

    /// The place a pointer of type `ty` leads to, for `*` and `[]`. A
    /// pointer of type `!` is never computed, so neither is the place.
    fn through_pointer(&mut self, ty: Ty, pointer: &Expr, error: Diagnostic) -> PlaceInfo {
        match ty {
            Ty::Ptr(ptr) => PlaceInfo::through(ptr, pointer.span),
            Ty::Never => PlaceInfo {
                ty: Ty::Never,
                ..PlaceInfo::ERROR
            },
            Ty::Error => PlaceInfo::ERROR,
            _ => {
                self.diags.push(error.label(pointer.span, "not a pointer"));
                PlaceInfo::ERROR
            }
        }
    }

    /// Checks a read or write of a place. A volatile access is exactly one
    /// instruction (ADR 0023), so it cannot be wider than the target's
    /// widest access.
    fn access(&mut self, place: &PlaceInfo, span: Span) {
        let volatile = place.via.is_some_and(|ptr| ptr.volatile);
        if let (true, Ty::Int(int)) = (volatile, place.ty)
            && int.bits() > target::WIDEST_ACCESS_BITS
        {
            let half = target::WIDEST_ACCESS_BITS;
            self.diags.push(
                Diagnostic::error(
                    "ty::volatile-too-wide",
                    format!("a volatile `{int}` cannot be accessed in one instruction"),
                )
                .label(span, format!("the widest single access is {half} bits"))
                .help(format!(
                    "access it as `u{half}` halves, in the order the device needs (ADR 0023)"
                )),
            );
        }
    }

    /// Types a place expression: a local, `*pointer` or `pointer[index]`.
    /// Returns `None`, having typed nothing, if `expr` is not a place.
    fn place_info(&mut self, f: &Fn, expr: &Expr) -> Option<PlaceInfo> {
        let info = match &expr.kind {
            ExprKind::Local(id) => {
                let local = f.local(*id);
                PlaceInfo {
                    ty: local.ty,
                    immutable: (!local.mutable).then_some(Immutable::Local(*id)),
                    via: None,
                }
            }
            ExprKind::Deref(pointer) => {
                let ty = self.check(f, pointer, None);
                let error = Diagnostic::error(
                    "ty::deref-non-pointer",
                    format!("cannot dereference `{ty}`"),
                );
                self.through_pointer(ty, pointer, error)
            }
            ExprKind::Index(base, index) => {
                let ty = self.check(f, base, None);
                self.check(f, index, Some(Ty::Int(IntTy::Usize)));
                let error = Diagnostic::error(
                    "ty::cannot-index",
                    format!("cannot index a value of type `{ty}`"),
                );
                // Unchecked, as in C (ADR 0021).
                self.through_pointer(ty, base, error)
            }
            _ => return None,
        };
        self.expr_types[expr.id.0 as usize] = info.ty;
        Some(info)
    }

    /// Checks that `place` can be assigned to and returns its type.
    fn place(&mut self, f: &Fn, place: &Expr) -> Ty {
        let Some(info) = self.place_info(f, place) else {
            self.check(f, place, None);
            self.diags.push(
                Diagnostic::error("ty::not-assignable", "cannot assign to this expression")
                    .label(place.span, "not a variable, `*pointer` or `pointer[index]`"),
            );
            return Ty::Error;
        };
        match info.immutable {
            Some(Immutable::Local(id)) => {
                let local = f.local(id);
                self.diags.push(
                    Diagnostic::error(
                        "ty::assign-immutable",
                        format!("cannot assign to `{}`, which is not `mut`", local.name),
                    )
                    .label(place.span, "assigned here")
                    .label(
                        local.span,
                        format!("help: declare it `let mut {}`", local.name),
                    ),
                );
            }
            Some(Immutable::ConstPointer(ptr, pointer)) => {
                self.diags.push(
                    Diagnostic::error(
                        "ty::write-through-const",
                        "cannot write through a `*const` pointer",
                    )
                    .label(pointer, format!("has type `{}`", Ty::Ptr(ptr)))
                    .help("only `*mut` pointers can be written through"),
                );
            }
            None => {}
        }
        self.access(&info, place.span);
        info.ty
    }

    /// `&raw const place` or `&raw mut place` (ADR 0021). The address of a
    /// place is taken without accessing it.
    fn addr_of(&mut self, f: &Fn, mutable: bool, place: &Expr) -> Ty {
        let Some(info) = self.place_info(f, place) else {
            self.check(f, place, None);
            self.diags.push(
                Diagnostic::error("ty::not-a-place", "`&raw` needs a place in memory")
                    .label(place.span, "a value, not a place")
                    .help("take the address of a variable, `*pointer` or `pointer[index]`; to point at a value, store it in a `let` first"),
            );
            return Ty::Error;
        };
        if mutable {
            match info.immutable {
                Some(Immutable::Local(id)) => {
                    let local = f.local(id);
                    self.diags.push(
                        Diagnostic::error(
                            "ty::address-of-immutable",
                            format!(
                                "cannot take a `*mut` address of `{}`, which is not `mut`",
                                local.name
                            ),
                        )
                        .label(place.span, "address taken here")
                        .label(
                            local.span,
                            format!("help: declare it `let mut {}`", local.name),
                        ),
                    );
                }
                Some(Immutable::ConstPointer(ptr, pointer)) => {
                    self.diags.push(
                        Diagnostic::error(
                            "ty::address-of-immutable",
                            "cannot take a `*mut` address through a `*const` pointer",
                        )
                        .label(pointer, format!("has type `{}`", Ty::Ptr(ptr)))
                        .help("use `&raw const`, or a `*mut` pointer"),
                    );
                }
                None => {}
            }
        }
        match info.ty {
            Ty::Error => Ty::Error,
            ty => Ty::ptr(mutable, false, ty),
        }
    }

    fn require(&mut self, expr: &Expr, ty: Ty, op: &str, allowed: Operands) {
        let ok = match ty {
            Ty::Error | Ty::Never => true,
            Ty::Int(_) => true,
            Ty::Bool => matches!(allowed, Operands::IntOrBool | Operands::Equatable),
            Ty::Ptr(_) => matches!(allowed, Operands::Equatable | Operands::Ordered),
            Ty::Unit => false,
        };
        if !ok {
            let what = match allowed {
                Operands::Int => "an integer",
                Operands::IntOrBool => "an integer or `bool`",
                Operands::Equatable => "integers, pointers or `bool`",
                Operands::Ordered => "integers or pointers",
            };
            self.diags.push(
                Diagnostic::error(
                    "ty::invalid-operand",
                    format!("`{op}` needs {what}, found `{ty}`"),
                )
                .label(expr.span, format!("has type `{ty}`")),
            );
        }
    }
}

/// Lists every function's locals and expression types, for `--emit=typeck`.
pub fn dump(hir: &Hir, results: &TypeckResults) -> String {
    let mut out = String::new();
    for (_, f) in hir.fns() {
        let kind = match f.kind {
            FnKind::Plain => "",
            FnKind::Entry => "entry ",
            FnKind::Panic => "panic ",
        };
        let params: Vec<String> = f
            .params()
            .iter()
            .map(|p| format!("{}: {}", p.name, p.ty))
            .collect();
        writeln!(
            out,
            "{kind}fn {}({}) -> {}",
            f.name,
            params.join(", "),
            f.ret
        )
        .unwrap();
        for local in &f.locals[f.param_count..] {
            let mutable = if local.mutable { "mut " } else { "" };
            writeln!(out, "    let {mutable}{}: {}", local.name, local.ty).unwrap();
        }
        f.body.walk(&mut |e| {
            let ty = results.expr_ty(e.id);
            writeln!(
                out,
                "    expr {} {} {:?}: {ty}",
                e.id.0,
                expr_label(f, e),
                e.span
            )
            .unwrap();
        });
    }
    out
}

/// The types an operator accepts.
#[derive(Clone, Copy)]
enum Operands {
    Int,
    IntOrBool,
    /// `==` and `!=`
    Equatable,
    /// `<`, `<=`, `>`, `>=`
    Ordered,
}

fn expr_label(f: &Fn, expr: &Expr) -> String {
    match &expr.kind {
        ExprKind::Bool(value) => format!("bool {value}"),
        ExprKind::Logical(op, _, _) => format!("logical {}", op.symbol()),
        ExprKind::If { .. } => "if".to_owned(),
        ExprKind::While { .. } => "while".to_owned(),
        ExprKind::Break { .. } => "break".to_owned(),
        ExprKind::Continue { .. } => "continue".to_owned(),
        ExprKind::Return(_) => "return".to_owned(),
        ExprKind::Int(value) => format!("int {value}"),
        ExprKind::Byte(value) => format!("byte {value}"),
        ExprKind::Unit => "unit".to_owned(),
        ExprKind::Local(id) => format!("local {}", f.local(*id).name),
        ExprKind::Call { .. } => "call".to_owned(),
        ExprKind::Unary(op, _) => format!("{op:?}").to_lowercase(),
        ExprKind::Deref(_) => "deref".to_owned(),
        ExprKind::Index(..) => "index".to_owned(),
        ExprKind::AddrOf { mutable: true, .. } => "&raw mut".to_owned(),
        ExprKind::AddrOf { mutable: false, .. } => "&raw const".to_owned(),
        ExprKind::Binary(op, _, _) => format!("binary {}", op.symbol()),
        ExprKind::Cast(_, ty, _) => format!("as {ty}"),
        ExprKind::Assign(..) => "assign".to_owned(),
        ExprKind::CompoundAssign(op, _, _) => format!("assign {}=", op.symbol()),
        ExprKind::Block(_) => "block".to_owned(),
        ExprKind::Loop(_) => "loop".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_and_error_coerce_to_anything() {
        let u8_ = Ty::Int(IntTy::U8);
        assert!(coerces(Ty::Never, u8_));
        assert!(coerces(Ty::Error, Ty::Unit));
        assert!(coerces(u8_, Ty::Error));
        assert!(!coerces(Ty::Int(IntTy::U32), u8_));
        assert!(!coerces(Ty::Unit, Ty::Never));
    }
}
