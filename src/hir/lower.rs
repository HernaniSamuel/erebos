//! AST -> HIR lowering with name resolution.

use std::collections::HashMap;

use crate::diag::{Diagnostic, Diagnostics};
use crate::hir::{Block, Expr, ExprId, ExprKind, Fn, FnId, FnKind, Hir, Ty};
use crate::span::Span;
use crate::syntax::ast;

/// Returns `None` only when no HIR can be built at all (no entry or panic
/// function); other errors are reported and lowering continues.
pub fn lower(file: &ast::File, diags: &mut Diagnostics) -> Option<Hir> {
    let mut lowerer = Lowerer { next_expr: 0 };
    let fns: Vec<Fn> = file
        .items
        .iter()
        .map(|item| match item {
            ast::Item::Fn(decl) => lowerer.fn_decl(decl),
        })
        .collect();

    check_duplicate_names(&fns, diags);
    let entry = find_unique(&fns, FnKind::Entry, diags);
    let panic = find_unique(&fns, FnKind::Panic, diags);

    Some(Hir {
        fns,
        entry: entry?,
        panic: panic?,
        expr_count: lowerer.next_expr,
    })
}

fn check_duplicate_names(fns: &[Fn], diags: &mut Diagnostics) {
    let mut seen: HashMap<&str, Span> = HashMap::new();
    for f in fns {
        if let Some(&first) = seen.get(f.name.as_str()) {
            diags.push(
                Diagnostic::error(
                    "resolve::duplicate-function",
                    format!("the function `{}` is defined more than once", f.name),
                )
                .label(f.name_span, "redefined here")
                .label(first, "first defined here"),
            );
        } else {
            seen.insert(&f.name, f.name_span);
        }
    }
}

/// Finds the single function of a special kind (entry or panic), reporting
/// a missing or repeated one.
fn find_unique(fns: &[Fn], kind: FnKind, diags: &mut Diagnostics) -> Option<FnId> {
    let (what, example) = match kind {
        FnKind::Entry => ("entry", "entry fn main() -> ! { ... }"),
        FnKind::Panic => ("panic", "panic fn on_panic() -> ! { loop {} }"),
        FnKind::Plain => unreachable!("plain functions are not unique"),
    };
    let mut found = fns
        .iter()
        .enumerate()
        .filter(|(_, f)| f.kind == kind)
        .map(|(i, f)| (FnId(i as u32), f));

    let Some((id, first)) = found.next() else {
        diags.push(
            Diagnostic::error(
                if kind == FnKind::Entry {
                    "resolve::missing-entry"
                } else {
                    "resolve::missing-panic"
                },
                format!("no {what} function"),
            )
            .help(format!("every program declares exactly one: `{example}`")),
        );
        return None;
    };
    for (_, extra) in found {
        diags.push(
            Diagnostic::error(
                if kind == FnKind::Entry {
                    "resolve::multiple-entries"
                } else {
                    "resolve::multiple-panics"
                },
                format!("a program has exactly one {what} function"),
            )
            .label(extra.name_span, format!("second {what} function"))
            .label(first.name_span, format!("first {what} function")),
        );
    }
    Some(id)
}

struct Lowerer {
    next_expr: u32,
}

impl Lowerer {
    fn fn_decl(&mut self, decl: &ast::FnDecl) -> Fn {
        Fn {
            name: decl.name.name.clone(),
            name_span: decl.name.span,
            kind: match decl.kind {
                ast::FnKind::Plain => FnKind::Plain,
                ast::FnKind::Entry => FnKind::Entry,
                ast::FnKind::Panic => FnKind::Panic,
            },
            ret: decl.ret.as_ref().map_or(Ty::Unit, lower_ty),
            ret_span: decl.ret.as_ref().map(|ty| ty.span),
            body: self.block(&decl.body),
            span: decl.span,
        }
    }

    fn block(&mut self, block: &ast::Block) -> Block {
        Block {
            stmts: block
                .stmts
                .iter()
                .map(|stmt| match stmt {
                    ast::Stmt::Expr(expr) => self.expr(expr),
                })
                .collect(),
            span: block.span,
        }
    }

    fn expr(&mut self, expr: &ast::Expr) -> Expr {
        let id = ExprId(self.next_expr);
        self.next_expr += 1;
        let kind = match &expr.kind {
            ast::ExprKind::Loop(body) => ExprKind::Loop(self.block(body)),
        };
        Expr {
            id,
            kind,
            span: expr.span,
        }
    }
}

fn lower_ty(ty: &ast::Ty) -> Ty {
    match ty.kind {
        ast::TyKind::Never => Ty::Never,
    }
}
