//! Typed HIR -> MIR, one function at a time (ADR 0028).

use std::collections::HashMap;

use crate::eval::ConstValue;
use crate::hir;
use crate::mir::{
    BasicBlock, BasicBlockData, BinOp, Body, BodyId, Const, DataId, Local, LocalDecl, Operand,
    PanicCause, Place, PlaceElem, Rvalue, Statement, Terminator, Ty, UnOp,
};
use crate::query::{DataSource, Queries};
use crate::span::Span;

/// The MIR of a function that passed type checking, or `None` if a value
/// it needs from compile-time evaluation failed, which was reported.
pub fn build_body(q: &mut Queries, id: hir::FnId) -> Option<Body> {
    let hir = q.hir;
    let f = hir.fn_(id);
    let ret = q.mir_ty(f.ret);
    let mut builder = Builder::new(q, id, ret);
    if let Some(body) = &f.body {
        let start = builder.new_block();
        builder.current = Some(start);
        let value = builder.block(body);
        if ret != Ty::Unit {
            builder.assign(Place::local(Local::RETURN), Rvalue::Use(value));
        }
        if builder.current.is_some() {
            builder.terminate(Terminator::Return);
        }
    }
    builder.finish(
        f.name.clone(),
        f.span,
        f.comptime.is_some(),
        f.runtime,
        f.body.is_none(),
    )
}

/// A body that computes `expr`, part of function `id`, alone: the MIR that
/// `comptime expr` runs (ADR 0029).
pub fn build_expr_body(q: &mut Queries, id: hir::FnId, expr: &hir::Expr) -> Option<Body> {
    let ty = q.tables.expr_ty(expr.id);
    let ret = q.mir_ty(ty);
    let mut builder = Builder::new(q, id, ret);
    builder.span = expr.span;
    let start = builder.new_block();
    builder.current = Some(start);
    let value = builder.expr(expr);
    if !matches!(ret, Ty::Unit | Ty::Never) {
        builder.assign(Place::local(Local::RETURN), Rvalue::Use(value));
    }
    if builder.current.is_some() {
        builder.terminate(Terminator::Return);
    }
    builder.finish("comptime".to_owned(), expr.span, true, false, false)
}

struct PartialBlock {
    statements: Vec<Statement>,
    spans: Vec<Span>,
    terminator: Option<(Terminator, Span)>,
}

struct Builder<'a, 'h> {
    q: &'a mut Queries<'h>,
    /// The function being built, or containing the expression being built.
    fn_id: hir::FnId,
    /// The source of the code being built, for statements' spans.
    span: Span,
    /// A value from compile-time evaluation was missing; the body cannot
    /// be used.
    failed: bool,
    /// The local bound to each piece of constant data used.
    const_locals: HashMap<DataId, Local>,
    locals: Vec<LocalDecl>,
    /// MIR local of each HIR local.
    hir_locals: Vec<Local>,
    blocks: Vec<PartialBlock>,
    /// The block code is currently appended to; `None` after control flow
    /// has diverged, so following code is unreachable and not emitted.
    current: Option<BasicBlock>,
    /// Enclosing loops, innermost last.
    loops: Vec<LoopScope>,
}

struct LoopScope {
    /// The HIR expression of the loop, which `break`/`continue` name.
    id: hir::ExprId,
    /// Where `continue` jumps.
    continue_to: BasicBlock,
    /// Where `break` jumps; created by the first `break`.
    break_to: Option<BasicBlock>,
    /// Where `break value` stores the value, for loops with a value.
    result: Option<Place>,
}

impl<'a, 'h> Builder<'a, 'h> {
    fn new(q: &'a mut Queries<'h>, fn_id: hir::FnId, ret: Ty) -> Builder<'a, 'h> {
        let hir = q.hir;
        let f = hir.fn_(fn_id);
        let mut builder = Builder {
            q,
            fn_id,
            span: f.span,
            failed: false,
            const_locals: HashMap::new(),
            locals: vec![LocalDecl {
                ty: ret,
                name: None,
                constant: None,
            }],
            hir_locals: Vec::new(),
            blocks: Vec::new(),
            current: None,
            loops: Vec::new(),
        };
        // HIR locals become MIR locals `_1..`, in declaration order.
        for local in &f.locals {
            let ty = builder.q.mir_ty(local.ty);
            let mir_local = builder.new_local(ty, Some(local.name.clone()));
            builder.hir_locals.push(mir_local);
        }
        builder
    }

