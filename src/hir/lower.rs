//! AST -> HIR lowering with name resolution.

use std::collections::HashMap;

use crate::diag::{Diagnostic, Diagnostics};
use crate::hir::{
    Block, Expr, ExprId, ExprKind, Fn, FnId, FnKind, Hir, IntTy, Local, LocalId, Stmt, Ty,
};
use crate::span::Span;
use crate::syntax::ast;

/// Returns `None` only when no HIR can be built at all (no entry or panic
/// function); other errors are reported and lowering continues.
pub fn lower(file: &ast::File, diags: &mut Diagnostics) -> Option<Hir> {
    let mut lowerer = Lowerer {
        next_expr: 0,
        diags,
        locals: Vec::new(),
        scopes: Vec::new(),
    };
    let fns: Vec<Fn> = file
        .items
        .iter()
        .map(|item| match item {
            ast::Item::Fn(decl) => lowerer.fn_decl(decl),
        })
        .collect();
    let expr_count = lowerer.next_expr;

    check_duplicate_names(&fns, diags);
    let entry = find_unique(&fns, FnKind::Entry, diags);
    let panic = find_unique(&fns, FnKind::Panic, diags);

    Some(Hir {
        fns,
        entry: entry?,
        panic: panic?,
        expr_count,
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

struct Lowerer<'a> {
    next_expr: u32,
    diags: &'a mut Diagnostics,
    /// Locals of the function being lowered.
    locals: Vec<Local>,
    /// Innermost scope last. A `let` shadows earlier names, as in Rust.
    scopes: Vec<HashMap<String, LocalId>>,
}

impl Lowerer<'_> {
    fn fn_decl(&mut self, decl: &ast::FnDecl) -> Fn {
        self.locals.clear();
        self.scopes.clear();
        let ret = decl.ret.as_ref().map_or(Ty::Unit, |ty| self.ty(ty));
        let body = self.block(&decl.body);
        Fn {
            name: decl.name.name.clone(),
            name_span: decl.name.span,
            kind: match decl.kind {
                ast::FnKind::Plain => FnKind::Plain,
                ast::FnKind::Entry => FnKind::Entry,
                ast::FnKind::Panic => FnKind::Panic,
            },
            ret,
            ret_span: decl.ret.as_ref().map(|ty| ty.span),
            locals: std::mem::take(&mut self.locals),
            body,
            span: decl.span,
        }
    }

    fn ty(&mut self, ty: &ast::Ty) -> Ty {
        match &ty.kind {
            ast::TyKind::Never => Ty::Never,
            ast::TyKind::Unit => Ty::Unit,
            ast::TyKind::Named(name) => match IntTy::from_name(&name.name) {
                Some(int) => Ty::Int(int),
                None => {
                    self.diags.push(
                        Diagnostic::error(
                            "resolve::unknown-type",
                            format!("unknown type `{}`", name.name),
                        )
                        .label(name.span, "not a type"),
                    );
                    Ty::Error
                }
            },
        }
    }

    fn block(&mut self, block: &ast::Block) -> Block {
        self.scopes.push(HashMap::new());
        let stmts = block.stmts.iter().map(|stmt| self.stmt(stmt)).collect();
        let tail = block.tail.as_ref().map(|e| Box::new(self.expr(e)));
        self.scopes.pop();
        Block {
            stmts,
            tail,
            span: block.span,
        }
    }

    fn stmt(&mut self, stmt: &ast::Stmt) -> Stmt {
        match stmt {
            ast::Stmt::Let(let_) => {
                // The initializer is resolved before the new name exists, so
                // `let x: u32 = x;` refers to an outer `x`.
                let init = self.expr(&let_.init);
                let ty = self.ty(&let_.ty);
                let local = LocalId(self.locals.len() as u32);
                self.locals.push(Local {
                    name: let_.name.name.clone(),
                    ty,
                    mutable: let_.mutable,
                    span: let_.name.span,
                });
                self.scopes
                    .last_mut()
                    .expect("statements are inside a block")
                    .insert(let_.name.name.clone(), local);
                Stmt::Let { local, init }
            }
            ast::Stmt::Expr { expr, semi } => Stmt::Expr {
                expr: self.expr(expr),
                semi: *semi,
            },
        }
    }

    fn resolve(&mut self, name: &ast::Ident) -> Option<LocalId> {
        let found = self
            .scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(&name.name).copied());
        if found.is_none() {
            self.diags.push(
                Diagnostic::error(
                    "resolve::unknown-name",
                    format!("cannot find `{}` in this scope", name.name),
                )
                .label(name.span, "not found"),
            );
        }
        found
    }

    fn expr(&mut self, expr: &ast::Expr) -> Expr {
        let id = ExprId(self.next_expr);
        self.next_expr += 1;
        let kind = match &expr.kind {
            // Rejected literals were reported by the lexer, and the pipeline
            // stops before HIR when there are syntax errors.
            ast::ExprKind::Int(value) => ExprKind::Int(value.unwrap_or(0)),
            ast::ExprKind::Byte(value) => ExprKind::Byte(value.unwrap_or(0)),
            ast::ExprKind::Unit => ExprKind::Unit,
            ast::ExprKind::Name(name) => match self.resolve(name) {
                Some(local) => ExprKind::Local(local),
                // Reported; any placeholder works since lowering errors stop
                // the pipeline.
                None => ExprKind::Unit,
            },
            ast::ExprKind::Unary(op, operand) => ExprKind::Unary(*op, self.boxed(operand)),
            ast::ExprKind::Binary(op, lhs, rhs) => {
                ExprKind::Binary(*op, self.boxed(lhs), self.boxed(rhs))
            }
            ast::ExprKind::Cast(operand, ty) => {
                ExprKind::Cast(self.boxed(operand), self.ty(ty), ty.span)
            }
            ast::ExprKind::Assign(place, value) => {
                ExprKind::Assign(self.boxed(place), self.boxed(value))
            }
            ast::ExprKind::CompoundAssign(op, place, value) => {
                ExprKind::CompoundAssign(*op, self.boxed(place), self.boxed(value))
            }
            ast::ExprKind::Block(block) => ExprKind::Block(self.block(block)),
            ast::ExprKind::Loop(body) => ExprKind::Loop(self.block(body)),
        };
        Expr {
            id,
            kind,
            span: expr.span,
        }
    }

    fn boxed(&mut self, expr: &ast::Expr) -> Box<Expr> {
        Box::new(self.expr(expr))
    }
}
