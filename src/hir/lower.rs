//! AST -> HIR lowering with name resolution.

use std::collections::HashMap;

use crate::diag::{Diagnostic, Diagnostics};
use crate::hir::{
    Block, Expr, ExprId, ExprKind, Fn, FnId, FnKind, Hir, IntTy, Local, LocalId, Stmt, Ty,
};
use crate::span::Span;
use crate::syntax::ast;

/// Lowers a program, together with the runtime support functions when
/// given (ADR 0020). Returns `None` only when no HIR can be built at all (no
/// entry or panic function); other errors are reported and lowering
/// continues.
pub fn lower(
    program: &ast::File,
    runtime: Option<&ast::File>,
    diags: &mut Diagnostics,
) -> Option<Hir> {
    let decls: Vec<&ast::FnDecl> = program
        .items
        .iter()
        .chain(runtime.into_iter().flat_map(|rt| &rt.items))
        .map(|item| match item {
            ast::Item::Fn(decl) => decl,
        })
        .collect();

    // Names starting with `__` belong to the runtime (ADR 0020).
    for item in &program.items {
        let ast::Item::Fn(decl) = item;
        if decl.name.name.starts_with("__") {
            diags.push(
                Diagnostic::error(
                    "resolve::reserved-name",
                    format!("`{}` is a reserved name", decl.name.name),
                )
                .label(decl.name.span, "names starting with `__` are reserved")
                .help("the runtime support functions use them (ADR 0020)"),
            );
        }
    }

    // Functions are global: any body may call any function, declared before
    // or after it. A repeated name keeps its first definition.
    let mut fn_ids = HashMap::new();
    for (i, decl) in decls.iter().enumerate() {
        fn_ids
            .entry(decl.name.name.clone())
            .or_insert((FnId(i as u32), decl.kind));
    }

    let mut lowerer = Lowerer {
        fn_ids,
        next_expr: 0,
        diags,
        locals: Vec::new(),
        scopes: Vec::new(),
        loops: Vec::new(),
    };
    let fns: Vec<Fn> = decls.iter().map(|decl| lowerer.fn_decl(decl)).collect();
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
        // The compiler defines `_start` itself (ADR 0012).
        if f.name == "_start" {
            diags.push(
                Diagnostic::error("resolve::reserved-name", "`_start` is a reserved name")
                    .label(f.name_span, "reserved for the startup code")
                    .help("the compiler generates `_start`, which sets up the stack and jumps to the entry function (ADR 0012)"),
            );
        }
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
    /// Every function's id and kind, by name.
    fn_ids: HashMap<String, (FnId, ast::FnKind)>,
    next_expr: u32,
    diags: &'a mut Diagnostics,
    /// Locals of the function being lowered.
    locals: Vec<Local>,
    /// Innermost scope last. A `let` shadows earlier names, as in Rust.
    scopes: Vec<HashMap<String, LocalId>>,
    /// Enclosing loops, innermost last: the expression id and whether it is
    /// a `while` (whose `break` carries no value).
    loops: Vec<(ExprId, bool)>,
}

