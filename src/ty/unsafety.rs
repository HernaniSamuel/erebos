//! The check of `unsafe` (ADR 0119): an unsafe operation is allowed only
//! directly inside an `unsafe` block, and every `unsafe` block holds one.
//!
//! The pass runs on a body once it is typed, and reads the typed body only:
//! whether an operation is unsafe can depend on a type or on the function a
//! method call resolves to, and the blocks are the structure of HIR, which
//! MIR does not keep. Nothing after type checking sees `unsafe`.

use crate::diag::Diagnostic;
use crate::hir::{
    Asm, AsmOperandKind, Block, Expr, ExprKind, Fn, FnKind, ReceiverKind, StaticId, Stmt, Ty,
};
use crate::query::Queries;
use crate::span::Span;

/// The unsafe operations an expression can be.
enum Operation {
    /// A call to an `unsafe fn`, by its path (ADR 0119, operation 5).
    CallsUnsafeFn(String),
    /// An `asm` expression outside the body of a `#[naked]` function
    /// (ADR 0119, operation 2).
    Asm,
    /// A call to an `extern fn`, by its path (ADR 0119, operation 3).
    CallsExternFn(String),
    /// A call to a `#[naked]` function, by its path (ADR 0119, operation 3).
    CallsNakedFn(String),
    /// A read, write or borrow of an `extern static`, or of a part of one,
    /// by its path (ADR 0119, operation 4).
    ExternStatic(String, Access),
}

/// How the expression around a place touches it. A place that only has its
/// address taken (`&raw const`, `&raw mut`) is not touched, and is visited
/// with no `Access`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Access {
    Read,
    Write,
    /// A compound assignment: both.
    Update,
    /// `&` and `&mut`, and a method call whose receiver is a reference.
    Borrow,
}

impl Operation {
    /// What the diagnostic says is wrong.
    fn message(&self) -> String {
        match self {
            Operation::CallsUnsafeFn(path) => {
                format!("calling `unsafe fn {path}` needs an `unsafe` block")
            }
            Operation::Asm => "`asm` needs an `unsafe` block".to_owned(),
            Operation::CallsExternFn(path) => {
                format!("calling `extern fn {path}` needs an `unsafe` block")
            }
            Operation::CallsNakedFn(path) => {
                format!("calling the `#[naked]` function `{path}` needs an `unsafe` block")
            }
            Operation::ExternStatic(path, how) => {
                let verb = match how {
                    Access::Read => "reading",
                    Access::Write => "writing",
                    Access::Update => "reading and writing",
                    Access::Borrow => "borrowing",
                };
                format!("{verb} `extern static {path}` needs an `unsafe` block")
            }
        }
    }

    /// What the label at the operation says it is.
    fn label(&self) -> String {
        match self {
            Operation::CallsUnsafeFn(path) => format!("this calls `unsafe fn {path}`"),
            Operation::Asm => "this `asm`".to_owned(),
            Operation::CallsExternFn(_) => "this calls an `extern fn`".to_owned(),
            Operation::CallsNakedFn(_) => "this calls a naked function".to_owned(),
            Operation::ExternStatic(_, how) => {
                let verb = match how {
                    Access::Read => "reads",
                    Access::Write => "writes",
                    Access::Update => "reads and writes",
                    Access::Borrow => "borrows",
                };
                format!("this {verb} an `extern static`")
            }
        }
    }

    /// What the caller does about it.
    fn help(&self) -> &'static str {
        match self {
            Operation::CallsUnsafeFn(_) => {
                "the caller vouches for the contract of an `unsafe fn`: call it inside an `unsafe { }` block, after checking that its contract holds (ADR 0119)"
            }
            Operation::Asm => {
                "`asm` can write any register or any memory, and the compiler checks none of it: put it inside an `unsafe { }` block, after checking that it is sound where it is (ADR 0119); the body of a `#[naked]` function needs no block"
            }
            Operation::CallsExternFn(_) => {
                "the compiler checks nothing of what an `extern fn` does, or of whether its declaration matches its definition: call it inside an `unsafe { }` block, after checking that it is sound (ADR 0119)"
            }
            Operation::CallsNakedFn(_) => {
                "the compiler checks nothing of what a `#[naked]` function does, since its body is one `asm`: call it inside an `unsafe { }` block, after checking that it is sound (ADR 0119)"
            }
            Operation::ExternStatic(..) => {
                "the compiler does not see what defines an `extern static`, or who else writes it: access it inside an `unsafe { }` block, after checking that it is sound; `&raw const` of it needs no block (ADR 0119)"
            }
        }
    }
}

/// An `unsafe` block around the expression being visited.
struct Open {
    /// Its `unsafe` keyword.
    keyword: Span,
    /// An unsafe operation is directly inside it.
    used: bool,
}