    fn finish(
        self,
        name: String,
        span: Span,
        comptime: bool,
        runtime: bool,
        external: bool,
    ) -> Option<Body> {
        if self.failed {
            return None;
        }
        let param_count = if comptime {
            0
        } else {
            self.q.hir.fn_(self.fn_id).param_count
        };
        Some(Body {
            name,
            span,
            comptime,
            runtime,
            external,
            ret: self.locals[0].ty,
            param_count,
            locals: self.locals,
            blocks: self
                .blocks
                .into_iter()
                .map(|b| {
                    let (terminator, terminator_span) =
                        b.terminator.expect("every block is terminated");
                    BasicBlockData {
                        statements: b.statements,
                        spans: b.spans,
                        terminator,
                        terminator_span,
                    }
                })
                .collect(),
        })
    }
}

impl Builder<'_, '_> {
    fn new_local(&mut self, ty: Ty, name: Option<String>) -> Local {
        self.locals.push(LocalDecl {
            ty,
            name,
            constant: None,
        });
        Local(self.locals.len() as u32 - 1)
    }

    fn new_block(&mut self) -> BasicBlock {
        self.blocks.push(PartialBlock {
            statements: Vec::new(),
            spans: Vec::new(),
            terminator: None,
        });
        BasicBlock(self.blocks.len() as u32 - 1)
    }

    fn assign(&mut self, place: Place, rvalue: Rvalue) {
        self.push(Statement::Assign(place, rvalue));
    }

    fn push(&mut self, statement: Statement) {
        if let Some(bb) = self.current {
            let block = &mut self.blocks[bb.0 as usize];
            block.statements.push(statement);
            block.spans.push(self.span);
        }
    }

    /// Ends the current block. In unreachable code there is none, and
    /// nothing is emitted.
    fn terminate(&mut self, terminator: Terminator) {
        if let Some(bb) = self.current.take() {
            self.blocks[bb.0 as usize].terminator = Some((terminator, self.span));
        }
    }

    /// The local bound to a value computed at compile time, for an
    /// aggregate, which is read from read-only data in place (ADR 0029).
    fn const_local(&mut self, ty: Ty, source: DataSource, value: &ConstValue) -> Local {
        let data = self.q.const_data(source, value, ty.layout().align);
        if let Some(&local) = self.const_locals.get(&data) {
            return local;
        }
        self.locals.push(LocalDecl {
            ty,
            name: None,
            constant: Some(data),
        });
        let local = Local(self.locals.len() as u32 - 1);
        self.const_locals.insert(data, local);
        local
    }

    /// A value computed at compile time, as an operand of type `ty`.
    fn const_operand(&mut self, ty: Ty, source: DataSource, value: Option<ConstValue>) -> Operand {
        let Some(value) = value else {
            // Reported where it failed; the body is not used.
            self.failed = true;
            return match ty {
                Ty::Int(int) => Operand::Const(Const::Int(0, int)),
                Ty::Bool => Operand::Const(Const::Bool(false)),
                Ty::Unit | Ty::Never => Operand::Const(Const::Unit),
                ty => Operand::Copy(Place::local(self.new_local(ty, None))),
            };
        };
        match ty {
            Ty::Int(int) => Operand::Const(Const::Int(value.int(int.bytes() as usize), int)),
            Ty::Bool => Operand::Const(Const::Bool(value.bytes[0] != 0)),
            Ty::Unit | Ty::Never => Operand::Const(Const::Unit),
            // Made from an integer: an evaluation cannot give any other.
            Ty::Ptr(_) => {
                let usize_ty = hir::IntTy::Usize;
                let address =
                    Operand::Const(Const::Int(value.int(usize_ty.bytes() as usize), usize_ty));
                self.temp(ty, Rvalue::Cast(address, ty))
            }
            Ty::Array(..) | Ty::Struct(_) => {
                Operand::Copy(Place::local(self.const_local(ty, source, &value)))
            }
        }
    }

    /// Stores `rvalue` in a new temporary and returns a read of it.
    fn temp(&mut self, ty: Ty, rvalue: Rvalue) -> Operand {
        let place = Place::local(self.new_local(ty, None));
        self.assign(place, rvalue);
        Operand::Copy(place)
    }

