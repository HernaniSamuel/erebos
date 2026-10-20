//! The compile-time evaluator (ADR 0027, 0029): runs MIR the way the
//! target would, and is the evaluation layer of ADR 0033, which gives
//! values back to type checking and MIR building.
//!
//! Memory is bytes laid out as on the target (ADR 0022), each byte known to
//! be written or not. A pointer is an object and an offset, never a host
//! address; pointers stored in memory are recorded beside the bytes, since
//! their addresses only exist after linking. A pointer made from an integer
//! is just that integer: it can be computed with, but not read through.

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use crate::diag::Diagnostic;
use crate::int::IntTy;
use crate::mir::{
    BasicBlock, BinOp, Body, Const, EnumDef, Local, Operand, PanicCause, Place, PlaceElem,
    Relocation, Rvalue, Statement, StaticId, StaticRef, Terminator, Ty, UnOp,
};
use crate::query::Queries;
use crate::span::Span;
use crate::target;

/// The most MIR statements and terminators one evaluation may execute.
pub const STEP_LIMIT: u64 = 10_000_000;
/// The most calls one evaluation may nest.
pub const CALL_DEPTH: usize = 256;

/// The value of an evaluation: its bytes as laid out on the target, with
/// bytes never written, such as padding, set to zero (ADR 0027), and the
/// pointers to statics among them (ADR 0030).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConstValue {
    /// Zero where a relocation is.
    pub bytes: Vec<u8>,
    pub relocations: Vec<Relocation>,
}

impl ConstValue {
    /// The integer in the first `size` bytes, little-endian. A pointer to
    /// a static reads as zero: its address only exists after linking.
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
        statics: HashMap::new(),
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

/// One object: a local, a constant's data, or a static.
struct Alloc {
    bytes: Vec<u8>,
    written: Vec<bool>,
    /// Pointers into compiler memory stored here, by offset.
    pointers: BTreeMap<u32, AllocId>,
    /// False once the call it belongs to has returned: its stack slot is
    /// reused at run time, so pointers to it are dangling (ADR 0030).
    live: bool,
    /// The static this object is, if it is one.
    static_: Option<StaticObject>,
}

/// A static at compile time (ADR 0030): read-only, and its bytes loaded
/// from its value only when read, so that taking its address needs no
/// value.
struct StaticObject {
    def: StaticRef,
    loaded: bool,
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
    /// The object of each static used, by id: one per evaluation.
    statics: HashMap<u32, AllocId>,
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

