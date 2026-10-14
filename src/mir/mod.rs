//! Mid-level IR: each function body is a control-flow graph of basic blocks
//! holding simple statements over numbered locals. This is the core of the
//! compiler (ADR 0003): analyses run here and codegen consumes only this.
//! Apart from the shared vocabulary (integer types, operators) it shares no
//! types with the HIR.
//!
//! Every value lives in a local, and every read of a local is explicit
//! (`copy _1`). That is the shape later analyses, such as borrow checking
//! (ADR 0009), need.

mod build;

pub use build::{build_body, build_expr_body};

use std::fmt;
use std::ptr;
use std::sync::OnceLock;

pub use crate::int::IntTy;
pub use crate::layout::Layout;
pub use crate::ops::{BinOp, UnOp};
use crate::span::Span;
use crate::target;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodyId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BasicBlock(pub u32);

impl BasicBlock {
    pub const START: BasicBlock = BasicBlock(0);
}

/// A local slot of a body. `_0` holds the return value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Local(pub u32);

impl Local {
    pub const RETURN: Local = Local(0);
}

#[derive(Debug)]
pub struct Mir {
    /// Every struct type, for dumps; types refer to them directly.
    pub structs: Vec<StructRef>,
    /// Read-only data computed at compile time, which constant locals are
    /// bound to (ADR 0029).
    pub data: Vec<ConstData>,
    pub bodies: Vec<Body>,
    pub entry: BodyId,
    pub panic: BodyId,
}

impl Mir {
    pub fn bodies(&self) -> impl Iterator<Item = (BodyId, &Body)> {
        self.bodies
            .iter()
            .enumerate()
            .map(|(i, b)| (BodyId(i as u32), b))
    }

    pub fn body(&self, id: BodyId) -> &Body {
        &self.bodies[id.0 as usize]
    }
}

#[derive(Clone, Debug)]
pub struct Body {
    /// Symbol name; Erebos uses the C ABI and does not mangle (ADR 0006).
    pub name: String,
    pub span: Span,
    /// Runs only at compile time: a constant's value or a `comptime`
    /// block, never part of the program (ADR 0029).
    pub comptime: bool,
    pub ret: Ty,
    /// Locals `_1..=_n` hold the `n` parameters, in order.
    pub param_count: usize,
    /// `locals[0]` is the return place.
    pub locals: Vec<LocalDecl>,
    /// `blocks[0]` is the start block; nothing jumps back to it.
    pub blocks: Vec<BasicBlockData>,
    /// A runtime support function (ADR 0020). LLVM must not turn its code
    /// into calls to the runtime, which may be the function itself.
    pub runtime: bool,
    /// Defined outside the program (ADR 0026): no locals besides the
    /// parameters, and no blocks.
    pub external: bool,
}

impl Body {
    pub fn blocks(&self) -> impl Iterator<Item = (BasicBlock, &BasicBlockData)> {
        self.blocks
            .iter()
            .enumerate()
            .map(|(i, b)| (BasicBlock(i as u32), b))
    }

    pub fn locals(&self) -> impl Iterator<Item = (Local, &LocalDecl)> {
        self.locals
            .iter()
            .enumerate()
            .map(|(i, l)| (Local(i as u32), l))
    }

    pub fn params(&self) -> &[LocalDecl] {
        &self.locals[1..=self.param_count]
    }

    pub fn local_ty(&self, local: Local) -> Ty {
        self.locals[local.0 as usize].ty
    }

    pub fn place_ty(&self, place: Place) -> Ty {
        place
            .projection
            .iter()
            .fold(self.local_ty(place.local), |ty, elem| match (elem, ty) {
                (PlaceElem::Deref | PlaceElem::Offset(_), Ty::Ptr(ptr)) => *ptr.pointee,
                (PlaceElem::Index(_) | PlaceElem::ConstIndex(_), Ty::Array(elem, _)) => *elem,
                (PlaceElem::Field(index), Ty::Struct(s)) => s.def().fields()[*index as usize].ty,
                (elem, ty) => unreachable!("projection {elem:?} of `{ty}`"),
            })
    }

    pub fn operand_ty(&self, operand: &Operand) -> Ty {
        match operand {
            Operand::Copy(place) => self.place_ty(*place),
            Operand::Const(Const::Int(_, int)) => Ty::Int(*int),
            Operand::Const(Const::Bool(_)) => Ty::Bool,
            Operand::Const(Const::Unit) => Ty::Unit,
        }
    }
}

