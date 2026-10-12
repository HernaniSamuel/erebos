//! Typed HIR -> MIR.

use crate::hir::{self, Hir};
use crate::mir::{
    BasicBlock, BasicBlockData, Body, BodyId, Const, Local, LocalDecl, Mir, Operand, Place, Ptr,
    Rvalue, Statement, Terminator, Ty,
};
use crate::ty::TypeckResults;

/// The input must have passed type checking without errors.
pub fn build(hir: &Hir, types: &TypeckResults) -> Mir {
    let bodies = hir.fns().map(|(_, f)| build_body(f, types)).collect();
    Mir {
        bodies,
        // MIR bodies are built in HIR function order.
        entry: BodyId(hir.entry.0),
        panic: BodyId(hir.panic.0),
    }
}

fn build_body(f: &hir::Fn, types: &TypeckResults) -> Body {
    let ret = lower_ty(f.ret);
    let mut builder = Builder {
        types,
        locals: vec![LocalDecl {
            ty: ret,
            name: None,
        }],
        hir_locals: Vec::new(),
        blocks: Vec::new(),
        current: None,
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
    locals: Vec<LocalDecl>,
    /// MIR local of each HIR local.
    hir_locals: Vec<Local>,
    blocks: Vec<PartialBlock>,
    /// The block code is currently appended to; `None` after control flow
    /// has diverged, so following code is unreachable and not emitted.
    current: Option<BasicBlock>,
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

    fn terminate(&mut self, terminator: Terminator) {
        let bb = self.current.take().expect("no block to terminate");
        self.blocks[bb.0 as usize].terminator = Some(terminator);
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
            hir::ExprKind::Unit => Operand::Const(Const::Unit),
            hir::ExprKind::Local(local) => Operand::Copy(self.local(*local)),
            hir::ExprKind::Deref(_) => {
                let place = self.place(expr);
                if ty == Ty::Unit {
                    Operand::Const(Const::Unit)
                } else {
                    // Read now: a volatile read must happen exactly here, and
                    // a later write must not change what was read.
                    self.temp(ty, Rvalue::Use(Operand::Copy(place)))
                }
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
                self.temp(ty, Rvalue::Binary(*op, lhs, rhs))
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
                self.assign(place, Rvalue::Binary(*op, Operand::Copy(place), value));
                Operand::Const(Const::Unit)
            }
            hir::ExprKind::Block(block) => self.block(block),
            hir::ExprKind::Loop(body) => {
                let head = self.new_block();
                self.terminate(Terminator::Goto(head));
                self.current = Some(head);
                self.block(body);
                if self.current.is_some() {
                    self.terminate(Terminator::Goto(head));
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

    fn local(&self, local: hir::LocalId) -> Place {
        Place::local(self.hir_locals[local.0 as usize])
    }

    fn operand_ty(&self, operand: Operand) -> Ty {
        match operand {
            Operand::Copy(place) => {
                let ty = self.locals[place.local.0 as usize].ty;
                match (ty, place.deref) {
                    (Ty::Ptr(ptr), true) => *ptr.pointee,
                    (ty, _) => ty,
                }
            }
            Operand::Const(Const::Int(_, int)) => Ty::Int(int),
            Operand::Const(Const::Unit) => Ty::Unit,
        }
    }

    /// Lowers a place expression: a local, or `*pointer`.
    fn place(&mut self, expr: &hir::Expr) -> Place {
        match &expr.kind {
            hir::ExprKind::Local(local) => self.local(*local),
            hir::ExprKind::Deref(pointer) => {
                let pointer_ty = lower_ty(self.types.expr_ty(pointer.id));
                let local = match self.expr(pointer) {
                    Operand::Copy(place) if !place.deref => place.local,
                    // The pointer is computed (e.g. a cast): store it first.
                    other => match self.temp(pointer_ty, Rvalue::Use(other)) {
                        Operand::Copy(place) => place.local,
                        Operand::Const(_) => unreachable!("temporaries are places"),
                    },
                };
                Place { local, deref: true }
            }
            _ => unreachable!("the type checker only allows locals and `*pointer` as places"),
        }
    }
}

/// Expressions whose evaluation cannot write to any place.
fn is_simple(expr: &hir::Expr) -> bool {
    matches!(
        expr.kind,
        hir::ExprKind::Int(_)
            | hir::ExprKind::Byte(_)
            | hir::ExprKind::Unit
            | hir::ExprKind::Local(_)
    )
}