    /// How diagnostics name a static: by its path (ADR 0049), not by its
    /// symbol; a byte string by its bytes.
    fn static_name(&self, def: &crate::mir::StaticDef) -> String {
        if def.byte_string.is_some() {
            return def.name.clone();
        }
        self.q
            .hir
            .static_(crate::hir::StaticId(def.id.0))
            .path
            .clone()
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
                    let data = self.q.data(data);
                    let (bytes, relocations) = (data.bytes.clone(), data.relocations.clone());
                    let alloc = self.new_alloc(Vec::new(), true);
                    self.fill(alloc, bytes, &relocations);
                    alloc
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
            live: true,
            static_: None,
        });
        AllocId(self.allocs.len() as u32 - 1)
    }

    /// Sets the bytes of an object to a value computed at compile time,
    /// with its pointers to statics.
    fn fill(&mut self, alloc: AllocId, mut bytes: Vec<u8>, relocations: &[Relocation]) {
        let mut pointers = BTreeMap::new();
        for relocation in relocations {
            let at = relocation.offset as usize;
            bytes[at..at + target::POINTER_BYTES as usize]
                .copy_from_slice(&relocation.addend.to_le_bytes());
            pointers.insert(relocation.offset, self.static_alloc_of(relocation.target));
        }
        let object = &mut self.allocs[alloc.0 as usize];
        object.written = vec![true; bytes.len()];
        object.bytes = bytes;
        object.pointers = pointers;
    }

    /// The object of the static `id`.
    fn static_alloc_of(&mut self, id: StaticId) -> AllocId {
        let s = self.q.mir_static_by_id(id);
        self.static_alloc(s)
    }

    /// The object of a static, made when first used.
    fn static_alloc(&mut self, s: StaticRef) -> AllocId {
        let id = s.def().id.0;
        if let Some(&alloc) = self.statics.get(&id) {
            return alloc;
        }
        let alloc = self.new_alloc(vec![0; s.def().ty.layout().size as usize], false);
        self.allocs[alloc.0 as usize].static_ = Some(StaticObject {
            def: s,
            loaded: false,
        });
        self.statics.insert(id, alloc);
        alloc
    }

    fn frame(&self) -> &Frame {
        self.frames.last().expect("a frame runs")
    }

    fn statement(&mut self, statement: &Statement, span: Span) -> EvalResult<()> {
        match statement {
            Statement::Assign(place, rvalue) => {
                let ty = self.frame().body.place_ty(*place);
                // No memory has type `!`, so a place of type `!` is only
                // reached by code after an expression that does not finish,
                // which is never built (ADR 0034).
                if ty == Ty::Never {
                    unreachable!("a place of type `!` is never reached (ADR 0034)")
                }
                // Nothing is stored, but the source and the destination
                // are still accessed, and checked.
                if ty == Ty::Unit {
                    self.rvalue(rvalue, span)?;
                    let dest = self.place(*place)?;
                    return self.write_at(dest, Scalar::Int(0), span);
                }
                if let Rvalue::Slice { ptr, len } = rvalue {
                    let address = self.operand(ptr, span)?;
                    let len = self.operand(len, span)?;
                    let (address_part, len_part) = slice_parts(self.place(*place)?);
                    self.write_at(address_part, address, span)?;
                    return self.write_at(len_part, len, span);
                }
                if ty.is_aggregate() {
                    let Rvalue::Use(Operand::Copy(source)) = rvalue else {
                        unreachable!("aggregates are only copied, and slices made")
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
                    f.path
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
            Terminator::Switch {
                discr,
                cases,
                otherwise,
            } => {
                let value = self.operand(&discr, span)?.int();
                cases
                    .iter()
                    .find(|(case, _)| *case == value)
                    .map_or(otherwise, |&(_, target)| target)
            }
            Terminator::Return => return self.return_(),
            // It follows an expression of type `!`, which does not finish:
            // no memory holds a `!` to read (ADR 0034).
            Terminator::Unreachable => {
                unreachable!("control never reaches `unreachable` in well-typed MIR (ADR 0034)")
            }
            Terminator::Panic(cause) => {
                let (code, message) = match cause {
                    PanicCause::AssertionFailed(None) => {
                        ("eval::assertion-failed", "assertion failed".to_owned())
                    }
                    PanicCause::AssertionFailed(Some((message, message_span))) => {
                        let text = self.message(message, message_span)?;
                        (
                            "eval::assertion-failed",
                            format!("assertion failed: {text}"),
                        )
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

    /// The bytes of the slice `message`, an `assert` message, as text:
    /// printable ASCII as it is, any other byte as `\xNN` (ADR 0031). A
    /// byte that cannot be read fails at `span`, the message's.
    fn message(&mut self, message: Operand, span: Span) -> EvalResult<String> {
        let Operand::Copy(place) = message else {
            unreachable!("a slice is an aggregate, read from its place")
        };
        let (address, len) = slice_parts(self.place(place)?);
        let Scalar::Ptr(start) = self.read_at(address, span)? else {
            unreachable!("a pointer holds a pointer")
        };
        let len = self.read_at(len, span)?.int();
        let mut text = String::new();
        for i in 0..len {
            let byte = PlaceRef {
                ptr: start.add(i),
                volatile: false,
                ty: Ty::Int(IntTy::U8),
            };
            match self.read_at(byte, span)?.int() as u8 {
                byte @ b' '..=b'~' => text.push(char::from(byte)),
                byte => text.push_str(&format!("\\x{byte:02x}")),
            }
        }
        Ok(text)
    }

    /// Leaves the innermost frame; the last one gives the result.
    fn return_(&mut self) -> EvalResult<Option<ConstValue>> {
        let frame = self.frames.pop().expect("a frame runs");
        let result = frame.locals[0];
        let ret = frame.body.ret;
        if self.frames.is_empty() {
            return self.result(result, ret, frame.call_span).map(Some);
        }
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
        // Constant data is a global at run time, not a variable of the
        // call, so it outlives the call.
        for (alloc, decl) in frame.locals.iter().zip(&frame.body.locals) {
            if decl.constant.is_none() {
                self.allocs[alloc.0 as usize].live = false;
            }
        }
        Ok(None)
    }

    /// The bytes of the result, of type `ty`; bytes never written are
    /// zero, those of an enum with payloads outside its variant among them
    /// (ADR 0039). A pointer into a static, into it or one past its end,
    /// becomes a relocation, and a slice must lie inside its static; one
    /// into the evaluation's own memory cannot leave it (ADR 0030, 0031).
    fn result(&mut self, alloc: AllocId, ty: Ty, span: Span) -> EvalResult<ConstValue> {
        let object = &self.allocs[alloc.0 as usize];
        let mut bytes: Vec<u8> = object
            .bytes
            .iter()
            .zip(&object.written)
            .map(|(&byte, &written)| if written { byte } else { 0 })
            .collect();
        let pointers = object.pointers.clone();
        let mut relocations = Vec::new();
        for (&offset, &target) in &pointers {
            let target = &self.allocs[target.0 as usize];
            let Some(s) = &target.static_ else {
                return Err(self.error(
                    "eval::pointer-escapes",
                    "the value holds a pointer into memory that exists only at compile time"
                        .to_owned(),
                    "this value",
                    span,
                ));
            };
            let at = offset as usize..offset as usize + target::POINTER_BYTES as usize;
            let addend = u32::from_le_bytes(
                bytes
                    .get(at.clone())
                    .expect("accesses keep pointers whole and inside their object")
                    .try_into()
                    .expect("a pointer is four bytes"),
            );
            // The size of an `extern static` is its declaration's, which
            // need not be its definition's: the linker adds any offset.
            let size = target.bytes.len();
            if addend as usize > size && !s.def.def().external {
                return Err(self.error(
                    "eval::pointer-out-of-bounds",
                    format!(
                        "the value holds a pointer to offset {} of `{}`, which is {size} bytes long",
                        addend as i32,
                        self.static_name(s.def.def())
                    ),
                    "this value",
                    span,
                ));
            }
            let def = s.def.def();
            if !def.external
                && let Some(elem) = self.slice_at(ty, u64::from(offset), &bytes)?
            {
                self.slice_in_bounds(&pointers, &bytes, offset, elem, span)?;
            }
            bytes[at].fill(0);
            relocations.push(Relocation {
                offset,
                target: def.id,
                addend,
            });
        }
        Ok(ConstValue { bytes, relocations })
    }

    /// The element type of the slice at `offset` bytes into a value of type
    /// `ty` whose bytes, unwritten ones zero, are `bytes`, if a slice
    /// starts there. Of an enum with payloads, only the payload of the
    /// variant its tag names is a value (ADR 0039).
    fn slice_at(&mut self, ty: Ty, offset: u64, bytes: &[u8]) -> EvalResult<Option<Ty>> {
        // Where `offset` is in a part at `at` of type `part`, if it is.
        let inside = |at: u64, part: Ty| {
            offset
                .checked_sub(at)
                .filter(|&within| within < part.layout().size)
        };
        match ty {
            Ty::Ptr(_) if offset == 0 => Ok(ty.slice_elem()),
            Ty::Array(elem, len) => {
                let size = elem.layout().size;
                if size == 0 || offset / size >= len {
                    return Ok(None);
                }
                let at = offset / size * size;
                self.slice_at(*elem, offset - at, &bytes[at as usize..])
            }
            Ty::Struct(s) => {
                for field in s.def().fields() {
                    if let Some(within) = inside(field.offset, field.ty) {
                        let part = &bytes[field.offset as usize..];
                        return self.slice_at(field.ty, within, part);
                    }
                }
                Ok(None)
            }
            // Its variants' values are asked for only if a slice may be
            // there, so that a value that holds none never needs them.
            Ty::Enum(e) if contains_slice(ty) => {
                let def = e.def();
                let tag = bytes[..def.int.bytes() as usize]
                    .iter()
                    .rev()
                    .fold(0u64, |value, &byte| value << 8 | u64::from(byte));
                let Some(variant) = self.variant_of(def, tag)? else {
                    return Ok(None);
                };
                for field in &def.variants()[variant].payload {
                    if let Some(within) = inside(field.offset, field.ty) {
                        let part = &bytes[field.offset as usize..];
                        return self.slice_at(field.ty, within, part);
                    }
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    /// The index of the variant of `def` whose value is `tag`, if one is.
    fn variant_of(&mut self, def: &EnumDef, tag: u64) -> EvalResult<Option<usize>> {
        let id = crate::hir::EnumId(def.id.0);
        for index in 0..def.variants().len() {
            let value = self
                .q
                .discriminant(id, index as u32)
                .ok_or(Failure::Poisoned)?;
            if value == tag {
                return Ok(Some(index));
            }
        }
        Ok(None)
    }

    /// Refuses a slice of elements `elem` at `offset` of a result with
    /// bytes `bytes` and pointers `pointers`, whose address points into a
    /// static, if its elements do not all lie inside that static (ADR 0031).
    fn slice_in_bounds(
        &self,
        pointers: &BTreeMap<u32, AllocId>,
        bytes: &[u8],
        offset: u32,
        elem: Ty,
        span: Span,
    ) -> EvalResult<()> {
        let width = target::POINTER_BYTES as u32;
        let target = &self.allocs[pointers[&offset].0 as usize];
        let name = &self.static_name(
            target
                .static_
                .as_ref()
                .expect("a slice of a static")
                .def
                .def(),
        );
        let size = target.bytes.len();
        let address = &bytes[offset as usize..(offset + width) as usize];
        let addend = u32::from_le_bytes(address.try_into().expect("an address is four bytes"));
        let len_at = offset + width;
        // The bound needs the length as a number, which a pointer is not
        // until linking. A pointer stored in the length of any other slice,
        // such as one made from an integer address, is a relocation like
        // any other: at run time the length holds that address.
        if pointers
            .range(len_at.saturating_sub(width - 1)..len_at + width)
            .next()
            .is_some()
        {
            return Err(self.error(
                "eval::pointer-to-int",
                "the length of a slice in this value holds part of a pointer, whose address only exists after linking"
                    .to_owned(),
                "this value",
                span,
            ));
        }
        let len_bytes = &bytes[len_at as usize..(len_at + width) as usize];
        let len = u32::from_le_bytes(len_bytes.try_into().expect("a length is four bytes"));
        let end = checked_size(elem)
            .and_then(|elem_size| elem_size.checked_mul(u64::from(len)))
            .and_then(|bytes| bytes.checked_add(u64::from(addend)));
        if end.is_some_and(|end| end <= size as u64) {
            return Ok(());
        }
        Err(self.error(
            "eval::pointer-out-of-bounds",
            format!(
                "the value holds a slice of {len} elements of `{elem}` from offset {addend} of `{name}`, which is {size} bytes long"
            ),
            "this value",
            span,
        ))
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
                // Reached by offset, as fields are (ADR 0039).
                (PlaceElem::Tag, Ty::Enum(e)) => PlaceRef {
                    ty: Ty::Int(e.def().int),
                    ..current
                },
                (PlaceElem::Payload { variant, index }, Ty::Enum(e)) => {
                    let value = e.def().payload(variant, index);
                    PlaceRef {
                        ptr: current.ptr.add(value.offset),
                        ty: value.ty,
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

    /// The object and offset an access of `size` bytes reaches, a write if
    /// `write`; `None` for an access of no bytes, which is checked all the
    /// same. Only reads and writes are refused here; computing a place
    /// never is (ADR 0029).
    fn access(
        &mut self,
        place: PlaceRef,
        size: u64,
        span: Span,
        write: bool,
    ) -> EvalResult<Option<(AllocId, usize)>> {
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
                let object = &self.allocs[alloc.0 as usize];
                if !object.live {
                    return Err(self.dangling("accessed here", span));
                }
                if let Some(s) = &object.static_ {
                    let def = s.def.def();
                    let label = if write { "written here" } else { "read here" };
                    if def.external {
                        return Err(self.error(
                            "eval::extern-static",
                            format!(
                                "`{}` is an `extern static`, defined outside the program, so it cannot be read or written at compile time",
                                self.static_name(def)
                            ),
                            label,
                            span,
                        ));
                    }
                    if def.mutable {
                        return Err(self.error(
                            "eval::static-mut",
                            format!(
                                "`{}` is a `static mut`, whose value is only known when the program runs",
                                self.static_name(def)
                            ),
                            label,
                            span,
                        ));
                    }
                    if write {
                        let what = match def.byte_string {
                            Some(_) => "a byte string".to_owned(),
                            None => format!("`{}` is a `static`, which", self.static_name(def)),
                        };
                        return Err(self.error(
                            "eval::read-only-write",
                            format!("{what} cannot be written"),
                            label,
                            span,
                        ));
                    }
                }
                if size == 0 {
                    return Ok(None);
                }
                let len = object.bytes.len() as u64;
                if u64::from(offset) + size > len {
                    return Err(self.error(
                        "eval::invalid-access",
                        "this access reaches outside the object it points into".to_owned(),
                        "accessed here",
                        span,
                    ));
                }
                self.load_static(alloc)?;
                Ok(Some((alloc, offset as usize)))
            }
        }
    }

    /// Loads the bytes of a static's object from its value, once.
    fn load_static(&mut self, alloc: AllocId) -> EvalResult<()> {
        let object = &self.allocs[alloc.0 as usize];
        let Some(StaticObject { def, loaded: false }) = object.static_ else {
            return Ok(());
        };
        if let Some(bytes) = &def.def().byte_string {
            self.fill(alloc, bytes.clone(), &[]);
            self.allocs[alloc.0 as usize].static_ = Some(StaticObject { def, loaded: true });
            return Ok(());
        }
        let id = crate::hir::StaticId(def.def().id.0);
        let value = self.q.static_value(id).ok_or(Failure::Poisoned)?;
        self.fill(alloc, value.bytes, &value.relocations);
        self.allocs[alloc.0 as usize].static_ = Some(StaticObject { def, loaded: true });
        Ok(())
    }

    fn read_at(&mut self, place: PlaceRef, span: Span) -> EvalResult<Scalar> {
        let size = place.ty.layout().size;
        let Some((alloc, offset)) = self.access(place, size, span, false)? else {
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
        let Some((alloc, offset)) = self.access(place, size, span, true)? else {
            return Ok(());
        };
        let size = size as usize;
        self.whole_pointers(alloc, offset, size, true, span)?;
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
    /// pointers among them, which it copies whole.
    fn copy_at(&mut self, to: PlaceRef, from: PlaceRef, span: Span) -> EvalResult<()> {
        let size = to.ty.layout().size;
        let Some((source, at)) = self.access(from, size, span, false)? else {
            return Ok(());
        };
        let Some((dest, to_at)) = self.access(to, size, span, true)? else {
            return Ok(());
        };
        let size = size as usize;
        self.whole_pointers(source, at, size, false, span)?;
        self.whole_pointers(dest, to_at, size, true, span)?;
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

    /// Refuses an access to bytes `offset..offset + size` of an object that
    /// takes only part of a pointer stored there: its bytes are never plain
    /// data, since its address only exists after linking (ADR 0030).
    fn whole_pointers(
        &self,
        alloc: AllocId,
        offset: usize,
        size: usize,
        write: bool,
        span: Span,
    ) -> EvalResult<()> {
        let width = target::POINTER_BYTES as usize;
        let start = offset.saturating_sub(width - 1) as u32;
        let end = (offset + size) as u32;
        let partial = self.allocs[alloc.0 as usize]
            .pointers
            .range(start..end)
            .any(|(&at, _)| (at as usize) < offset || at as usize + width > offset + size);
        if !partial {
            return Ok(());
        }
        let (message, label) = if write {
            (
                "this write overwrites part of a pointer, whose address only exists after linking",
                "written here",
            )
        } else {
            (
                "these bytes hold part of a pointer, whose address only exists after linking",
                "read here",
            )
        };
        Err(self.error("eval::pointer-to-int", message.to_owned(), label, span))
    }

    // ------------------------------------------------------------ values

    fn operand(&mut self, operand: &Operand, span: Span) -> EvalResult<Scalar> {
        match operand {
            Operand::Copy(place) => {
                let place = self.place(*place)?;
                self.read_at(place, span)
            }
            Operand::Const(Const::Int(value, int)) => Ok(Scalar::Int(value & int.mask())),
            Operand::Const(Const::Bool(value)) => Ok(Scalar::Int(u64::from(*value))),
            Operand::Const(Const::Unit) => Ok(Scalar::Int(0)),
            Operand::Const(Const::Static(s)) => Ok(Scalar::Ptr(Pointer::Mem {
                alloc: self.static_alloc(*s),
                offset: 0,
            })),
        }
    }

    fn rvalue(&mut self, rvalue: &Rvalue, span: Span) -> EvalResult<Scalar> {
        match rvalue {
            Rvalue::Use(operand) => self.operand(operand, span),
            Rvalue::Unary(op, operand) => {
                let ty = self.frame().body.operand_ty(operand);
                let value = self.operand(operand, span)?.int();
                Ok(Scalar::Int(unary(*op, ty, value)))
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
            Rvalue::SliceAddress(place) => {
                let (address, _) = slice_parts(self.place(*place)?);
                self.read_at(address, span)
            }
            Rvalue::SliceLen(place) => {
                let (_, len) = slice_parts(self.place(*place)?);
                self.read_at(len, span)
            }
            Rvalue::Slice { .. } => unreachable!("a slice is written to its place whole"),
        }
    }

    /// Compares as the target would, or fails where the run-time answer
    /// is not known (ADR 0030).
    fn compare(&self, op: BinOp, ty: Ty, l: Scalar, r: Scalar, span: Span) -> EvalResult<bool> {
        let ordering = match (l, r) {
            (Scalar::Int(a), Scalar::Int(b)) => int_ordering(ty, a, b),
            (Scalar::Ptr(l), Scalar::Ptr(r)) => self.compare_pointers(op, l, r, span)?,
            (l, r) => unreachable!("comparing {l:?} with {r:?}: operands of one type"),
        };
        Ok(answer(op, ordering))
    }

    /// How two pointers compare at run time, or an error where that is not
    /// known. An answer for `==` or `!=` between different objects is
    /// returned as "not equal".
    fn compare_pointers(
        &self,
        op: BinOp,
        l: Pointer,
        r: Pointer,
        span: Span,
    ) -> EvalResult<std::cmp::Ordering> {
        let fail = |message: &str| {
            Err(self.error(
                "eval::pointer-compare",
                message.to_owned(),
                "compared here",
                span,
            ))
        };
        for pointer in [l, r] {
            if let Pointer::Mem { alloc, .. } = pointer
                && !self.allocs[alloc.0 as usize].live
            {
                return Err(self.dangling("compared here", span));
            }
        }
        let ordered = !matches!(op, BinOp::Eq | BinOp::Ne);
        match (l, r) {
            (Pointer::Addr(a), Pointer::Addr(b)) => Ok(a.cmp(&b)),
            (
                Pointer::Mem {
                    alloc: a,
                    offset: x,
                },
                Pointer::Mem {
                    alloc: b,
                    offset: y,
                },
            ) if a == b => {
                // Outside [0, size] the address may have wrapped around
                // the address space, which reverses the order.
                let size = self.allocs[a.0 as usize].bytes.len() as u64;
                if ordered && (u64::from(x) > size || u64::from(y) > size) {
                    return fail(
                        "a pointer outside its object may have wrapped around the address space, so the order of these pointers is not known at compile time",
                    );
                }
                Ok(x.cmp(&y))
            }
            (
                Pointer::Mem {
                    alloc: a,
                    offset: x,
                },
                Pointer::Mem {
                    alloc: b,
                    offset: y,
                },
            ) => {
                if ordered {
                    return fail(
                        "pointers into different objects have no order known at compile time, since their addresses only exist after linking",
                    );
                }
                // An `extern static` is any memory the linker finds: it may
                // be the other object (ADR 0030).
                let external = |alloc: AllocId| {
                    self.allocs[alloc.0 as usize]
                        .static_
                        .as_ref()
                        .is_some_and(|s| s.def.def().external)
                };
                if external(a) || external(b) {
                    return fail(
                        "an `extern static` may be the same memory as another object, so these pointers may be equal at run time",
                    );
                }
                // Byte strings may share memory with each other and with
                // other read-only data holding the same bytes (ADR 0031).
                let byte_string = |alloc: AllocId| {
                    self.allocs[alloc.0 as usize]
                        .static_
                        .as_ref()
                        .is_some_and(|s| s.def.def().byte_string.is_some())
                };
                if byte_string(a) || byte_string(b) {
                    return fail(
                        "a byte string may share memory with other read-only data, so these pointers may be equal at run time",
                    );
                }
                // One object may start where another ends, and one of
                // size zero may share its address with another.
                let inside = |alloc: AllocId, offset: u32| {
                    (offset as usize) < self.allocs[alloc.0 as usize].bytes.len()
                };
                if !inside(a, x) || !inside(b, y) {
                    return fail(
                        "a pointer that is not strictly inside its object may equal a pointer into another object at run time",
                    );
                }
                Ok(std::cmp::Ordering::Less)
            }
            _ => fail(
                "these pointers cannot be compared at compile time, since their addresses only exist after linking",
            ),
        }
    }

    fn dangling(&self, label: &str, span: Span) -> Failure {
        self.error(
            "eval::dangling-pointer",
            "this pointer points to a variable of a call that has returned".to_owned(),
            label,
            span,
        )
    }

    /// Conversions with `as` (ADR 0019).
    fn cast(&self, value: Scalar, from: Ty, to: Ty, span: Span) -> EvalResult<Scalar> {
        Ok(match (from, to, value) {
            (Ty::Int(from), Ty::Int(to), Scalar::Int(v)) => Scalar::Int(cast_int(v, from, to)),
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

/// The two parts of the slice at `slice` (ADR 0031): its address, a thin
/// pointer to its first element, then its length, a `usize`.
fn slice_parts(slice: PlaceRef) -> (PlaceRef, PlaceRef) {
    let address = PlaceRef {
        ty: slice.ty.slice_address_ty().expect("a slice"),
        ..slice
    };
    let len = PlaceRef {
        ptr: slice.ptr.add(target::POINTER_BYTES),
        ty: Ty::Int(IntTy::Usize),
        ..slice
    };
    (address, len)
}

/// Whether a value of type `ty` holds a slice, not behind a pointer. No
/// type contains itself by value, so this ends.
fn contains_slice(ty: Ty) -> bool {
    match ty {
        Ty::Ptr(_) => ty.slice_elem().is_some(),
        Ty::Array(elem, _) => contains_slice(*elem),
        Ty::Struct(s) => s
            .def()
            .fields()
            .iter()
            .any(|field| contains_slice(field.ty)),
        Ty::Enum(e) => e
            .def()
            .variants()
            .iter()
            .flat_map(|variant| &variant.payload)
            .any(|value| contains_slice(value.ty)),
        _ => false,
    }
}

/// The size of `ty`, or `None` if it has none, or one too large to
/// compute: the elements of a slice are not laid out until accessed.
fn checked_size(ty: Ty) -> Option<u64> {
    match ty {
        Ty::Array(elem, len) => checked_size(*elem)?.checked_mul(len),
        Ty::Slice(_) => None,
        ty => Some(ty.layout().size),
    }
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

pub(crate) fn sign_extend(value: u64, int: IntTy) -> i64 {
    let shift = 64 - int.bits();
    ((value << shift) as i64) >> shift
}

/// Unary `-` and `!` as the target does them (ADR 0007): `-` wraps.
pub(crate) fn unary(op: UnOp, ty: Ty, value: u64) -> u64 {
    match (op, ty) {
        (UnOp::Neg, Ty::Int(int)) => value.wrapping_neg() & int.mask(),
        (UnOp::Not, Ty::Int(int)) => !value & int.mask(),
        (UnOp::Not, _) => u64::from(value == 0),
        (op, ty) => unreachable!("{op:?} of `{ty}`"),
    }
}

/// A comparison of two integers or `bool`s of type `ty`.
pub(crate) fn compare_ints(op: BinOp, ty: Ty, a: u64, b: u64) -> bool {
    answer(op, int_ordering(ty, a, b))
}

fn int_ordering(ty: Ty, a: u64, b: u64) -> std::cmp::Ordering {
    match ty {
        Ty::Int(int) if int.is_signed() => sign_extend(a, int).cmp(&sign_extend(b, int)),
        _ => a.cmp(&b),
    }
}

/// Whether `op` holds between two values that compare as `ordering`.
fn answer(op: BinOp, ordering: std::cmp::Ordering) -> bool {
    use std::cmp::Ordering;
    match op {
        BinOp::Eq => ordering == Ordering::Equal,
        BinOp::Ne => ordering != Ordering::Equal,
        BinOp::Lt => ordering == Ordering::Less,
        BinOp::Le => ordering != Ordering::Greater,
        BinOp::Gt => ordering == Ordering::Greater,
        BinOp::Ge => ordering != Ordering::Less,
        _ => unreachable!("`{}` is not a comparison", op.symbol()),
    }
}

/// `value as to`, from an integer of type `from` (ADR 0019): sign- or
/// zero-extended, then truncated.
pub(crate) fn cast_int(value: u64, from: IntTy, to: IntTy) -> u64 {
    let wide = if from.is_signed() {
        sign_extend(value, from) as u64
    } else {
        value
    };
    wide & to.mask()
}

/// Arithmetic as the target does it (ADR 0007): wrapping, shifts masked to
/// the width, division truncating. Its callers rule out zero divisors;
/// `MIN / -1` wraps to `MIN`.
pub(crate) fn arith(op: BinOp, ty: Ty, amount_ty: Ty, l: u64, r: u64) -> u64 {
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
