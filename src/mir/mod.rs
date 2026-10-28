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
pub use crate::layout::{Layout, PeripheralLayout};
pub use crate::memory::{MemoryMap, Region};
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
    /// Every bit-field struct, for dumps: MIR sees only its integer type,
    /// and its fields as shifts and masks of it (ADR 0059).
    pub bit_structs: Vec<BitStruct>,
    /// Every enum, for dumps: MIR sees only the integer type of an enum
    /// without payloads (ADR 0035), and an enum with payloads is a type of
    /// its own (ADR 0039).
    pub enums: Vec<Enum>,
    /// Every peripheral type, for dumps: types refer to them directly, and
    /// nothing is emitted for them (ADR 0058).
    pub peripheral_types: Vec<PeripheralTypeRef>,
    /// Every peripheral, for dumps: operands refer to them directly, and
    /// nothing is emitted for them but their addresses (ADR 0058).
    pub peripherals: Vec<PeripheralRef>,
    /// Read-only data computed at compile time, which constant locals are
    /// bound to (ADR 0029).
    pub data: Vec<ConstData>,
    /// Every static, in source order (ADR 0030), then every byte string
    /// (ADR 0031).
    pub statics: Vec<Static>,
    pub bodies: Vec<Body>,
    pub entry: BodyId,
    pub panic: BodyId,
    /// The program's memory map, of which the linker script is made
    /// (ADR 0057); `None` without a `memory` block.
    pub memory: Option<MemoryMap>,
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

    /// Leaves out what `left_out` marks, as dumps leave `core` out
    /// (ADR 0052). A body that a body kept calls stays as a declaration, as
    /// an `extern fn` would be, so that the call still names it; the other
    /// bodies marked go. A static or byte string marked stays if what is
    /// kept points to it. Read-only data that only the bodies left out use
    /// goes with them. What is kept is renumbered.
    pub fn leave_out(&mut self, left_out: &LeftOut) {
        let mut called = vec![false; self.bodies.len()];
        for (_, body) in self
            .bodies()
            .filter(|(id, _)| !left_out.bodies[id.0 as usize])
        {
            for block in &body.blocks {
                for statement in &block.statements {
                    if let Statement::Call { callee, .. } = statement {
                        called[callee.0 as usize] = true;
                    }
                }
            }
        }

        // The data and statics that the bodies kept whole use.
        let mut data_kept = vec![false; self.data.len()];
        let mut data_dropped = vec![false; self.data.len()];
        let mut statics_used = vec![false; self.statics.len()];
        for (i, body) in self.bodies.iter_mut().enumerate() {
            let used = if left_out.bodies[i] {
                &mut data_dropped
            } else {
                &mut data_kept
            };
            for local in &body.locals {
                if let Some(data) = local.constant {
                    used[data.0 as usize] = true;
                }
            }
            body.for_each_site(&mut |site| {
                if let PanicInfoAt::Data(data) = site.info {
                    used[data.0 as usize] = true;
                }
            });
            if !left_out.bodies[i] {
                body.for_each_site(&mut |site| statics_used[site.file.def().id.0 as usize] = true);
                body.for_each_operand(&mut |operand| {
                    if let Operand::Const(Const::Static(s)) = operand {
                        statics_used[s.def().id.0 as usize] = true;
                    }
                });
            }
        }
        // Data no body uses, if any, stays as it was.
        let data_keep: Vec<bool> = (data_kept.iter().zip(&data_dropped))
            .map(|(&kept, &dropped)| kept || !dropped)
            .collect();
        for (data, _) in self.data.iter().zip(&data_keep).filter(|(_, keep)| **keep) {
            for relocation in &data.relocations {
                statics_used[relocation.target.0 as usize] = true;
            }
        }
        // A static kept may point to one marked, which is then kept too.
        let mut statics_keep: Vec<bool> = (left_out.statics.iter().zip(&statics_used))
            .map(|(&marked, &used)| !marked || used)
            .collect();
        loop {
            let mut changed = false;
            for (i, s) in self.statics.iter().enumerate() {
                if !statics_keep[i] {
                    continue;
                }
                for relocation in &s.relocations {
                    let target = relocation.target.0 as usize;
                    changed |= !std::mem::replace(&mut statics_keep[target], true);
                }
            }
            if !changed {
                break;
            }
        }

        let mut ids = Vec::with_capacity(self.bodies.len());
        let mut kept = Vec::new();
        for (i, mut body) in std::mem::take(&mut self.bodies).into_iter().enumerate() {
            if left_out.bodies[i] && !called[i] {
                ids.push(None);
                continue;
            }
            if left_out.bodies[i] {
                body.external = true;
                body.runtime = false;
                body.blocks.clear();
                body.locals.truncate(1 + body.param_count);
            }
            ids.push(Some(BodyId(kept.len() as u32)));
            kept.push(body);
        }
        let new_id = |id: BodyId| ids[id.0 as usize].expect("a body kept calls only bodies kept");
        for body in &mut kept {
            for block in &mut body.blocks {
                for statement in &mut block.statements {
                    if let Statement::Call { callee, .. } = statement {
                        *callee = new_id(*callee);
                    }
                }
            }
        }
        self.entry = new_id(self.entry);
        self.panic = new_id(self.panic);
        self.bodies = kept;

        self.structs = unmarked(std::mem::take(&mut self.structs), &left_out.structs);
        self.bit_structs = unmarked(std::mem::take(&mut self.bit_structs), &left_out.bit_structs);
        self.enums = unmarked(std::mem::take(&mut self.enums), &left_out.enums);
        self.peripheral_types = unmarked(
            std::mem::take(&mut self.peripheral_types),
            &left_out.peripheral_types,
        );
        self.peripherals = unmarked(std::mem::take(&mut self.peripherals), &left_out.peripherals);

        // Statics and data, renumbered.
        let mut statics = Vec::new();
        let mut new_statics = Vec::with_capacity(self.statics.len());
        for (s, keep) in std::mem::take(&mut self.statics)
            .into_iter()
            .zip(statics_keep)
        {
            if keep {
                let def = s.def.renumbered(StaticId(statics.len() as u32));
                new_statics.push(Some(def));
                statics.push(Static { def, ..s });
            } else {
                new_statics.push(None);
            }
        }
        let new_static = |s: StaticRef| {
            new_statics[s.def().id.0 as usize].expect("what is kept points only to statics kept")
        };
        let mut data_ids = Vec::with_capacity(self.data.len());
        let mut data = Vec::new();
        for (d, keep) in std::mem::take(&mut self.data).into_iter().zip(data_keep) {
            data_ids.push(keep.then_some(DataId(data.len() as u32)));
            if keep {
                data.push(d);
            }
        }
        let relocations = (statics.iter_mut().map(|s| &mut s.relocations))
            .chain(data.iter_mut().map(|d| &mut d.relocations));
        for relocations in relocations {
            for relocation in relocations {
                let target = new_statics[relocation.target.0 as usize]
                    .expect("what is kept points only to statics kept");
                relocation.target = target.def().id;
            }
        }
        for body in &mut self.bodies {
            for local in &mut body.locals {
                if let Some(id) = &mut local.constant {
                    *id = data_ids[id.0 as usize].expect("a body kept uses only data kept");
                }
            }
            body.for_each_site(&mut |site| {
                if let PanicInfoAt::Data(data) = &mut site.info {
                    *data = data_ids[data.0 as usize].expect("a body kept uses only data kept");
                }
                site.file = new_static(site.file);
            });
            body.for_each_operand(&mut |operand| {
                if let Operand::Const(Const::Static(s)) = operand {
                    *s = new_static(*s);
                }
            });
        }
        self.statics = statics;
        self.data = data;
    }
}