struct Unsafety<'a, 'h> {
    q: &'a mut Queries<'h>,
    /// The `unsafe` blocks around the expression being visited, innermost
    /// last.
    open: Vec<Open>,
    /// The body is the body of an `unsafe fn`, which is not an `unsafe`
    /// block (ADR 0119).
    in_unsafe_fn: bool,
}

/// Checks the typed body of `f`: reports each unsafe operation outside an
/// `unsafe` block as `ty::unsafe-required` and each `unsafe` block with no
/// unsafe operation directly inside it as `ty::unused-unsafe`. Every body of
/// HIR is a root, checked on its own; a `comptime(..)` operand is an
/// expression of the body it is written in, so it is visited from there
/// under the blocks around it (ADR 0119).
pub(super) fn check(q: &mut Queries, f: &Fn, body: &Block) {
    let mut pass = Unsafety {
        q,
        open: Vec::new(),
        in_unsafe_fn: f.is_unsafe,
    };
    if f.kind == FnKind::Naked {
        // The `asm` that is the body of a naked function is the one `asm`
        // that needs no block (ADR 0119): `#[naked]` is the mark, at its
        // calls. Only the body's own statements and tail are exempt; the
        // rest of a malformed body is `ty::naked-body`'s.
        let mut roots = Vec::new();
        block_exprs(body, &mut roots);
        for root in roots {
            if let ExprKind::Asm(asm) = &root.kind {
                pass.asm_operands(asm);
            } else {
                pass.expr(root);
            }
        }
    } else {
        pass.block(body);
    }
}

impl Unsafety<'_, '_> {
    fn block(&mut self, block: &Block) {
        for stmt in &block.stmts {
            match stmt {
                Stmt::Let { init, .. } => self.expr(init),
                Stmt::Expr { expr, .. } => self.expr(expr),
            }
        }
        if let Some(tail) = &block.tail {
            self.expr(tail);
        }
    }

    /// Visits `expr`, whose value is read.
    fn expr(&mut self, expr: &Expr) {
        self.visit(expr, Some(Access::Read));
    }

    /// Visits `expr`, a place touched as `access` says, or whose address
    /// only is taken (`None`). Only the places that make up a place pass
    /// the access on to what they are made of (a field, an index, a range
    /// of a place); every other expression reads its operands.
    fn visit(&mut self, expr: &Expr, access: Option<Access>) {
        match &expr.kind {
            ExprKind::Unsafe(block) => {
                // `unsafe` starts the expression.
                let keyword = Span {
                    hi: expr.span.lo + "unsafe".len() as u32,
                    ..expr.span
                };
                self.open.push(Open {
                    keyword,
                    used: false,
                });
                self.block(block);
                if let Some(closed) = self.open.pop()
                    && !closed.used
                {
                    self.unused(closed.keyword);
                }
            }
            ExprKind::Static(id) => self.static_(expr, *id, access),
            ExprKind::Field(base, ..) => self.visit(base, access),
            ExprKind::Index(base, index, _) => {
                self.visit(base, access);
                self.expr(index);
            }
            ExprKind::Subslice { base, from, to, .. } => {
                self.visit(base, access);
                for bound in [from, to].into_iter().flatten() {
                    self.expr(bound);
                }
            }
            ExprKind::Assign(to, value) => {
                self.visit(to, Some(Access::Write));
                self.expr(value);
            }
            ExprKind::CompoundAssign(_, to, value, _) => {
                self.visit(to, Some(Access::Update));
                self.expr(value);
            }
            ExprKind::AddrOf { place, .. } => self.visit(place, None),
            ExprKind::Asm(asm) => {
                self.operation(expr);
                self.asm_operands(asm);
            }
            ExprKind::Ref { place, .. } => self.visit(place, Some(Access::Borrow)),
            ExprKind::MethodCall { receiver, args, .. } => {
                self.operation(expr);
                // The receiver is borrowed when the method takes `&self` or
                // `&mut self`, and read otherwise.
                let borrowed = self
                    .q
                    .tables
                    .methods
                    .get(&expr.id)
                    .and_then(|&callee| self.q.hir.fn_(callee).owner)
                    .and_then(|owner| owner.receiver)
                    .is_some_and(|kind| matches!(kind, ReceiverKind::Ref | ReceiverKind::RefMut));
                self.visit(
                    receiver,
                    Some(if borrowed {
                        Access::Borrow
                    } else {
                        Access::Read
                    }),
                );
                for arg in args {
                    self.expr(arg);
                }
            }
            _ => {
                self.operation(expr);
                self.children(expr);
            }
        }
    }