#[derive(Clone, Debug)]
pub struct LocalDecl {
    pub ty: Ty,
    /// The source name, for user variables; temporaries have none.
    pub name: Option<String>,
    /// Bound to read-only data computed at compile time: the local holds
    /// it from the start, and is never written (ADR 0029).
    pub constant: Option<DataId>,
}

/// Read-only data computed at compile time, indexing [`Mir::data`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DataId(pub u32);

#[derive(Clone, Debug)]
pub struct ConstData {
    /// Laid out as on the target, padding zero (ADR 0027).
    pub bytes: Vec<u8>,
    pub align: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    Unit,
    Never,
    Bool,
    Int(IntTy),
    Ptr(Ptr),
    /// `[T; N]`; the element type is leaked like pointee types.
    Array(&'static Ty, u64),
    Struct(StructRef),
}

/// A struct type with its layout (ADR 0022), shared by every type naming
/// it. Leaked like pointee types, so that types stay `Copy`.
pub struct StructDef {
    pub name: String,
    pub layout: Layout,
    /// `#[packed]`: its fields may be misaligned (ADR 0022).
    pub packed: bool,
    /// Set once every struct exists, since a field may point back to the
    /// struct that contains it.
    fields: OnceLock<Vec<Field>>,
}

#[derive(Debug)]
pub struct Field {
    pub name: String,
    pub ty: Ty,
    /// In bytes, from the start of the struct.
    pub offset: u64,
}

impl StructDef {
    pub fn fields(&self) -> &[Field] {
        self.fields.get().expect("fields are set when MIR is built")
    }

    pub fn set_fields(&self, fields: Vec<Field>) {
        self.fields
            .set(fields)
            .expect("the fields of a struct are set once");
    }
}

/// A struct type. Two are the same type exactly when they are the same
/// definition; comparing contents could recurse through self-pointers.
#[derive(Clone, Copy)]
pub struct StructRef(&'static StructDef);

impl StructRef {
    /// Defines a struct; its fields are set later, with `set_fields`.
    pub fn new(name: String, layout: Layout, packed: bool) -> StructRef {
        StructRef(Box::leak(Box::new(StructDef {
            name,
            layout,
            packed,
            fields: OnceLock::new(),
        })))
    }

    pub fn def(self) -> &'static StructDef {
        self.0
    }
}

impl PartialEq for StructRef {
    fn eq(&self, other: &Self) -> bool {
        ptr::eq(self.0, other.0)
    }
}

impl Eq for StructRef {}

impl fmt::Debug for StructRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.name)
    }
}

impl Ty {
    /// Arrays and structs: values made of other values, which are copied
    /// in memory rather than held in registers.
    pub fn is_aggregate(self) -> bool {
        matches!(self, Ty::Array(..) | Ty::Struct(_))
    }

