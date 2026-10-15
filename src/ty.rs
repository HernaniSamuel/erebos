//! Type checking (ADR 0008): every type is written or fixed by the
//! construct, so this pass only checks; it never infers.
//!
//! Checking is bidirectional: an expression may receive the type expected
//! where it appears. That is how literals get their types (`let x: u8 = 1`)
//! and why `x + 1` works: the `1` is checked against the type of `x`.
//!
//! The result is a side table from [`ExprId`] to [`Ty`]; HIR plus this table
//! is the "typed HIR" that MIR building consumes. Functions are checked one
//! at a time, on demand (ADR 0028).

use std::collections::HashMap;
use std::fmt::Write;

use crate::diag::Diagnostic;
use crate::hir::{
    self, BinOp, Block, Expr, ExprId, ExprKind, Fn, FnKind, Hir, IntTy, LocalId, Ptr, Stmt, Ty,
    UnOp,
};
use crate::layout;
use crate::query::Queries;
use crate::span::Span;
use crate::target;

/// What type checking found out about expressions, for every function
/// checked so far.
pub struct Tables {
    expr_types: Vec<Ty>,
    /// The field index of each field access.
    fields: HashMap<ExprId, u32>,
}

impl Tables {
    pub fn new(expr_count: u32) -> Tables {
        Tables {
            // Overwritten for every expression checked; the placeholder is
            // never read.
            expr_types: vec![Ty::Error; expr_count as usize],
            fields: HashMap::new(),
        }
    }

    pub fn expr_ty(&self, id: ExprId) -> Ty {
        self.expr_types[id.0 as usize]
    }

    /// The index of the field that the field access `id` reads.
    pub fn field(&self, id: ExprId) -> u32 {
        self.fields[&id]
    }
}

/// Checks one function; `query` decides when.
pub fn check_fn(q: &mut Queries, f: &Fn) {
    let hir = q.hir;
    let mut checker = Checker {
        q,
        hir,
        ret: Ty::Error,
        loops: Vec::new(),
    };
    checker.fn_(f);
}

/// Whether a value of type `from` may be used where `to` is expected.
/// `!` has no values, so it fits anywhere; `{error}` was already reported.
/// The one conversion is from `*mut T` to `*const T` (ADR 0024).
fn coerces(from: Ty, to: Ty) -> bool {
    from == to
        || matches!(from, Ty::Never | Ty::Error)
        || to == Ty::Error
        || matches!((from, to), (Ty::Ptr(from), Ty::Ptr(to))
            if from.mutable && !to.mutable
                && from.volatile == to.volatile
                && from.pointee == to.pointee)
}

/// The type that accepts `ty` and, if it is a pointer, the pointer with the
/// other mutability: the type a comparison expects of its second operand.
fn comparable(ty: Ty) -> Ty {
    match ty {
        Ty::Ptr(ptr) => Ty::Ptr(Ptr {
            mutable: false,
            ..ptr
        }),
        ty => ty,
    }
}

/// Expressions that cannot be typed on their own: integer literals, and
/// operators applied only to them.
fn needs_expected_type(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Int(_) => true,
        ExprKind::Unary(_, operand) | ExprKind::Comptime(operand) => needs_expected_type(operand),
        // A comparison is a `bool` whatever its operands are.
        ExprKind::Binary(op, _, _) if op.is_comparison() => false,
        ExprKind::Binary(op, lhs, _) if op.is_shift() => needs_expected_type(lhs),
        ExprKind::Binary(_, lhs, rhs) => needs_expected_type(lhs) && needs_expected_type(rhs),
        _ => false,
    }
}

/// The element type an expected array type asks for.
fn element_of(expected: Option<Ty>) -> Option<Ty> {
    match expected {
        Some(Ty::Array(elem, _)) => Some(*elem),
        Some(Ty::Error) => Some(Ty::Error),
        _ => None,
    }
}

struct Checker<'a, 'h> {
    q: &'a mut Queries<'h>,
    hir: &'h Hir,
    /// The return type of the function being checked, normalized.
    ret: Ty,
    /// Enclosing `loop`/`while` expressions, innermost last.
    loops: Vec<LoopCtx>,
}