    fn block(&mut self, block: &hir::Block) -> Operand {
        for stmt in &block.stmts {
            if self.current.is_none() {
                // Unreachable code: nothing to emit.
                break;
            }
            match stmt {
                hir::Stmt::Let { local, init } => {
                    let value = self.expr(init);
                    let local = self.hir_locals[local.0 as usize];
                    self.assign(Place::local(local), Rvalue::Use(value));
                }
                hir::Stmt::Expr { expr, .. } => {
                    self.expr(expr);
                }
            }
        }
        match &block.tail {
            Some(tail) if self.current.is_some() => self.expr(tail),
            _ => Operand::Const(Const::Unit),
        }
    }

    /// Lowers an expression and returns its value. Expressions of type `()`
    /// and `!` return `const ()`.
    fn expr(&mut self, expr: &hir::Expr) -> Operand {
        // Unreachable code emits nothing. Lowering it anyway could open
        // blocks that look reachable, and use the `()` that stands for the
        // value of what diverged.
        if self.current.is_none() {
            return Operand::Const(Const::Unit);
        }
        let outer = std::mem::replace(&mut self.span, expr.span);
        let value = self.expr_kind(expr);
        self.span = outer;
        value
    }

    fn expr_kind(&mut self, expr: &hir::Expr) -> Operand {
        let ty = self.ty_of(expr);
        let value = match &expr.kind {
            hir::ExprKind::Int(value) => {
                let Ty::Int(int) = ty else {
                    unreachable!("integer literals have integer types")
                };
                Operand::Const(Const::Int(*value, int))
            }
            hir::ExprKind::Byte(value) => Operand::Const(Const::Int(*value as u64, hir::IntTy::U8)),
            hir::ExprKind::Bool(value) => Operand::Const(Const::Bool(*value)),
            hir::ExprKind::Unit => Operand::Const(Const::Unit),
            hir::ExprKind::Const(id) => {
                let value = self.q.const_value(*id);
                self.const_operand(ty, DataSource::Const(*id), value)
            }
            hir::ExprKind::Comptime(inner) => {
                let value = self.q.comptime_value(self.fn_id, inner);
                self.const_operand(ty, DataSource::Comptime(inner.id), value)
            }
            // Layout queries are constants (ADR 0022).
            hir::ExprKind::SizeOf(ty) => usize_const(self.layout(*ty).size),
            hir::ExprKind::AlignOf(ty) => usize_const(self.layout(*ty).align),
            hir::ExprKind::OffsetOf(ty, _, _) => {
                let hir::Ty::Struct(s) = ty else {
                    unreachable!("the type checker allows only structs")
                };
                let layout = self
                    .q
                    .struct_layout(s.id)
                    .expect("well-typed programs have layouts");
                usize_const(layout.offsets[self.q.tables.field(expr.id) as usize])
            }
            hir::ExprKind::Array(elems) => {
                let array = Place::local(self.new_local(ty, None));
                // Each element is stored as soon as it is evaluated, so a
                // later element cannot change it (ADR 0018).
                for (i, elem) in elems.iter().enumerate() {
                    let value = self.expr(elem);
                    let place = array.project(PlaceElem::ConstIndex(i as u64));
                    self.assign(place, Rvalue::Use(value));
                }
                Operand::Copy(array)
            }
            hir::ExprKind::Repeat(value, len) => {
                let len = &self
                    .q
                    .array_len(*len)
                    .expect("well-typed programs have their lengths");
                // Evaluated exactly once, whatever the length (ADR 0021).
                let value = self.expr(value);
                let array = Place::local(self.new_local(ty, None));
                self.fill(array, value, *len);
                Operand::Copy(array)
            }
            hir::ExprKind::Local(local) => Operand::Copy(self.local(*local)),
            hir::ExprKind::Call { callee, args } => {
                let args = self.operands(args);
                let dest = self.result_place(ty);
                // MIR bodies are built in HIR function order.
                self.call(dest, BodyId(callee.0), args);
                self.read_result(dest)
            }
            hir::ExprKind::StructLit(_, fields) => {
                let value = Place::local(self.new_local(ty, None));
                // In the order written, each stored as soon as it is
                // evaluated (ADR 0018).
                for (index, init) in fields {
                    let init = self.expr(init);
                    self.assign(value.project(PlaceElem::Field(*index)), Rvalue::Use(init));
                }
                Operand::Copy(value)
            }
            hir::ExprKind::Deref(_) | hir::ExprKind::Index(..) | hir::ExprKind::Field(..) => {
                let place = self.place(expr);
                if matches!(ty, Ty::Unit | Ty::Never) {
                    Operand::Const(Const::Unit)
                } else {
                    // Read now: a volatile read must happen exactly here, and
                    // a later write must not change what was read.
                    self.temp(ty, Rvalue::Use(Operand::Copy(place)))
                }
            }
            // A negative literal is a constant, so `x / -1` is known to divide
            // by -1 (two's complement: `-v` is `0 - v`, wrapped).
            hir::ExprKind::Unary(UnOp::Neg, operand)
                if let hir::ExprKind::Int(value) = operand.kind
                    && let Ty::Int(int) = ty =>
            {
                Operand::Const(Const::Int(value.wrapping_neg() & int.mask(), int))
            }
            hir::ExprKind::Unary(op, operand) => {
                let operand = self.expr(operand);
                self.temp(ty, Rvalue::Unary(*op, operand))
            }
            hir::ExprKind::Binary(op, lhs, rhs) => {
                let mut lhs_value = self.expr(lhs);
                // Reading a local is deferred to where its operand is used;
                // if evaluating the right side could assign it, read now.
                if matches!(lhs_value, Operand::Copy(_)) && !is_simple(rhs) {
                    lhs_value = self.temp(self.operand_ty(lhs_value), Rvalue::Use(lhs_value));
                }
                let lhs = lhs_value;
                let rhs = self.expr(rhs);
                if matches!(op, BinOp::Div | BinOp::Rem) {
                    self.divide(*op, lhs, rhs, ty)
                } else {
                    self.temp(ty, Rvalue::Binary(*op, lhs, rhs))
                }
            }
            hir::ExprKind::AddrOf { mutable, place } => {
                let place = self.place_then(place, false, true);
                self.temp(
                    ty,
                    Rvalue::AddressOf {
                        mutable: *mutable,
                        place,
                    },
                )
            }
            hir::ExprKind::Cast(operand, _, _) => {
                let operand = self.expr(operand);
                self.temp(ty, Rvalue::Cast(operand, ty))
            }
            hir::ExprKind::Assign(place, value) => {
                // The value is evaluated before the place, as in Rust.
                let value = self.expr(value);
                let place = self.place(place);
                self.assign(place, Rvalue::Use(value));
                Operand::Const(Const::Unit)
            }
            hir::ExprKind::CompoundAssign(op, place, value) => {
                let value = self.expr(value);
                let place = self.place(place);
                if matches!(op, BinOp::Div | BinOp::Rem) {
                    let ty = self.operand_ty(Operand::Copy(place));
                    let quotient = self.divide(*op, Operand::Copy(place), value, ty);
                    self.assign(place, Rvalue::Use(quotient));
                } else {
                    self.assign(place, Rvalue::Binary(*op, Operand::Copy(place), value));
                }
                Operand::Const(Const::Unit)
            }
            hir::ExprKind::Assert(cond) => {
                let cond = self.expr(cond);
                self.panic_unless(cond, PanicCause::AssertionFailed);
                Operand::Const(Const::Unit)
            }
            hir::ExprKind::Block(block) => self.block(block),
            hir::ExprKind::Logical(op, lhs, rhs) => self.logical(*op, lhs, rhs),
            hir::ExprKind::If { cond, then, else_ } => {
                let cond = self.expr(cond);
                let result = self.result_place(ty);
                let then_bb = self.new_block();
                let else_bb = self.new_block();
                self.terminate(Terminator::If {
                    cond,
                    then: then_bb,
                    else_: else_bb,
                });

                self.current = Some(then_bb);
                let value = self.block(then);
                self.store_result(result, value);
                let then_end = self.current.take();

                self.current = Some(else_bb);
                if let Some(else_) = else_ {
                    let value = self.expr(else_);
                    self.store_result(result, value);
                }
                let else_end = self.current.take();

                self.join(&[then_end, else_end]);
                self.read_result(result)
            }
            hir::ExprKind::While { cond, body } => {
                let head = self.new_block();
                self.terminate(Terminator::Goto(head));
                self.current = Some(head);
                let cond = self.expr(cond);
                let body_bb = self.new_block();
                let exit = self.new_block();
                self.terminate(Terminator::If {
                    cond,
                    then: body_bb,
                    else_: exit,
                });
                self.current = Some(body_bb);
                self.in_loop(expr.id, head, Some(exit), None, |this| {
                    this.block(body);
                });
                if self.current.is_some() {
                    self.terminate(Terminator::Goto(head));
                }
                self.current = Some(exit);
                Operand::Const(Const::Unit)
            }
            hir::ExprKind::Loop(body) => {
                let result = self.result_place(ty);
                let head = self.new_block();
                self.terminate(Terminator::Goto(head));
                self.current = Some(head);
                let scope = self.in_loop(expr.id, head, None, result, |this| {
                    this.block(body);
                });
                if self.current.is_some() {
                    self.terminate(Terminator::Goto(head));
                }
                // Without a `break`, nothing follows the loop.
                self.current = scope.break_to;
                self.read_result(result)
            }
            hir::ExprKind::Break { target, value } => {
                let value = value.as_ref().map(|v| self.expr(v));
                let index = self.loop_index(*target);
                if let (Some(place), Some(value)) = (self.loops[index].result, value) {
                    self.assign(place, Rvalue::Use(value));
                }
                if self.current.is_some() {
                    let exit = match self.loops[index].break_to {
                        Some(exit) => exit,
                        None => {
                            let exit = self.new_block();
                            self.loops[index].break_to = Some(exit);
                            exit
                        }
                    };
                    self.terminate(Terminator::Goto(exit));
                }
                Operand::Const(Const::Unit)
            }
            hir::ExprKind::Continue { target } => {
                let head = self.loops[self.loop_index(*target)].continue_to;
                if self.current.is_some() {
                    self.terminate(Terminator::Goto(head));
                }
                Operand::Const(Const::Unit)
            }
            hir::ExprKind::Return(value) => {
                let value = value.as_ref().map(|v| self.expr(v));
                if let Some(value) = value
                    && self.locals[0].ty != Ty::Unit
                {
                    self.assign(Place::local(Local::RETURN), Rvalue::Use(value));
                }
                if self.current.is_some() {
                    self.terminate(Terminator::Return);
                }
                Operand::Const(Const::Unit)
            }
        };

        // Control cannot continue past an expression of type `!`.
        if ty == Ty::Never && self.current.is_some() {
            self.terminate(Terminator::Unreachable);
        }
        value
    }