    /// The operands of an `asm`, each touched as it says (ADR 0079): `in` and
    /// `const` are read, `out` and `lateout` places are written, `inout`
    /// without `=>` is read and written, and with `=>` its value is read and
    /// its place written. `sym` and `clobber_abi` hold no expression.
    fn asm_operands(&mut self, asm: &Asm) {
        for operand in &asm.operands {
            match &operand.kind {
                AsmOperandKind::In { expr, .. } | AsmOperandKind::Const(expr) => self.expr(expr),
                AsmOperandKind::Out { place, .. } => {
                    if let Some(place) = place {
                        self.visit(place, Some(Access::Write));
                    }
                }
                AsmOperandKind::InOut {
                    expr, out: None, ..
                } => {
                    self.visit(expr, Some(Access::Update));
                }
                AsmOperandKind::InOut {
                    expr,
                    out: Some(out),
                    ..
                } => {
                    self.expr(expr);
                    if let Some(place) = out {
                        self.visit(place, Some(Access::Write));
                    }
                }
                AsmOperandKind::Sym(_) | AsmOperandKind::ClobberAbi(_) => {}
            }
        }
    }

    /// A use of the static `id`, written as `expr`: an unsafe operation if
    /// it is an `extern static` and more than its address is taken.
    fn static_(&mut self, expr: &Expr, id: StaticId, access: Option<Access>) {
        let def = self.q.hir.static_(id);
        let (Some(access), None) = (access, def.init) else {
            return;
        };
        let operation = Operation::ExternStatic(def.path.clone(), access);
        // A static whose type was reported (a reference) is in error itself.
        let in_error = self.q.tables.expr_ty(expr.id) == Ty::Error;
        self.report(operation, expr.span, in_error);
    }

    /// The expressions directly inside `expr`.
    fn children(&mut self, expr: &Expr) {
        let mut children = Vec::new();
        children_of(expr, &mut children);
        for child in children {
            self.expr(child);
        }
    }

    /// Handles `expr` if it is an unsafe operation.
    fn operation(&mut self, expr: &Expr) {
        let tables = &self.q.tables;
        let in_error = |e: &Expr| tables.expr_ty(e.id) == Ty::Error;
        let (operation, at, operand_in_error) = match &expr.kind {
            ExprKind::Call { callee, args } => {
                let Some(operation) = call_operation(self.q.hir.fn_(*callee)) else {
                    return;
                };
                (operation, expr.span, args.iter().any(in_error))
            }
            ExprKind::MethodCall {
                receiver,
                args,
                name_span,
                ..
            } => {
                // A call that found no method was reported: it counts as
                // unsafe for the blocks around it, and is not reported
                // again.
                let Some(&callee) = self.q.tables.methods.get(&expr.id) else {
                    self.mark_used();
                    return;
                };
                let Some(operation) = call_operation(self.q.hir.fn_(callee)) else {
                    return;
                };
                (
                    operation,
                    *name_span,
                    in_error(receiver) || args.iter().any(in_error),
                )
            }
            ExprKind::Asm(_) => {
                let mut operands = Vec::new();
                children_of(expr, &mut operands);
                (
                    Operation::Asm,
                    expr.span,
                    operands.into_iter().any(in_error),
                )
            }
            _ => return,
        };
        self.report(operation, at, operand_in_error);
    }

    /// Reports `operation`, at `at`, unless an `unsafe` block is around it,
    /// which it then marks used. An operand in error was reported: the
    /// operation counts as unsafe for the block around it, and is not
    /// reported on top of that.
    fn report(&mut self, operation: Operation, at: Span, operand_in_error: bool) {
        if self.mark_used() || operand_in_error {
            return;
        }
        let mut help = operation.help().to_owned();
        if self.in_unsafe_fn {
            help += "; the body of an `unsafe fn` is not an `unsafe` block, so it needs its own";
        }
        self.q.diags.push(
            Diagnostic::error("ty::unsafe-required", operation.message())
                .label(at, operation.label())
                .help(help),
        );
    }

    /// Marks the innermost `unsafe` block used; returns whether there is one.
    fn mark_used(&mut self) -> bool {
        match self.open.last_mut() {
            Some(block) => {
                block.used = true;
                true
            }
            None => false,
        }
    }

    /// `ty::unused-unsafe` for the block just closed, whose `unsafe` keyword
    /// is `keyword`: a nested one also points at the block around it.
    fn unused(&mut self, keyword: Span) {
        let mut diag = Diagnostic::error(
            "ty::unused-unsafe",
            "this `unsafe` block holds no unsafe operation",
        )
        .label(keyword, "nothing in it needs `unsafe`");
        if let Some(outer) = self.open.last() {
            diag = diag.label(outer.keyword, "inside this `unsafe` block");
        }
        self.q.diags.push(
            diag.help(
                "an `unsafe` block marks where the program relies on its own reasoning: remove it, or move into it what needs it (ADR 0119)",
            ),
        );
    }
}