/// What the type checker knows about a place expression.
#[derive(Clone, Copy)]
struct PlaceInfo {
    ty: Ty,
    /// Why the place cannot be written, if it cannot.
    immutable: Option<Immutable>,
    /// The last pointer on the path to the place, which decides whether
    /// accesses are volatile (ADR 0023).
    via: Option<Ptr>,
    /// The path from that pointer goes through a field of a `#[packed]`
    /// struct, so the place may be misaligned (ADR 0022).
    misaligned: bool,
    /// Part of a constant's value, which has no address (ADR 0029): it
    /// can be read, but `&raw` of it is not a place.
    constant: bool,
}

impl PlaceInfo {
    /// The place of an expression that failed to type; already reported.
    const ERROR: PlaceInfo = PlaceInfo {
        ty: Ty::Error,
        immutable: None,
        via: None,
        misaligned: false,
        constant: false,
    };

    /// The value of `base`, which is not a place expression, indexed or
    /// projected where it is.
    fn temporary(ty: Ty, base: &Expr) -> PlaceInfo {
        PlaceInfo {
            ty,
            immutable: Some(Immutable::Temporary(base.span)),
            via: None,
            misaligned: false,
            constant: matches!(base.kind, ExprKind::Const(_)),
        }
    }

    /// The place a pointer points to; `pointer` is the pointer's expression.
    fn through(ptr: Ptr, pointer: Span) -> PlaceInfo {
        PlaceInfo {
            ty: *ptr.pointee,
            immutable: (!ptr.mutable).then_some(Immutable::ConstPointer(ptr, pointer)),
            via: Some(ptr),
            // Pointers are assumed aligned (ADR 0019).
            misaligned: false,
            constant: false,
        }
    }
}

#[derive(Clone, Copy)]
enum Immutable {
    /// A local declared without `mut`.
    Local(LocalId),
    /// A place reached through a `*const` pointer, written at the span.
    ConstPointer(Ptr, Span),
    /// Part of a value that lives in no variable, written at the span.
    Temporary(Span),
}

struct LoopCtx {
    id: ExprId,
    /// The type expected of the loop's value, if any.
    expected: Option<Ty>,
    /// The type of the first `break`, which later ones must match.
    break_ty: Option<Ty>,
}

impl Checker<'_, '_> {
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
        self.ret = self.norm(f.ret);
        if let Some((code, what, why)) = special
            && f.ret != Ty::Never
        {
            self.q.diags.push(
                Diagnostic::error(code, format!("the {what} function must never return"))
                    .label(
                        f.ret_span.unwrap_or(f.name_span),
                        format!("return type is `{}`", f.ret),
                    )
                    .help(format!("{why}; declare it `-> !` and end it with a `loop`")),
            );
        }

        self.params(f);
        // A constant's type is not a return type.
        let because = if f.comptime.is_some() {
            None
        } else {
            f.ret_span
        };
        for local in &f.locals {
            if self.q.layout_of(local.ty).is_none() {
                self.too_large(local.ty, local.span, "does not fit in the address space");
            }
        }

