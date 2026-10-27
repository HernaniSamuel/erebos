//! Lowering blocks, statements and expressions, with the scopes of
//! locals.

use std::collections::HashMap;

use crate::diag::Diagnostic;
use crate::hir::{
    BitStructId, Block, EnumId, Expr, ExprId, ExprKind, FnId, Local, LocalId, ModuleId,
    NeverPosition, Stmt, StructId, Ty, UnsizedPosition,
};
use crate::span::Span;
use crate::syntax::ast;

use super::{Def, Lowerer, Named, plain_name, written};

impl Lowerer<'_> {
    /// `Name { field: value, ... }`: every field exactly once, in any order;
    /// or, for a bit-field struct, ending with `..` (`rest`), which gives
    /// every field it leaves out its default, or for a field with a write
    /// effect the value whose write has no effect (ADR 0059).
    fn struct_lit(
        &mut self,
        expr: &ast::Expr,
        path: &ast::Path,
        inits: &[ast::FieldInit],
        rest: Option<Span>,
    ) -> ExprKind {
        let of = match self.lookup_path(path) {
            Named::Item(Def::Struct(id)) => Some(LitOf::Struct(id)),
            Named::Item(Def::BitStruct(id)) => Some(LitOf::BitStruct(id)),
            Named::Item(Def::Module(_)) => {
                self.misused_module(path, "a struct");
                None
            }
            Named::Failed => None,
            named => {
                let diag = Diagnostic::error(
                    "resolve::unknown-type",
                    format!("unknown struct `{}`", written(path)),
                );
                let diag = match named {
                    Named::Missing(module) => self.missing(diag, path, module, "not a struct"),
                    Named::Item(def) => {
                        diag.label(path.span, format!("{}, not a struct", def.what()))
                    }
                    _ => diag.label(path.span, "a variant, not a struct"),
                };
                self.diags.push(diag);
                None
            }
        };
        let decl = of.map(|of| self.lit_decl(of));
        let fields: Vec<(Option<u32>, Expr)> = inits
            .iter()
            .map(|init| {
                let index = decl.as_ref().and_then(|decl| {
                    decl.fields
                        .iter()
                        .position(|f| f.name.name == init.name.name)
                });
                (index.map(|i| i as u32), self.expr(&init.value))
            })
            .collect();
        let (Some(of), Some(decl)) = (of, decl) else {
            return ExprKind::Unit;
        };
        let name = decl.name;
        let mut given: Vec<Option<Span>> = vec![None; decl.fields.len()];
        for (init, (index, _)) in inits.iter().zip(&fields) {
            match index {
                None => self.diags.push(
                    Diagnostic::error(
                        "resolve::unknown-field",
                        format!("`{name}` has no field `{}`", init.name.name),
                    )
                    .label(init.name.span, "unknown field")
                    .label(decl.name_span, format!("`{name}` is declared here")),
                ),
                Some(i) => match given[*i as usize] {
                    Some(first) => self.diags.push(
                        Diagnostic::error(
                            "resolve::duplicate-field",
                            format!("the field `{}` is given more than once", init.name.name),
                        )
                        .label(init.name.span, "given again here")
                        .label(first, "first given here"),
                    ),
                    None => given[*i as usize] = Some(init.name.span),
                },
            }
        }
        // Only a bit-field struct's fields have defaults (ADR 0059).
        let bit_struct = matches!(of, LitOf::BitStruct(_));
        if let Some(rest) = rest
            && !bit_struct
        {
            self.diags.push(
                Diagnostic::error(
                    "resolve::missing-default",
                    format!("the fields of `{name}` have no defaults"),
                )
                .label(rest, format!("`{name}` is not a bit-field struct"))
                .help(format!(
                    "`..` gives the fields it leaves out their defaults, which only the fields of a bit-field struct have (ADR 0059); a literal of `{name}` gives every field (ADR 0021)"
                )),
            );
        }
        // A field declared twice was reported; only its first counts.
        let first = |i: usize| {
            decl.fields[..i]
                .iter()
                .all(|earlier| earlier.name.name != decl.fields[i].name.name)
        };
        // Without `..`, a literal gives every field (ADR 0021), so outside
        // the visibility of a private field it cannot be written, whether
        // it gives that field or not (ADR 0050); what it misses is not
        // reported then. With `..`, it may leave out a private field that
        // has a default or a write effect (ADR 0059). A private struct is
        // named from outside its module only by a path already reported as
        // private (ADR 0050); its fields are not reported again.
        if !self.is_within(self.module, decl.module) && decl.vis != ast::Visibility::Private {
            let private: Vec<(&LitField, Option<Span>)> = (decl.fields.iter().zip(&given))
                .enumerate()
                .filter(|&(i, (field, given))| {
                    field.vis == ast::Visibility::Private
                        && first(i)
                        && (rest.is_none() || given.is_some() || !field.fillable)
                })
                .map(|(_, (field, &given))| (field, given))
                .collect();
            if !private.is_empty() {
                self.private_fields(path, name, decl.module, &private, bit_struct, rest);
                return ExprKind::Unit;
            }
        }
        let missing: Vec<&LitField> = (decl.fields.iter().zip(&given))
            .enumerate()
            .filter(|&(i, (_, given))| given.is_none() && first(i))
            .map(|(_, (field, _))| field)
            .collect();
        match rest {
            None if !missing.is_empty() => {
                let mut diag = Diagnostic::error(
                    "resolve::missing-fields",
                    format!("missing fields in `{name}`: {}", named(&missing).join(", ")),
                )
                .label(expr.span, "every field must be given (ADR 0021)");
                if bit_struct && missing.iter().all(|field| field.fillable) {
                    diag = diag.help("each field left out has a default or a write effect: end the literal with `..` to give each its default, or the value whose write has no effect (ADR 0059)");
                }
                self.diags.push(diag);
            }
            Some(rest) if bit_struct => {
                let unfilled: Vec<&LitField> = (missing.into_iter())
                    .filter(|field| !field.fillable)
                    .collect();
                if !unfilled.is_empty() {
                    let (message, left_out) = match unfilled.as_slice() {
                        [field] => (
                            format!("the field `{}` of `{name}` has no default", field.name.name),
                            "leaves out a field without a default",
                        ),
                        _ => {
                            let names = named(&unfilled);
                            let (last, others) = names.split_last().expect("several fields");
                            let message = format!(
                                "the fields {} and {last} of `{name}` have no default",
                                others.join(", ")
                            );
                            (message, "leaves out fields without a default")
                        }
                    };
                    let mut diag = Diagnostic::error("resolve::missing-default", message)
                        .label(rest, left_out);
                    for field in &unfilled {
                        diag = diag.label(field.name.span, "declared here without a default");
                    }
                    self.diags.push(diag.help(
                        "`..` gives each field it leaves out its default, written `= value` after the field's bits, or for a field with a write effect the value whose write has no effect: name such a field in the literal, or declare a default for it (ADR 0059)",
                    ));
                }
            }
            _ => {}
        }
        let fields = (fields.into_iter())
            .filter_map(|(index, value)| Some((index?, value)))
            .collect();
        match of {
            LitOf::Struct(id) => ExprKind::StructLit(self.struct_tys[id.0 as usize], fields),
            LitOf::BitStruct(id) => {
                ExprKind::BitStructLit(self.bit_struct_tys[id.0 as usize], fields, rest)
            }
        }
    }

    /// `resolve::private-field` for a literal, written at `path`, of the
    /// struct or bit-field struct `name` of `module`, outside the
    /// visibility of its fields `private`, each with where the literal
    /// gives it, if it does (ADR 0050); `rest` is the literal's `..`, which
    /// leaves out only the private fields of a bit-field struct that have
    /// a default or a write effect (ADR 0059).
    fn private_fields(
        &mut self,
        path: &ast::Path,
        name: &str,
        module: ModuleId,
        private: &[(&LitField, Option<Span>)],
        bit_struct: bool,
        rest: Option<Span>,
    ) {
        let names: Vec<String> = private
            .iter()
            .map(|(field, _)| format!("`{}`", field.name.name))
            .collect();
        let message = match names.split_last() {
            Some((last, [])) => format!("the field {last} of `{name}` is private"),
            Some((last, others)) => format!(
                "the fields {} and {last} of `{name}` are private",
                others.join(", ")
            ),
            None => unreachable!("a struct with a private field"),
        };
        let mut diag = Diagnostic::error("resolve::private-field", message)
            .label(path.span, "cannot be built here");
        for (field, given) in private {
            if let Some(given) = given {
                diag = diag.label(*given, "not visible here");
            }
            // Left out by `..`, it would need a default.
            let declared = if bit_struct && rest.is_some() && given.is_none() {
                "declared here without `pub` or a default"
            } else {
                "declared here without `pub`"
            };
            diag = diag.label(field.name.span, declared);
        }
        let module = self.module_name(module);
        let help = if bit_struct {
            format!(
                "a literal of a bit-field struct gives every field, or ends with `..`, which leaves out only fields with a default or a write effect (ADR 0059); a private field is visible only in {module} and the modules inside it: build the value with a function of that module, or declare the field `pub` (ADR 0050)"
            )
        } else {
            format!(
                "a struct literal gives every field (ADR 0021), and a private field is visible only in {module} and the modules inside it: build the struct with a function of that module, or declare the field `pub` (ADR 0050)"
            )
        };
        self.diags.push(diag.help(help));
    }

    pub(super) fn block(&mut self, block: &ast::Block) -> Block {
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
                let ty = self.ty(&let_.ty, UnsizedPosition::Variable);
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
    pub(super) fn declare(&mut self, name: &ast::Ident, ty: Ty, mutable: bool) -> LocalId {
        let local = self.new_local(name, ty, mutable);
        self.scopes
            .last_mut()
            .expect("locals are declared inside a scope")
            .insert(name.name.clone(), local);
        local
    }

    /// A local, not yet visible in any scope.
    pub(super) fn new_local(&mut self, name: &ast::Ident, ty: Ty, mutable: bool) -> LocalId {
        let local = LocalId(self.locals.len() as u32);
        self.locals.push(Local {
            name: name.name.clone(),
            ty,
            mutable,
            span: name.span,
        });
        local
    }

    pub(super) fn find_local(&self, name: &str) -> Option<LocalId> {
        self.find_local_scope(name).map(|(local, _)| local)
    }

    /// The local `name` refers to, and the index of its scope.
    fn find_local_scope(&self, name: &str) -> Option<(LocalId, usize)> {
        self.scopes
            .iter()
            .enumerate()
            .rev()
            .find_map(|(depth, scope)| scope.get(name).map(|&local| (local, depth)))
    }

    /// A path used as a value: a local, shadowing an item of the same name,
    /// a constant, a static, a peripheral (ADR 0058), or `Enum::Variant`
    /// (ADR 0035).
    fn resolve(&mut self, path: &ast::Path) -> Option<ExprKind> {
        if let Some(name) = plain_name(path)
            && let Some((local, depth)) = self.find_local_scope(&name.name)
        {
            if self.comptime_floor.is_some_and(|floor| depth < floor) {
                self.diags.push(
                    Diagnostic::error(
                        "ty::not-comptime",
                        format!("`{}` is only known at run time", name.name),
                    )
                    .label(name.span, "a variable of the running program, used at compile time")
                    .help("compile-time code can use constants, literals and function calls (ADR 0029)"),
                );
                return None;
            }
            return Some(ExprKind::Local(local));
        }
        let named = self.lookup_path(path);
        let message = if path.segments.len() == 1 {
            format!("cannot find `{}` in this scope", written(path))
        } else {
            format!("cannot find `{}`", written(path))
        };
        let diag = Diagnostic::error("resolve::unknown-name", message);
        let diag = match named {
            Named::Item(Def::Const(id)) => return Some(ExprKind::Const(id)),
            Named::Item(Def::Static(id)) => return Some(ExprKind::Static(id)),
            Named::Item(Def::Peripheral(id)) => return Some(ExprKind::Peripheral(id)),
            Named::Variant(id, index) => return Some(ExprKind::Variant(id, index, None)),
            Named::Failed => return None,
            Named::Item(Def::Module(_)) => {
                self.misused_module(path, "a value");
                return None;
            }
            Named::Item(Def::Fn(_)) => diag
                .label(path.span, "a function, which is not a value")
                .help(format!("call it: `{}(...)`", written(path))),
            Named::Item(def) => {
                diag.label(path.span, format!("{}, which is not a value", def.what()))
            }
            Named::Missing(module) => self.missing(diag, path, module, "not found"),
        };
        self.diags.push(diag);
        None
    }

    /// Resolves the callee of a call: a function, or a variant built with
    /// its payload's values (ADR 0039). Locals shadow functions, as in
    /// Rust.
    fn resolve_fn(&mut self, path: &ast::Path) -> Option<Callee> {
        if let Some(name) = plain_name(path)
            && let Some(local) = self.find_local(&name.name)
        {
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
        let diag = Diagnostic::error(
            "resolve::unknown-function",
            format!("cannot find function `{}`", written(path)),
        );
        let diag = match self.lookup_path(path) {
            Named::Item(Def::Fn(id)) => {
                if self.fn_decl_of(id).0.kind == ast::FnKind::Entry {
                    self.diags.push(
                        Diagnostic::error(
                            "resolve::call-entry",
                            "the entry function cannot be called",
                        )
                        .label(path.span, "called here")
                        .help("execution starts there; it is not a function to call"),
                    );
                    return None;
                }
                return Some(Callee::Fn(id));
            }
            Named::Variant(id, index) => return Some(Callee::Variant(id, index)),
            Named::Failed => return None,
            Named::Item(Def::Module(_)) => {
                self.misused_module(path, "a function");
                return None;
            }
            Named::Item(def) => diag.label(path.span, format!("{}, not a function", def.what())),
            Named::Missing(module) => self.missing(diag, path, module, "not found"),
        };
        self.diags.push(diag);
        None
    }

    pub(super) fn expr(&mut self, expr: &ast::Expr) -> Expr {
        let id = ExprId(self.next_expr);
        self.next_expr += 1;
        let kind = match &expr.kind {
            // Rejected literals were reported by the lexer, and the pipeline
            // stops before HIR when there are syntax errors.
            ast::ExprKind::Int(value) => ExprKind::Int(value.unwrap_or(0)),
            ast::ExprKind::Byte(value) => ExprKind::Byte(value.unwrap_or(0)),
            // A rejected one was reported by the lexer.
            ast::ExprKind::ByteString(bytes) => {
                ExprKind::ByteString(bytes.clone().unwrap_or_default())
            }
            ast::ExprKind::Bool(value) => ExprKind::Bool(*value),
            ast::ExprKind::Unit => ExprKind::Unit,
            ast::ExprKind::Array(elems) => {
                ExprKind::Array(elems.iter().map(|e| self.expr(e)).collect())
            }
            ast::ExprKind::Repeat(value, len) => {
                let value = self.boxed(value);
                ExprKind::Repeat(value, self.array_len(len))
            }
            ast::ExprKind::Name(path) => match self.resolve(path) {
                Some(kind) => kind,
                // Reported; any placeholder works since lowering errors
                // stop the pipeline.
                None => ExprKind::Unit,
            },
            // Its enum is the expected type, known to type checking
            // (ADR 0044).
            ast::ExprKind::ContextVariant(variant, args) => ExprKind::ContextVariant(
                variant.name.clone(),
                variant.span,
                args.as_ref()
                    .map(|args| args.iter().map(|arg| self.expr(arg)).collect()),
            ),
            ast::ExprKind::Call(path, args)
                if let Some(name) = plain_name(path)
                    && name.name == "assert" =>
            {
                let mut args: Vec<Expr> = args.iter().map(|arg| self.expr(arg)).collect();
                if !matches!(args.len(), 1 | 2) {
                    let given = if args.len() == 1 { "was" } else { "were" };
                    self.diags.push(
                        Diagnostic::error(
                            "ty::wrong-arg-count",
                            format!(
                                "`assert` takes 1 or 2 arguments, but {} {given} given",
                                args.len()
                            ),
                        )
                        .label(expr.span, "in this call")
                        .help(
                            "`assert(cond)` checks one `bool`, and `assert(cond, msg)` adds a message, a `*const [u8]` (ADR 0029, 0031)",
                        ),
                    );
                    return Expr {
                        id,
                        kind: ExprKind::Unit,
                        span: expr.span,
                    };
                }
                let message = (args.len() == 2).then(|| Box::new(args.remove(1)));
                ExprKind::Assert(Box::new(args.remove(0)), message, name.span)
            }
            // Built in, whatever else has the name (ADR 0031).
            ast::ExprKind::Call(path, args)
                if let Some(name) = plain_name(path)
                    && matches!(name.name.as_str(), "len" | "slice") =>
            {
                let mut args: Vec<Expr> = args.iter().map(|arg| self.expr(arg)).collect();
                let (params, help) = if name.name == "len" {
                    (1, "`len(s)` gives the length of one slice (ADR 0031)")
                } else {
                    (2, "`slice(p, n)` takes a pointer and a length (ADR 0031)")
                };
                if args.len() != params {
                    let plural = if params == 1 { "" } else { "s" };
                    let given = if args.len() == 1 { "was" } else { "were" };
                    self.diags.push(
                        Diagnostic::error(
                            "ty::wrong-arg-count",
                            format!(
                                "`{}` takes {params} argument{plural}, but {} {given} given",
                                name.name,
                                args.len()
                            ),
                        )
                        .label(expr.span, "in this call")
                        .help(help),
                    );
                    return Expr {
                        id,
                        kind: ExprKind::Unit,
                        span: expr.span,
                    };
                }
                let first = Box::new(args.remove(0));
                match args.pop() {
                    Some(len) => ExprKind::Slice(first, Box::new(len)),
                    None => ExprKind::Len(first),
                }
            }
            // A function's arguments, or the values of a variant's payload
            // (ADR 0039), which type checking counts.
            ast::ExprKind::Call(path, args) => {
                let callee = self.resolve_fn(path);
                let args = args.iter().map(|arg| self.expr(arg)).collect();
                match callee {
                    Some(Callee::Fn(callee)) => ExprKind::Call { callee, args },
                    Some(Callee::Variant(id, index)) => ExprKind::Variant(id, index, Some(args)),
                    // Reported; lowering errors stop the pipeline.
                    None => ExprKind::Unit,
                }
            }
            ast::ExprKind::Unary(op, operand) => ExprKind::Unary(*op, self.boxed(operand)),
            ast::ExprKind::Deref(operand) => ExprKind::Deref(self.boxed(operand)),
            ast::ExprKind::StructLit(path, fields, rest) => {
                self.struct_lit(expr, path, fields, *rest)
            }
            ast::ExprKind::Field(base, field) => {
                ExprKind::Field(self.boxed(base), field.name.clone(), field.span)
            }
            ast::ExprKind::SizeOf(ty) => {
                ExprKind::SizeOf(self.memory_ty(ty, NeverPosition::LayoutQuery("size_of")))
            }
            ast::ExprKind::AlignOf(ty) => {
                ExprKind::AlignOf(self.memory_ty(ty, NeverPosition::LayoutQuery("align_of")))
            }
            ast::ExprKind::OffsetOf(ty, field) => {
                let ty = self.memory_ty(ty, NeverPosition::LayoutQuery("offset_of"));
                ExprKind::OffsetOf(ty, field.name.clone(), field.span)
            }
            ast::ExprKind::Index(base, index, open) => {
                ExprKind::Index(self.boxed(base), self.boxed(index), *open)
            }
            ast::ExprKind::AddrOf { mutable, place } => ExprKind::AddrOf {
                mutable: *mutable,
                place: self.boxed(place),
            },
            ast::ExprKind::Binary(op, lhs, rhs, op_span) => {
                ExprKind::Binary(*op, self.boxed(lhs), self.boxed(rhs), *op_span)
            }
            ast::ExprKind::Logical(op, lhs, rhs) => {
                ExprKind::Logical(*op, self.boxed(lhs), self.boxed(rhs))
            }
            ast::ExprKind::Cast(operand, ty) => {
                let operand = self.boxed(operand);
                ExprKind::Cast(operand, self.ty(ty, UnsizedPosition::Cast), ty.span)
            }
            ast::ExprKind::Assign(place, value) => {
                ExprKind::Assign(self.boxed(place), self.boxed(value))
            }
            ast::ExprKind::CompoundAssign(op, place, value, op_span) => {
                ExprKind::CompoundAssign(*op, self.boxed(place), self.boxed(value), *op_span)
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
            ast::ExprKind::Match {
                scrutinee,
                arms,
                keyword,
            } => {
                let scrutinee = self.boxed(scrutinee);
                let arms = arms.iter().map(|arm| self.arm(arm)).collect();
                ExprKind::Match {
                    scrutinee,
                    arms,
                    keyword: *keyword,
                }
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
            ast::ExprKind::Return(value) => {
                if self.in_comptime {
                    self.diags.push(
                        Diagnostic::error(
                            "resolve::return-in-comptime",
                            "`return` cannot leave code that runs at compile time",
                        )
                        .label(expr.span, "no function to return from")
                        .help(
                            "compile-time code gives its value as its last expression (ADR 0029)",
                        ),
                    );
                }
                ExprKind::Return(value.as_ref().map(|e| self.boxed(e)))
            }
            // It may return from the enclosing function, as `return` does
            // (ADR 0046).
            ast::ExprKind::Try(operand, question) => {
                if self.in_comptime {
                    self.diags.push(
                        Diagnostic::error(
                            "resolve::return-in-comptime",
                            "`?` cannot leave code that runs at compile time",
                        )
                        .label(expr.span, "returns on a failure, with no function to return from")
                        .help(
                            "compile-time code gives its value as its last expression; take the value apart with a `match`, or call a function that uses `?` (ADR 0029, 0046)",
                        ),
                    );
                }
                ExprKind::Try(self.boxed(operand), *question)
            }
            ast::ExprKind::Comptime(inner) => {
                // The enclosing function's variables and loops exist only at
                // run time (ADR 0029).
                let floor = self.comptime_floor.replace(self.scopes.len());
                let loops = std::mem::take(&mut self.loops);
                let was_comptime = std::mem::replace(&mut self.in_comptime, true);
                self.scopes.push(HashMap::new());
                let inner = self.boxed(inner);
                self.scopes.pop();
                self.in_comptime = was_comptime;
                self.loops = loops;
                self.comptime_floor = floor;
                ExprKind::Comptime(inner)
            }
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

impl<'a> Lowerer<'a> {
    /// The declaration of what a literal builds, as the literal checks it.
    fn lit_decl(&self, of: LitOf) -> LitDecl<'a> {
        match of {
            LitOf::Struct(id) => {
                let (decl, module) = self.struct_decls[id.0 as usize];
                LitDecl {
                    name: self.struct_tys[id.0 as usize].name,
                    name_span: decl.name.span,
                    module,
                    vis: decl.vis,
                    fields: (decl.fields.iter())
                        .map(|field| LitField {
                            name: &field.name,
                            vis: field.vis,
                            fillable: false,
                        })
                        .collect(),
                }
            }
            LitOf::BitStruct(id) => {
                let (decl, module) = self.bit_struct_decls[id.0 as usize];
                LitDecl {
                    name: self.bit_struct_tys[id.0 as usize].name,
                    name_span: decl.name.span,
                    module,
                    vis: decl.vis,
                    fields: (decl.fields.iter())
                        .map(|field| LitField {
                            name: &field.name,
                            vis: field.vis,
                            fillable: field.default.is_some() || field.write_effect.is_some(),
                        })
                        .collect(),
                }
            }
        }
    }
}

/// What a struct literal builds (ADR 0021, 0059).
#[derive(Clone, Copy)]
enum LitOf {
    Struct(StructId),
    BitStruct(BitStructId),
}

/// The declaration of what a struct literal builds, as the literal checks
/// it.
struct LitDecl<'d> {
    /// Its path, as diagnostics show it.
    name: &'static str,
    name_span: Span,
    module: ModuleId,
    vis: ast::Visibility,
    fields: Vec<LitField<'d>>,
}

/// A field of what a struct literal builds.
struct LitField<'d> {
    name: &'d ast::Ident,
    vis: ast::Visibility,
    /// Whether `..` may leave it out: a field of a bit-field struct with a
    /// default or a write effect (ADR 0059).
    fillable: bool,
}

/// The names of `fields`, as messages write them: "`a`".
fn named(fields: &[&LitField]) -> Vec<String> {
    (fields.iter())
        .map(|field| format!("`{}`", field.name.name))
        .collect()
}

/// What a call's path names: a function, or a variant built with its
/// payload's values (ADR 0039).
enum Callee {
    Fn(FnId),
    Variant(EnumId, u32),
}
