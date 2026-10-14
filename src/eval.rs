//! The compile-time evaluator (ADR 0027, 0029): runs MIR the way the
//! target would, and is the "const evaluation" layer of ADR 0003.
//!
//! Memory is bytes laid out as on the target (ADR 0022), each byte known to
//! be written or not. A pointer is an object and an offset, never a host
//! address; pointers stored in memory are recorded beside the bytes, since
//! their addresses only exist after linking. A pointer made from an integer
//! is just that integer: it can be computed with, but not read through.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::diag::Diagnostic;
use crate::int::IntTy;
use crate::mir::{
    BasicBlock, BinOp, Body, Const, Local, Operand, PanicCause, Place, PlaceElem, Rvalue,
    Statement, Terminator, Ty, UnOp,
};
use crate::query::Queries;
use crate::span::Span;
use crate::target;

/// The most MIR statements and terminators one evaluation may execute.
pub const STEP_LIMIT: u64 = 10_000_000;
/// The most calls one evaluation may nest.
pub const CALL_DEPTH: usize = 256;

/// The value of an evaluation: its bytes as laid out on the target, with
/// bytes never written, such as padding, set to zero (ADR 0027).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConstValue {
    pub bytes: Vec<u8>,
}

impl ConstValue {
    /// The integer in the first `size` bytes, little-endian.
    pub fn int(&self, size: usize) -> u64 {
        let mut value = 0u64;
        for (i, byte) in self.bytes[..size].iter().enumerate() {
            value |= u64::from(*byte) << (8 * i);
        }
        value
    }
}

/// What is being evaluated, for diagnostics.
pub struct Origin {
    /// Such as "the constant `TABLE`".
    pub what: String,
    pub span: Span,
}