        if let Some(body) = &f.body {
            self.checked_block(f, body, self.ret, because);
        }
    }

    /// A type as written, with its array lengths computed (ADR 0029).
    fn norm(&mut self, ty: Ty) -> Ty {
        self.q.normalize(ty)
    }

    fn local_ty(&mut self, f: &Fn, id: LocalId) -> Ty {
        self.norm(f.local(id).ty)
    }

    /// The type queried by `size_of`, `align_of` or `offset_of` must have a
    /// layout (ADR 0022).
    fn queried_layout(&mut self, ty: Ty, span: Span) {
        if self.q.layout_of(ty).is_none() {
            self.too_large(ty, span, "larger than any object can be");
        }
    }

    fn too_large(&mut self, ty: Ty, span: Span, label: &str) {
        self.q.diags.push(
            Diagnostic::error("ty::too-large", format!("`{ty}` is too large"))
                .label(span, label)
                .help(format!(
                    "objects are limited to {} bytes on this target",
                    layout::MAX_SIZE
                )),
        );
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
            self.q.diags.push(
                Diagnostic::error(
                    "ty::special-fn-params",
                    format!("the {what} function takes no parameters"),
                )
                .label(first.span, "parameter here"),
            );
        }
        for param in f.params() {
            if matches!(param.ty, Ty::Unit | Ty::Never) {
                self.q.diags.push(
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
            && coerces(self.q.tables.expr_types[expr.id.0 as usize], expected)
        {
            diag = diag
                .label(*semi, "remove this semicolon to return the value")
                .help("a block's value is its last expression without `;`");
        }
        self.q.diags.push(diag);
    }

    /// A block's type is its tail's, or `()` without one; a block that cannot
    /// reach its end has type `!`.
    fn block(&mut self, f: &Fn, block: &Block, expected: Option<Ty>) -> Ty {
        let mut diverges = false;
        for stmt in &block.stmts {
            let ty = match stmt {
                Stmt::Let { local, init } => {
                    let ty = self.local_ty(f, *local);
                    self.check(f, init, Some(ty))
                }
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
        self.q.tables.expr_types[expr.id.0 as usize] = ty;
        // Variables were checked where they are declared; any other
        // aggregate value needs a temporary of its size.
        if ty.is_aggregate()
            && !matches!(expr.kind, ExprKind::Local(_))
            && self.q.layout_of(ty).is_none()
        {
            self.too_large(
                ty,
                expr.span,
                "this value does not fit in the address space",
            );
        }
        // A block passes the expected type to its tail, which reports any
        // mismatch itself.
        let reported_inside = matches!(&expr.kind, ExprKind::Block(b) if b.tail.is_some())
            || matches!(expr.kind, ExprKind::Comptime(_));
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
        if let (Ty::Ptr(want), Ty::Ptr(have)) = (expected, found)
            && want.mutable
            && !have.mutable
        {
            diag = diag.help(
                "a `*const` pointer never becomes `*mut` implicitly; cast with `as` if writing through it is allowed",
            );
        }
        if expected == Ty::Never && matches!(expr.kind, ExprKind::While { .. }) {
            diag =
                diag.help("a `while` loop can finish even with `true`; use `loop` to run forever");
        }
        self.q.diags.push(diag);
    }

    fn expr_kind(&mut self, f: &Fn, expr: &Expr, expected: Option<Ty>) -> Ty {
        match &expr.kind {
            ExprKind::Int(value) => self.int_literal(expr, *value, false, expected),
            ExprKind::Byte(_) => Ty::Int(IntTy::U8),
            ExprKind::Bool(_) => Ty::Bool,
            ExprKind::Unit => Ty::Unit,
            ExprKind::Array(elems) => self.array(f, expr, elems, expected),
            ExprKind::Repeat(value, len) => {
                let ty = self.check(f, value, element_of(expected));
                match (ty, self.q.array_len(*len)) {
                    (Ty::Error, _) | (_, None) => Ty::Error,
                    (ty, Some(len)) => Ty::array(ty, len),
                }
            }
            ExprKind::Local(local) => self.local_ty(f, *local),
            ExprKind::Const(id) => self.norm(self.hir.const_(*id).ty),
            // The value is computed later; its type is checked here, with
            // the expected type passed through (ADR 0029).
            ExprKind::Comptime(inner) => self.check(f, inner, expected),
            ExprKind::SizeOf(ty) | ExprKind::AlignOf(ty) => {
                self.queried_layout(*ty, expr.span);
                Ty::Int(IntTy::Usize)
            }
            ExprKind::OffsetOf(ty, name, name_span) => {
                self.queried_layout(*ty, expr.span);
                match ty {
                    // The struct is not accessed: any place will do.
                    Ty::Struct(_) => {
                        self.field(expr, *ty, PlaceInfo::ERROR, name, *name_span);
                    }
                    Ty::Error => {}
                    // Even `!`: unlike an expression, a written type is
                    // not unreachable code.
                    other => self.q.diags.push(
                        Diagnostic::error("ty::no-fields", format!("`{other}` has no fields"))
                            .label(*name_span, "no such field"),
                    ),
                }
                Ty::Int(IntTy::Usize)
            }
            ExprKind::StructLit(s, fields) => {
                let hir = self.hir;
                let def = hir.struct_(s.id);
                for (index, value) in fields {
                    let ty = self.norm(def.fields[*index as usize].ty);
                    self.check(f, value, Some(ty));
                }
                Ty::Struct(*s)
            }
            ExprKind::Call { callee, args } => self.call(f, expr, *callee, args),
            ExprKind::Unary(op, operand) => self.unary(f, expr, *op, operand, expected),
            ExprKind::Binary(op, lhs, rhs) if op.is_shift() => {
                let lhs_ty = self.check(f, lhs, expected);
                self.require(lhs, lhs_ty, op.symbol(), Operands::Int);
                self.shift_amount(f, rhs, lhs_ty, op.symbol());
                lhs_ty
            }
            ExprKind::Binary(op, lhs, rhs) if op.is_comparison() => {
                // Both sides have one type, never converted (ADR 0008),
                // except that pointers may differ in mutability (ADR 0024).
                let ty = self.same_type_operands(f, lhs, rhs, None, comparable);
                let allowed = if matches!(op, BinOp::Eq | BinOp::Ne) {
                    Operands::Equatable
                } else {
                    Operands::Ordered
                };
                self.require(expr, ty, op.symbol(), allowed);
                Ty::Bool
            }
            ExprKind::Binary(op, lhs, rhs) => {
                let ty = self.same_type_operands(f, lhs, rhs, expected, |ty| ty);
                let allowed = if op.is_bitwise() {
                    Operands::IntOrBool
                } else {
                    Operands::Int
                };
                self.require(expr, ty, op.symbol(), allowed);
                ty
            }
            ExprKind::Logical(_, lhs, rhs) => {
                self.check(f, lhs, Some(Ty::Bool));
                self.check(f, rhs, Some(Ty::Bool));
                Ty::Bool
            }
            ExprKind::Cast(operand, target, target_span) => {
                let target = self.norm(*target);
                self.cast(f, operand, target, *target_span)
            }
            ExprKind::Deref(_) | ExprKind::Index(..) | ExprKind::Field(..) => {
                let info = self
                    .place_info(f, expr)
                    .expect("dereferences, indexing and fields are places");
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
                }
                Ty::Unit
            }
            ExprKind::Assert(cond) => {
                self.check(f, cond, Some(Ty::Bool));
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
                        self.q.diags.push(
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
                        self.check(f, value, Some(self.ret));
                    }
                    None if !coerces(Ty::Unit, self.ret) => self.mismatch(expr, self.ret, Ty::Unit),
                    None => {}
                }
                Ty::Never
            }
        }
    }

    /// `[a, b, c]`: the elements share one type, which the expected array
    /// type gives or, without one, the first element typed on its own.
    fn array(&mut self, f: &Fn, expr: &Expr, elems: &[Expr], expected: Option<Ty>) -> Ty {
        let elem_ty = match element_of(expected) {
            Some(want) => {
                for elem in elems {
                    self.check(f, elem, Some(want));
                }
                want
            }
            None => match elems.iter().position(|e| !needs_expected_type(e)) {
                Some(first) => {
                    let ty = self.check(f, &elems[first], None);
                    for (i, elem) in elems.iter().enumerate() {
                        if i != first {
                            self.check(f, elem, Some(ty));
                        }
                    }
                    ty
                }
                None => {
                    self.q.diags.push(
                        Diagnostic::error(
                            "ty::untyped-literal",
                            "cannot tell the type of this array's elements",
                        )
                        .label(expr.span, "no type is expected here")
                        .help("bind it first, e.g. `let a: [u8; 3] = ...` (ADR 0008)"),
                    );
                    for elem in elems {
                        self.check(f, elem, Some(Ty::Error));
                    }
                    Ty::Error
                }
            },
        };
        match elem_ty {
            Ty::Error => Ty::Error,
            ty => Ty::array(ty, elems.len() as u64),
        }
    }

    /// Each argument is checked against its parameter's type.
    fn call(&mut self, f: &Fn, expr: &Expr, callee: hir::FnId, args: &[Expr]) -> Ty {
        let target = self.hir.fn_(callee);
        let params = target.params();
        if args.len() != params.len() {
            let plural = if params.len() == 1 { "" } else { "s" };
            self.q.diags.push(
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
            let expected = self.norm(expected);
            self.check(f, arg, Some(expected));
        }
        self.norm(target.ret)
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
                    self.q.diags.push(
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
                self.q.diags.push(
                    Diagnostic::error("ty::mismatched-types", "mismatched types")
                        .label(expr.span, format!("expected `{other}`, found an integer")),
                );
                Ty::Error
            }
            None => {
                self.q.diags.push(
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
                self.q.tables.expr_types[operand.id.0 as usize] = ty;
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
                self.q.diags.push(
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
    /// `second` gives the type expected of the operand typed second from
    /// the type of the first.
    fn same_type_operands(
        &mut self,
        f: &Fn,
        lhs: &Expr,
        rhs: &Expr,
        expected: Option<Ty>,
        second: fn(Ty) -> Ty,
    ) -> Ty {
        if expected.is_none() && needs_expected_type(lhs) && !needs_expected_type(rhs) {
            let rhs_ty = self.check(f, rhs, None);
            self.check(f, lhs, Some(second(rhs_ty)));
            rhs_ty
        } else {
            let lhs_ty = self.check(f, lhs, expected);
            self.check(f, rhs, Some(second(lhs_ty)));
            lhs_ty
        }
    }

    /// Shift amounts may be any integer type and are masked to the operand
    /// width (ADR 0007); a literal amount takes the shifted type. Whether a
    /// known amount is in range is checked with MIR (ADR 0032).
    fn shift_amount(&mut self, f: &Fn, amount: &Expr, shifted: Ty, op: &str) {
        if needs_expected_type(amount) {
            self.check(f, amount, Some(shifted));
        } else {
            let ty = self.check(f, amount, None);
            self.require(amount, ty, op, Operands::Int);
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
                self.q.diags.push(
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
            self.q.diags.push(
                Diagnostic::error("ty::invalid-cast", format!("cannot cast `{from}`")).label(
                    operand.span,
                    "only integers, pointers and `bool` can be cast",
                ),
            );
        }
        if target == Ty::Bool {
            self.q.diags.push(
                Diagnostic::error("ty::invalid-cast", "cannot cast to `bool`")
                    .label(target_span, "no conversion to `bool`")
                    .help("say what you mean with a comparison, such as `x != 0`"),
            );
            return Ty::Error;
        }
        if !castable(target) {
            self.q.diags.push(
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
                self.q.diags.push(
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
                self.q.diags.push(
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
                self.q.diags.push(error);
                PlaceInfo::ERROR
            }
        }
    }

    /// Checks a read or write of a place. A volatile access is exactly one
    /// instruction (ADR 0023), so it cannot be wider than the target's
    /// widest access, nor a whole aggregate.
    fn access(&mut self, place: &PlaceInfo, span: Span) {
        let volatile = place.via.is_some_and(|ptr| ptr.volatile);
        if volatile && !place.ty.is_aggregate() && self.may_be_misaligned(place) {
            self.q.diags.push(
                Diagnostic::error(
                    "ty::volatile-packed-field",
                    format!("a volatile `{}` here may be misaligned", place.ty),
                )
                .label(span, "reached through a field of a `#[packed]` struct")
                .help("it would be split into several accesses; register blocks use the default layout, with explicit padding fields (ADR 0022, 0023)"),
            );
        }
        if volatile && place.ty.is_aggregate() {
            self.q.diags.push(
                Diagnostic::error(
                    "ty::volatile-aggregate",
                    format!("a volatile `{}` cannot be accessed as a whole", place.ty),
                )
                .label(span, "access its elements or fields one at a time")
                .help("the width and number of the bus accesses would be up to the compiler (ADR 0023)"),
            );
        }
        if let (true, Ty::Int(int)) = (volatile, place.ty)
            && int.bits() > target::WIDEST_ACCESS_BITS
        {
            let half = target::WIDEST_ACCESS_BITS;
            self.q.diags.push(
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
                    ty: self.norm(local.ty),
                    immutable: (!local.mutable).then_some(Immutable::Local(*id)),
                    via: None,
                    misaligned: false,
                    constant: false,
                }
            }
            ExprKind::Deref(pointer) => {
                let ty = self.check(f, pointer, None);
                let error = Diagnostic::error(
                    "ty::deref-non-pointer",
                    format!("cannot dereference `{ty}`"),
                )
                .label(pointer.span, "not a pointer");
                self.through_pointer(ty, pointer, error)
            }
            ExprKind::Field(base, name, name_span) => {
                let (ty, base_place) = self.base(f, base);
                self.field(expr, ty, base_place, name, *name_span)
            }
            ExprKind::Index(base, index) => {
                // An array is indexed where it is; anything else is read as
                // a value, which must be a pointer.
                let base_place = self.place_info(f, base);
                let ty = match base_place {
                    Some(info) => info.ty,
                    None => self.check(f, base, None),
                };
                self.check(f, index, Some(Ty::Int(IntTy::Usize)));
                if let Ty::Array(elem, _) = ty {
                    let base_place = base_place.unwrap_or_else(|| PlaceInfo::temporary(ty, base));
                    PlaceInfo {
                        ty: *elem,
                        ..base_place
                    }
                } else {
                    if let Some(info) = base_place {
                        self.access(&info, base.span);
                    }
                    if let Ty::Ptr(ptr) = ty
                        && self.q.layout_of(*ptr.pointee).is_none()
                    {
                        self.too_large(
                            *ptr.pointee,
                            base.span,
                            "the distance between elements does not fit in the address space",
                        );
                    }
                    let error = Diagnostic::error(
                        "ty::cannot-index",
                        format!("cannot index a value of type `{ty}`"),
                    )
                    .label(base.span, "not an array or a pointer");
                    // Unchecked, as in C (ADR 0021).
                    self.through_pointer(ty, base, error)
                }
            }
            _ => return None,
        };
        self.q.tables.expr_types[expr.id.0 as usize] = info.ty;
        Some(info)
    }

    /// The type of the base of a field access or of indexing, and its
    /// place: the base's own if it is a place expression, or else a
    /// temporary holding its value.
    fn base(&mut self, f: &Fn, base: &Expr) -> (Ty, PlaceInfo) {
        match self.place_info(f, base) {
            Some(info) => (info.ty, info),
            None => {
                let ty = self.check(f, base, None);
                (ty, PlaceInfo::temporary(ty, base))
            }
        }
    }

    /// `base.name`, where `base` has type `ty` and place `base_place`.
    fn field(
        &mut self,
        expr: &Expr,
        ty: Ty,
        base_place: PlaceInfo,
        name: &str,
        name_span: Span,
    ) -> PlaceInfo {
        let hir = self.hir;
        let s = match ty {
            Ty::Struct(s) => s,
            Ty::Never => {
                return PlaceInfo {
                    ty: Ty::Never,
                    ..PlaceInfo::ERROR
                };
            }
            Ty::Error => return PlaceInfo::ERROR,
            other => {
                let mut diag =
                    Diagnostic::error("ty::no-fields", format!("`{other}` has no fields"))
                        .label(name_span, "no such field");
                if let Ty::Ptr(ptr) = other
                    && let Ty::Struct(_) = *ptr.pointee
                {
                    diag = diag.help(format!(
                        "dereference the pointer first: `(*p).{name}` (ADR 0021)"
                    ));
                }
                self.q.diags.push(diag);
                return PlaceInfo::ERROR;
            }
        };
        let def = hir.struct_(s.id);
        let Some(index) = def.fields.iter().position(|field| field.name == name) else {
            let names: Vec<String> = def.fields.iter().map(|f| format!("`{}`", f.name)).collect();
            let mut diag = Diagnostic::error(
                "ty::unknown-field",
                format!("`{}` has no field `{name}`", s.name),
            )
            .label(name_span, "unknown field");
            diag = if names.is_empty() {
                diag.help(format!("`{}` has no fields", s.name))
            } else {
                diag.help(format!("its fields are {}", names.join(", ")))
            };
            self.q.diags.push(diag);
            return PlaceInfo::ERROR;
        };
        self.q.tables.fields.insert(expr.id, index as u32);
        PlaceInfo {
            ty: self.norm(def.fields[index].ty),
            misaligned: base_place.misaligned || def.packed,
            ..base_place
        }
    }

    /// Checks that `place` can be assigned to and returns its type.
    fn place(&mut self, f: &Fn, place: &Expr) -> Ty {
        let Some(info) = self.place_info(f, place) else {
            self.check(f, place, None);
            self.q.diags.push(
                Diagnostic::error("ty::not-assignable", "cannot assign to this expression")
                    .label(place.span, "not a variable, `*pointer` or `a[index]`"),
            );
            return Ty::Error;
        };
        match info.immutable {
            Some(Immutable::Local(id)) => {
                let local = f.local(id);
                self.q.diags.push(
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
                self.q.diags.push(
                    Diagnostic::error(
                        "ty::write-through-const",
                        "cannot write through a `*const` pointer",
                    )
                    .label(pointer, format!("has type `{}`", Ty::Ptr(ptr)))
                    .help("only `*mut` pointers can be written through"),
                );
            }
            Some(Immutable::Temporary(base)) => {
                self.q.diags.push(
                    Diagnostic::error(
                        "ty::not-assignable",
                        "cannot assign to part of a temporary value",
                    )
                    .label(base, "this value lives in no variable")
                    .help("store it in a `let mut` first"),
                );
            }
            None => {}
        }
        self.access(&info, place.span);
        info.ty
    }

    /// A place inside a packed struct whose type needs alignment.
    fn may_be_misaligned(&mut self, place: &PlaceInfo) -> bool {
        place.misaligned && self.q.layout_of(place.ty).is_some_and(|l| l.align > 1)
    }

    /// `&raw const place` or `&raw mut place` (ADR 0021). The address of a
    /// place is taken without accessing it.
    fn addr_of(&mut self, f: &Fn, mutable: bool, place: &Expr) -> Ty {
        let not_a_place = Diagnostic::error("ty::not-a-place", "`&raw` needs a place in memory")
            .label(place.span, "a value, not a place")
            .help("take the address of a variable, `*pointer` or `a[index]`; to point at a value, store it in a `let` first");
        let Some(info) = self.place_info(f, place) else {
            self.check(f, place, None);
            self.q.diags.push(not_a_place);
            return Ty::Error;
        };
        // An element or field of a constant is read where the constant's
        // data is, but has no address of its own (ADR 0029).
        if info.constant {
            self.q.diags.push(not_a_place);
            return Ty::Error;
        }
        if mutable {
            match info.immutable {
                Some(Immutable::Local(id)) => {
                    let local = f.local(id);
                    self.q.diags.push(
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
                    self.q.diags.push(
                        Diagnostic::error(
                            "ty::address-of-immutable",
                            "cannot take a `*mut` address through a `*const` pointer",
                        )
                        .label(pointer, format!("has type `{}`", Ty::Ptr(ptr)))
                        .help("use `&raw const`, or a `*mut` pointer"),
                    );
                }
                Some(Immutable::Temporary(base)) => {
                    self.q.diags.push(
                        Diagnostic::error(
                            "ty::address-of-immutable",
                            "cannot take a `*mut` address of part of a temporary value",
                        )
                        .label(base, "this value lives in no variable")
                        .help("store it in a `let mut` first"),
                    );
                }
                None => {}
            }
        }
        if self.may_be_misaligned(&info) {
            self.q.diags.push(
                Diagnostic::error(
                    "ty::packed-field-address",
                    format!("cannot take the address of a `{}` that may be misaligned", info.ty),
                )
                .label(place.span, "reached through a field of a `#[packed]` struct")
                .help(format!(
                    "a `*const {}` promises alignment, and dereferencing a misaligned one may trap; copy the value into a `let` first (ADR 0022)",
                    info.ty
                )),
            );
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
            // Aggregates compare part by part (ADR 0021).
            Ty::Unit | Ty::Array(..) | Ty::Struct(_) => false,
        };
        if !ok {
            let what = match allowed {
                Operands::Int => "an integer",
                Operands::IntOrBool => "an integer or `bool`",
                Operands::Equatable => "integers, pointers or `bool`",
                Operands::Ordered => "integers or pointers",
            };
            self.q.diags.push(
                Diagnostic::error(
                    "ty::invalid-operand",
                    format!("`{op}` needs {what}, found `{ty}`"),
                )
                .label(expr.span, format!("has type `{ty}`")),
            );
        }
    }
}

/// Lists struct layouts, then every function's locals and expression types,
/// for `--emit=typeck`.
pub fn dump(q: &mut Queries) -> String {
    let hir = q.hir;
    let mut out = String::new();
    for (i, def) in hir.structs.iter().enumerate() {
        let Some(layout) = q.struct_layout(hir::StructId(i as u32)) else {
            continue;
        };
        let fields: Vec<String> = def
            .fields
            .iter()
            .zip(&layout.offsets)
            .map(|(field, offset)| {
                let ty = q.normalize(field.ty);
                format!("{}: {ty} at {offset}", field.name)
            })
            .collect();
        let fields = if fields.is_empty() {
            "{}".to_owned()
        } else {
            format!("{{ {} }}", fields.join(", "))
        };
        let attrs = match (def.packed, def.align) {
            (true, _) => "#[packed] ".to_owned(),
            (false, Some((align, _))) => match q.array_len(align) {
                Some(align) => format!("#[align({align})] "),
                None => "#[align(?)] ".to_owned(),
            },
            (false, None) => String::new(),
        };
        writeln!(
            out,
            "{attrs}struct {} {fields} size {}, align {}",
            def.name, layout.layout.size, layout.layout.align
        )
        .unwrap();
    }
    for (_, f) in hir.fns() {
        let kind = match f.kind {
            FnKind::Plain => "",
            FnKind::Entry => "entry ",
            FnKind::Panic => "panic ",
        };
        let params: Vec<String> = f
            .params()
            .iter()
            .map(|p| format!("{}: {}", p.name, q.normalize(p.ty)))
            .collect();
        let external = if f.body.is_none() { "extern " } else { "" };
        let external = if f.comptime.is_some() {
            "comptime "
        } else {
            external
        };
        writeln!(
            out,
            "{external}{kind}fn {}({}) -> {}",
            f.name,
            params.join(", "),
            q.normalize(f.ret)
        )
        .unwrap();
        for local in &f.locals[f.param_count..] {
            let mutable = if local.mutable { "mut " } else { "" };
            let ty = q.normalize(local.ty);
            writeln!(out, "    let {mutable}{}: {ty}", local.name).unwrap();
        }
        let Some(body) = &f.body else {
            continue;
        };
        body.walk(&mut |e| {
            let ty = q.tables.expr_ty(e.id);
            writeln!(
                out,
                "    expr {} {} {:?}: {ty}",
                e.id.0,
                expr_label(hir, f, e),
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

fn expr_label(hir: &Hir, f: &Fn, expr: &Expr) -> String {
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
        ExprKind::Const(id) => format!("const {}", hir.const_(*id).name),
        ExprKind::Comptime(_) => "comptime".to_owned(),
        ExprKind::Array(_) => "array".to_owned(),
        ExprKind::StructLit(s, _) => format!("struct {}", s.name),
        ExprKind::Field(_, name, _) => format!("field {name}"),
        ExprKind::SizeOf(ty) => format!("size_of({ty})"),
        ExprKind::AlignOf(ty) => format!("align_of({ty})"),
        ExprKind::OffsetOf(ty, name, _) => format!("offset_of({ty}, {name})"),
        ExprKind::Repeat(..) => "repeat".to_owned(),
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
        ExprKind::Assert(_) => "assert".to_owned(),
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

    #[test]
    fn mut_pointers_coerce_to_const() {
        let u8_ = Ty::Int(IntTy::U8);
        let mut_ = Ty::ptr(true, false, u8_);
        let const_ = Ty::ptr(false, false, u8_);
        assert!(coerces(mut_, const_));
        assert!(!coerces(const_, mut_));
        // Volatility never changes implicitly.
        assert!(!coerces(mut_, Ty::ptr(false, true, u8_)));
        // Only the outermost pointer converts.
        assert!(coerces(
            Ty::ptr(true, false, mut_),
            Ty::ptr(false, false, mut_)
        ));
        assert!(!coerces(
            Ty::ptr(true, false, mut_),
            Ty::ptr(true, false, const_)
        ));
    }
}
