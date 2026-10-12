//! Type checking (ADR 0008): every type is written or fixed by the
//! construct, so this pass only checks; it never infers.
//!
//! The result is a side table from [`ExprId`] to [`Ty`]; HIR plus this table
//! is the "typed HIR" that MIR building consumes.

use std::fmt::Write;

use crate::diag::{Diagnostic, Diagnostics};
use crate::hir::{Block, Expr, ExprId, ExprKind, Fn, Hir, Ty};
use crate::span::Span;

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
        expr_types: vec![Ty::Unit; hir.expr_count as usize],
        diags,
    };
    for (_, f) in hir.fns() {
        checker.fn_(f);
    }
    TypeckResults {
        expr_types: checker.expr_types,
    }
}

/// `!` has no values, so it can stand in for any type.
fn coerces(from: Ty, to: Ty) -> bool {
    from == to || from == Ty::Never
}

struct Checker<'a> {
    expr_types: Vec<Ty>,
    diags: &'a mut Diagnostics,
}

impl Checker<'_> {
    fn fn_(&mut self, f: &Fn) {
        if f.is_entry && f.ret != Ty::Never {
            self.diags.push(
                Diagnostic::error(
                    "ty::entry-returns",
                    "the entry function must never return",
                )
                .label(
                    f.ret_span.unwrap_or(f.name_span),
                    format!("return type is `{}`", f.ret),
                )
                .help("there is nothing to return to on bare metal; declare it `-> !` and end it with a `loop`"),
            );
        }

        let body_ty = self.block(&f.body);
        if !coerces(body_ty, f.ret) {
            let found_at = f.body.stmts.last().map_or(f.body.span, |e| e.span);
            let mut diag = Diagnostic::error("ty::mismatched-types", "mismatched types")
                .label(found_at, format!("expected `{}`, found `{body_ty}`", f.ret));
            if let Some(ret_span) = f.ret_span {
                diag = diag.label(ret_span, "expected because of this return type");
            }
            self.diags.push(diag);
        }
    }

    /// A block diverges if any of its statements does; otherwise it has the
    /// type of its last statement, or `()` when empty.
    fn block(&mut self, block: &Block) -> Ty {
        let types: Vec<Ty> = block.stmts.iter().map(|e| self.expr(e)).collect();
        if types.contains(&Ty::Never) {
            Ty::Never
        } else {
            types.last().copied().unwrap_or(Ty::Unit)
        }
    }

    fn expr(&mut self, expr: &Expr) -> Ty {
        let ty = match &expr.kind {
            ExprKind::Loop(body) => {
                let body_ty = self.block(body);
                self.expect(body_ty, Ty::Unit, body.span);
                // There is no `break` yet, so a loop never finishes.
                Ty::Never
            }
        };
        self.expr_types[expr.id.0 as usize] = ty;
        ty
    }

    fn expect(&mut self, found: Ty, expected: Ty, span: Span) {
        if !coerces(found, expected) {
            self.diags.push(
                Diagnostic::error("ty::mismatched-types", "mismatched types")
                    .label(span, format!("expected `{expected}`, found `{found}`")),
            );
        }
    }
}

/// Lists every function signature and expression type, for `--emit=typeck`.
pub fn dump(hir: &Hir, results: &TypeckResults) -> String {
    fn block(out: &mut String, b: &Block, results: &TypeckResults, depth: usize) {
        for e in &b.stmts {
            let ExprKind::Loop(body) = &e.kind;
            let indent = "    ".repeat(depth);
            let ty = results.expr_ty(e.id);
            writeln!(out, "{indent}expr {} loop {:?}: {ty}", e.id.0, e.span).unwrap();
            block(out, body, results, depth + 1);
        }
    }

    let mut out = String::new();
    for (_, f) in hir.fns() {
        let entry = if f.is_entry { "entry " } else { "" };
        writeln!(out, "{entry}fn {}() -> {}", f.name, f.ret).unwrap();
        block(&mut out, &f.body, results, 1);
    }
    out
}