/// The items of `list` that `marks` does not mark; those past its end are
/// not marked.
fn unmarked<T>(list: Vec<T>, marks: &[bool]) -> Vec<T> {
    let marks = marks.iter().copied().chain(std::iter::repeat(false));
    (list.into_iter().zip(marks))
        .filter(|&(_, marked)| !marked)
        .map(|(item, _)| item)
        .collect()
}

/// What dumps leave out of the MIR: the parts of `core` (ADR 0052), each
/// list a mask over the MIR's list of its kind.
pub struct LeftOut {
    /// Over the bodies.
    pub bodies: Vec<bool>,
    /// Over the structs.
    pub structs: Vec<bool>,
    /// Over the bit-field structs.
    pub bit_structs: Vec<bool>,
    /// Over the declared enums, which `Mir::enums` lists first; the
    /// `Option(T)` and `Result(T, E)` after them are kept.
    pub enums: Vec<bool>,
    /// Over the peripheral types.
    pub peripheral_types: Vec<bool>,
    /// Over the peripherals.
    pub peripherals: Vec<bool>,
    /// Over the statics, then the byte strings.
    pub statics: Vec<bool>,
}

#[derive(Clone, Debug)]
pub struct Body {
    /// Its symbol (ADR 0051): the function's path joined by dots, as in
    /// `uart.init`, or its own name for an item of the root module, an
    /// `#[export]` function or an `extern fn`; Erebos has no other mangling
    /// and uses the C ABI (ADR 0006). For a body run at compile time, which
    /// has no symbol, its path as diagnostics show it.
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
    /// A function of `core`, which holds the runtime support functions
    /// (ADR 0020, 0052). LLVM must not turn its code into calls to the
    /// runtime, which may be the function itself.
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