    /// Size and alignment (ADR 0022), as computed for the type checker.
    pub fn layout(self) -> Layout {
        match self {
            Ty::Unit | Ty::Never => Layout { size: 0, align: 1 },
            Ty::Bool => Layout { size: 1, align: 1 },
            Ty::Int(int) => Layout {
                size: int.bytes(),
                align: int.bytes(),
            },
            Ty::Ptr(_) => Layout {
                size: target::POINTER_BYTES,
                align: target::POINTER_BYTES,
            },
            Ty::Array(elem, len) => {
                let elem = elem.layout();
                Layout {
                    size: elem.size * len,
                    align: elem.align,
                }
            }
            Ty::Struct(s) => s.def().layout,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ptr {
    pub mutable: bool,
    pub volatile: bool,
    pub pointee: &'static Ty,
}

#[derive(Clone, Debug)]
pub struct BasicBlockData {
    pub statements: Vec<Statement>,
    /// Where each statement comes from in the source, for the errors of
    /// compile-time evaluation.
    pub spans: Vec<Span>,
    pub terminator: Terminator,
    pub terminator_span: Span,
}

#[derive(Clone, Debug)]
pub enum Statement {
    Assign(Place, Rvalue),
    /// Calls a function; `dest` receives the result, if it has one.
    Call {
        dest: Option<Place>,
        callee: BodyId,
        args: Vec<Operand>,
    },
}

/// A memory location: a local, followed by projections that step from it
/// to another location. Accesses through a `volatile` pointer are volatile
/// (ADR 0013).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Place {
    pub local: Local,
    /// Leaked like pointee types (see `hir::Ty::ptr`), so places stay `Copy`.
    pub projection: &'static [PlaceElem],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaceElem {
    /// What the pointer held in the place points to.
    Deref,
    /// The element `i` places after the one the pointer held in the place
    /// points to, with `i` the `usize` in the local. The address arithmetic
    /// wraps and is never checked (ADR 0021).
    Offset(Local),
    /// Element `i` of the array in the place, with `i` the `usize` in the
    /// local. MIR building has already checked the bounds.
    Index(Local),
    /// Element `i` of the array in the place, for an `i` known in bounds.
    ConstIndex(u64),
    /// Field `i` of the struct in the place, in declaration order.
    Field(u32),
}

impl Place {
    pub fn local(local: Local) -> Place {
        Place {
            local,
            projection: &[],
        }
    }

    /// This place followed by one more projection.
    pub fn project(self, elem: PlaceElem) -> Place {
        let mut projection = self.projection.to_vec();
        projection.push(elem);
        Place {
            local: self.local,
            projection: projection.leak(),
        }
    }

    /// Whether the place reaches memory through a pointer.
    pub fn is_indirect(self) -> bool {
        self.projection
            .iter()
            .any(|elem| matches!(elem, PlaceElem::Deref | PlaceElem::Offset(_)))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operand {
    Copy(Place),
    Const(Const),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Const {
    /// The bits of an integer of the given type.
    Int(u64, IntTy),
    Bool(bool),
    Unit,
}

#[derive(Clone, Debug)]
pub enum Rvalue {
    Use(Operand),
    Unary(UnOp, Operand),
    /// `Div` and `Rem` here always have a valid divisor: the builder emits
    /// the zero and `MIN / -1` checks of ADR 0007 before them.
    Binary(BinOp, Operand, Operand),
    Cast(Operand, Ty),
    /// `&raw const place` or `&raw mut place`.
    AddressOf {
        mutable: bool,
        place: Place,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Terminator {
    Goto(BasicBlock),
    /// Jumps to `then` if `cond` (a `bool`) is true, else to `else_`.
    If {
        cond: Operand,
        then: BasicBlock,
        else_: BasicBlock,
    },
    Return,
    /// Control never reaches the end of this block.
    Unreachable,
    /// A check failed: calls the panic function (ADR 0014), which never
    /// returns.
    Panic(PanicCause),
}

/// Why the compiler's own checks call the panic function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanicCause {
    /// A zero divisor (ADR 0007).
    DivisionByZero,
    /// An array index not below the length (ADR 0021).
    IndexOutOfBounds,
    /// `assert` with a false condition (ADR 0029).
    AssertionFailed,
}

impl PanicCause {
    pub fn describe(self) -> &'static str {
        match self {
            PanicCause::DivisionByZero => "division by zero",
            PanicCause::IndexOutOfBounds => "index out of bounds",
            PanicCause::AssertionFailed => "assertion failed",
        }
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Unit => f.write_str("()"),
            Ty::Never => f.write_str("!"),
            Ty::Bool => f.write_str("bool"),
            Ty::Int(int) => write!(f, "{int}"),
            Ty::Ptr(ptr) => {
                let mutability = if ptr.mutable { "mut" } else { "const" };
                let volatile = if ptr.volatile { " volatile" } else { "" };
                write!(f, "*{mutability}{volatile} {}", ptr.pointee)
            }
            Ty::Array(elem, len) => write!(f, "[{elem}; {len}]"),
            Ty::Struct(s) => f.write_str(&s.def().name),
        }
    }
}

impl fmt::Display for BasicBlock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bb{}", self.0)
    }
}

impl fmt::Display for Local {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "_{}", self.0)
    }
}

impl fmt::Display for Place {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut text = self.local.to_string();
        for elem in self.projection {
            text = match elem {
                PlaceElem::Deref => format!("(*{text})"),
                PlaceElem::Offset(index) => format!("(*({text} + {index}))"),
                PlaceElem::Index(index) => format!("{text}[{index}]"),
                PlaceElem::ConstIndex(index) => format!("{text}[{index}]"),
                PlaceElem::Field(index) => format!("{text}.{index}"),
            };
        }
        f.write_str(&text)
    }
}

