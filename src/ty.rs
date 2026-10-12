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
use crate::hir::{Block, Expr, ExprId, ExprKind, Fn, FnKind, Hir, IntTy, Stmt, Ty, UnOp};

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
        // Overwritten for every expression; the placeholder is never read.
        expr_types: vec![Ty::Error; hir.expr_count as usize],
        diags,
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
        ExprKind::Binary(op, lhs, _) if op.is_shift() => needs_expected_type(lhs),
        ExprKind::Binary(_, lhs, rhs) => needs_expected_type(lhs) && needs_expected_type(rhs),
        _ => false,
    }
}

/// The value of a constant shift amount, if the amount is a literal.
fn literal_value(expr: &Expr) -> Option<i128> {
    match &expr.kind {
        ExprKind::Int(value) => Some(*value as i128),
        ExprKind::Unary(UnOp::Neg, operand) => literal_value(operand).map(|v| -v),
        _ => None,
    }
}

struct Checker<'a> {
    expr_types: Vec<Ty>,
    diags: &'a mut Diagnostics,
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

        let body_ty = self.block(f, &f.body, Some(f.ret));
        if !coerces(body_ty, f.ret) {
            self.report_body_mismatch(f, body_ty);
        }
    }

    fn report_body_mismatch(&mut self, f: &Fn, body_ty: Ty) {
        let body = &f.body;
        let found_at = match (&body.tail, body.stmts.last()) {
            (Some(tail), _) => tail.span,
            (None, Some(Stmt::Expr { expr, .. })) => expr.span,
            _ => body.span,
        };
        let mut diag = Diagnostic::error("ty::mismatched-types", "mismatched types")
            .label(found_at, format!("expected `{}`, found `{body_ty}`", f.ret));
        if let Some(ret_span) = f.ret_span {
            diag = diag.label(ret_span, "expected because of this return type");
        }
        // ADR 0015: a value followed by `;` by habit.
        if body.tail.is_none()
            && let Some(Stmt::Expr {
                expr,
                semi: Some(semi),
            }) = body.stmts.last()
            && coerces(self.expr_types[expr.id.0 as usize], f.ret)
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
        if let Some(expected) = expected
            && !coerces(ty, expected)
        {
            self.mismatch(expr, expected, ty);
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
        self.diags.push(diag);
    }

    fn expr_kind(&mut self, f: &Fn, expr: &Expr, expected: Option<Ty>) -> Ty {
        match &expr.kind {
            ExprKind::Int(value) => self.int_literal(expr, *value, false, expected),
            ExprKind::Byte(_) => Ty::Int(IntTy::U8),
            ExprKind::Unit => Ty::Unit,
            ExprKind::Local(local) => f.local(*local).ty,
            ExprKind::Unary(op, operand) => self.unary(f, expr, *op, operand, expected),
            ExprKind::Binary(op, lhs, rhs) if op.is_shift() => {
                let lhs_ty = self.check(f, lhs, expected);
                self.require_int(lhs, lhs_ty, op.symbol());
                self.shift_amount(f, rhs, lhs_ty);
                lhs_ty
            }
            ExprKind::Binary(op, lhs, rhs) => {
                let ty = self.same_type_operands(f, lhs, rhs, expected);
                self.require_int(expr, ty, op.symbol());
                ty
            }
            ExprKind::Cast(operand, target, target_span) => {
                let from = self.check(f, operand, None);
                self.require_int(operand, from, "as");
                if !matches!(target, Ty::Int(_) | Ty::Error) {
                    self.diags.push(
                        Diagnostic::error("ty::invalid-cast", format!("cannot cast to `{target}`"))
                            .label(*target_span, "only integer types can be cast to"),
                    );
                    return Ty::Error;
                }
                *target
            }
            ExprKind::Assign(place, value) => {
                let place_ty = self.place(f, place);
                self.check(f, value, Some(place_ty));
                Ty::Unit
            }
            ExprKind::CompoundAssign(op, place, value) => {
                let place_ty = self.place(f, place);
                self.require_int(place, place_ty, op.symbol());
                if op.is_shift() {
                    self.shift_amount(f, value, place_ty);
                } else {
                    self.check(f, value, Some(place_ty));
                }
                Ty::Unit
            }
            ExprKind::Block(block) => self.block(f, block, expected),
            ExprKind::Loop(body) => {
                self.block(f, body, Some(Ty::Unit));
                // There is no `break` yet, so a loop never finishes.
                Ty::Never
            }
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
            _ => {
                let symbol = if op == UnOp::Neg { "-" } else { "!" };
                self.require_int(expr, ty, symbol);
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
    fn shift_amount(&mut self, f: &Fn, amount: &Expr, shifted: Ty) {
        if needs_expected_type(amount) {
            self.check(f, amount, Some(shifted));
        } else {
            let ty = self.check(f, amount, None);
            self.require_int(amount, ty, "<<");
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

    /// Checks that `place` can be assigned to and returns its type.
    fn place(&mut self, f: &Fn, place: &Expr) -> Ty {
        let ExprKind::Local(id) = place.kind else {
            self.check(f, place, None);
            self.diags.push(
                Diagnostic::error("ty::not-assignable", "cannot assign to this expression")
                    .label(place.span, "not a variable"),
            );
            return Ty::Error;
        };
        let local = f.local(id);
        if !local.mutable {
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
        self.expr_types[place.id.0 as usize] = local.ty;
        local.ty
    }

    fn require_int(&mut self, expr: &Expr, ty: Ty, op: &str) {
        if !matches!(ty, Ty::Int(_) | Ty::Error | Ty::Never) {
            self.diags.push(
                Diagnostic::error(
                    "ty::invalid-operand",
                    format!("`{op}` needs an integer, found `{ty}`"),
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
        writeln!(out, "{kind}fn {}() -> {}", f.name, f.ret).unwrap();
        for local in &f.locals {
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

fn expr_label(f: &Fn, expr: &Expr) -> String {
    match &expr.kind {
        ExprKind::Int(value) => format!("int {value}"),
        ExprKind::Byte(value) => format!("byte {value}"),
        ExprKind::Unit => "unit".to_owned(),
        ExprKind::Local(id) => format!("local {}", f.local(*id).name),
        ExprKind::Unary(op, _) => format!("{op:?}").to_lowercase(),
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