    /// Calls `f` on every operand of its statements and terminators.
    fn for_each_operand(&mut self, f: &mut impl FnMut(&mut Operand)) {
        for block in &mut self.blocks {
            for statement in &mut block.statements {
                match statement {
                    Statement::Assign(_, rvalue) => match rvalue {
                        Rvalue::Use(operand)
                        | Rvalue::Unary(_, operand)
                        | Rvalue::Cast(operand, _) => f(operand),
                        Rvalue::Binary(_, lhs, rhs) => {
                            f(lhs);
                            f(rhs);
                        }
                        Rvalue::Slice { ptr, len } => {
                            f(ptr);
                            f(len);
                        }
                        Rvalue::AddressOf { .. }
                        | Rvalue::SliceAddress(_)
                        | Rvalue::SliceLen(_) => {}
                    },
                    Statement::Call { args, .. } => args.iter_mut().for_each(&mut *f),
                }
            }
            match &mut block.terminator {
                Terminator::If { cond: operand, .. }
                | Terminator::Switch { discr: operand, .. }
                | Terminator::Panic(PanicCause::AssertionFailed(Some((operand, _))), _) => {
                    f(operand)
                }
                Terminator::Goto(_)
                | Terminator::Return
                | Terminator::Unreachable
                | Terminator::Panic(..) => {}
            }
        }
    }

    /// Calls `f` on where each check is, if the panic function learns it
    /// (ADR 0053).
    fn for_each_site(&mut self, f: &mut impl FnMut(&mut PanicSite)) {
        for block in &mut self.blocks {
            if let Terminator::Panic(_, Some(site)) = &mut block.terminator {
                f(site);
            }
        }
    }

    pub fn place_ty(&self, place: Place) -> Ty {
        place
            .projection
            .iter()
            .fold(self.local_ty(place.local), |ty, elem| match (elem, ty) {
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
            })
    }

    pub fn operand_ty(&self, operand: &Operand) -> Ty {
        match operand {
            Operand::Copy(place) => self.place_ty(*place),
            Operand::Const(Const::Int(_, int)) => Ty::Int(*int),
            Operand::Const(Const::Bool(_)) => Ty::Bool,
            Operand::Const(Const::Unit) => Ty::Unit,
            Operand::Const(Const::Static(s)) => s.def().address_ty(),
            Operand::Const(Const::Peripheral(p)) => p.def().address_ty(),
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
    /// The pointers to statics among the bytes, which hold zero there.
    pub relocations: Vec<Relocation>,
}

/// A pointer into a static inside a value computed at compile time: its
/// address only exists after linking, so the linker fills it in (ADR 0030).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Relocation {
    /// Where the pointer is in the value, in bytes.
    pub offset: u32,
    /// The static it points into.
    pub target: StaticId,
    /// Its offset from the start of the static, in bytes: between 0 and
    /// the static's size, inclusive.
    pub addend: u32,
}

/// A static, indexing [`Mir::statics`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StaticId(pub u32);

/// What MIR knows of a static (ADR 0030); its memory is a [`Static`].
#[derive(Clone)]
pub struct StaticDef {
    pub id: StaticId,
    /// Its symbol (ADR 0030, 0051), as a function's; for a byte string,
    /// which has none, its bytes as written.
    pub name: String,
    pub ty: Ty,
    /// `static mut`: written by the program.
    pub mutable: bool,
    /// An `extern static`, defined outside the program: it has no bytes.
    pub external: bool,
    /// For a byte string (ADR 0031), an unnamed read-only static of
    /// alignment 1: its bytes.
    pub byte_string: Option<Vec<u8>>,
    /// `*mut T` for a `static mut`, else `*const T`.
    address_ty: Ty,
}

impl StaticDef {
    /// The type of its address.
    pub fn address_ty(&self) -> Ty {
        self.address_ty
    }
}

/// A static, as operands name it. Leaked like struct definitions, so that
/// constants stay `Copy` and dumps can name it.
#[derive(Clone, Copy)]
pub struct StaticRef(&'static StaticDef);

impl StaticRef {
    pub fn new(id: StaticId, name: String, ty: Ty, mutable: bool, external: bool) -> StaticRef {
        let address_ty = Ty::Ptr(Ptr {
            mutable,
            volatile: false,
            pointee: Box::leak(Box::new(ty)),
        });
        StaticRef(Box::leak(Box::new(StaticDef {
            id,
            name,
            ty,
            mutable,
            external,
            byte_string: None,
            address_ty,
        })))
    }

    /// A byte string (ADR 0031): read-only, of type `[u8; N]`.
    pub fn byte_string(id: StaticId, bytes: Vec<u8>) -> StaticRef {
        let ty = Ty::Array(Box::leak(Box::new(Ty::Int(IntTy::U8))), bytes.len() as u64);
        let address_ty = Ty::Ptr(Ptr {
            mutable: false,
            volatile: false,
            pointee: Box::leak(Box::new(ty)),
        });
        StaticRef(Box::leak(Box::new(StaticDef {
            id,
            // As written, with escapes for bytes not printable (ADR 0016).
            name: format!("b\"{}\"", bytes.escape_ascii()),
            ty,
            mutable: false,
            external: false,
            byte_string: Some(bytes),
            address_ty,
        })))
    }

    pub fn def(self) -> &'static StaticDef {
        self.0
    }

    /// The same static, numbered `id`: itself if it is already.
    fn renumbered(self, id: StaticId) -> StaticRef {
        if self.0.id == id {
            return self;
        }
        StaticRef(Box::leak(Box::new(StaticDef {
            id,
            ..self.0.clone()
        })))
    }
}

impl PartialEq for StaticRef {
    fn eq(&self, other: &Self) -> bool {
        self.0.id == other.0.id
    }
}

impl Eq for StaticRef {}

impl fmt::Debug for StaticRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.name)
    }
}