    /// Division and remainder with the checks of ADR 0007, so that the
    /// `Div`/`Rem` left in MIR always have a valid divisor:
    ///
    /// - a zero divisor calls the panic function;
    /// - for signed types, a divisor of -1 gives `-lhs` (so `MIN / -1` wraps
    ///   to `MIN`) and a remainder of 0, never reaching the division.
    ///
    /// Checks that a constant divisor makes pointless are left out.
    fn divide(&mut self, op: BinOp, lhs: Operand, rhs: Operand, ty: Ty) -> Operand {
        let Ty::Int(int) = ty else {
            unreachable!("division of `{ty}`")
        };
        if self.current.is_none() {
            return Operand::Const(Const::Int(0, int));
        }
        let constant = match rhs {
            Operand::Const(Const::Int(value, _)) => Some(value & int.mask()),
            _ => None,
        };

        // The type checker rejects literal zero divisors; this keeps the
        // guarantee for a zero constant that reaches MIR any other way.
        if constant == Some(0) {
            self.terminate(Terminator::Panic(PanicCause::DivisionByZero));
            return Operand::Const(Const::Int(0, int));
        }
        if constant.is_none() {
            let zero = Operand::Const(Const::Int(0, int));
            let is_zero = self.temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, rhs, zero));
            let panic_bb = self.new_block();
            let ok_bb = self.new_block();
            self.terminate(Terminator::If {
                cond: is_zero,
                then: panic_bb,
                else_: ok_bb,
            });
            self.current = Some(panic_bb);
            self.terminate(Terminator::Panic(PanicCause::DivisionByZero));
            self.current = Some(ok_bb);
        }

        let minus_one = int.mask();
        let by_minus_one = || match op {
            BinOp::Div => Rvalue::Unary(UnOp::Neg, lhs),
            _ => Rvalue::Use(Operand::Const(Const::Int(0, int))),
        };
        match constant {
            _ if !int.is_signed() => self.temp(ty, Rvalue::Binary(op, lhs, rhs)),
            Some(value) if value == minus_one => self.temp(ty, by_minus_one()),
            Some(_) => self.temp(ty, Rvalue::Binary(op, lhs, rhs)),
            None => {
                let result = Place::local(self.new_local(ty, None));
                let m1 = Operand::Const(Const::Int(minus_one, int));
                let is_m1 = self.temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, rhs, m1));
                let m1_bb = self.new_block();
                let div_bb = self.new_block();
                self.terminate(Terminator::If {
                    cond: is_m1,
                    then: m1_bb,
                    else_: div_bb,
                });

                self.current = Some(m1_bb);
                self.assign(result, by_minus_one());
                let m1_end = self.current.take();

                self.current = Some(div_bb);
                self.assign(result, Rvalue::Binary(op, lhs, rhs));
                let div_end = self.current.take();

                self.join(&[m1_end, div_end]);
                Operand::Copy(result)
            }
        }
    }

    fn call(&mut self, dest: Option<Place>, callee: BodyId, args: Vec<Operand>) {
        if let Some(bb) = self.current {
            let block = &mut self.blocks[bb.0 as usize];
            block
                .statements
                .push(Statement::Call { dest, callee, args });
            block.spans.push(self.span);
        }
    }

    /// Lowers expressions left to right. A deferred local read is taken
    /// before any later expression that could assign it.
    fn operands(&mut self, exprs: &[hir::Expr]) -> Vec<Operand> {
        let mut operands = Vec::with_capacity(exprs.len());
        for (i, expr) in exprs.iter().enumerate() {
            let mut operand = self.expr(expr);
            if matches!(operand, Operand::Copy(_)) && !exprs[i + 1..].iter().all(is_simple) {
                operand = self.temp(self.operand_ty(operand), Rvalue::Use(operand));
            }
            operands.push(operand);
        }
        operands
    }

    /// `a && b` is `if a { b } else { false }`; `a || b` is
    /// `if a { true } else { b }`.
    fn logical(&mut self, op: hir::LogicalOp, lhs: &hir::Expr, rhs: &hir::Expr) -> Operand {
        let lhs = self.expr(lhs);
        let result = Place::local(self.new_local(Ty::Bool, None));
        let rhs_bb = self.new_block();
        let short_bb = self.new_block();
        let (then, else_) = match op {
            hir::LogicalOp::And => (rhs_bb, short_bb),
            hir::LogicalOp::Or => (short_bb, rhs_bb),
        };
        self.terminate(Terminator::If {
            cond: lhs,
            then,
            else_,
        });

        self.current = Some(rhs_bb);
        let value = self.expr(rhs);
        self.assign(result, Rvalue::Use(value));
        let rhs_end = self.current.take();

        self.current = Some(short_bb);
        let short = op == hir::LogicalOp::Or;
        self.assign(result, Rvalue::Use(Operand::Const(Const::Bool(short))));
        let short_end = self.current.take();

        self.join(&[rhs_end, short_end]);
        Operand::Copy(result)
    }

    /// Continues in a new block reached from every branch end that is still
    /// open; if none is, the code after the branches is unreachable.
    fn join(&mut self, ends: &[Option<BasicBlock>]) {
        let open: Vec<BasicBlock> = ends.iter().flatten().copied().collect();
        if open.is_empty() {
            return;
        }
        let join = self.new_block();
        for end in open {
            self.current = Some(end);
            self.terminate(Terminator::Goto(join));
        }
        self.current = Some(join);
    }

    /// A place for the value of a branching expression, if it has one.
    fn result_place(&mut self, ty: Ty) -> Option<Place> {
        (!matches!(ty, Ty::Unit | Ty::Never)).then(|| Place::local(self.new_local(ty, None)))
    }

    /// Stores `value` in every element of the array `array`: a loop over
    /// the indices, which LLVM turns into `memset` where it can.
    fn fill(&mut self, array: Place, value: Operand, len: u64) {
        let usize_ty = hir::IntTy::Usize;
        if len == 1 {
            self.assign(array.project(PlaceElem::ConstIndex(0)), Rvalue::Use(value));
            return;
        }
        if len == 0 || self.current.is_none() {
            return;
        }
        let i = self.new_local(Ty::Int(usize_ty), None);
        let index = Place::local(i);
        self.assign(index, Rvalue::Use(Operand::Const(Const::Int(0, usize_ty))));
        let head = self.new_block();
        self.terminate(Terminator::Goto(head));
        self.current = Some(head);
        let more = self.temp(
            Ty::Bool,
            Rvalue::Binary(
                BinOp::Lt,
                Operand::Copy(index),
                Operand::Const(Const::Int(len, usize_ty)),
            ),
        );
        let body = self.new_block();
        let exit = self.new_block();
        self.terminate(Terminator::If {
            cond: more,
            then: body,
            else_: exit,
        });
        self.current = Some(body);
        // `i < len`, so the element exists.
        self.assign(array.project(PlaceElem::Index(i)), Rvalue::Use(value));
        self.assign(
            index,
            Rvalue::Binary(
                BinOp::Add,
                Operand::Copy(index),
                Operand::Const(Const::Int(1, usize_ty)),
            ),
        );
        self.terminate(Terminator::Goto(head));
        self.current = Some(exit);
    }

    /// Continues only if `ok` is true; otherwise calls the panic function.
    fn panic_unless(&mut self, ok: Operand, cause: PanicCause) {
        if self.current.is_none() {
            return;
        }
        let ok_bb = self.new_block();
        let panic_bb = self.new_block();
        self.terminate(Terminator::If {
            cond: ok,
            then: ok_bb,
            else_: panic_bb,
        });
        self.current = Some(panic_bb);
        self.terminate(Terminator::Panic(cause));
        self.current = Some(ok_bb);
    }

    fn store_result(&mut self, result: Option<Place>, value: Operand) {
        if let Some(place) = result {
            self.assign(place, Rvalue::Use(value));
        }
    }

    fn read_result(&self, result: Option<Place>) -> Operand {
        result.map_or(Operand::Const(Const::Unit), Operand::Copy)
    }

    fn in_loop(
        &mut self,
        id: hir::ExprId,
        continue_to: BasicBlock,
        break_to: Option<BasicBlock>,
        result: Option<Place>,
        body: impl FnOnce(&mut Self),
    ) -> LoopScope {
        self.loops.push(LoopScope {
            id,
            continue_to,
            break_to,
            result,
        });
        body(self);
        self.loops.pop().expect("pushed above")
    }

    fn loop_index(&self, target: hir::ExprId) -> usize {
        self.loops
            .iter()
            .rposition(|scope| scope.id == target)
            .expect("lowering resolved the loop")
    }

    fn layout(&mut self, ty: hir::Ty) -> crate::layout::Layout {
        self.q
            .layout_of(ty)
            .expect("well-typed programs have layouts")
    }

    /// The MIR type of an expression, from the type checker.
    fn ty_of(&mut self, expr: &hir::Expr) -> Ty {
        let ty = self.q.tables.expr_ty(expr.id);
        self.q.mir_ty(ty)
    }

    fn local(&self, local: hir::LocalId) -> Place {
        Place::local(self.hir_locals[local.0 as usize])
    }

    fn operand_ty(&self, operand: Operand) -> Ty {
        match operand {
            Operand::Copy(place) => self.place_ty(place),
            Operand::Const(Const::Int(_, int)) => Ty::Int(int),
            Operand::Const(Const::Bool(_)) => Ty::Bool,
            Operand::Const(Const::Unit) => Ty::Unit,
        }
    }

    fn place_ty(&self, place: Place) -> Ty {
        place
            .projection
            .iter()
            .fold(self.locals[place.local.0 as usize].ty, |ty, elem| {
                match (elem, ty) {
                    (PlaceElem::Deref | PlaceElem::Offset(_), Ty::Ptr(ptr)) => *ptr.pointee,
                    (PlaceElem::Index(_) | PlaceElem::ConstIndex(_), Ty::Array(elem, _)) => *elem,
                    (PlaceElem::Field(index), Ty::Struct(s)) => {
                        s.def().fields()[*index as usize].ty
                    }
                    (elem, ty) => unreachable!("projection {elem:?} of `{ty}`"),
                }
            })
    }

    /// A place holding the value of a pointer expression. If code evaluated
    /// later could assign the pointer, it is read now (ADR 0018).
    fn pointer_place(&mut self, pointer: &hir::Expr, later_writes: bool) -> Place {
        let pointer_ty = self.ty_of(pointer);
        match self.expr(pointer) {
            Operand::Copy(place) if !later_writes => place,
            // Computed (e.g. a cast), or about to be read late: store it.
            other => match self.temp(pointer_ty, Rvalue::Use(other)) {
                Operand::Copy(place) => place,
                Operand::Const(_) => unreachable!("temporaries are places"),
            },
        }
    }

    /// Lowers a place expression: a local, `*pointer` or `base[index]`.
    /// Any other expression is evaluated into a temporary, whose place is
    /// returned.
    fn place(&mut self, expr: &hir::Expr) -> Place {
        self.place_then(expr, false, false)
    }

    /// `later_writes`: code evaluated after this place, but before it is
    /// accessed, may assign variables (ADR 0018). `address`: the place's
    /// address is taken, so a value computed at compile time is copied
    /// into a local first: its data has no address (ADR 0029).
    fn place_then(&mut self, expr: &hir::Expr, later_writes: bool, address: bool) -> Place {
        match &expr.kind {
            hir::ExprKind::Local(local) => self.local(*local),
            hir::ExprKind::Deref(pointer) => self
                .pointer_place(pointer, later_writes)
                .project(PlaceElem::Deref),
            hir::ExprKind::Index(base, index) => {
                let later_writes = later_writes || !is_simple(index);
                match self.ty_of(base) {
                    Ty::Array(_, len) => {
                        let base = self.place_then(base, later_writes, address);
                        let elem = self.array_index(index, len);
                        base.project(elem)
                    }
                    _ => {
                        let base = self.pointer_place(base, later_writes);
                        let index = self.fixed_index(index);
                        base.project(PlaceElem::Offset(index))
                    }
                }
            }
            hir::ExprKind::Field(base, _, _) => {
                let base_place = self.place_then(base, later_writes, address);
                // A base of type `!` is never computed, and has no fields.
                if self.ty_of(base) == Ty::Never {
                    return base_place;
                }
                base_place.project(PlaceElem::Field(self.q.tables.field(expr.id)))
            }
            // A value, not a place: it is used where it is evaluated
            // (ADR 0018), so it is copied unless nothing else can write
            // where it is, as with a call's result.
            _ => {
                let ty = self.ty_of(expr);
                match self.expr(expr) {
                    Operand::Copy(place) if !self.must_copy(place, address) => place,
                    other => match self.temp(ty, Rvalue::Use(other)) {
                        Operand::Copy(place) => place,
                        Operand::Const(_) => unreachable!("temporaries are places"),
                    },
                }
            }
        }
    }

    /// Whether a value held at `place` must be copied before it is used as
    /// a place. A variable (only variables have names) can be written by
    /// later code; data computed at compile time has no address
    /// (ADR 0029), so it is copied only when its address is taken.
    fn must_copy(&self, place: Place, address: bool) -> bool {
        let decl = &self.locals[place.local.0 as usize];
        decl.name.is_some() || (address && decl.constant.is_some())
    }

    /// Evaluates an index into a new local, so that a later write to its
    /// variable does not move the place.
    fn fixed_index(&mut self, index: &hir::Expr) -> Local {
        let value = self.expr(index);
        let local = self.new_local(Ty::Int(hir::IntTy::Usize), None);
        self.assign(Place::local(local), Rvalue::Use(value));
        local
    }

    /// The projection to element `index` of an array of `len` elements,
    /// after checking the bounds (ADR 0021). A constant index was checked
    /// by the type checker.
    fn array_index(&mut self, index: &hir::Expr, len: u64) -> PlaceElem {
        if let hir::ExprKind::Int(value) = index.kind
            && value < len
        {
            return PlaceElem::ConstIndex(value);
        }
        let local = self.fixed_index(index);
        let usize_ty = hir::IntTy::Usize;
        let in_bounds = self.temp(
            Ty::Bool,
            Rvalue::Binary(
                BinOp::Lt,
                Operand::Copy(Place::local(local)),
                Operand::Const(Const::Int(len, usize_ty)),
            ),
        );
        self.panic_unless(in_bounds, PanicCause::IndexOutOfBounds);
        PlaceElem::Index(local)
    }
}

fn usize_const(value: u64) -> Operand {
    Operand::Const(Const::Int(value, hir::IntTy::Usize))
}

/// Expressions whose evaluation cannot write to any place.
fn is_simple(expr: &hir::Expr) -> bool {
    match &expr.kind {
        hir::ExprKind::Int(_)
        | hir::ExprKind::Byte(_)
        | hir::ExprKind::Bool(_)
        | hir::ExprKind::Unit
        | hir::ExprKind::Local(_) => true,
        hir::ExprKind::Unary(UnOp::Neg, operand) => matches!(operand.kind, hir::ExprKind::Int(_)),
        _ => false,
    }
}
