//! Typed HIR -> MIR, one function at a time (ADR 0028).

use std::collections::HashMap;

use crate::eval::ConstValue;
use crate::hir;
use crate::mir::{
    BasicBlock, BasicBlockData, BinOp, Body, BodyId, Const, DataId, Local, LocalDecl, Operand,
    PanicCause, PanicInfoAt, PanicSite, Place, PlaceElem, Rvalue, Statement, StaticRef, Terminator,
    Ty, UnOp,
};
use crate::query::{DataSource, Queries};
use crate::span::Span;

use known::Known;

mod bit_struct;
mod known;

/// The MIR of a function that passed type checking, or `None` if a value
/// it needs from compile-time evaluation failed, or a check on a value
/// known at compile time failed (ADR 0032), both reported.
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
    // A body run at compile time has no symbol, and is shown by what it
    // computes (ADR 0049, 0051).
    builder.finish(
        f.symbol.clone().unwrap_or_else(|| f.path.clone()),
        f.span,
        f.comptime.is_some(),
        f.runtime,
        f.body.is_none(),
    )
}

/// A body that computes `expr`, part of function `id`, alone: the MIR that
/// `comptime expr` runs (ADR 0029).
pub fn build_expr_body(q: &mut Queries, id: hir::FnId, expr: &hir::Expr) -> Option<Body> {
    // Its value is converted where it is, if it is (ADR 0031).
    let ty = q
        .tables
        .coercion(expr.id)
        .unwrap_or_else(|| q.tables.expr_ty(expr.id));
    let ret = q.mir_ty(ty);
    let mut builder = Builder::new(q, id, ret);
    builder.span = expr.span;
    builder.located = false;
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
    /// A value from compile-time evaluation was missing, or a check
    /// failed; the body cannot be used.
    failed: bool,
    /// Whether the checks of ADR 0032 apply: off in code that cannot run.
    checks: bool,
    /// Whether the compiler's checks pass where they are to the panic
    /// function: when it takes the information, in code that runs on the
    /// target, `core` included; code run at compile time never calls it
    /// (ADR 0053).
    located: bool,
    /// Values known at compile time, by expression.
    known: HashMap<hir::ExprId, Known>,
    /// The values of the constants and `comptime` expressions used, asked
    /// once: asking again for one being computed would report its cycle
    /// again.
    const_values: HashMap<hir::ConstId, Option<ConstValue>>,
    comptime_values: HashMap<hir::ExprId, Option<ConstValue>>,
    /// The values of the variants used, likewise (ADR 0035).
    discriminants: HashMap<(hir::EnumId, u32), Option<u64>>,
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

/// Whether `pattern` names one value of its position, as a literal, a
/// constant or a variant written without a payload: consecutive
/// alternatives of this kind are one switch (ADR 0042).
fn is_value(pattern: &hir::Pattern) -> bool {
    matches!(
        pattern.kind,
        hir::PatternKind::Int { .. }
            | hir::PatternKind::Byte(_)
            | hir::PatternKind::Bool(_)
            | hir::PatternKind::Const(_)
            | hir::PatternKind::Variant(_, _, None)
            | hir::PatternKind::ContextVariant(_, _, _, None)
    )
}

/// The place of the value at `position` in `scrutinee`.
fn position_place(scrutinee: Place, position: &[hir::PayloadStep]) -> Place {
    position.iter().fold(scrutinee, |place, step| {
        place.project(PlaceElem::Payload {
            variant: step.variant,
            index: step.index,
        })
    })
}

/// The place a test reads at `position` in `scrutinee`, whose type is
/// `ty`, and the type of what it reads: an integer, a `bool`, the integer
/// of an enum without payloads, or the tag of one with payloads.
fn test_place(scrutinee: Place, position: &[hir::PayloadStep], ty: hir::Ty) -> (Place, Ty) {
    let place = position_place(scrutinee, position);
    match ty {
        hir::Ty::Enum(e) if e.payloads => (place.project(PlaceElem::Tag), Ty::Int(e.int)),
        hir::Ty::Enum(e) => (place, Ty::Int(e.int)),
        hir::Ty::Int(int) => (place, Ty::Int(int)),
        hir::Ty::Bool => (place, Ty::Bool),
        _ => unreachable!("only integers, `bool` and enums are tested"),
    }
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
        let located = f.comptime.is_none() && q.takes_panic_info();
        let mut builder = Builder {
            q,
            fn_id,
            span: f.span,
            failed: false,
            checks: true,
            located,
            known: HashMap::new(),
            const_values: HashMap::new(),
            comptime_values: HashMap::new(),
            discriminants: HashMap::new(),
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
            // A pointer to a static: its address only exists after linking,
            // so it is data the linker completes (ADR 0030). A slice is an
            // aggregate, read from its data likewise (ADR 0031).
            Ty::Ptr(_) if !value.relocations.is_empty() || ty.is_aggregate() => {
                Operand::Copy(Place::local(self.const_local(ty, source, &value)))
            }
            // Made from an integer.
            Ty::Ptr(_) => {
                let usize_ty = hir::IntTy::Usize;
                let address =
                    Operand::Const(Const::Int(value.int(usize_ty.bytes() as usize), usize_ty));
                self.temp(ty, Rvalue::Cast(address, ty))
            }
            Ty::Array(..) | Ty::Struct(_) | Ty::Enum(_) => {
                Operand::Copy(Place::local(self.const_local(ty, source, &value)))
            }
            Ty::Slice(_) => unreachable!("`[T]` is unsized, and no value has it (ADR 0031)"),
            Ty::Peripheral(_) => unreachable!("no value has a peripheral type (ADR 0058)"),
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
        let mut value = self.expr_kind(expr);
        // A pointer to an array converted to a slice, with its length
        // (ADR 0031).
        if let Some(slice_ty) = self.q.tables.coercion(expr.id)
            && self.current.is_some()
        {
            let slice_ty = self.q.mir_ty(slice_ty);
            value = self.array_slice(value, slice_ty);
        }
        self.span = outer;
        value
    }

    /// The slice of type `slice_ty` of the whole array `pointer` points to.
    fn array_slice(&mut self, pointer: Operand, slice_ty: Ty) -> Operand {
        let Ty::Ptr(ptr) = self.operand_ty(pointer) else {
            unreachable!("only pointers to arrays convert to slices")
        };
        let Ty::Array(_, len) = *ptr.pointee else {
            unreachable!("only pointers to arrays convert to slices")
        };
        self.temp(
            slice_ty,
            Rvalue::Slice {
                ptr: pointer,
                len: usize_const(len),
            },
        )
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
                let value = self.const_value(*id);
                self.const_operand(ty, DataSource::Const(*id), value)
            }
            hir::ExprKind::Comptime(inner) => {
                let value = self.comptime_value(inner);
                self.const_operand(ty, DataSource::Comptime(inner.id), value)
            }
            hir::ExprKind::Variant(id, index, args) => self.variant(ty, *id, *index, args),
            // A device is read one register at a time: the type checker
            // rejects reading one whole (ADR 0058), and its registers are
            // places reached through `place`.
            hir::ExprKind::Peripheral(_) => {
                unreachable!("a peripheral is only reached through its registers")
            }
            // Resolved by type checking, as `E::V` is by name (ADR 0044).
            hir::ExprKind::ContextVariant(_, _, args) => {
                let (id, index) = self.q.tables.variant(expr.id);
                self.variant(ty, id, index, args)
            }
            hir::ExprKind::SizeOf(_) | hir::ExprKind::AlignOf(_) | hir::ExprKind::OffsetOf(..) => {
                usize_const(self.layout_query(expr))
            }
            // An element does not finish, so there is no array: the
            // elements run, and nothing is stored (ADR 0034).
            hir::ExprKind::Array(elems) if ty == Ty::Never => {
                for elem in elems {
                    self.expr(elem);
                }
                Operand::Const(Const::Unit)
            }
            hir::ExprKind::Repeat(value, _) if ty == Ty::Never => {
                self.expr(value);
                Operand::Const(Const::Unit)
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
            hir::ExprKind::BitStructLit(s, fields, rest) => self.bit_struct_lit(*s, fields, *rest),
            // A bit field is no place in memory: the whole value is read,
            // and the field taken from it (ADR 0059).
            hir::ExprKind::Field(base, _, _)
                if let Some((id, index)) = self.q.tables.bit_field(expr.id) =>
            {
                self.read_bit_field(base, id, index)
            }
            hir::ExprKind::Deref(_)
            | hir::ExprKind::Index(..)
            | hir::ExprKind::Field(..)
            | hir::ExprKind::Static(_)
            | hir::ExprKind::ByteString(_) => {
                let place = self.place(expr);
                if matches!(ty, Ty::Unit | Ty::Never) {
                    // Read all the same, to check it at compile time: it
                    // may be a static that cannot be read (ADR 0030).
                    self.temp(ty, Rvalue::Use(Operand::Copy(place)));
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
            hir::ExprKind::Binary(op, lhs, rhs, op_span) => {
                let mut lhs_value = self.expr(lhs);
                // Reading a local is deferred to where its operand is used;
                // if evaluating the right side could assign it, read now.
                if matches!(lhs_value, Operand::Copy(_)) && !is_simple(rhs) {
                    lhs_value = self.temp(self.operand_ty(lhs_value), Rvalue::Use(lhs_value));
                }
                let lhs_ty = self.operand_ty(lhs_value);
                let lhs = lhs_value;
                let rhs_value = self.expr(rhs);
                if op.is_shift() {
                    self.check_shift(lhs_ty, rhs);
                }
                // After a reported zero divisor the body is not used, but
                // the code after it is built, to check it too.
                let reported = self.check_divisor(*op, rhs);
                let rhs = rhs_value;
                if matches!(op, BinOp::Div | BinOp::Rem) && !reported {
                    self.divide(*op, lhs, rhs, ty, *op_span)
                } else {
                    self.temp(ty, Rvalue::Binary(*op, lhs, rhs))
                }
            }
            // What a slice points to has no address of its own: `&raw` of
            // it is the slice, with the mutability asked for (ADR 0031).
            hir::ExprKind::AddrOf { place, .. }
                if let hir::ExprKind::Deref(pointer) = &place.kind
                    && self.ty_of(pointer).slice_elem().is_some() =>
            {
                let slice = self.expr(pointer);
                if self.current.is_none() {
                    // The slice did not finish, and nothing follows.
                    return Operand::Const(Const::Unit);
                }
                // The slice may be `*mut` or volatile, and the result not:
                // both types have one representation, so it is a copy.
                self.temp(ty, Rvalue::Use(slice))
            }
            // The peripheral's address, with the mutability asked for: a
            // peripheral is not `mut`, and both pointers have one
            // representation (ADR 0058).
            hir::ExprKind::AddrOf { place, .. }
                if let hir::ExprKind::Peripheral(id) = &place.kind =>
            {
                let p = self.q.mir_peripheral(*id);
                self.temp(ty, Rvalue::Use(Operand::Const(Const::Peripheral(p))))
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
                self.cast(operand, ty)
            }
            // Reads the slice, never what it points to (ADR 0031).
            hir::ExprKind::Len(slice) => match self.expr(slice) {
                Operand::Copy(place) if self.current.is_some() => {
                    self.temp(ty, Rvalue::SliceLen(place))
                }
                // The slice did not finish, and nothing follows.
                _ => usize_const(0),
            },
            hir::ExprKind::Slice(pointer, len) => {
                let mut ptr = self.expr(pointer);
                // Read now if evaluating the length could assign it.
                if matches!(ptr, Operand::Copy(_)) && !is_simple(len) {
                    ptr = self.temp(self.operand_ty(ptr), Rvalue::Use(ptr));
                }
                let len = self.expr(len);
                if ty == Ty::Never {
                    Operand::Const(Const::Unit)
                } else {
                    self.temp(ty, Rvalue::Slice { ptr, len })
                }
            }
            hir::ExprKind::Assign(place, value)
                if let hir::ExprKind::Field(base, _, name) = &place.kind
                    && let Some((id, index)) = self.q.tables.bit_field(place.id) =>
            {
                self.assign_bit_field(base, id, index, *name, value);
                Operand::Const(Const::Unit)
            }
            hir::ExprKind::Assign(place, value) => {
                // The value is evaluated before the place, as in Rust.
                let value = self.expr(value);
                let place = self.place(place);
                self.assign(place, Rvalue::Use(value));
                Operand::Const(Const::Unit)
            }
            hir::ExprKind::CompoundAssign(op, place, value, op_span)
                if let hir::ExprKind::Field(base, _, name) = &place.kind
                    && let Some((id, index)) = self.q.tables.bit_field(place.id) =>
            {
                self.compound_assign_bit_field(*op, base, id, index, *name, value, *op_span);
                Operand::Const(Const::Unit)
            }
            hir::ExprKind::CompoundAssign(op, place, value_expr, op_span) => {
                let value = self.expr(value_expr);
                let place = self.place(place);
                if op.is_shift() {
                    let shifted = self.operand_ty(Operand::Copy(place));
                    self.check_shift(shifted, value_expr);
                }
                let reported = self.check_divisor(*op, value_expr);
                if matches!(op, BinOp::Div | BinOp::Rem) && !reported {
                    let ty = self.operand_ty(Operand::Copy(place));
                    let quotient = self.divide(*op, Operand::Copy(place), value, ty, *op_span);
                    self.assign(place, Rvalue::Use(quotient));
                } else {
                    self.assign(place, Rvalue::Binary(*op, Operand::Copy(place), value));
                }
                Operand::Const(Const::Unit)
            }
            hir::ExprKind::Assert(cond, None, keyword) => {
                let cond = self.expr(cond);
                self.panic_unless(cond, PanicCause::AssertionFailed(None), *keyword);
                Operand::Const(Const::Unit)
            }
            hir::ExprKind::Assert(cond_expr, Some(message), keyword) => {
                let cond = self.expr(cond_expr);
                // Like the right operand of `||`: the message is evaluated
                // only if the assertion fails (ADR 0031), so it cannot run
                // when the condition is known to hold (ADR 0032).
                let holds = self.known_condition(cond_expr) == Some(true);
                if self.current.is_some() {
                    let ok_bb = self.new_block();
                    let fail_bb = self.new_block();
                    self.terminate(Terminator::If {
                        cond,
                        then: ok_bb,
                        else_: fail_bb,
                    });
                    self.current = Some(fail_bb);
                    let value = self.checked_if(!holds, |this| this.expr(message));
                    // A message that diverges, as `return 4`, leaves no
                    // block to fill and call from.
                    if self.current.is_some() {
                        let panic = self.assert_panic(value, message.span, *keyword);
                        self.terminate(panic);
                    }
                    self.current = Some(ok_bb);
                }
                Operand::Const(Const::Unit)
            }
            hir::ExprKind::Block(block) => self.block(block),
            hir::ExprKind::Logical(op, lhs, rhs) => self.logical(*op, lhs, rhs),
            hir::ExprKind::If { cond, then, else_ } => {
                let cond_expr = cond;
                let cond = self.expr(cond_expr);
                let known = self.known_condition(cond_expr);
                let result = self.result_place(ty);
                let then_bb = self.new_block();
                let else_bb = self.new_block();
                self.terminate(Terminator::If {
                    cond,
                    then: then_bb,
                    else_: else_bb,
                });

                self.current = Some(then_bb);
                let value = self.checked_if(known != Some(false), |this| this.block(then));
                self.store_result(result, value);
                let then_end = self.current.take();

                self.current = Some(else_bb);
                if let Some(else_) = else_ {
                    let value = self.checked_if(known != Some(true), |this| this.expr(else_));
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
                let cond_expr = cond;
                let cond = self.expr(cond_expr);
                let known = self.known_condition(cond_expr);
                let body_bb = self.new_block();
                let exit = self.new_block();
                self.terminate(Terminator::If {
                    cond,
                    then: body_bb,
                    else_: exit,
                });
                self.current = Some(body_bb);
                self.checked_if(known != Some(false), |this| {
                    this.in_loop(expr.id, head, Some(exit), None, |this| {
                        this.block(body);
                    })
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
            hir::ExprKind::Match {
                scrutinee,
                arms,
                keyword,
            } => self.match_(ty, scrutinee, arms, *keyword),
            hir::ExprKind::Try(operand, question) => self.try_(ty, operand, *question),
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

    /// `match` (ADR 0037, 0040), of type `ty`: the scrutinee is read once,
    /// before any arm. On an integer, a `bool` or an enum without
    /// payloads, a switch on it jumps to the first arm whose pattern
    /// matches. The catch-all arm is the default. Without one, every value
    /// of an integer or `bool` is listed, so the default cannot be reached,
    /// while an enum may hold a value that is no variant, for which the
    /// default calls the panic function (ADR 0036). With a known scrutinee,
    /// only the arm it takes is checked (ADR 0032). On an enum with
    /// payloads, a switch on the tag starts, for each variant the arms
    /// name, the chain of the alternatives naming it, tested in arm order
    /// (ADR 0042). Either way, each arm's expression is built once. The
    /// call to the panic function is located at `keyword`, the `match`
    /// (ADR 0053).
    fn match_(
        &mut self,
        ty: Ty,
        scrutinee: &hir::Expr,
        arms: &[hir::Arm],
        keyword: Span,
    ) -> Operand {
        let scrutinee_ty = self.ty_of(scrutinee);
        let hir_ty = self.q.tables.expr_ty(scrutinee.id);
        let value = self.expr(scrutinee);
        if self.current.is_none() {
            // The scrutinee did not finish, and nothing follows.
            return Operand::Const(Const::Unit);
        }
        // On an enum the `match` needs the values of, one of which failed
        // or repeats, which was reported, exhaustiveness was not checked,
        // so a variant may lack an arm at some position, and the `match`
        // would panic on it as if it were no variant. The body is not used,
        // so nothing more is reported (ADR 0040). Type checking asked about
        // the same enums.
        let needed = self.q.enums_needed(hir_ty, arms);
        if needed.into_iter().any(|id| !self.q.enum_ok(id)) {
            self.failed = true;
        }
        // Read where it is written (ADR 0018), into a temporary that the
        // tests and the variables the arms bind share, unless it is one.
        let discr = match value {
            Operand::Copy(place)
                if !place.projection.is_empty()
                    || self.locals[place.local.0 as usize].name.is_some() =>
            {
                self.temp(scrutinee_ty, Rvalue::Use(value))
            }
            _ => value,
        };
        let blocks: Vec<BasicBlock> = arms.iter().map(|_| self.new_block()).collect();
        let chains = matches!(hir_ty, hir::Ty::Enum(e) if e.payloads);
        let taken = if chains {
            let Operand::Copy(place) = discr else {
                unreachable!("an enum with payloads is an aggregate, read from a place")
            };
            self.chains(place, hir_ty, arms, &blocks, keyword);
            // No value of an enum with payloads is known (ADR 0039).
            None
        } else {
            self.switch(scrutinee, scrutinee_ty, discr, arms, &blocks, keyword)
        };

        let result = self.result_place(ty);
        let mut ends = Vec::with_capacity(arms.len());
        for (i, (arm, &block)) in arms.iter().zip(&blocks).enumerate() {
            self.current = Some(block);
            // After a switch, a `let` arm copies the scrutinee; no
            // alternative of an or-pattern binds a variable there, since a
            // `let` alternative is dead or binds a name the others do not.
            // The chains copied what their alternatives bind on their way
            // (ADR 0042).
            if !chains && let hir::PatternKind::Binding { local, .. } = arm.pattern.kind {
                let local = self.hir_locals[local.0 as usize];
                self.assign(Place::local(local), Rvalue::Use(discr));
            }
            let can_run = taken.is_none_or(|taken| taken == Some(i));
            let value = self.checked_if(can_run, |this| this.expr(&arm.body));
            self.store_result(result, value);
            ends.push(self.current.take());
        }
        self.join(&ends);
        self.read_result(result)
    }

    /// `operand?` (ADR 0046), of type `ty`: the operand is read once, into
    /// a temporary unless it is one, and a switch on its tag goes, for
    /// `Some` or `Ok`, to a block that copies the payload into the result;
    /// for `None` or `Err`, to one that builds `.None` or `.Err(x)` in the
    /// return place and returns; and for a tag that is no variant, to the
    /// panic function, as a `match` without a catch-all arm does
    /// (ADR 0036, 0040), located at `question`, the `?` (ADR 0053).
    fn try_(&mut self, ty: Ty, operand: &hir::Expr, question: Span) -> Operand {
        let operand_ty = self.ty_of(operand);
        let hir::Ty::Enum(e) = self.q.tables.expr_ty(operand.id) else {
            // `!`: nothing follows the operand.
            self.expr(operand);
            return Operand::Const(Const::Unit);
        };
        let value = self.expr(operand);
        if self.current.is_none() {
            return Operand::Const(Const::Unit);
        }
        let place = match value {
            Operand::Copy(place)
                if place.projection.is_empty()
                    && self.locals[place.local.0 as usize].name.is_none() =>
            {
                place
            }
            _ => {
                let temp = Place::local(self.new_local(operand_ty, None));
                self.assign(temp, Rvalue::Use(value));
                temp
            }
        };
        let builtin = self
            .q
            .builtin_enum(e.id)
            .expect("`?` takes an `Option` or a `Result`");
        let (success, failure) = builtin.try_variants();
        let success_bb = self.new_block();
        let failure_bb = self.new_block();
        let panic_bb = self.panic_block(question);
        // Known without evaluation (ADR 0045).
        let tag = |this: &mut Self, id: hir::EnumId, index: u32| {
            this.discriminant(id, index)
                .expect("the values of `Option` and `Result` are known")
        };
        let cases = vec![
            (tag(self, e.id, success), success_bb),
            (tag(self, e.id, failure), failure_bb),
        ];
        self.terminate(Terminator::Switch {
            discr: Operand::Copy(place.project(PlaceElem::Tag)),
            cases: cases.leak(),
            otherwise: panic_bb,
        });

        // The function returns the same kind of enum, with the operand's
        // error type for a `Result` (ADR 0046).
        self.current = Some(failure_bb);
        let hir = self.q.hir;
        let ret_ty = hir.fn_(self.fn_id).ret;
        let hir::Ty::Enum(r) = self.q.normalize(ret_ty) else {
            unreachable!("`?` is in a function returning an `Option` or a `Result`")
        };
        let ret = Place::local(Local::RETURN);
        let Ty::Enum(ret_enum) = self.locals[Local::RETURN.0 as usize].ty else {
            unreachable!("an `Option` or a `Result` is an enum with payloads")
        };
        if builtin == hir::BuiltinEnum::Result {
            let error = place.project(PlaceElem::Payload {
                variant: failure,
                index: 0,
            });
            let returned = ret.project(PlaceElem::Payload {
                variant: failure,
                index: 0,
            });
            self.assign(returned, Rvalue::Use(Operand::Copy(error)));
        }
        let ret_tag = tag(self, r.id, failure);
        self.assign(
            ret.project(PlaceElem::Tag),
            Rvalue::Use(Operand::Const(Const::Int(ret_tag, ret_enum.def().int))),
        );
        self.terminate(Terminator::Return);

        self.current = Some(success_bb);
        let payload = place.project(PlaceElem::Payload {
            variant: success,
            index: 0,
        });
        if matches!(ty, Ty::Unit) {
            Operand::Const(Const::Unit)
        } else {
            self.temp(ty, Rvalue::Use(Operand::Copy(payload)))
        }
    }

    /// The switch of a `match` on `discr`, an integer, `bool` or enum
    /// without payloads, of type `ty`, to the `blocks` of its arms; returns
    /// the arm a known scrutinee takes, as `known_arm` does. A call to the
    /// panic function is located at `at` (ADR 0053).
    fn switch(
        &mut self,
        scrutinee: &hir::Expr,
        ty: Ty,
        discr: Operand,
        arms: &[hir::Arm],
        blocks: &[BasicBlock],
        at: Span,
    ) -> Option<Option<usize>> {
        let is_enum = matches!(self.q.tables.expr_ty(scrutinee.id), hir::Ty::Enum(_));
        // For each arm, the values its alternatives match, `None` for every
        // value. Type checking rejects an arm after a catch-all, and a value
        // listed twice, except on an enum two of whose variants have one
        // value, which was reported (ADR 0035, 0037). The arms keep their
        // order all the same: a value goes to the first arm that lists it,
        // and nothing after a catch-all is a case, so that the switch never
        // holds a value twice.
        let mut values = Vec::with_capacity(arms.len());
        let mut cases = Vec::new();
        let mut otherwise = None;
        for (arm, &block) in arms.iter().zip(blocks) {
            let arm_values: Vec<Option<u64>> = arm
                .pattern
                .alternatives()
                .iter()
                .map(|alternative| self.pattern_value(alternative, ty))
                .collect();
            if arm_values.contains(&None) {
                otherwise = otherwise.or(Some(block));
            } else if otherwise.is_none() {
                for &value in arm_values.iter().flatten() {
                    if !cases.iter().any(|&(case, _)| case == value) {
                        cases.push((value, block));
                    }
                }
            }
            values.push(arm_values);
        }
        let taken = self.known_arm(scrutinee, &values);
        let unlisted = otherwise.is_none().then(|| self.new_block());
        self.terminate(Terminator::Switch {
            discr,
            cases: cases.leak(),
            otherwise: otherwise.or(unlisted).expect("a default block exists"),
        });
        if let Some(block) = unlisted {
            self.current = Some(block);
            // Never assumed to hold a variant (ADR 0036).
            let end = if is_enum {
                self.panic(PanicCause::NoVariant, at)
            } else {
                Terminator::Unreachable
            };
            self.terminate(end);
        }
        taken
    }

    /// The tests of a `match` on an enum with payloads, held in
    /// `scrutinee`, of type `ty`, from the current block to the `blocks`
    /// of its arms (ADR 0042). A switch on the tag has a case for each
    /// variant that an alternative at the top names, in the order first
    /// named, leading to the variant's chain: each alternative naming it,
    /// in arm order, tested in turn, a failed test going to the next, and
    /// then the first catch-all arm, or else the call to the panic
    /// function. The default goes to the catch-all arm, or calls the panic
    /// function, for the variants no arm names and the tags that are no
    /// variant. Without a catch-all arm, the default and every chain share
    /// one block calling the panic function. A `let` catch-all arm is
    /// entered at one block, which copies the scrutinee into its variable.
    /// The call to the panic function is located at `at` (ADR 0053).
    fn chains(
        &mut self,
        scrutinee: Place,
        ty: hir::Ty,
        arms: &[hir::Arm],
        blocks: &[BasicBlock],
        at: Span,
    ) {
        let hir::Ty::Enum(e) = ty else {
            unreachable!("chains test enums with payloads")
        };
        let head = self.current;
        // The alternatives naming each variant, with their arms, up to the
        // first `_` or `let` alternative, which makes its arm the
        // catch-all one: an arm after it is dead, which is an error
        // (ADR 0037).
        let mut chains: Vec<(u32, Vec<(usize, &hir::Pattern)>)> = Vec::new();
        let mut catch_all = None;
        'arms: for (arm, a) in arms.iter().enumerate() {
            for alternative in a.pattern.alternatives() {
                let Some((_, variant, _)) = self.variant_pattern(alternative) else {
                    catch_all = Some((arm, alternative));
                    break 'arms;
                };
                match chains.iter_mut().find(|(named, _)| *named == variant) {
                    Some((_, chain)) => chain.push((arm, alternative)),
                    None => chains.push((variant, vec![(arm, alternative)])),
                }
            }
        }
        let entry = catch_all.map(|(arm, alternative)| match alternative.kind {
            hir::PatternKind::Binding { local, .. } => {
                let block = self.new_block();
                self.current = Some(block);
                let local = self.hir_locals[local.0 as usize];
                self.assign(Place::local(local), Rvalue::Use(Operand::Copy(scrutinee)));
                self.terminate(Terminator::Goto(blocks[arm]));
                block
            }
            _ => blocks[arm],
        });
        // Without a catch-all arm, the default and the end of every chain
        // are one block calling the panic function.
        let end = entry.unwrap_or_else(|| self.panic_block(at));
        let mut cases = Vec::with_capacity(chains.len());
        for (_, chain) in chains {
            let value = self
                .pattern_value(chain[0].1, Ty::Int(e.int))
                .expect("a variant names a value");
            let start = self.chain(scrutinee, &chain, blocks, end);
            // Two variants with one value were reported; the body is not
            // used, and the switch holds each value once all the same.
            if !cases.iter().any(|&(case, _)| case == value) {
                cases.push((value, start));
            }
        }
        self.current = head;
        self.terminate(Terminator::Switch {
            discr: Operand::Copy(scrutinee.project(PlaceElem::Tag)),
            cases: cases.leak(),
            otherwise: end,
        });
    }

    /// The chain of a variant: its alternatives at the top, with their
    /// arms, each tested in turn, one that fails going to the next, the
    /// last to `end`. Returns the block where it starts. An alternative
    /// that tests and binds nothing, such as `Msg::Nop` or
    /// `Msg::Wide(_)`, is its arm's block itself.
    fn chain(
        &mut self,
        scrutinee: Place,
        chain: &[(usize, &hir::Pattern)],
        blocks: &[BasicBlock],
        end: BasicBlock,
    ) -> BasicBlock {
        // The block where an alternative starts, and whether it tests or
        // binds anything there: one that does neither starts at its arm's
        // block.
        let start = |this: &mut Self, (arm, alternative): (usize, &hir::Pattern)| {
            let Some((_, _, patterns)) = this.variant_pattern(alternative) else {
                unreachable!("a chain holds variants")
            };
            let wildcards = patterns
                .unwrap_or_default()
                .iter()
                .all(|pattern| matches!(pattern.kind, hir::PatternKind::Wildcard));
            if wildcards {
                (blocks[arm], false)
            } else {
                (this.new_block(), true)
            }
        };
        let (mut block, mut tests) = start(self, chain[0]);
        let first = block;
        for (k, &(arm, alternative)) in chain.iter().enumerate() {
            let next = chain.get(k + 1).map(|&next| start(self, next));
            if tests {
                let Some((id, variant, Some(patterns))) = self.variant_pattern(alternative) else {
                    unreachable!("an alternative that tests something has a payload")
                };
                self.current = Some(block);
                let fail = next.map_or(end, |(next, _)| next);
                // The tag was switched on already.
                self.payload(scrutinee, id, variant, patterns, &[], fail);
                self.terminate(Terminator::Goto(blocks[arm]));
            }
            if let Some(next) = next {
                (block, tests) = next;
            }
        }
        first
    }

    /// Tests the payload values of `variant` of enum `id`, held at
    /// `position` of `scrutinee`, against `patterns`, in order, from the
    /// current block, which continues where they all passed.
    fn payload(
        &mut self,
        scrutinee: Place,
        id: hir::EnumId,
        variant: u32,
        patterns: &[hir::Pattern],
        position: &[hir::PayloadStep],
        fail: BasicBlock,
    ) {
        let values = &self.q.enum_def(id).variants[variant as usize].payload;
        for (index, (pattern, value)) in patterns.iter().zip(values).enumerate() {
            let mut at = position.to_vec();
            at.push(hir::PayloadStep {
                variant,
                index: index as u32,
            });
            // An `Option(T)` there is tested as the enum it is (ADR 0045).
            let ty = self.q.normalize(value.ty);
            self.test(scrutinee, pattern, &at, ty, fail);
        }
    }

    /// Tests `pattern` at `position` of `scrutinee`, whose type is `ty`,
    /// from the current block, which continues where it passed; where it
    /// fails, it goes to `fail` (ADR 0042). `_` tests nothing; a `let`
    /// copies its position where it is reached, before the tests after it.
    fn test(
        &mut self,
        scrutinee: Place,
        pattern: &hir::Pattern,
        position: &[hir::PayloadStep],
        ty: hir::Ty,
        fail: BasicBlock,
    ) {
        match &pattern.kind {
            hir::PatternKind::Wildcard => {}
            hir::PatternKind::Binding { local, .. } => {
                let local = self.hir_locals[local.0 as usize];
                let value = Operand::Copy(position_place(scrutinee, position));
                self.assign(Place::local(local), Rvalue::Use(value));
            }
            hir::PatternKind::Or(alternatives) => {
                self.or_pattern(scrutinee, alternatives, position, ty, fail)
            }
            _ => {
                // A value, or a variant's tag, then its payload.
                let pass = self.new_block();
                let one = std::slice::from_ref(pattern);
                self.switch_values(scrutinee, one, position, ty, pass, fail);
                self.current = Some(pass);
                if let Some((id, variant, Some(patterns))) = self.variant_pattern(pattern) {
                    self.payload(scrutinee, id, variant, patterns, position, fail);
                }
            }
        }
    }

    /// An or-pattern at `position`: each alternative is tested in turn,
    /// one that fails going to the next, the last to `fail`, and every one
    /// that passes to one block, which continues, so that the positions
    /// after the or-pattern are tested once. Consecutive alternatives that
    /// each name one value are one switch.
    fn or_pattern(
        &mut self,
        scrutinee: Place,
        alternatives: &[hir::Pattern],
        position: &[hir::PayloadStep],
        ty: hir::Ty,
        fail: BasicBlock,
    ) {
        let join = self.new_block();
        let mut rest = alternatives;
        while let Some(first) = rest.first() {
            let count = if is_value(first) {
                rest.iter()
                    .take_while(|alternative| is_value(alternative))
                    .count()
            } else {
                1
            };
            let (group, after) = rest.split_at(count);
            let next = if after.is_empty() {
                fail
            } else {
                self.new_block()
            };
            if is_value(first) {
                self.switch_values(scrutinee, group, position, ty, join, next);
            } else {
                self.test(scrutinee, first, position, ty, next);
                self.terminate(Terminator::Goto(join));
            }
            if !after.is_empty() {
                self.current = Some(next);
            }
            rest = after;
        }
        self.current = Some(join);
    }

    /// A switch on `position` of `scrutinee`, whose type is `ty`, ending
    /// the current block: the values `patterns` name go to `to`, every
    /// other value to `otherwise`.
    fn switch_values(
        &mut self,
        scrutinee: Place,
        patterns: &[hir::Pattern],
        position: &[hir::PayloadStep],
        ty: hir::Ty,
        to: BasicBlock,
        otherwise: BasicBlock,
    ) {
        let (place, value_ty) = test_place(scrutinee, position, ty);
        let mut cases = Vec::with_capacity(patterns.len());
        for pattern in patterns {
            let value = self
                .pattern_value(pattern, value_ty)
                .expect("a test names a value");
            // A value named twice was reported as dead; the switch holds
            // it once all the same.
            if !cases.iter().any(|&(case, _)| case == value) {
                cases.push((value, to));
            }
        }
        self.terminate(Terminator::Switch {
            discr: Operand::Copy(place),
            cases: cases.leak(),
            otherwise,
        });
    }

    /// A new block that calls the panic function for a value that is no
    /// variant (ADR 0036), located at `at` (ADR 0053), leaving the current
    /// block as it is.
    fn panic_block(&mut self, at: Span) -> BasicBlock {
        let block = self.new_block();
        let panic = self.panic(PanicCause::NoVariant, at);
        self.blocks[block.0 as usize].terminator = Some((panic, self.span));
        block
    }

    /// The call to the panic function for `cause`, from the check located
    /// at the first byte of `at`: with its information when the panic
    /// function takes it (ADR 0053).
    fn panic(&mut self, cause: PanicCause, at: Span) -> Terminator {
        let site = self.located.then(|| self.q.panic_site(cause, at));
        Terminator::Panic(cause, site)
    }

    /// The call to the panic function for an `assert` located at `at`,
    /// whose `message`, from the expression at `message_span`, the current
    /// block has computed. When the panic function takes the information,
    /// the current block also fills a new local `core::PanicInfo` with the
    /// fields known at compile time and the message, in declaration order,
    /// and the call passes its address (ADR 0053).
    fn assert_panic(&mut self, message: Operand, message_span: Span, at: Span) -> Terminator {
        let cause = PanicCause::AssertionFailed(Some((message, message_span)));
        if !self.located {
            return Terminator::Panic(cause, None);
        }
        let hir = self.q.hir;
        let s = self.q.mir_struct(hir.panic_info);
        let info = self.new_local(Ty::Struct(s), None);
        let fields = self.q.panic_fields(cause, at);
        let int = |value: u64, ty: Ty| match ty {
            Ty::Int(int) => Rvalue::Use(Operand::Const(Const::Int(value, int))),
            ty => unreachable!("`core::PanicInfo` has an integer here, not `{ty}`"),
        };
        for (index, field) in s.def().fields().iter().enumerate() {
            let value = match field.name.as_str() {
                // A slice of the whole name (ADR 0031).
                "file" => Rvalue::Slice {
                    ptr: Operand::Const(Const::Static(fields.file)),
                    len: usize_const(fields.name_len() as u64),
                },
                "line" => int(u64::from(fields.line), field.ty),
                "column" => int(u64::from(fields.column), field.ty),
                // Its type is the one read-only data takes, `reason_ty`.
                "reason" => {
                    Rvalue::Use(Operand::Const(Const::Int(fields.reason, fields.reason_ty)))
                }
                "message" => Rvalue::Use(message),
                name => unreachable!("`core::PanicInfo` has no field `{name}`"),
            };
            let place = Place::local(info).project(PlaceElem::Field(index as u32));
            self.assign(place, value);
        }
        let site = PanicSite {
            info: PanicInfoAt::Local(info),
            file: fields.file,
            line: fields.line,
            column: fields.column,
        };
        Terminator::Panic(cause, Some(site))
    }

    /// `Enum::Variant` or `Enum::Variant(args)`, of type `ty`. Without
    /// payloads, its integer, which is all MIR sees of the enum (ADR 0035).
    /// With payloads, a new value: the arguments are evaluated left to
    /// right, then each payload value and the tag are written; the other
    /// bytes are left as they are (ADR 0039).
    fn variant(
        &mut self,
        ty: Ty,
        id: hir::EnumId,
        index: u32,
        args: &Option<Vec<hir::Expr>>,
    ) -> Operand {
        let int = match ty {
            Ty::Int(int) => int,
            Ty::Enum(e) => e.def().int,
            _ => unreachable!("an enum is its integer type, or an enum with payloads"),
        };
        let tag = match self.discriminant(id, index) {
            Some(value) => Operand::Const(Const::Int(value, int)),
            None => {
                // Reported where it failed; the body is not used.
                self.failed = true;
                Operand::Const(Const::Int(0, int))
            }
        };
        let Ty::Enum(_) = ty else {
            return tag;
        };
        let values = match args {
            Some(args) => self.operands(args),
            None => Vec::new(),
        };
        let value = Place::local(self.new_local(ty, None));
        for (i, payload) in values.into_iter().enumerate() {
            let place = value.project(PlaceElem::Payload {
                variant: index,
                index: i as u32,
            });
            self.assign(place, Rvalue::Use(payload));
        }
        self.assign(value.project(PlaceElem::Tag), Rvalue::Use(tag));
        Operand::Copy(value)
    }

    /// The enum and the variant a variant pattern names, by its path or,
    /// for `.V`, as type checking resolved it (ADR 0044), and its payload
    /// patterns if written with parentheses; `None` for any other pattern.
    fn variant_pattern<'p>(
        &self,
        pattern: &'p hir::Pattern,
    ) -> Option<(hir::EnumId, u32, Option<&'p [hir::Pattern]>)> {
        match &pattern.kind {
            hir::PatternKind::Variant(id, index, patterns) => {
                Some((*id, *index, patterns.as_deref()))
            }
            hir::PatternKind::ContextVariant(id, _, _, patterns) => {
                let (enum_id, index) = self.q.tables.pattern_variant(*id);
                Some((enum_id, index, patterns.as_deref()))
            }
            _ => None,
        }
    }

    /// The bits a pattern matches, masked to the width of `ty`, the type
    /// matched (`bool` is 0 or 1); `None` for every value.
    fn pattern_value(&mut self, pattern: &hir::Pattern, ty: Ty) -> Option<u64> {
        let int = match ty {
            Ty::Int(int) => Some(int),
            _ => None,
        };
        let mask = int.map_or(1, |int| int.mask());
        Some(match &pattern.kind {
            hir::PatternKind::Wildcard | hir::PatternKind::Binding { .. } => return None,
            hir::PatternKind::Int { value, negated } => {
                let bits = if *negated {
                    value.wrapping_neg()
                } else {
                    *value
                };
                bits & mask
            }
            hir::PatternKind::Byte(value) => u64::from(*value),
            hir::PatternKind::Bool(value) => u64::from(*value),
            hir::PatternKind::Const(id) => match (self.const_value(*id), int) {
                (Some(value), Some(int)) => value.int(int.bytes() as usize),
                (Some(value), None) => u64::from(value.bytes[0] != 0),
                (None, _) => {
                    // Reported where it failed; the body is not used.
                    self.failed = true;
                    0
                }
            },
            // Its value, masked already as `Const::Int` holds it.
            hir::PatternKind::Variant(..) | hir::PatternKind::ContextVariant(..) => {
                let (id, index, _) = self
                    .variant_pattern(pattern)
                    .expect("a variant pattern names a variant");
                match self.discriminant(id, index) {
                    Some(value) => value,
                    None => {
                        // Reported where it failed; the body is not used.
                        self.failed = true;
                        0
                    }
                }
            }
            hir::PatternKind::Or(_) => unreachable!("an alternative is not an or-pattern"),
        })
    }

    /// Division and remainder with the checks of ADR 0007, so that the
    /// `Div`/`Rem` left in MIR always have a valid divisor:
    ///
    /// - a zero divisor calls the panic function;
    /// - for signed types, a divisor of -1 gives `-lhs` (so `MIN / -1` wraps
    ///   to `MIN`) and a remainder of 0, never reaching the division.
    ///
    /// Checks that a constant divisor makes pointless are left out. The
    /// check is located at `at`, the operator (ADR 0053).
    fn divide(&mut self, op: BinOp, lhs: Operand, rhs: Operand, ty: Ty, at: Span) -> Operand {
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

        // A known zero divisor is rejected where it can run (ADR 0032); in
        // code that cannot, this keeps the `Div`/`Rem` left in MIR valid.
        if constant == Some(0) {
            let panic = self.panic(PanicCause::DivisionByZero, at);
            self.terminate(panic);
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
            let panic = self.panic(PanicCause::DivisionByZero, at);
            self.terminate(panic);
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

    /// `operand as ty` (ADR 0019, 0031): a slice converts to the address of
    /// its first element, or to a slice of other mutability or volatility;
    /// a pointer to an array to a slice of its elements.
    fn cast(&mut self, operand: Operand, ty: Ty) -> Operand {
        // The operand did not finish, and nothing follows.
        if self.current.is_none() {
            return Operand::Const(Const::Unit);
        }
        let from = self.operand_ty(operand);
        match (operand, from.slice_elem(), ty.slice_elem()) {
            (Operand::Copy(place), Some(_), None) => self.temp(ty, Rvalue::SliceAddress(place)),
            (_, Some(_), Some(_)) => self.temp(ty, Rvalue::Use(operand)),
            (_, None, Some(_)) => self.array_slice(operand, ty),
            _ => self.temp(ty, Rvalue::Cast(operand, ty)),
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
        let lhs_expr = lhs;
        let lhs = self.expr(lhs_expr);
        // The right side never runs when the left one decides the result.
        let short = op == hir::LogicalOp::Or;
        let decided = self.known_condition(lhs_expr) == Some(short);
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
        let value = self.checked_if(!decided, |this| this.expr(rhs));
        self.assign(result, Rvalue::Use(value));
        let rhs_end = self.current.take();

        self.current = Some(short_bb);
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

    /// Continues only if `ok` is true; otherwise calls the panic function,
    /// from the check located at `at` (ADR 0053).
    fn panic_unless(&mut self, ok: Operand, cause: PanicCause, at: Span) {
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
        let panic = self.panic(cause, at);
        self.terminate(panic);
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

    /// The value of a constant, asked once per body.
    fn const_value(&mut self, id: hir::ConstId) -> Option<ConstValue> {
        if let Some(value) = self.const_values.get(&id) {
            return value.clone();
        }
        let value = self.q.const_value(id);
        self.const_values.insert(id, value.clone());
        value
    }

    /// The value of `comptime inner`, asked once per body.
    fn comptime_value(&mut self, inner: &hir::Expr) -> Option<ConstValue> {
        if let Some(value) = self.comptime_values.get(&inner.id) {
            return value.clone();
        }
        let value = self.q.comptime_value(self.fn_id, inner);
        self.comptime_values.insert(inner.id, value.clone());
        value
    }

    /// The value of a variant, asked once per body.
    fn discriminant(&mut self, id: hir::EnumId, index: u32) -> Option<u64> {
        if let Some(&value) = self.discriminants.get(&(id, index)) {
            return value;
        }
        let value = self.q.discriminant(id, index);
        self.discriminants.insert((id, index), value);
        value
    }

    /// `size_of`, `align_of` or `offset_of`, which are constants
    /// (ADR 0022).
    fn layout_query(&mut self, expr: &hir::Expr) -> u64 {
        match &expr.kind {
            hir::ExprKind::SizeOf(ty) => self.layout(*ty).size,
            hir::ExprKind::AlignOf(ty) => self.layout(*ty).align,
            hir::ExprKind::OffsetOf(ty, _, _) => {
                let hir::Ty::Struct(s) = ty else {
                    unreachable!("the type checker allows only structs")
                };
                let layout = self
                    .q
                    .struct_layout(s.id)
                    .expect("well-typed programs have layouts");
                layout.offsets[self.q.tables.field(expr.id) as usize]
            }
            _ => unreachable!("not a layout query"),
        }
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
            Operand::Const(Const::Static(s)) => s.def().address_ty(),
            Operand::Const(Const::Peripheral(p)) => p.def().address_ty(),
        }
    }

    fn place_ty(&self, place: Place) -> Ty {
        place
            .projection
            .iter()
            .fold(self.locals[place.local.0 as usize].ty, projected_ty)
    }

    /// Whether accesses to `place` are volatile, as code generation makes
    /// them in `address` (`src/codegen/mod.rs`), which must agree: when
    /// the last pointer on its path is volatile (ADR 0023), or
    /// when it is a register (ADR 0058).
    fn is_volatile(&self, place: Place) -> bool {
        let mut ty = self.locals[place.local.0 as usize].ty;
        let mut volatile = false;
        for elem in place.projection {
            match (elem, ty) {
                (PlaceElem::Deref | PlaceElem::Offset(_), Ty::Ptr(ptr)) => volatile = ptr.volatile,
                (
                    PlaceElem::Field(_) | PlaceElem::Element(..) | PlaceElem::ConstElement(..),
                    Ty::Peripheral(_),
                ) => volatile = true,
                _ => {}
            }
            ty = projected_ty(ty, elem);
        }
        volatile
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

    /// Lowers a place expression: a local, a static, a byte string, a
    /// peripheral, `*pointer`, `base[index]`, `base.field` or
    /// `base.register`. Any other expression is
    /// evaluated into a temporary, whose place is returned.
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
            // Reached through its address, so that every access happens
            // where it is written (ADR 0030).
            hir::ExprKind::Static(id) => {
                let s = self.q.mir_static(*id);
                self.static_place(s)
            }
            // Like an unnamed `static`, one per occurrence (ADR 0031).
            hir::ExprKind::ByteString(bytes) => {
                let s = self.q.mir_byte_string(expr.id, bytes);
                self.static_place(s)
            }
            // Reached through its address, as a static is, so that its
            // registers are reached as through any pointer to its type
            // (ADR 0058).
            hir::ExprKind::Peripheral(id) => {
                let p = self.q.mir_peripheral(*id);
                let address = Rvalue::Use(Operand::Const(Const::Peripheral(p)));
                match self.temp(p.def().address_ty(), address) {
                    Operand::Copy(place) => place.project(PlaceElem::Deref),
                    Operand::Const(_) => unreachable!("temporaries are places"),
                }
            }
            hir::ExprKind::Deref(pointer) => self
                .pointer_place(pointer, later_writes)
                .project(PlaceElem::Deref),
            // An element of a register array or a cluster array: reached
            // from the peripheral type that holds the entry, by the entry's
            // step, which only the layout knows (ADR 0064). The index runs
            // after the place it indexes and before the access (ADR 0018).
            hir::ExprKind::Index(array, index, open)
                if self.q.tables.is_entry_access(expr.id)
                    && let hir::ExprKind::Field(owner, _, _) = &array.kind
                    && let Ty::Peripheral(p) = self.ty_of(owner) =>
            {
                let entry = self.q.tables.field(array.id);
                let later_writes = later_writes || !is_simple(index);
                let owner = self.place_then(owner, later_writes, address);
                let len = p.def().length(entry);
                let elem = match self.array_index(index, len, *open) {
                    PlaceElem::Index(local) => PlaceElem::Element(entry, local),
                    PlaceElem::ConstIndex(value) => PlaceElem::ConstElement(entry, value),
                    // `array_index` returns only the two above; this arm keeps
                    // the match total without a panic.
                    other => other,
                };
                owner.project(elem)
            }
            hir::ExprKind::Index(base, index, open) => {
                let later_writes = later_writes || !is_simple(index);
                match self.ty_of(base) {
                    Ty::Array(_, len) => {
                        let base = self.place_then(base, later_writes, address);
                        let elem = self.array_index(index, len, *open);
                        base.project(elem)
                    }
                    Ty::Slice(_) => {
                        let hir::ExprKind::Deref(pointer) = &base.kind else {
                            unreachable!("only what a slice points to has type `[T]`")
                        };
                        self.slice_index(pointer, index, later_writes, *open)
                    }
                    _ => {
                        let base = self.pointer_place(base, later_writes);
                        let index = self.fixed_index(index);
                        base.project(PlaceElem::Offset(index))
                    }
                }
            }
            // A bit field is a value, read below (ADR 0059).
            hir::ExprKind::Field(base, _, _) if self.q.tables.bit_field(expr.id).is_none() => {
                let base_place = self.place_then(base, later_writes, address);
                // A base of type `!` is never computed, and has no fields.
                if self.ty_of(base) == Ty::Never {
                    return base_place;
                }
                // A field, or a register of a peripheral type (ADR 0058).
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

    /// The place of a static, reached through its address.
    fn static_place(&mut self, s: StaticRef) -> Place {
        let address = Rvalue::Use(Operand::Const(Const::Static(s)));
        match self.temp(s.def().address_ty(), address) {
            Operand::Copy(place) => place.project(PlaceElem::Deref),
            Operand::Const(_) => unreachable!("temporaries are places"),
        }
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
    /// after checking the bounds (ADR 0021): an index known at compile time
    /// while building, any other at run time (ADR 0032), located at `at`,
    /// the `[` (ADR 0053).
    fn array_index(&mut self, index: &hir::Expr, len: u64, at: Span) -> PlaceElem {
        self.check_index(index, len);
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
        self.panic_unless(in_bounds, PanicCause::IndexOutOfBounds, at);
        PlaceElem::Index(local)
    }

    /// `(*slice)[index]` (ADR 0031): the index is checked against the
    /// length at run time, which is never known while building, and the
    /// element is then reached through the address of the first one, a
    /// thin pointer of the slice's volatility. The check is located at
    /// `at`, the `[` (ADR 0053).
    fn slice_index(
        &mut self,
        slice: &hir::Expr,
        index: &hir::Expr,
        later_writes: bool,
        at: Span,
    ) -> Place {
        let slice = self.pointer_place(slice, later_writes);
        let index = self.fixed_index(index);
        let slice_ty = self.place_ty(slice);
        let len = self.temp(Ty::Int(hir::IntTy::Usize), Rvalue::SliceLen(slice));
        let in_bounds = self.temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Lt, Operand::Copy(Place::local(index)), len),
        );
        self.panic_unless(in_bounds, PanicCause::IndexOutOfBounds, at);
        let address_ty = slice_ty
            .slice_address_ty()
            .expect("only slices point to a `[T]`");
        match self.temp(address_ty, Rvalue::SliceAddress(slice)) {
            // `index < len`, so the element is inside the slice.
            Operand::Copy(address) => address.project(PlaceElem::Offset(index)),
            Operand::Const(_) => unreachable!("temporaries are places"),
        }
    }
}

fn usize_const(value: u64) -> Operand {
    Operand::Const(Const::Int(value, hir::IntTy::Usize))
}

/// The type of the place one projection `elem` reaches from a place of
/// type `ty`.
fn projected_ty(ty: Ty, elem: &PlaceElem) -> Ty {
    match (elem, ty) {
        (PlaceElem::Deref | PlaceElem::Offset(_), Ty::Ptr(ptr)) => *ptr.pointee,
        (PlaceElem::Index(_) | PlaceElem::ConstIndex(_), Ty::Array(elem, _)) => *elem,
        (PlaceElem::Field(index), Ty::Struct(s)) => s.def().fields()[*index as usize].ty,
        (
            PlaceElem::Field(index)
            | PlaceElem::Element(index, _)
            | PlaceElem::ConstElement(index, _),
            Ty::Peripheral(p),
        ) => p.def().registers[*index as usize].ty,
        (PlaceElem::Tag, Ty::Enum(e)) => Ty::Int(e.def().int),
        (PlaceElem::Payload { variant, index }, Ty::Enum(e)) => {
            e.def().payload(*variant, *index).ty
        }
        (elem, ty) => unreachable!("projection {elem:?} of `{ty}`"),
    }
}

/// Expressions whose evaluation cannot write to any place.
fn is_simple(expr: &hir::Expr) -> bool {
    match &expr.kind {
        hir::ExprKind::Int(_)
        | hir::ExprKind::Byte(_)
        | hir::ExprKind::Bool(_)
        | hir::ExprKind::Unit
        | hir::ExprKind::Variant(_, _, None)
        | hir::ExprKind::ContextVariant(_, _, None)
        | hir::ExprKind::Local(_) => true,
        hir::ExprKind::Variant(_, _, Some(args))
        | hir::ExprKind::ContextVariant(_, _, Some(args)) => args.iter().all(is_simple),
        hir::ExprKind::Unary(UnOp::Neg, operand) => matches!(operand.kind, hir::ExprKind::Int(_)),
        _ => false,
    }
}