/// The memory of a static: its initial value, laid out as on the target
/// with padding zero (ADR 0027), and its alignment.
#[derive(Clone, Debug)]
pub struct Static {
    pub def: StaticRef,
    pub bytes: Vec<u8>,
    pub align: u64,
    /// The pointers to statics among the bytes, which hold zero there.
    pub relocations: Vec<Relocation>,
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
    /// `[T]`, unsized: only the pointee of a slice (ADR 0031). Leaked
    /// likewise.
    Slice(&'static Ty),
    Struct(StructRef),
    /// An enum with payloads (ADR 0039).
    Enum(EnumRef),
    /// A peripheral type (ADR 0058): only the pointee of a pointer, through
    /// which its registers are reached. No value has it.
    Peripheral(PeripheralTypeRef),
}

/// An enum, listed in dumps (ADR 0035): its integer type, and the value of
/// each variant as the bits of that type; with payloads, also its type
/// (ADR 0039).
#[derive(Clone, Debug)]
pub struct Enum {
    pub name: String,
    pub int: IntTy,
    pub variants: Vec<(String, u64)>,
    /// The MIR type of an enum with payloads.
    pub def: Option<EnumRef>,
}

/// A bit-field struct, listed in dumps (ADR 0059): its integer type, which
/// is all MIR sees of it, and the bits of each field.
#[derive(Clone, Debug)]
pub struct BitStruct {
    pub name: String,
    pub int: IntTy,
    pub fields: Vec<BitField>,
}

/// A field of a bit-field struct: its MIR type, and its lowest and highest
/// bit.
#[derive(Clone, Debug)]
pub struct BitField {
    pub name: String,
    pub ty: Ty,
    pub low: u32,
    pub high: u32,
    /// `read` or `write` if the field declares a mode, for the dump only
    /// (ADR 0065): MIR builds every access as to any other field.
    pub access: Option<&'static str>,
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

/// An enum with payloads, numbered as type checking numbers enums: the
/// declared ones, then each `Option(T)` and `Result(T, E)` as it is made
/// (ADR 0045).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EnumId(pub u32);

/// An enum with payloads (ADR 0039): a tag of its integer type at offset
/// 0, and the payload values of the variant the tag names, each variant
/// laid out as a struct. Leaked like struct definitions. The values of its
/// variants are not part of the type; building a variant writes its value
/// as a constant.
pub struct EnumDef {
    pub id: EnumId,
    pub name: String,
    /// The type of the tag.
    pub int: IntTy,
    pub layout: Layout,
    /// Set once the type exists, since a payload may point back to the
    /// enum that contains it.
    variants: OnceLock<Vec<Variant>>,
}

/// A variant of an enum with payloads: its payload values, each at its
/// offset from the start of the enum, named by index.
#[derive(Debug)]
pub struct Variant {
    pub name: String,
    pub payload: Vec<Field>,
}

impl EnumDef {
    pub fn variants(&self) -> &[Variant] {
        self.variants
            .get()
            .expect("variants are set when MIR is built")
    }

    pub fn set_variants(&self, variants: Vec<Variant>) {
        self.variants
            .set(variants)
            .expect("the variants of an enum are set once");
    }

    /// Payload value `index` of variant `variant`.
    pub fn payload(&self, variant: u32, index: u32) -> &Field {
        &self.variants()[variant as usize].payload[index as usize]
    }
}

/// An enum with payloads as a type; the same type exactly when the same
/// definition, as structs are.
#[derive(Clone, Copy)]
pub struct EnumRef(&'static EnumDef);

impl EnumRef {
    /// Defines an enum; its variants are set later, with `set_variants`.
    pub fn new(id: EnumId, name: String, int: IntTy, layout: Layout) -> EnumRef {
        EnumRef(Box::leak(Box::new(EnumDef {
            id,
            name,
            int,
            layout,
            variants: OnceLock::new(),
        })))
    }

    pub fn def(self) -> &'static EnumDef {
        self.0
    }
}

impl PartialEq for EnumRef {
    fn eq(&self, other: &Self) -> bool {
        ptr::eq(self.0, other.0)
    }
}

impl Eq for EnumRef {}

impl fmt::Debug for EnumRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.name)
    }
}

