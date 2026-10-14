//! Typed HIR -> MIR.

use crate::hir::{self, Hir};
use crate::mir::{
    BasicBlock, BasicBlockData, BinOp, Body, BodyId, Const, Local, LocalDecl, Mir, Operand, Place,
    PlaceElem, Ptr, Rvalue, Statement, Terminator, Ty, UnOp,
};
use crate::ty::TypeckResults;

/// The input must have passed type checking without errors.
pub fn build(hir: &Hir, types: &TypeckResults) -> Mir {
    // MIR bodies are built in HIR function order.
    let panic = BodyId(hir.panic.0);
    let bodies = hir
        .fns()
        .map(|(_, f)| build_body(f, types, panic))
        .collect();
    Mir {
        bodies,
        entry: BodyId(hir.entry.0),
        panic,
    }
}

fn build_body(f: &hir::Fn, types: &TypeckResults, panic: BodyId) -> Body {
    let ret = lower_ty(f.ret);
    let mut builder = Builder {
        types,
        panic,
        locals: vec![LocalDecl {
            ty: ret,
            name: None,
        }],
        hir_locals: Vec::new(),
        blocks: Vec::new(),
        current: None,
        loops: Vec::new(),
    };
    // HIR locals become MIR locals `_1..`, in declaration order.
    for local in &f.locals {
        let mir_local = builder.new_local(lower_ty(local.ty), Some(local.name.clone()));
        builder.hir_locals.push(mir_local);
    }

    let start = builder.new_block();
    builder.current = Some(start);
    let value = builder.block(&f.body);
    if ret != Ty::Unit {
        builder.assign(Place::local(Local::RETURN), Rvalue::Use(value));
    }
    if builder.current.is_some() {
        builder.terminate(Terminator::Return);
    }

    Body {
        name: f.name.clone(),
        ret,
        param_count: f.param_count,
        locals: builder.locals,
        blocks: builder
            .blocks
            .into_iter()
            .map(|b| BasicBlockData {
                statements: b.statements,
                terminator: b.terminator.expect("every block is terminated"),
            })
            .collect(),
    }
}

fn lower_ty(ty: hir::Ty) -> Ty {
    match ty {
        hir::Ty::Unit => Ty::Unit,
        hir::Ty::Never => Ty::Never,
        hir::Ty::Bool => Ty::Bool,
        hir::Ty::Int(int) => Ty::Int(int),
        hir::Ty::Ptr(ptr) => Ty::Ptr(Ptr {
            mutable: ptr.mutable,
            volatile: ptr.volatile,
            // Leaked like HIR pointee types; see `hir::Ty::ptr`.
            pointee: Box::leak(Box::new(lower_ty(*ptr.pointee))),
        }),
        hir::Ty::Error => unreachable!("MIR is only built for well-typed programs"),
    }
}

struct PartialBlock {
    statements: Vec<Statement>,
    terminator: Option<Terminator>,
}

struct Builder<'a> {
    types: &'a TypeckResults,
    /// The panic function (ADR 0014).
    panic: BodyId,
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

impl Builder<'_> {
    fn new_local(&mut self, ty: Ty, name: Option<String>) -> Local {
        self.locals.push(LocalDecl { ty, name });
        Local(self.locals.len() as u32 - 1)
    }

    fn new_block(&mut self) -> BasicBlock {
        self.blocks.push(PartialBlock {
            statements: Vec::new(),
            terminator: None,
        });
        BasicBlock(self.blocks.len() as u32 - 1)
    }

    fn assign(&mut self, place: Place, rvalue: Rvalue) {
        if let Some(bb) = self.current {
            self.blocks[bb.0 as usize]
                .statements
                .push(Statement::Assign(place, rvalue));
        }
    }

    /// Ends the current block. In unreachable code there is none, and
    /// nothing is emitted.
    fn terminate(&mut self, terminator: Terminator) {
        if let Some(bb) = self.current.take() {
            self.blocks[bb.0 as usize].terminator = Some(terminator);
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
        let ty = lower_ty(self.types.expr_ty(expr.id));
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
            hir::ExprKind::Local(local) => Operand::Copy(self.local(*local)),
            hir::ExprKind::Call { callee, args } => {
                let args = self.operands(args);
                let dest = self.result_place(ty);
                // MIR bodies are built in HIR function order.
                self.call(dest, BodyId(callee.0), args);
                self.read_result(dest)
            }
            hir::ExprKind::Deref(_) | hir::ExprKind::Index(..) => {
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
                let place = self.place(place);
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
        let constant = match rhs {
            Operand::Const(Const::Int(value, _)) => Some(value & int.mask()),
            _ => None,
        };

        // The type checker rejects literal zero divisors; this keeps the
        // guarantee for a zero constant that reaches MIR any other way.
        if constant == Some(0) {
            self.call(None, self.panic, Vec::new());
            self.terminate(Terminator::Unreachable);
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
            self.call(None, self.panic, Vec::new());
            self.terminate(Terminator::Unreachable);
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
            self.blocks[bb.0 as usize]
                .statements
                .push(Statement::Call { dest, callee, args });
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
        matches!(ty, Ty::Bool | Ty::Int(_) | Ty::Ptr(_))
            .then(|| Place::local(self.new_local(ty, None)))
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
                    (_, ty) => unreachable!("dereference of `{ty}`"),
                }
            })
    }

    /// A place holding the value of a pointer expression. With `then`, an
    /// expression evaluated after the pointer: if it could assign the
    /// pointer, the pointer is read first (ADR 0018).
    fn pointer_place(&mut self, pointer: &hir::Expr, then: Option<&hir::Expr>) -> Place {
        let pointer_ty = lower_ty(self.types.expr_ty(pointer.id));
        match self.expr(pointer) {
            Operand::Copy(place) if then.is_none_or(is_simple) => place,
            // Computed (e.g. a cast), or about to be read late: store it.
            other => match self.temp(pointer_ty, Rvalue::Use(other)) {
                Operand::Copy(place) => place,
                Operand::Const(_) => unreachable!("temporaries are places"),
            },
        }
    }

    /// Lowers a place expression: a local, `*pointer` or `pointer[index]`.
    fn place(&mut self, expr: &hir::Expr) -> Place {
        match &expr.kind {
            hir::ExprKind::Local(local) => self.local(*local),
            hir::ExprKind::Deref(pointer) => {
                self.pointer_place(pointer, None).project(PlaceElem::Deref)
            }
            hir::ExprKind::Index(pointer, index) => {
                let base = self.pointer_place(pointer, Some(index));
                // The index is fixed now: a later write to its variable
                // must not move the place.
                let value = self.expr(index);
                let index = self.new_local(Ty::Int(hir::IntTy::Usize), None);
                self.assign(Place::local(index), Rvalue::Use(value));
                base.project(PlaceElem::Offset(index))
            }
            _ => unreachable!("the type checker only allows locals and `*pointer` as places"),
        }
    }
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