/// The unsafe operation a call to `callee` is, if it is one: the function is
/// an `unsafe fn`, an `extern fn` (it has no body) or a `#[naked]` function
/// (ADR 0119, operations 3 and 5). A function that is more than one of them
/// is reported as the first of that list.
fn call_operation(callee: &Fn) -> Option<Operation> {
    let path = || callee.path.clone();
    if callee.is_unsafe {
        Some(Operation::CallsUnsafeFn(path()))
    } else if callee.body.is_none() {
        Some(Operation::CallsExternFn(path()))
    } else if callee.kind == FnKind::Naked {
        Some(Operation::CallsNakedFn(path()))
    } else {
        None
    }
}

/// The expressions of the statements of `block` and its tail, in source order.
fn block_exprs<'a>(block: &'a Block, out: &mut Vec<&'a Expr>) {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Let { init, .. } => out.push(init),
            Stmt::Expr { expr, .. } => out.push(expr),
        }
    }
    out.extend(block.tail.as_deref());
}

/// The expressions directly inside `expr`, in source order; the statements
/// of its blocks are among them.
fn children_of<'a>(expr: &'a Expr, out: &mut Vec<&'a Expr>) {
    let block = block_exprs;
    match &expr.kind {
        ExprKind::Int(_)
        | ExprKind::Byte(_)
        | ExprKind::ByteString(_)
        | ExprKind::Bool(_)
        | ExprKind::Unit
        | ExprKind::SizeOf(_)
        | ExprKind::AlignOf(_)
        | ExprKind::OffsetOf(..)
        | ExprKind::Const(_)
        | ExprKind::Static(_)
        | ExprKind::Peripheral(_)
        | ExprKind::Variant(_, _, None)
        | ExprKind::ContextVariant(_, _, None)
        | ExprKind::Local(_)
        | ExprKind::Continue { .. }
        | ExprKind::Break { value: None, .. }
        | ExprKind::Return(None) => {}
        ExprKind::Break { value: Some(e), .. }
        | ExprKind::Return(Some(e))
        | ExprKind::Repeat(e, _)
        | ExprKind::Field(e, _, _)
        | ExprKind::Assert(e, None, _)
        | ExprKind::Len(e)
        | ExprKind::End(e)
        | ExprKind::Comptime(e)
        | ExprKind::Try(e, _)
        | ExprKind::Unary(_, e)
        | ExprKind::Deref(e)
        | ExprKind::Cast(e, _, _)
        | ExprKind::AddrOf { place: e, .. }
        | ExprKind::Ref { place: e, .. } => out.push(e),
        ExprKind::Call { args, .. }
        | ExprKind::Array(args)
        | ExprKind::Variant(_, _, Some(args))
        | ExprKind::ContextVariant(_, _, Some(args)) => out.extend(args),
        ExprKind::MethodCall { receiver, args, .. } => {
            out.push(receiver);
            out.extend(args);
        }
        ExprKind::StructLit(_, fields) => out.extend(fields.iter().map(|(_, value)| value)),
        ExprKind::BitStructLit(_, fields, _) => {
            out.extend(fields.iter().map(|(_, _, value)| value));
        }
        ExprKind::Subslice { base, from, to, .. } => {
            out.push(base);
            out.extend(from.as_deref());
            out.extend(to.as_deref());
        }
        ExprKind::Binary(_, a, b, _)
        | ExprKind::Index(a, b, _)
        | ExprKind::Slice(a, b)
        | ExprKind::Assert(a, Some(b), _)
        | ExprKind::Logical(_, a, b)
        | ExprKind::Assign(a, b)
        | ExprKind::CompoundAssign(_, a, b, _) => {
            out.push(a);
            out.push(b);
        }
        // `Unsafe` is handled by the visit itself, which keeps its stack.
        ExprKind::Block(b) | ExprKind::Critical(b) | ExprKind::Unsafe(b) | ExprKind::Loop(b) => {
            block(b, out);
        }
        ExprKind::Asm(asm) => {
            for operand in &asm.operands {
                match &operand.kind {
                    AsmOperandKind::In { expr, .. } | AsmOperandKind::Const(expr) => {
                        out.push(expr);
                    }
                    AsmOperandKind::Out { place, .. } => out.extend(place),
                    AsmOperandKind::InOut { expr, out: to, .. } => {
                        out.push(expr);
                        out.extend(to.iter().flatten());
                    }
                    AsmOperandKind::Sym(_) | AsmOperandKind::ClobberAbi(_) => {}
                }
            }
        }
        ExprKind::If { cond, then, else_ } => {
            out.push(cond);
            block(then, out);
            out.extend(else_.as_deref());
        }
        ExprKind::While { cond, body } => {
            out.push(cond);
            block(body, out);
        }
        ExprKind::Match {
            scrutinee, arms, ..
        } => {
            out.push(scrutinee);
            out.extend(arms.iter().map(|arm| &arm.body));
        }
    }
}