/// A peripheral type (ADR 0058): its registers, each reached by index
/// through a pointer to it, and its layout. Leaked like struct definitions.
/// Every access to a register is volatile.
pub struct PeripheralTypeDef {
    /// Its path, as the HIR shows it (ADR 0049).
    pub name: String,
    /// Its entries in declaration order. Their types are `u8`, `u16` or
    /// `u32` for registers, and peripheral types for clusters, in a program
    /// without errors.
    pub registers: Vec<Register>,
    /// Set once computed, which may be after the type is made: a struct's
    /// field or a static may need the type before anything needs its
    /// layout. Every body reaching a register is built after it is set.
    layout: OnceLock<PeripheralLayout>,
}

/// An entry of a peripheral type, a register or a cluster: its offset is in
/// the type's layout, and so are the length and the step of an entry that is
/// an array.
#[derive(Debug)]
pub struct Register {
    pub name: String,
    /// The type of the entry's element (ADR 0064): of the one register or
    /// cluster, or of each of an array's; a cluster's is its peripheral type.
    pub ty: Ty,
    /// Its alternate group (ADR 0061), for the dump only: accesses to it
    /// are built as to any other register.
    pub group: Option<String>,
}

impl PeripheralTypeDef {
    /// Its layout, if computed already.
    pub fn layout(&self) -> Option<&PeripheralLayout> {
        self.layout.get()
    }

    /// The offset of register `index`, from the start of the peripheral.
    pub fn offset(&self, index: u32) -> u64 {
        self.laid_out().offsets[index as usize]
    }

    /// The number of elements of entry `index`, which is an array
    /// (ADR 0064); 0 for an entry that is no array.
    pub fn length(&self, index: u32) -> u64 {
        self.laid_out().arrays[index as usize].map_or(0, |(len, _)| len)
    }

    /// The distance between the elements of entry `index`, which is an
    /// array (ADR 0064): its `step`, or the width of a register. 0 for an
    /// entry that is no array.
    pub fn step(&self, index: u32) -> u64 {
        self.laid_out().arrays[index as usize].map_or(0, |(_, step)| step)
    }

    fn laid_out(&self) -> &PeripheralLayout {
        self.layout().expect("the layout is set when MIR is used")
    }

    pub fn set_layout(&self, layout: PeripheralLayout) {
        self.layout
            .set(layout)
            .expect("the layout of a peripheral type is set once");
    }
}

/// A peripheral type as a type; the same type exactly when the same
/// definition, as structs are.
#[derive(Clone, Copy)]
pub struct PeripheralTypeRef(&'static PeripheralTypeDef);

impl PeripheralTypeRef {
    /// Defines a peripheral type; its layout is set later, with
    /// `set_layout`.
    pub fn new(name: String, registers: Vec<Register>) -> PeripheralTypeRef {
        PeripheralTypeRef(Box::leak(Box::new(PeripheralTypeDef {
            name,
            registers,
            layout: OnceLock::new(),
        })))
    }

    pub fn def(self) -> &'static PeripheralTypeDef {
        self.0
    }
}

impl PartialEq for PeripheralTypeRef {
    fn eq(&self, other: &Self) -> bool {
        ptr::eq(self.0, other.0)
    }
}

impl Eq for PeripheralTypeRef {}

impl fmt::Debug for PeripheralTypeRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.name)
    }
}

/// A peripheral (ADR 0058): a device of a peripheral type at an address,
/// with no symbol and no storage. Operands hold its address.
pub struct PeripheralDef {
    /// Its path, as the HIR shows it (ADR 0049).
    pub name: String,
    pub address: u64,
    pub ty: PeripheralTypeRef,
    /// `*mut T`, with `T` its type.
    address_ty: Ty,
}

impl PeripheralDef {
    /// The type of its address: a peripheral is not `mut`, but what may be
    /// written is its registers' business (ADR 0058).
    pub fn address_ty(&self) -> Ty {
        self.address_ty
    }
}

/// A peripheral, as operands name it. Leaked like static definitions.
#[derive(Clone, Copy)]
pub struct PeripheralRef(&'static PeripheralDef);

impl PeripheralRef {
    pub fn new(name: String, address: u64, ty: PeripheralTypeRef) -> PeripheralRef {
        let address_ty = Ty::Ptr(Ptr {
            mutable: true,
            volatile: false,
            pointee: Box::leak(Box::new(Ty::Peripheral(ty))),
        });
        PeripheralRef(Box::leak(Box::new(PeripheralDef {
            name,
            address,
            ty,
            address_ty,
        })))
    }

    pub fn def(self) -> &'static PeripheralDef {
        self.0
    }
}

impl PartialEq for PeripheralRef {
    fn eq(&self, other: &Self) -> bool {
        ptr::eq(self.0, other.0)
    }
}