impl fmt::Display for Operand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Operand::Copy(place) => write!(f, "copy {place}"),
            Operand::Const(Const::Int(bits, int)) => {
                write!(f, "const {}_{int}", int.display_bits(*bits))
            }
            Operand::Const(Const::Bool(value)) => write!(f, "const {value}"),
            Operand::Const(Const::Unit) => f.write_str("const ()"),
        }
    }
}

impl fmt::Display for Rvalue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Rvalue::Use(operand) => write!(f, "{operand}"),
            Rvalue::Unary(op, operand) => write!(f, "{op:?}({operand})"),
            Rvalue::Binary(op, lhs, rhs) => write!(f, "{op:?}({lhs}, {rhs})"),
            Rvalue::Cast(operand, ty) => write!(f, "{operand} as {ty}"),
            Rvalue::AddressOf { mutable, place } => {
                let mutability = if *mutable { "mut" } else { "const" };
                write!(f, "&raw {mutability} {place}")
            }
        }
    }
}

impl Mir {
    fn fmt_statement(&self, statement: &Statement, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match statement {
            Statement::Assign(place, rvalue) => write!(f, "{place} = {rvalue}"),
            Statement::Call { dest, callee, args } => {
                if let Some(dest) = dest {
                    write!(f, "{dest} = ")?;
                }
                write!(f, "call {}(", self.body(*callee).name)?;
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{arg}")?;
                }
                f.write_str(")")
            }
        }
    }
}

impl fmt::Display for Terminator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Terminator::Goto(target) => write!(f, "goto -> {target}"),
            Terminator::If { cond, then, else_ } => {
                write!(f, "if {cond} -> [true: {then}, false: {else_}]")
            }
            Terminator::Return => f.write_str("return"),
            Terminator::Unreachable => f.write_str("unreachable"),
            Terminator::Panic(cause) => write!(f, "panic({})", cause.describe()),
        }
    }
}

impl fmt::Display for Mir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for s in &self.structs {
            let def = s.def();
            let fields: Vec<String> = def
                .fields()
                .iter()
                .enumerate()
                .map(|(i, field)| format!("{i} {}: {} at {}", field.name, field.ty, field.offset))
                .collect();
            let fields = if fields.is_empty() {
                "{}".to_owned()
            } else {
                format!("{{ {} }}", fields.join(", "))
            };
            let packed = if def.packed { "#[packed] " } else { "" };
            writeln!(
                f,
                "{packed}struct {} {fields} // size {}, align {}",
                def.name, def.layout.size, def.layout.align
            )?;
        }
        if !self.structs.is_empty() {
            writeln!(f)?;
        }
        for (i, (id, body)) in self.bodies().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            let kind = if id == self.entry {
                "entry "
            } else if id == self.panic {
                "panic "
            } else {
                ""
            };
            let external = if body.external { "extern " } else { "" };
            let comptime = if body.comptime { "comptime " } else { "" };
            write!(f, "{external}{comptime}{kind}fn {}(", body.name)?;
            for (i, param) in body.params().iter().enumerate() {
                if i > 0 {
                    f.write_str(", ")?;
                }
                write!(f, "_{}: {}", i + 1, param.ty)?;
            }
            if body.external {
                writeln!(f, ") -> {};", body.ret)?;
                continue;
            }
            writeln!(f, ") -> {} {{", body.ret)?;
            for (local, decl) in body.locals() {
                write!(f, "    let {local}: {}", decl.ty)?;
                if let Some(data) = decl.constant {
                    write!(f, " = data{}", data.0)?;
                }
                f.write_str(";")?;
                match &decl.name {
                    Some(name) => writeln!(f, " // {name}")?,
                    None => writeln!(f)?,
                }
            }
            for (bb, data) in body.blocks() {
                writeln!(f)?;
                writeln!(f, "    {bb}: {{")?;
                for statement in &data.statements {
                    f.write_str("        ")?;
                    self.fmt_statement(statement, f)?;
                    writeln!(f, ";")?;
                }
                writeln!(f, "        {};", data.terminator)?;
                writeln!(f, "    }}")?;
            }
            writeln!(f, "}}")?;
        }
        Ok(())
    }
}