/// Runs `body`, which takes no parameters, and returns its value, or `None`
/// after reporting why it has none. Each call is a separate evaluation,
/// with its own step budget and call depth (ADR 0029).
pub fn evaluate(q: &mut Queries, body: Rc<Body>, origin: Origin) -> Option<ConstValue> {
    let mut interp = Interp {
        q,
        allocs: Vec::new(),
        frames: Vec::new(),
        steps: 0,
    };
    match interp.run(body) {
        Ok(value) => Some(value),
        Err(Failure::Poisoned) => None,
        Err(Failure::Error(error)) => {
            let diag = interp.diagnostic(error, &origin);
            interp.q.diags.push(diag);
            None
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AllocId(u32);

/// One object: a local, or a constant's data.
struct Alloc {
    bytes: Vec<u8>,
    written: Vec<bool>,
    /// Pointers into compiler memory stored here, by offset.
    pointers: BTreeMap<u32, AllocId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pointer {
    /// Into an object the evaluator holds.
    Mem { alloc: AllocId, offset: u32 },
    /// Made from an integer: an address on the target, such as a device
    /// register, which only exists when the program runs.
    Addr(u32),
}

impl Pointer {
    /// Address arithmetic wraps, as at run time (ADR 0021).
    fn add(self, bytes: u64) -> Pointer {
        let bytes = bytes as u32;
        match self {
            Pointer::Mem { alloc, offset } => Pointer::Mem {
                alloc,
                offset: offset.wrapping_add(bytes),
            },
            Pointer::Addr(address) => Pointer::Addr(address.wrapping_add(bytes)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scalar {
    /// An integer or `bool`, masked to its width.
    Int(u64),
    Ptr(Pointer),
}

impl Scalar {
    fn int(self) -> u64 {
        match self {
            Scalar::Int(value) => value,
            Scalar::Ptr(_) => unreachable!("the type checker keeps pointers out of arithmetic"),
        }
    }
}

/// A place: where it is, whether accesses to it are volatile, and its type.
#[derive(Clone, Copy)]
struct PlaceRef {
    ptr: Pointer,
    volatile: bool,
    ty: Ty,
}

struct Frame {
    body: Rc<Body>,
    locals: Vec<AllocId>,
    block: BasicBlock,
    statement: usize,
    /// Where the caller wants the result, and the span of the call.
    dest: Option<Place>,
    call_span: Span,
}

/// Why an evaluation stopped.
enum Failure {
    Error(EvalError),
    /// Something it needs failed and was reported already.
    Poisoned,
}

struct EvalError {
    code: &'static str,
    message: String,
    label: String,
    span: Span,
    /// The call sites on the way, innermost first.
    calls: Vec<Span>,
}

type EvalResult<T> = Result<T, Failure>;

struct Interp<'a, 'h> {
    q: &'a mut Queries<'h>,
    allocs: Vec<Alloc>,
    frames: Vec<Frame>,
    steps: u64,
}

impl Interp<'_, '_> {
    fn run(&mut self, body: Rc<Body>) -> EvalResult<ConstValue> {
        let span = body.span;
        self.push_frame(body, Vec::new(), None, span)?;
        loop {
            self.step()?;
            let frame = self.frames.last().expect("a frame runs");
            let data = &frame.body.blocks[frame.block.0 as usize];
            if frame.statement < data.statements.len() {
                let statement = data.statements[frame.statement].clone();
                let span = data.spans[frame.statement];
                self.frames.last_mut().expect("a frame runs").statement += 1;
                self.statement(&statement, span)?;
                continue;
            }
            let terminator = data.terminator;
            let span = data.terminator_span;
            if let Some(value) = self.terminator(terminator, span)? {
                return Ok(value);
            }
        }
    }

    fn step(&mut self) -> EvalResult<()> {
        self.steps += 1;
        if self.steps > STEP_LIMIT {
            let span = self.current_span();
            return Err(self.error(
                "eval::step-limit",
                format!("the evaluation did not finish within {STEP_LIMIT} steps"),
                "still running here",
                span,
            ));
        }
        Ok(())
    }

    /// The span of what the innermost frame is about to execute.
    fn current_span(&self) -> Span {
        let frame = self.frames.last().expect("a frame runs");
        let data = &frame.body.blocks[frame.block.0 as usize];
        data.spans
            .get(frame.statement)
            .copied()
            .unwrap_or(data.terminator_span)
    }

    fn error(&self, code: &'static str, message: String, label: &str, span: Span) -> Failure {
        Failure::Error(EvalError {
            code,
            message,
            label: label.to_owned(),
            span,
            calls: self
                .frames
                .iter()
                .skip(1)
                .rev()
                .map(|f| f.call_span)
                .collect(),
        })
    }

    /// Points at the failure, at every call on the way and at what was
    /// being evaluated, as far as they are in one file: a diagnostic's
    /// labels must be, and the evaluation's own file is the one to show.
    fn diagnostic(&self, error: EvalError, origin: &Origin) -> Diagnostic {
        let file = origin.span.file;
        // Recursion calls from one place many times; one label is enough.
        let mut seen = Vec::new();
        let mut spans = std::iter::once((error.span, error.label.as_str()))
            .chain(error.calls.iter().map(|span| (*span, "called here")))
            .filter(|(span, _)| span.file == file)
            .filter(|(span, _)| {
                let new = !seen.contains(span);
                seen.push(*span);
                new
            });
        let (span, label) = spans.next().unwrap_or((origin.span, error.label.as_str()));
        let mut diag = Diagnostic::error(error.code, error.message).label(span, label);
        for (span, label) in spans {
            diag = diag.label(span, label);
        }
        diag.label(origin.span, format!("while evaluating {}", origin.what))
            .help("this code runs at compile time (ADR 0027)")
    }

    fn push_frame(
        &mut self,
        body: Rc<Body>,
        args: Vec<Arg>,
        dest: Option<Place>,
        call_span: Span,
    ) -> EvalResult<()> {
        if self.frames.len() >= CALL_DEPTH {
            return Err(self.error(
                "eval::call-depth",
                format!("the evaluation nested more than {CALL_DEPTH} calls"),
                "one call too deep",
                call_span,
            ));
        }
        let mut locals = Vec::with_capacity(body.locals.len());
        for decl in &body.locals {
            let alloc = match decl.constant {
                Some(data) => {
                    let bytes = self.q.data(data).to_vec();
                    self.new_alloc(bytes, true)
                }
                None => self.new_alloc(vec![0; decl.ty.layout().size as usize], false),
            };
            locals.push(alloc);
        }
        self.frames.push(Frame {
            body,
            locals,
            block: BasicBlock::START,
            statement: 0,
            dest,
            call_span,
        });
        for (i, arg) in args.into_iter().enumerate() {
            let place = Place::local(Local(i as u32 + 1));
            let dest = self.place(place)?;
            match arg {
                Arg::Scalar(value) => self.write(dest, value)?,
                Arg::Bytes(source) => self.copy(dest, source)?,
            }
        }
        Ok(())
    }

    fn new_alloc(&mut self, bytes: Vec<u8>, written: bool) -> AllocId {
        let len = bytes.len();
        self.allocs.push(Alloc {
            bytes,
            written: vec![written; len],
            pointers: BTreeMap::new(),
        });
        AllocId(self.allocs.len() as u32 - 1)
    }

    fn frame(&self) -> &Frame {
        self.frames.last().expect("a frame runs")
    }

    fn statement(&mut self, statement: &Statement, span: Span) -> EvalResult<()> {
        match statement {
            Statement::Assign(place, rvalue) => {
                let ty = self.frame().body.place_ty(*place);
                if matches!(ty, Ty::Unit | Ty::Never) {
                    return Ok(());
                }
                if ty.is_aggregate() {
                    let Rvalue::Use(Operand::Copy(source)) = rvalue else {
                        unreachable!("aggregates are only copied")
                    };
                    let to = self.place(*place)?;
                    let from = self.place(*source)?;
                    return self.copy_at(to, from, span);
                }
                let value = self.rvalue(rvalue, span)?;
                let dest = self.place(*place)?;
                self.write_at(dest, value, span)
            }
            Statement::Call { dest, callee, args } => self.call(*dest, callee.0, args, span),
        }
    }

    fn call(
        &mut self,
        dest: Option<Place>,
        callee: u32,
        args: &[Operand],
        span: Span,
    ) -> EvalResult<()> {
        let hir = self.q.hir;
        let id = crate::hir::FnId(callee);
        if id == hir.panic {
            return Err(self.error(
                "eval::panic",
                "the evaluation called the panic function".to_owned(),
                "called here",
                span,
            ));
        }
        let f = hir.fn_(id);
        if f.body.is_none() {
            return Err(self.error(
                "eval::extern-call",
                format!(
                    "`{}` is an `extern fn`, which cannot run at compile time",
                    f.name
                ),
                "called here",
                span,
            ));
        }
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            let ty = self.frame().body.operand_ty(arg);
            values.push(match (arg, ty.is_aggregate()) {
                (Operand::Copy(place), true) => Arg::Bytes(self.place(*place)?),
                _ => Arg::Scalar(self.operand(arg, span)?),
            });
        }
        let body = self.q.mir_body(id).ok_or(Failure::Poisoned)?;
        self.push_frame(body, values, dest, span)
    }

    fn terminator(&mut self, terminator: Terminator, span: Span) -> EvalResult<Option<ConstValue>> {
        let target = match terminator {
            Terminator::Goto(target) => target,
            Terminator::If { cond, then, else_ } => {
                if self.operand(&cond, span)?.int() != 0 {
                    then
                } else {
                    else_
                }
            }
            Terminator::Return => return self.return_(),
            Terminator::Unreachable => {
                unreachable!("control never reaches `unreachable` in well-typed MIR")
            }
            Terminator::Panic(cause) => {
                let (code, message) = match cause {
                    PanicCause::AssertionFailed => {
                        ("eval::assertion-failed", "assertion failed".to_owned())
                    }
                    cause => (
                        "eval::panic",
                        format!(
                            "the evaluation called the panic function: {}",
                            cause.describe()
                        ),
                    ),
                };
                return Err(self.error(code, message, cause.describe(), span));
            }
        };
        let frame = self.frames.last_mut().expect("a frame runs");
        frame.block = target;
        frame.statement = 0;
        Ok(None)
    }

    /// Leaves the innermost frame; the last one gives the result.
    fn return_(&mut self) -> EvalResult<Option<ConstValue>> {
        let frame = self.frames.pop().expect("a frame runs");
        let result = frame.locals[0];
        if self.frames.is_empty() {
            return self.result(result, frame.call_span).map(Some);
        }
        let ret = frame.body.ret;
        if let Some(dest) = frame.dest
            && !matches!(ret, Ty::Unit | Ty::Never)
        {
            let source = PlaceRef {
                ptr: Pointer::Mem {
                    alloc: result,
                    offset: 0,
                },
                volatile: false,
                ty: ret,
            };
            let to = self.place(dest)?;
            self.copy_at(to, source, frame.call_span)?;
        }
        Ok(None)
    }

    /// The bytes of the result. A pointer into compiler memory cannot leave
    /// the evaluation; bytes never written are zero.
    fn result(&self, alloc: AllocId, span: Span) -> EvalResult<ConstValue> {
        let alloc = &self.allocs[alloc.0 as usize];
        if !alloc.pointers.is_empty() {
            return Err(self.error(
                "eval::pointer-escapes",
                "the value holds a pointer into memory that exists only at compile time".to_owned(),
                "this value",
                span,
            ));
        }
        let bytes = alloc
            .bytes
            .iter()
            .zip(&alloc.written)
            .map(|(&byte, &written)| if written { byte } else { 0 })
            .collect();
        Ok(ConstValue { bytes })
    }

    // ------------------------------------------------------------ places

    /// Where a place is. Computing it reads the pointers on its path, but
    /// nothing else (ADR 0029).
    fn place(&mut self, place: Place) -> EvalResult<PlaceRef> {
        let frame = self.frame();
        let mut current = PlaceRef {
            ptr: Pointer::Mem {
                alloc: frame.locals[place.local.0 as usize],
                offset: 0,
            },
            volatile: false,
            ty: frame.body.local_ty(place.local),
        };
        let span = self.current_span();
        for elem in place.projection {
            current = match (*elem, current.ty) {
                (PlaceElem::Deref, Ty::Ptr(ptr)) => {
                    let Scalar::Ptr(target) = self.read_at(current, span)? else {
                        unreachable!("a pointer holds a pointer")
                    };
                    PlaceRef {
                        ptr: target,
                        volatile: ptr.volatile,
                        ty: *ptr.pointee,
                    }
                }
                (PlaceElem::Offset(index), Ty::Ptr(ptr)) => {
                    let Scalar::Ptr(target) = self.read_at(current, span)? else {
                        unreachable!("a pointer holds a pointer")
                    };
                    let index = self.local_int(index, span)?;
                    let size = ptr.pointee.layout().size;
                    PlaceRef {
                        ptr: target.add(index.wrapping_mul(size)),
                        volatile: ptr.volatile,
                        ty: *ptr.pointee,
                    }
                }
                (PlaceElem::Index(index), Ty::Array(elem, _)) => {
                    let index = self.local_int(index, span)?;
                    PlaceRef {
                        ptr: current.ptr.add(index.wrapping_mul(elem.layout().size)),
                        ty: *elem,
                        ..current
                    }
                }
                (PlaceElem::ConstIndex(index), Ty::Array(elem, _)) => PlaceRef {
                    ptr: current.ptr.add(index.wrapping_mul(elem.layout().size)),
                    ty: *elem,
                    ..current
                },
                (PlaceElem::Field(index), Ty::Struct(s)) => {
                    let field = &s.def().fields()[index as usize];
                    PlaceRef {
                        ptr: current.ptr.add(field.offset),
                        ty: field.ty,
                        ..current
                    }
                }
                (elem, ty) => unreachable!("projection {elem:?} of `{ty}`"),
            };
        }
        Ok(current)
    }

    fn local_int(&mut self, local: Local, span: Span) -> EvalResult<u64> {
        let place = self.place(Place::local(local))?;
        Ok(self.read_at(place, span)?.int())
    }

    /// The object and offset an access of `size` bytes reaches. Only reads
    /// and writes are refused here; computing a place never is (ADR 0029).
    fn access(
        &self,
        place: PlaceRef,
        size: u64,
        span: Span,
    ) -> EvalResult<Option<(AllocId, usize)>> {
        if size == 0 {
            return Ok(None);
        }
        if place.volatile {
            return Err(self.error(
                "eval::volatile-access",
                "a volatile access cannot happen at compile time".to_owned(),
                "accessed here",
                span,
            ));
        }
        match place.ptr {
            Pointer::Addr(address) => Err(self.error(
                "eval::invalid-access",
                format!(
                    "address {address:#x} only exists when the program runs, so it cannot be read or written at compile time"
                ),
                "accessed here",
                span,
            )),
            Pointer::Mem { alloc, offset } => {
                let len = self.allocs[alloc.0 as usize].bytes.len() as u64;
                if u64::from(offset) + size > len {
                    return Err(self.error(
                        "eval::invalid-access",
                        "this access reaches outside the object it points into".to_owned(),
                        "accessed here",
                        span,
                    ));
                }
                Ok(Some((alloc, offset as usize)))
            }
        }
    }

    fn read_at(&self, place: PlaceRef, span: Span) -> EvalResult<Scalar> {
        let size = place.ty.layout().size;
        let Some((alloc, offset)) = self.access(place, size, span)? else {
            return Ok(Scalar::Int(0));
        };
        let size = size as usize;
        let object = &self.allocs[alloc.0 as usize];
        if object.written[offset..offset + size].contains(&false) {
            return Err(self.error(
                "eval::uninit-read",
                "these bytes were never written".to_owned(),
                "read here",
                span,
            ));
        }
        let bytes = &object.bytes[offset..offset + size];
        let mut value = 0u64;
        for (i, byte) in bytes.iter().enumerate() {
            value |= u64::from(*byte) << (8 * i);
        }
        let pointer_at = object.pointers.range(..(offset + size) as u32).next_back();
        let overlaps = pointer_at.filter(|(at, _)| (**at as usize) + 4 > offset);
        match (place.ty, overlaps) {
            (Ty::Ptr(_), Some((&at, &target))) if at as usize == offset => {
                Ok(Scalar::Ptr(Pointer::Mem {
                    alloc: target,
                    offset: value as u32,
                }))
            }
            (Ty::Ptr(_), None) => Ok(Scalar::Ptr(Pointer::Addr(value as u32))),
            (_, Some(_)) => Err(self.error(
                "eval::pointer-to-int",
                "these bytes hold part of a pointer, whose address only exists after linking"
                    .to_owned(),
                "read here",
                span,
            )),
            (Ty::Bool, None) => Ok(Scalar::Int(u64::from(value != 0))),
            (_, None) => Ok(Scalar::Int(value)),
        }
    }

    fn write(&mut self, place: PlaceRef, value: Scalar) -> EvalResult<()> {
        let span = self.current_span();
        self.write_at(place, value, span)
    }

    fn write_at(&mut self, place: PlaceRef, value: Scalar, span: Span) -> EvalResult<()> {
        let size = place.ty.layout().size;
        let Some((alloc, offset)) = self.access(place, size, span)? else {
            return Ok(());
        };
        let size = size as usize;
        let object = &mut self.allocs[alloc.0 as usize];
        clear_pointers(object, offset, size);
        let bits = match value {
            Scalar::Int(value) => value,
            Scalar::Ptr(Pointer::Addr(address)) => u64::from(address),
            Scalar::Ptr(Pointer::Mem {
                alloc: target,
                offset: target_offset,
            }) => {
                object.pointers.insert(offset as u32, target);
                u64::from(target_offset)
            }
        };
        for i in 0..size {
            object.bytes[offset + i] = (bits >> (8 * i)) as u8;
            object.written[offset + i] = true;
        }
        Ok(())
    }

    fn copy(&mut self, to: PlaceRef, from: PlaceRef) -> EvalResult<()> {
        let span = self.current_span();
        self.copy_at(to, from, span)
    }

    /// Copies an aggregate: its bytes, which of them were written, and the
    /// pointers among them.
    fn copy_at(&mut self, to: PlaceRef, from: PlaceRef, span: Span) -> EvalResult<()> {
        let size = to.ty.layout().size;
        let Some((source, at)) = self.access(from, size, span)? else {
            return Ok(());
        };
        let Some((dest, to_at)) = self.access(to, size, span)? else {
            return Ok(());
        };
        let size = size as usize;
        let object = &self.allocs[source.0 as usize];
        let bytes = object.bytes[at..at + size].to_vec();
        let written = object.written[at..at + size].to_vec();
        let pointers: Vec<(u32, AllocId)> = object
            .pointers
            .range(at as u32..(at + size) as u32)
            .map(|(&offset, &target)| (offset - at as u32, target))
            .collect();
        let object = &mut self.allocs[dest.0 as usize];
        clear_pointers(object, to_at, size);
        object.bytes[to_at..to_at + size].copy_from_slice(&bytes);
        object.written[to_at..to_at + size].copy_from_slice(&written);
        for (offset, target) in pointers {
            object.pointers.insert(to_at as u32 + offset, target);
        }
        Ok(())
    }

    // ------------------------------------------------------------ values

    fn operand(&mut self, operand: &Operand, span: Span) -> EvalResult<Scalar> {
        match operand {
            Operand::Copy(place) => {
                let place = self.place(*place)?;
                if matches!(place.ty, Ty::Unit | Ty::Never) {
                    return Ok(Scalar::Int(0));
                }
                self.read_at(place, span)
            }
            Operand::Const(Const::Int(value, int)) => Ok(Scalar::Int(value & int.mask())),
            Operand::Const(Const::Bool(value)) => Ok(Scalar::Int(u64::from(*value))),
            Operand::Const(Const::Unit) => Ok(Scalar::Int(0)),
        }
    }

    fn rvalue(&mut self, rvalue: &Rvalue, span: Span) -> EvalResult<Scalar> {
        match rvalue {
            Rvalue::Use(operand) => self.operand(operand, span),
            Rvalue::Unary(op, operand) => {
                let ty = self.frame().body.operand_ty(operand);
                let value = self.operand(operand, span)?.int();
                Ok(Scalar::Int(match (op, ty) {
                    (UnOp::Neg, Ty::Int(int)) => value.wrapping_neg() & int.mask(),
                    (UnOp::Not, Ty::Int(int)) => !value & int.mask(),
                    (UnOp::Not, _) => u64::from(value == 0),
                    (op, ty) => unreachable!("{op:?} of `{ty}`"),
                }))
            }
            Rvalue::Binary(op, lhs, rhs) => {
                let ty = self.frame().body.operand_ty(lhs);
                let shift_ty = self.frame().body.operand_ty(rhs);
                let l = self.operand(lhs, span)?;
                let r = self.operand(rhs, span)?;
                if op.is_comparison() {
                    return self
                        .compare(*op, ty, l, r, span)
                        .map(|b| Scalar::Int(u64::from(b)));
                }
                Ok(Scalar::Int(arith(*op, ty, shift_ty, l.int(), r.int())))
            }
            Rvalue::Cast(operand, to) => {
                let from = self.frame().body.operand_ty(operand);
                let value = self.operand(operand, span)?;
                self.cast(value, from, *to, span)
            }
            Rvalue::AddressOf { place, .. } => Ok(Scalar::Ptr(self.place(*place)?.ptr)),
        }
    }

    fn compare(&self, op: BinOp, ty: Ty, l: Scalar, r: Scalar, span: Span) -> EvalResult<bool> {
        use std::cmp::Ordering;
        let ordering = match (l, r) {
            (Scalar::Int(a), Scalar::Int(b)) => match ty {
                Ty::Int(int) if int.is_signed() => sign_extend(a, int).cmp(&sign_extend(b, int)),
                _ => a.cmp(&b),
            },
            (Scalar::Ptr(Pointer::Addr(a)), Scalar::Ptr(Pointer::Addr(b))) => a.cmp(&b),
            (
                Scalar::Ptr(Pointer::Mem {
                    alloc: a,
                    offset: x,
                }),
                Scalar::Ptr(Pointer::Mem {
                    alloc: b,
                    offset: y,
                }),
            ) if a == b => x.cmp(&y),
            // Distinct objects are distinct, unless one occupies no memory
            // and may share its address (ADR 0022); their order is unknown.
            (
                Scalar::Ptr(Pointer::Mem { alloc: a, .. }),
                Scalar::Ptr(Pointer::Mem { alloc: b, .. }),
            ) if matches!(op, BinOp::Eq | BinOp::Ne)
                && !self.allocs[a.0 as usize].bytes.is_empty()
                && !self.allocs[b.0 as usize].bytes.is_empty() =>
            {
                return Ok(op == BinOp::Ne);
            }
            _ => {
                return Err(self.error(
                    "eval::pointer-compare",
                    "these pointers cannot be compared at compile time, since their addresses only exist after linking".to_owned(),
                    "compared here",
                    span,
                ));
            }
        };
        Ok(match op {
            BinOp::Eq => ordering == Ordering::Equal,
            BinOp::Ne => ordering != Ordering::Equal,
            BinOp::Lt => ordering == Ordering::Less,
            BinOp::Le => ordering != Ordering::Greater,
            BinOp::Gt => ordering == Ordering::Greater,
            BinOp::Ge => ordering != Ordering::Less,
            _ => unreachable!("`{}` is not a comparison", op.symbol()),
        })
    }

    /// Conversions with `as` (ADR 0019).
    fn cast(&self, value: Scalar, from: Ty, to: Ty, span: Span) -> EvalResult<Scalar> {
        Ok(match (from, to, value) {
            (Ty::Int(from), Ty::Int(to), Scalar::Int(v)) => {
                let wide = if from.is_signed() {
                    sign_extend(v, from) as u64
                } else {
                    v
                };
                Scalar::Int(wide & to.mask())
            }
            (Ty::Bool, Ty::Int(_), value) => value,
            (Ty::Int(_), Ty::Ptr(_), Scalar::Int(v)) => Scalar::Ptr(Pointer::Addr(v as u32)),
            (Ty::Ptr(_), Ty::Int(to), Scalar::Ptr(Pointer::Addr(a))) => {
                Scalar::Int(u64::from(a) & to.mask())
            }
            (Ty::Ptr(_), Ty::Int(_), Scalar::Ptr(Pointer::Mem { .. })) => {
                return Err(self.error(
                    "eval::pointer-to-int",
                    "this pointer's address only exists after linking".to_owned(),
                    "converted here",
                    span,
                ));
            }
            (Ty::Ptr(_), Ty::Ptr(_), value) => value,
            (from, to, _) => unreachable!("cast from `{from}` to `{to}`"),
        })
    }
}

/// An argument of a call: a scalar's value, or where an aggregate is.
enum Arg {
    Scalar(Scalar),
    Bytes(PlaceRef),
}

/// Forgets the pointers that bytes `offset..offset + size` overlap.
fn clear_pointers(object: &mut Alloc, offset: usize, size: usize) {
    let start = offset.saturating_sub(target::POINTER_BYTES as usize - 1) as u32;
    let end = (offset + size) as u32;
    let stale: Vec<u32> = object
        .pointers
        .range(start..end)
        .map(|(&at, _)| at)
        .collect();
    for at in stale {
        object.pointers.remove(&at);
    }
}

fn sign_extend(value: u64, int: IntTy) -> i64 {
    let shift = 64 - int.bits();
    ((value << shift) as i64) >> shift
}

/// Arithmetic as the target does it (ADR 0007): wrapping, shifts masked to
/// the width, division truncating. MIR rules out zero divisors and
/// `MIN / -1` before a division is reached.
fn arith(op: BinOp, ty: Ty, amount_ty: Ty, l: u64, r: u64) -> u64 {
    let int = match ty {
        Ty::Int(int) => int,
        // `&`, `|` and `^` on `bool`.
        _ => {
            return match op {
                BinOp::BitAnd => l & r,
                BinOp::BitOr => l | r,
                BinOp::BitXor => l ^ r,
                _ => unreachable!("`{}` on `{ty}`", op.symbol()),
            };
        }
    };
    let mask = int.mask();
    let signed = int.is_signed();
    let (sl, sr) = (sign_extend(l, int), sign_extend(r, int));
    let shift = || {
        // As code generation does: the amount is cast to the shifted width
        // and masked to its number of bits.
        let amount = match amount_ty {
            Ty::Int(amount) => r & amount.mask(),
            _ => r,
        };
        (amount & mask & u64::from(int.bits() - 1)) as u32
    };
    let value = match op {
        BinOp::Add => l.wrapping_add(r),
        BinOp::Sub => l.wrapping_sub(r),
        BinOp::Mul => l.wrapping_mul(r),
        BinOp::Div if signed => sl.wrapping_div(sr) as u64,
        BinOp::Div => l / r,
        BinOp::Rem if signed => sl.wrapping_rem(sr) as u64,
        BinOp::Rem => l % r,
        BinOp::BitAnd => l & r,
        BinOp::BitOr => l | r,
        BinOp::BitXor => l ^ r,
        BinOp::Shl => l << shift(),
        BinOp::Shr if signed => (sl >> shift()) as u64,
        BinOp::Shr => l >> shift(),
        _ => unreachable!("`{}` is a comparison", op.symbol()),
    };
    value & mask
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic_matches_the_target() {
        let i8_ = Ty::Int(IntTy::I8);
        let u8_ = Ty::Int(IntTy::U8);
        assert_eq!(arith(BinOp::Add, u8_, u8_, 250, 10), 4);
        assert_eq!(arith(BinOp::Div, i8_, i8_, 0xF9, 2), 0xFD); // -7 / 2 = -3
        assert_eq!(arith(BinOp::Rem, i8_, i8_, 0xF9, 2), 0xFF); // -7 % 2 = -1
        assert_eq!(arith(BinOp::Shl, u8_, u8_, 1, 9), 2); // masked to 1
        assert_eq!(arith(BinOp::Shr, i8_, u8_, 0x80, 7), 0xFF);
        assert_eq!(sign_extend(0xFF, IntTy::I8), -1);
    }
}