impl Eq for PeripheralRef {}

impl fmt::Debug for PeripheralRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.name)
    }
}

impl Ty {
    /// Arrays, structs, slices and enums with payloads: values made of
    /// other values, which are copied in memory rather than held in
    /// registers. A slice is an address and a length (ADR 0031); an enum
    /// with payloads, a tag and the values of a variant (ADR 0039).
    pub fn is_aggregate(self) -> bool {
        matches!(self, Ty::Array(..) | Ty::Struct(_) | Ty::Enum(_)) || self.slice_elem().is_some()
    }

    /// The element type of a slice, `*const [T]` or `*mut [T]`.
    pub fn slice_elem(self) -> Option<Ty> {
        match self {
            Ty::Ptr(Ptr {
                pointee: Ty::Slice(elem),
                ..
            }) => Some(**elem),
            _ => None,
        }
    }

    /// The thin pointer to the first element of a slice of this type, of
    /// its mutability and volatility: the type of its address part.
    pub fn slice_address_ty(self) -> Option<Ty> {
        match self {
            Ty::Ptr(
                ptr @ Ptr {
                    pointee: Ty::Slice(elem),
                    ..
                },
            ) => Some(Ty::Ptr(Ptr {
                pointee: elem,
                ..ptr
            })),
            _ => None,
        }
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
            Ty::Ptr(_) if self.slice_elem().is_some() => Layout {
                size: 2 * target::POINTER_BYTES,
                align: target::POINTER_BYTES,
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
            Ty::Slice(_) => unreachable!("`[T]` is unsized, and no place has it (ADR 0031)"),
            Ty::Struct(s) => s.def().layout,
            Ty::Enum(e) => e.def().layout,
            Ty::Peripheral(_) => {
                unreachable!("a peripheral type has no size, and no value has it (ADR 0058)")
            }
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
/// (ADR 0013), and so are accesses to a register (ADR 0058).
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
    /// wraps and is never checked (ADR 0021); for an element of a slice,
    /// MIR building has already checked `i` against its length (ADR 0031).
    Offset(Local),
    /// Element `i` of the array in the place, with `i` the `usize` in the
    /// local. MIR building has already checked the bounds.
    Index(Local),
    /// Element `i` of the array in the place, for an `i` known in bounds.
    ConstIndex(u64),
    /// Field `i` of the struct in the place, or register `i` of the
    /// peripheral type, in declaration order.
    Field(u32),
    /// Element `i` of entry `entry` of the peripheral type in the place, an
    /// entry that is a register array or a cluster array (ADR 0064), with
    /// `i` the `usize` in the local: at the entry's offset plus `i` times its
    /// step, which is not its element's size. MIR building has already
    /// checked the bounds. Its type is the element's.
    Element(u32, Local),
    /// Element `i` of entry `entry`, for an `i` known in bounds.
    ConstElement(u32, u64),
    /// The tag of the enum with payloads in the place, an integer at
    /// offset 0 (ADR 0039).
    Tag,
    /// Payload value `index` of variant `variant` of the enum with payloads
    /// in the place, by index, where that variant lays it out (ADR 0039).
    Payload { variant: u32, index: u32 },
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
    /// The address of a static (ADR 0030), through which it is read and
    /// written.
    Static(StaticRef),
    /// The address of a peripheral, a pointer made from an integer, through
    /// which its registers are reached (ADR 0058).
    Peripheral(PeripheralRef),
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
    /// A slice of `len` elements, a `usize`, from where the thin pointer
    /// `ptr` points (ADR 0031). Assigned to a slice place, it writes both
    /// parts.
    Slice {
        ptr: Operand,
        len: Operand,
    },
    /// The address part of the slice in the place: a thin pointer to its
    /// first element.
    SliceAddress(Place),
    /// The length part of the slice in the place, a `usize`. It reads the
    /// slice, never what it points to.
    SliceLen(Place),
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
    /// Jumps to the block of the case whose value equals `discr`, an
    /// integer or `bool` (an enum is its integer), or else to `otherwise`:
    /// a `match` (ADR 0037).
    /// Each value is the bits of `discr`'s type, masked to its width as
    /// `Const::Int` holds them (`bool` is 0 or 1), and appears once. Leaked
    /// like projections, so that terminators stay `Copy`.
    Switch {
        discr: Operand,
        cases: &'static [(u64, BasicBlock)],
        otherwise: BasicBlock,
    },
    Return,
    /// Control never reaches the end of this block.
    Unreachable,
    /// A check failed: calls the panic function (ADR 0014), which never
    /// returns. When the panic function takes a `*const core::PanicInfo`,
    /// with where the check is, whose information the call passes
    /// (ADR 0053).
    Panic(PanicCause, Option<PanicSite>),
}

/// Where a check is, as the panic function learns it (ADR 0053).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanicSite {
    /// The check's `core::PanicInfo`, whose address the call passes.
    pub info: PanicInfoAt,
    /// What the information holds, for dumps: the byte string naming the
    /// file, which its `file` points to, its `line` and its `column`.
    pub file: StaticRef,
    pub line: u32,
    pub column: u32,
}

/// Where the `core::PanicInfo` of a check is (ADR 0053).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanicInfoAt {
    /// In read-only data of its own, when every field is known at compile
    /// time.
    Data(DataId),
    /// In a local of the failing function, for an `assert` with a message:
    /// the block that ends in the call fills it, with the message computed
    /// there.
    Local(Local),
}