impl Lowerer<'_> {
    fn fn_decl(&mut self, decl: &ast::FnDecl) -> Fn {
        self.locals.clear();
        self.scopes.clear();
        self.loops.clear();
        let ret = decl.ret.as_ref().map_or(Ty::Unit, |ty| self.ty(ty));
        self.scopes.push(HashMap::new());
        let mut param_names: HashMap<&str, Span> = HashMap::new();
        for param in &decl.params {
            if let Some(&first) = param_names.get(param.name.name.as_str()) {
                self.diags.push(
                    Diagnostic::error(
                        "resolve::duplicate-param",
                        format!(
                            "the parameter `{}` is declared more than once",
                            param.name.name
                        ),
                    )
                    .label(param.name.span, "declared again here")
                    .label(first, "first declared here"),
                );
            }
            param_names.insert(&param.name.name, param.name.span);
            let ty = self.ty(&param.ty);
            self.declare(&param.name, ty, param.mutable);
        }
        let body = self.block(&decl.body);
        self.scopes.pop();
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
            param_count: decl.params.len(),
            locals: std::mem::take(&mut self.locals),
            body,
            span: decl.span,
        }
    }

    fn ty(&mut self, ty: &ast::Ty) -> Ty {
        match &ty.kind {
            ast::TyKind::Never => Ty::Never,
            ast::TyKind::Unit => Ty::Unit,
            ast::TyKind::Ptr {
                mutable,
                volatile,
                pointee,
            } => {
                let pointee = self.ty(pointee);
                Ty::ptr(*mutable, *volatile, pointee)
            }
            ast::TyKind::Named(name) if name.name == "bool" => Ty::Bool,
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
                let local = self.declare(&let_.name, ty, let_.mutable);
                Stmt::Let { local, init }
            }
            ast::Stmt::Expr { expr, semi } => Stmt::Expr {
                expr: self.expr(expr),
                semi: *semi,
            },
        }
    }

    /// Adds a local to the innermost scope, shadowing earlier ones.
    fn declare(&mut self, name: &ast::Ident, ty: Ty, mutable: bool) -> LocalId {
        let local = LocalId(self.locals.len() as u32);
        self.locals.push(Local {
            name: name.name.clone(),
            ty,
            mutable,
            span: name.span,
        });
        self.scopes
            .last_mut()
            .expect("locals are declared inside a scope")
            .insert(name.name.clone(), local);
        local
    }

    fn find_local(&self, name: &str) -> Option<LocalId> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
    }

    fn resolve(&mut self, name: &ast::Ident) -> Option<LocalId> {
        let found = self.find_local(&name.name);
        if found.is_none() {
            let mut diag = Diagnostic::error(
                "resolve::unknown-name",
                format!("cannot find `{}` in this scope", name.name),
            );
            if self.fn_ids.contains_key(&name.name) {
                diag = diag
                    .label(name.span, "a function, which is not a value")
                    .help(format!("call it: `{}(...)`", name.name));
            } else {
                diag = diag.label(name.span, "not found");
            }
            self.diags.push(diag);
        }
        found
    }

    /// Resolves the callee of a call. Locals shadow functions, as in Rust.
    fn resolve_fn(&mut self, name: &ast::Ident) -> Option<FnId> {
        if let Some(local) = self.find_local(&name.name) {
            let declared = self.locals[local.0 as usize].span;
            self.diags.push(
                Diagnostic::error(
                    "resolve::not-a-function",
                    format!("`{}` is a variable, not a function", name.name),
                )
                .label(name.span, "called here")
                .label(declared, "declared here"),
            );
            return None;
        }
        match self.fn_ids.get(&name.name) {
            Some(&(_, ast::FnKind::Entry)) => {
                self.diags.push(
                    Diagnostic::error("resolve::call-entry", "the entry function cannot be called")
                        .label(name.span, "called here")
                        .help("execution starts there; it is not a function to call"),
                );
                None
            }
            Some(&(id, _)) => Some(id),
            None => {
                self.diags.push(
                    Diagnostic::error(
                        "resolve::unknown-function",
                        format!("cannot find function `{}`", name.name),
                    )
                    .label(name.span, "not found"),
                );
                None
            }
        }
    }

    fn expr(&mut self, expr: &ast::Expr) -> Expr {
        let id = ExprId(self.next_expr);
        self.next_expr += 1;
        let kind = match &expr.kind {
            // Rejected literals were reported by the lexer, and the pipeline
            // stops before HIR when there are syntax errors.
            ast::ExprKind::Int(value) => ExprKind::Int(value.unwrap_or(0)),
            ast::ExprKind::Byte(value) => ExprKind::Byte(value.unwrap_or(0)),
            ast::ExprKind::Bool(value) => ExprKind::Bool(*value),
            ast::ExprKind::Unit => ExprKind::Unit,
            ast::ExprKind::Name(name) => match self.resolve(name) {
                Some(local) => ExprKind::Local(local),
                // Reported; any placeholder works since lowering errors stop
                // the pipeline.
                None => ExprKind::Unit,
            },
            ast::ExprKind::Call(name, args) => {
                let callee = self.resolve_fn(name);
                let args = args.iter().map(|arg| self.expr(arg)).collect();
                match callee {
                    Some(callee) => ExprKind::Call { callee, args },
                    // Reported; lowering errors stop the pipeline.
                    None => ExprKind::Unit,
                }
            }
            ast::ExprKind::Unary(op, operand) => ExprKind::Unary(*op, self.boxed(operand)),
            ast::ExprKind::Deref(operand) => ExprKind::Deref(self.boxed(operand)),
            ast::ExprKind::Binary(op, lhs, rhs) => {
                ExprKind::Binary(*op, self.boxed(lhs), self.boxed(rhs))
            }
            ast::ExprKind::Logical(op, lhs, rhs) => {
                ExprKind::Logical(*op, self.boxed(lhs), self.boxed(rhs))
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
            ast::ExprKind::If { cond, then, else_ } => ExprKind::If {
                cond: self.boxed(cond),
                then: self.block(then),
                else_: else_.as_ref().map(|e| self.boxed(e)),
            },
            ast::ExprKind::While { cond, body } => {
                let cond = self.boxed(cond);
                self.loops.push((id, true));
                let body = self.block(body);
                self.loops.pop();
                ExprKind::While { cond, body }
            }
            ast::ExprKind::Loop(body) => {
                self.loops.push((id, false));
                let body = self.block(body);
                self.loops.pop();
                ExprKind::Loop(body)
            }
            ast::ExprKind::Break(value) => {
                let target = self.loop_target(expr.span, "break");
                if let (Some((_, true)), Some(value)) = (self.loops.last(), value) {
                    self.diags.push(
                        Diagnostic::error(
                            "resolve::break-value-in-while",
                            "`break` with a value is only allowed in `loop`",
                        )
                        .label(value.span, "a `while` loop has no value")
                        .help("use `loop` with an `if` to break with a value"),
                    );
                }
                ExprKind::Break {
                    target,
                    value: value.as_ref().map(|e| self.boxed(e)),
                }
            }
            ast::ExprKind::Continue => ExprKind::Continue {
                target: self.loop_target(expr.span, "continue"),
            },
            ast::ExprKind::Return(value) => ExprKind::Return(value.as_ref().map(|e| self.boxed(e))),
        };
        Expr {
            id,
            kind,
            span: expr.span,
        }
    }

    /// The innermost enclosing loop. Outside a loop this reports an error and
    /// returns a placeholder, since lowering errors stop the pipeline.
    fn loop_target(&mut self, span: Span, keyword: &str) -> ExprId {
        match self.loops.last() {
            Some(&(id, _)) => id,
            None => {
                self.diags.push(
                    Diagnostic::error(
                        "resolve::outside-loop",
                        format!("`{keyword}` outside of a loop"),
                    )
                    .label(span, "not inside `loop` or `while`"),
                );
                ExprId(0)
            }
        }
    }

    fn boxed(&mut self, expr: &ast::Expr) -> Box<Expr> {
        Box::new(self.expr(expr))
    }
}