/// Why the compiler's own checks call the panic function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanicCause {
    /// A zero divisor (ADR 0007).
    DivisionByZero,
    /// An array index not below the length (ADR 0021).
    IndexOutOfBounds,
    /// `assert` with a false condition (ADR 0029), with its message if it
    /// has one: a `*const [u8]` computed on the failing path only, and the
    /// span of the expression that gave it (ADR 0031).
    AssertionFailed(Option<(Operand, Span)>),
    /// A `match` on an enum without a catch-all arm, on a value that is
    /// no variant (ADR 0036).
    NoVariant,
    /// A value written to a field of a bit-field struct that does not fit
    /// in its bits (ADR 0059).
    FieldOutOfRange,
}

impl PanicCause {
    pub fn describe(self) -> &'static str {
        match self {
            PanicCause::DivisionByZero => "division by zero",
            PanicCause::IndexOutOfBounds => "index out of bounds",
            PanicCause::AssertionFailed(_) => "assertion failed",
            PanicCause::NoVariant => "enum value is no variant",
            PanicCause::FieldOutOfRange => "value does not fit its field",
        }
    }

    /// The variant of `core::PanicReason` that names it (ADR 0053).
    pub fn reason(self) -> &'static str {
        match self {
            PanicCause::DivisionByZero => "DivisionByZero",
            PanicCause::IndexOutOfBounds => "IndexOutOfBounds",
            PanicCause::AssertionFailed(_) => "AssertionFailed",
            PanicCause::NoVariant => "NoVariant",
            PanicCause::FieldOutOfRange => "FieldOutOfRange",
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
            Ty::Slice(elem) => write!(f, "[{elem}]"),
            Ty::Struct(s) => f.write_str(&s.def().name),
            Ty::Enum(e) => f.write_str(&e.def().name),
            Ty::Peripheral(p) => f.write_str(&p.def().name),
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
                PlaceElem::Element(entry, index) => format!("{text}.{entry}[{index}]"),
                PlaceElem::ConstElement(entry, index) => format!("{text}.{entry}[{index}]"),
                PlaceElem::Tag => format!("{text}.tag"),
                PlaceElem::Payload { variant, index } => format!("({text} as {variant}).{index}"),
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
            Operand::Const(Const::Static(s)) if s.def().byte_string.is_some() => {
                write!(f, "const {}", s.def().name)
            }
            Operand::Const(Const::Static(s)) => write!(f, "const static {}", s.def().name),
            Operand::Const(Const::Peripheral(p)) => write!(f, "const peripheral {}", p.def().name),
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
            Rvalue::Slice { ptr, len } => write!(f, "slice({ptr}, {len})"),
            Rvalue::SliceAddress(place) => write!(f, "address({place})"),
            Rvalue::SliceLen(place) => write!(f, "len({place})"),
        }
    }
}

impl Mir {
    /// A terminator of `body`; a switch shows its values in the type of
    /// what it switches on.
    fn fmt_terminator(
        &self,
        body: &Body,
        terminator: &Terminator,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match terminator {
            Terminator::Goto(target) => write!(f, "goto -> {target}"),
            Terminator::If { cond, then, else_ } => {
                write!(f, "if {cond} -> [true: {then}, false: {else_}]")
            }
            Terminator::Switch {
                discr,
                cases,
                otherwise,
            } => {
                let ty = body.operand_ty(discr);
                write!(f, "switch {discr} -> [")?;
                for (value, target) in cases.iter() {
                    match ty {
                        Ty::Int(int) => {
                            write!(f, "{}_{int}: {target}, ", int.display_bits(*value))?
                        }
                        _ => write!(f, "{}: {target}, ", *value != 0)?,
                    }
                }
                write!(f, "otherwise: {otherwise}]")
            }
            Terminator::Return => f.write_str("return"),
            Terminator::Unreachable => f.write_str("unreachable"),
            Terminator::Panic(cause, site) => {
                write!(f, "panic({}", cause.describe())?;
                if let PanicCause::AssertionFailed(Some((message, _))) = cause {
                    write!(f, ", {message}")?;
                }
                // The data passed, and the location it holds (ADR 0053).
                if let Some(site) = site {
                    let file = site.file.def().byte_string.as_ref().expect("a byte string");
                    match site.info {
                        PanicInfoAt::Data(data) => write!(f, ", data{}", data.0)?,
                        PanicInfoAt::Local(local) => write!(f, ", &raw const {local}")?,
                    }
                    write!(
                        f,
                        " at {}:{}:{}",
                        String::from_utf8_lossy(file),
                        site.line,
                        site.column
                    )?;
                }
                f.write_str(")")
            }
        }
    }

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

impl fmt::Display for Mir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(memory) = &self.memory {
            writeln!(f, "{memory}")?;
        }
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
        // A single bit as `at 7`, a range as `at 0..=1`, as written.
        for s in &self.bit_structs {
            let fields: Vec<String> = (s.fields.iter())
                .map(|field| {
                    let bits = if field.low == field.high {
                        field.low.to_string()
                    } else {
                        format!("{}..={}", field.low, field.high)
                    };
                    let mode = field
                        .access
                        .map_or(String::new(), |mode| format!("{mode} "));
                    format!("{mode}{}: {} at {bits}", field.name, field.ty)
                })
                .collect();
            writeln!(
                f,
                "struct {}: {} {{ {} }}",
                s.name,
                s.int,
                fields.join(", ")
            )?;
        }
        for e in &self.enums {
            // With payloads, its variants are numbered as projections name
            // them, with each payload value's type and offset (ADR 0039).
            let Some(def) = e.def else {
                let variants: Vec<String> = e
                    .variants
                    .iter()
                    .map(|(name, value)| format!("{name} = {}", e.int.display_bits(*value)))
                    .collect();
                writeln!(
                    f,
                    "enum {}: {} {{ {} }}",
                    e.name,
                    e.int,
                    variants.join(", ")
                )?;
                continue;
            };
            let def = def.def();
            let variants: Vec<String> = e
                .variants
                .iter()
                .zip(def.variants())
                .enumerate()
                .map(|(i, ((name, value), variant))| {
                    let payload: Vec<String> = variant
                        .payload
                        .iter()
                        .enumerate()
                        .map(|(j, value)| format!("{j} {} at {}", value.ty, value.offset))
                        .collect();
                    let payload = if payload.is_empty() {
                        String::new()
                    } else {
                        format!("({})", payload.join(", "))
                    };
                    format!("{i} {name}{payload} = {}", e.int.display_bits(*value))
                })
                .collect();
            writeln!(
                f,
                "enum {}: {} {{ {} }} // size {}, align {}",
                e.name,
                e.int,
                variants.join(", "),
                def.layout.size,
                def.layout.align
            )?;
        }
        if !self.structs.is_empty() || !self.bit_structs.is_empty() || !self.enums.is_empty() {
            writeln!(f)?;
        }
        // Registers are numbered as projections name them; offsets and
        // addresses in hex, as datasheets give them. An entry that is an
        // array shows its length and its step.
        for p in &self.peripheral_types {
            let def = p.def();
            let layout = def.layout().expect("the layout is set when MIR is used");
            let registers: Vec<String> = (def.registers.iter())
                .zip(layout.offsets.iter().zip(&layout.arrays))
                .enumerate()
                .map(|(i, (register, (offset, array)))| {
                    let group = match &register.group {
                        Some(group) => format!(" in {group}"),
                        None => String::new(),
                    };
                    let ty = match array {
                        Some((len, step)) => format!("[{}; {len}] step {step:#x}", register.ty),
                        None => register.ty.to_string(),
                    };
                    format!("{i} {}: {ty} at {offset:#x}{group}", register.name)
                })
                .collect();
            let registers = if registers.is_empty() {
                "{}".to_owned()
            } else {
                format!("{{ {} }}", registers.join(", "))
            };
            writeln!(
                f,
                "peripheral {} {registers} // extent {}, align {}",
                def.name, layout.extent, layout.align
            )?;
        }
        for p in &self.peripherals {
            let def = p.def();
            writeln!(
                f,
                "peripheral {}: {} at {:#x};",
                def.name,
                def.ty.def().name,
                def.address
            )?;
        }
        if !self.peripheral_types.is_empty() || !self.peripherals.is_empty() {
            writeln!(f)?;
        }
        // Byte strings show where they are used.
        let named = self
            .statics
            .iter()
            .filter(|s| s.def.def().byte_string.is_none());
        let mut any = false;
        for s in named {
            any = true;
            let def = s.def.def();
            let external = if def.external { "extern " } else { "" };
            let mutability = if def.mutable { "mut " } else { "" };
            writeln!(
                f,
                "{external}static {mutability}{}: {}; // align {}",
                def.name, def.ty, s.align
            )?;
        }
        if any {
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
                f.write_str("        ")?;
                self.fmt_terminator(body, &data.terminator, f)?;
                writeln!(f, ";")?;
                writeln!(f, "    }}")?;
            }
            writeln!(f, "}}")?;
        }
        Ok(())
    }
}
