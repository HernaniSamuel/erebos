//! On-demand compilation (ADR 0028). Type checking, layouts, MIR and
//! compile-time values are computed per item when first needed, remembered,
//! and protected against cycles; the driver then asks for every item, so
//! the whole program is still compiled.

use std::collections::HashMap;
use std::rc::Rc;

use crate::diag::{Diagnostic, Diagnostics};
use crate::eval::{self, ConstValue, Origin};
use crate::hir::{
    self, ArrayLen, BitStructId, BuiltinEnum, Comptime, ConstId, EnumDef, EnumId, ExprId, ExprKind,
    FnId, Hir, PatternKind, PeripheralId, PeripheralTypeId, StaticId, StructId, VariantValue,
};
use crate::layout::{
    self, BitLayout, Composite, EnumLayout, Layout, MAX_ALIGN, MAX_SIZE, PeripheralLayout,
    StructLayout,
};
use crate::mir::{
    self, BodyId, ConstData, DataId, EnumRef, Field, LeftOut, Mir, PanicCause, PanicInfoAt,
    PanicSite, PeripheralRef, PeripheralTypeRef, Ptr, StaticRef, StructRef,
};
use crate::span::Span;
use crate::target;
use crate::ty::{self, Tables};

mod bit_struct;
mod hardware;

pub(crate) use bit_struct::{bit_count, max_value};

/// Where a piece of constant data comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DataSource {
    Const(ConstId),
    Comptime(ExprId),
}

/// What the `core::PanicInfo` of a check holds, but its message: the
/// fields known at compile time (ADR 0053).
#[derive(Clone, Copy, Debug)]
pub struct PanicFields {
    /// The byte string naming the file, which `file` points to.
    pub file: StaticRef,
    pub line: u32,
    pub column: u32,
    /// The bits of the `core::PanicReason` variant, of type `reason_ty`.
    pub reason: u64,
    pub reason_ty: hir::IntTy,
}

impl PanicFields {
    /// The length of the file's name, in bytes.
    pub fn name_len(&self) -> usize {
        let name = self.file.def().byte_string.as_ref();
        name.expect("a file name is a byte string").len()
    }
}

/// An item being computed; one requested again while in progress depends
/// on itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
    /// Type checking a body, which may need values computed at compile
    /// time, such as the length of an array type.
    Check(FnId),
    Body(FnId),
    Const(ConstId),
    Static(StaticId),
    Comptime(ExprId),
    /// A `usize` computed at compile time, such as an array length, or an
    /// end of a bit field's bits, a `u32` (ADR 0059).
    Value(FnId),
    /// The value of a variant (ADR 0035).
    Discriminant(EnumId, u32),
    Layout(StructId),
    /// The layout of an enum with payloads (ADR 0039).
    EnumLayout(EnumId),
    /// The layout of a peripheral type (ADR 0058).
    PeripheralLayout(PeripheralTypeId),
    /// The bits of the fields of a bit-field struct (ADR 0059).
    BitLayout(BitStructId),
    /// The default of a field, by index, of a bit-field struct (ADR 0059).
    BitDefault(BitStructId, u32),
}

/// Which peripheral types `lacks_layout` counts as in error (ADR 0058).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PeripheralFailures {
    /// Every one that reaches a failed value, as ADR 0048's predicates
    /// need.
    All,
    /// Those whose failure is explained (`explain_peripherals`): an error
    /// reported whatever points to them, which silences
    /// `ty::register-type`.
    Explained,
    /// None: whether a type is in error without a peripheral type.
    Ignored,
}

/// How diagnostics name a body.
fn describe(q: &Queries, f: &hir::Fn) -> String {
    let hir = q.hir;
    match f.comptime {
        Some(Comptime::Const(id)) => format!("the constant `{}`", hir.const_(id).path),
        Some(Comptime::Static(id)) => format!("the static `{}`", hir.static_(id).path),
        Some(Comptime::Block) => "this `comptime` block".to_owned(),
        Some(Comptime::Value) => "this value".to_owned(),
        Some(Comptime::Discriminant(id, index)) => variant_name(q.enum_def(id), index),
        Some(Comptime::Bits(id, index)) => {
            let def = hir.bit_struct(id);
            let field = &def.fields[index as usize].name;
            format!("the bits of the field `{}::{field}`", def.path)
        }
        Some(Comptime::Default(id, index)) => {
            let def = hir.bit_struct(id);
            let field = &def.fields[index as usize].name;
            format!("the default of the field `{}::{field}`", def.path)
        }
        None => format!("the function `{}`", f.path),
    }
}

/// How diagnostics name a variant whose value is computed: "the variant
/// `Mode::Run`".
fn variant_name(def: &EnumDef, index: u32) -> String {
    format!("the variant `{}`", def.variant_path(index as usize))
}

/// An item being computed, as cycles report it.
struct InProgress {
    item: Item,
    /// `None` for the layout of an `Option(T)` or `Result(T, E)`, declared
    /// nowhere (ADR 0045).
    span: Option<Span>,
    what: String,
    /// A cycle through it was reported already: while it is computed, it
    /// may be asked for again, by the body of a constant a pattern uses
    /// and by that body's MIR, and the cycle is one failure (ADR 0032).
    cycle_reported: bool,
}

/// The progress of one item through a query.
enum State<T> {
    NotStarted,
    InProgress,
    Done(T),
}

pub struct Queries<'h> {
    pub hir: &'h Hir,
    pub diags: Diagnostics,
    /// What type checking found out about expressions so far.
    pub tables: Tables,
    /// Type checking of each function: whether it found no errors.
    checked: Vec<State<bool>>,
    /// The layout of each struct; `None` when it contains itself or is too
    /// large, both reported.
    layouts: Vec<State<Option<StructLayout>>>,
    /// The layout of each enum with payloads, likewise (ADR 0039).
    enum_layouts: Vec<State<Option<EnumLayout>>>,
    /// The layout of each peripheral type; `None` when it reaches a failed
    /// value, an offset or a register type in error, all reported
    /// (ADR 0058).
    peripheral_layouts: Vec<State<Option<PeripheralLayout>>>,
    /// Whether each peripheral type's failure is explained by an error
    /// reported elsewhere (`explain_peripherals`); computed for all of
    /// them at once, when first needed.
    peripherals_explained: Option<Vec<bool>>,
    /// The bits of the fields of each bit-field struct (ADR 0059).
    bit_layouts: Vec<State<Rc<BitLayout>>>,
    /// The default of each field of each bit-field struct, as the bits of
    /// its type; `None` without one, or if it failed or does not fit in its
    /// field (reported).
    bit_defaults: Vec<Vec<State<Option<u64>>>>,
    /// Each `Option(T)` and `Result(T, E)` made so far, numbered after the
    /// declared enums, with its type arguments (ADR 0045).
    instances: Vec<(BuiltinEnum, Vec<hir::Ty>, &'static EnumDef)>,
    /// Those of them too large for the target while their arguments have
    /// layouts: having no declaration, they are reported where they are
    /// written (ADR 0045).
    too_large_instances: Vec<EnumId>,

    /// The MIR type of each struct.
    mir_structs: Vec<Option<StructRef>>,
    /// The MIR type of each enum with payloads.
    mir_enums: Vec<Option<EnumRef>>,
    /// The MIR type of each peripheral type, and the MIR view of each
    /// peripheral (ADR 0058).
    mir_peripheral_types: Vec<Option<PeripheralTypeRef>>,
    mir_peripherals: Vec<Option<PeripheralRef>>,
    /// The MIR of each function; `None` if it cannot be built.
    bodies: Vec<State<Option<Rc<mir::Body>>>>,
    /// The value of each constant; `None` if it failed, which was reported.
    consts: Vec<State<Option<ConstValue>>>,
    /// The initial value of each static; `None` if it failed (reported).
    statics: Vec<State<Option<ConstValue>>>,
    /// The value of each variant of each enum, as the bits of its integer
    /// type; `None` if it failed (reported).
    discriminants: Vec<Vec<State<Option<u64>>>>,
    /// Whether each enum, once checked, has every value computed and no
    /// two alike (ADR 0035).
    enums_ok: Vec<Option<bool>>,
    /// The alignment of each static; `None` if its type has no layout
    /// (reported).
    static_aligns: Vec<Option<Option<u64>>>,
    /// The MIR view of each static.
    mir_statics: Vec<Option<StaticRef>>,
    /// Each byte string used, as a static numbered after the statics, in
    /// the order MIR building meets them (ADR 0031), after the names of the
    /// files, if any.
    byte_strings: Vec<StaticRef>,
    /// Whether each byte string is `core`'s (ADR 0052).
    byte_strings_in_core: Vec<bool>,
    /// When the panic function takes a `*const core::PanicInfo`, the name of
    /// each source file, by id, as a byte string its checks point to
    /// (ADR 0053); the first byte strings, made before any body is built,
    /// so that their numbers do not depend on the order bodies are built.
    file_names: Vec<StaticRef>,
    /// The index in `byte_strings` of the byte string at each expression.
    byte_string_ids: HashMap<ExprId, usize>,
    /// The value of each `comptime` expression.
    comptime: HashMap<ExprId, State<Option<ConstValue>>>,
    /// The value of each array length, repeat count and `#[align]`
    /// argument computed at compile time, and of each end of a bit field's
    /// bits (ADR 0059).
    values: HashMap<FnId, Option<u64>>,
    /// Read-only data computed at compile time, and what it came from.
    data: Vec<ConstData>,
    data_ids: HashMap<DataSource, DataId>,
    /// Items being computed, outermost first.
    in_progress: Vec<InProgress>,
}

impl<'h> Queries<'h> {
    pub fn new(hir: &'h Hir) -> Queries<'h> {
        let mut q = Queries {
            hir,
            diags: Diagnostics::default(),
            tables: Tables::new(hir.expr_count),
            checked: hir.fns.iter().map(|_| State::NotStarted).collect(),
            layouts: hir.structs.iter().map(|_| State::NotStarted).collect(),
            enum_layouts: hir.enums.iter().map(|_| State::NotStarted).collect(),
            peripheral_layouts: (hir.peripheral_types.iter())
                .map(|_| State::NotStarted)
                .collect(),
            peripherals_explained: None,
            bit_layouts: (hir.bit_structs.iter())
                .map(|_| State::NotStarted)
                .collect(),
            bit_defaults: (hir.bit_structs.iter())
                .map(|s| s.fields.iter().map(|_| State::NotStarted).collect())
                .collect(),
            instances: Vec::new(),
            too_large_instances: Vec::new(),

            mir_structs: vec![None; hir.structs.len()],
            mir_enums: vec![None; hir.enums.len()],
            mir_peripheral_types: vec![None; hir.peripheral_types.len()],
            mir_peripherals: vec![None; hir.peripherals.len()],
            bodies: hir.fns.iter().map(|_| State::NotStarted).collect(),
            consts: hir.consts.iter().map(|_| State::NotStarted).collect(),
            statics: hir.statics.iter().map(|_| State::NotStarted).collect(),
            discriminants: hir
                .enums
                .iter()
                .map(|e| e.variants.iter().map(|_| State::NotStarted).collect())
                .collect(),
            enums_ok: vec![None; hir.enums.len()],
            static_aligns: vec![None; hir.statics.len()],
            mir_statics: vec![None; hir.statics.len()],
            byte_strings: Vec::new(),
            byte_strings_in_core: Vec::new(),
            file_names: Vec::new(),
            byte_string_ids: HashMap::new(),
            comptime: HashMap::new(),
            values: HashMap::new(),
            data: Vec::new(),
            data_ids: HashMap::new(),
            in_progress: Vec::new(),
        };
        // Every file, whether or not it holds a check: that is only known
        // while building MIR (ADR 0032, 0053).
        if q.takes_panic_info() {
            for (i, file) in hir.files.iter().enumerate() {
                let s = q.new_byte_string(file.name.as_bytes(), i >= hir.core.file);
                q.file_names.push(s);
            }
        }
        q
    }

    /// Whether the panic function takes a parameter, through which the
    /// compiler's checks pass a `*const core::PanicInfo` (ADR 0053); one of
    /// another type was reported.
    pub fn takes_panic_info(&self) -> bool {
        self.hir.fn_(self.hir.panic).param_count == 1
    }

    /// Checks every item, in source order, whether or not something else
    /// needed it.
    pub fn check_all(&mut self) {
        let hir = self.hir;
        ty::report_never_in_memory(self);
        ty::report_unsized_types(self);
        ty::report_peripherals_by_value(self);
        ty::report_volatile_peripherals(self);
        for id in 0..hir.structs.len() {
            self.struct_layout(StructId(id as u32));
        }
        for (id, def) in hir.enums.iter().enumerate() {
            if def.ty.payloads {
                self.enum_layout(EnumId(id as u32));
            }
        }
        self.report_too_large_applications();
        for id in 0..hir.fns.len() {
            self.typeck_fn(FnId(id as u32));
        }
        // Every constant and check is evaluated, used or not (ADR 0029).
        for id in 0..hir.consts.len() {
            self.const_value(ConstId(id as u32));
        }
        // So is every variant's value (ADR 0035).
        for id in 0..hir.enums.len() {
            self.enum_ok(EnumId(id as u32));
        }
        // So is every static (ADR 0030).
        for id in 0..hir.statics.len() {
            self.static_align(StaticId(id as u32));
            self.static_value(StaticId(id as u32));
        }
        for &block in &hir.comptime_blocks {
            self.comptime_block(block);
        }
        for (id, f) in hir.fns() {
            if f.comptime == Some(Comptime::Value) {
                self.array_len(ArrayLen::Comptime(id));
            }
        }
        self.check_memory_map();
        self.check_peripherals();
        self.check_bit_structs();
        for (id, f) in hir.fns() {
            let Some(body) = &f.body else {
                continue;
            };
            if !self.typeck_fn(id) || self.needs_missing_layout(id) {
                continue;
            }
            let mut inner = Vec::new();
            body.walk(&mut |e| {
                if let ExprKind::Comptime(value) = &e.kind {
                    inner.push(&**value);
                }
            });
            for value in inner {
                self.comptime_value(id, value);
            }
        }
    }

    /// Type checks a function, once; returns whether it had no errors.
    pub fn typeck_fn(&mut self, id: FnId) -> bool {
        if let State::Done(ok) = self.checked[id.0 as usize] {
            return ok;
        }
        let hir = self.hir;
        let f = hir.fn_(id);
        if !self.start(Item::Check(id), f.name_span, describe(self, f)) {
            return false;
        }
        self.checked[id.0 as usize] = State::InProgress;
        let before = self.diags.len();
        let elsewhere = ty::check_fn(self, f);
        // An error found elsewhere, such as a length that failed to
        // evaluate, leaves `{error}` types here without a new diagnostic.
        let mut poisoned = [f.ret]
            .into_iter()
            .chain(f.locals.iter().map(|local| local.ty))
            .any(|ty| self.normalize(ty) == hir::Ty::Error);
        if let Some(body) = &f.body {
            body.walk(&mut |e| poisoned |= self.tables.expr_ty(e.id) == hir::Ty::Error);
        }
        let ok = self.diags.len() - before == elsewhere && !poisoned;
        self.finish(Item::Check(id));
        self.checked[id.0 as usize] = State::Done(ok);
        ok
    }

    /// Marks `item` in progress, or reports a cycle if it already is.
    fn start(&mut self, item: Item, span: Span, what: String) -> bool {
        self.start_at(item, Some(span), what)
    }

    /// `start`, for an item that may have no span.
    fn start_at(&mut self, item: Item, span: Option<Span>, what: String) -> bool {
        let Some(at) = self.in_progress.iter().position(|p| p.item == item) else {
            self.in_progress.push(InProgress {
                item,
                span,
                what,
                cycle_reported: false,
            });
            return true;
        };
        // Structs and enums with payloads laid out inside each other, with
        // no evaluation between: containment by value (ADR 0021, 0039).
        let types: Option<Vec<Composite>> = self.in_progress[at..]
            .iter()
            .map(|p| match p.item {
                Item::Layout(id) => Some(Composite::Struct(id)),
                Item::EnumLayout(id) => Some(Composite::Enum(id)),
                _ => None,
            })
            .collect();
        if let Some(types) = types {
            self.report_containment_cycle(&types);
            return false;
        }
        // Asked for again within the cycle reported already.
        if std::mem::replace(&mut self.in_progress[at].cycle_reported, true) {
            return false;
        }
        let first_what = &self.in_progress[at].what;
        let mut diag = Diagnostic::error(
            "ty::cycle",
            format!("{first_what} depends on its own value"),
        );
        // Checking a body and computing its value share a span. An item
        // without one, an `Option(T)` or `Result(T, E)`, gets no label: it
        // shows only where the message or a later label names it, and not
        // at all in `const N: usize = size_of(Option(S));` with `S`
        // holding `[u8; N]`.
        let mut seen: Vec<Span> = Vec::new();
        for (i, InProgress { span, what, .. }) in self.in_progress[at..].iter().enumerate() {
            let Some(span) = *span else {
                continue;
            };
            if seen.contains(&span) {
                continue;
            }
            seen.push(span);
            diag = if seen.len() == 1 && i == 0 {
                diag.label(span, "needed while computing itself")
            } else {
                diag.label(span, format!("through {what}"))
            };
        }
        self.diags
            .push(diag.help("break the cycle: a value cannot depend on itself (ADR 0028)"));
        false
    }

    fn finish(&mut self, item: Item) {
        let done = self.in_progress.pop().expect("started");
        debug_assert_eq!(done.item, item);
    }

    /// The value of a constant (ADR 0029), computed once.
    pub fn const_value(&mut self, id: ConstId) -> Option<ConstValue> {
        match &self.consts[id.0 as usize] {
            State::Done(value) => return value.clone(),
            State::InProgress | State::NotStarted => {}
        }
        let hir = self.hir;
        let def = hir.const_(id);
        let what = format!("the constant `{}`", def.path);
        if !self.start(Item::Const(id), def.name_span, what.clone()) {
            return None;
        }
        self.consts[id.0 as usize] = State::InProgress;
        let value = self.mir_body(def.init).and_then(|body| {
            let origin = Origin {
                what,
                span: def.name_span,
            };
            eval::evaluate(self, body, origin)
        });
        self.finish(Item::Const(id));
        self.consts[id.0 as usize] = State::Done(value.clone());
        value
    }

    /// The value of a variant (ADR 0035), computed once, as the bits of the
    /// enum's integer type.
    pub fn discriminant(&mut self, id: EnumId, index: u32) -> Option<u64> {
        let def = self.enum_def(id);
        let variant = &def.variants[index as usize];
        let body = match variant.value {
            VariantValue::Known(value) => return Some(value),
            VariantValue::Comptime(body) => body,
        };
        let slot = (id.0 as usize, index as usize);
        if let State::Done(value) = self.discriminants[slot.0][slot.1] {
            return value;
        }
        let span = variant
            .span
            .expect("a variant computed at compile time is declared");
        let what = variant_name(def, index);
        if !self.start(Item::Discriminant(id, index), span, what.clone()) {
            return None;
        }
        self.discriminants[slot.0][slot.1] = State::InProgress;
        let value = self.mir_body(body).and_then(|body| {
            let origin = Origin { what, span };
            eval::evaluate(self, body, origin)
        });
        self.finish(Item::Discriminant(id, index));
        let value = value.map(|value| value.int(def.ty.int.bytes() as usize));
        self.discriminants[slot.0][slot.1] = State::Done(value);
        value
    }

    /// Computes the value of every variant of an enum, once, and reports
    /// two variants with one value (ADR 0035). Returns whether every value
    /// was computed and none repeats: an enum with a value that failed or
    /// repeats was reported, and uses of it report nothing more.
    pub fn enum_ok(&mut self, id: EnumId) -> bool {
        let def = self.enum_def(id);
        // Its values are known, and differ (ADR 0045).
        if !def.is_declared() {
            return true;
        }
        if let Some(ok) = self.enums_ok[id.0 as usize] {
            return ok;
        }
        let mut ok = true;
        let mut seen: Vec<(u64, usize)> = Vec::new();
        for (index, variant) in def.variants.iter().enumerate() {
            let Some(value) = self.discriminant(id, index as u32) else {
                ok = false;
                continue;
            };
            match seen.iter().find(|(seen, _)| *seen == value) {
                Some(&(_, first)) => {
                    ok = false;
                    let first = &def.variants[first];
                    let shown = def.ty.int.display_bits(value);
                    let declared = "a declared enum's variants have spans";
                    self.diags.push(
                        Diagnostic::error(
                            "ty::duplicate-discriminant",
                            format!(
                                "`{0}::{1}` has the same value as `{0}::{2}`",
                                def.name, variant.name, first.name
                            ),
                        )
                        .label(variant.span.expect(declared), format!("is {shown}"))
                        .label(first.span.expect(declared), format!("is {shown} too"))
                        .help("each variant names a value of its own, so that a `match` can tell them apart (ADR 0035)"),
                    );
                }
                None => seen.push((value, index)),
            }
        }
        self.enums_ok[id.0 as usize] = Some(ok);
        ok
    }

    /// The initial value of a static (ADR 0030), computed once. An `extern
    /// static` has none, and evaluation never asks for it: it refuses to
    /// read one first.
    pub fn static_value(&mut self, id: StaticId) -> Option<ConstValue> {
        match &self.statics[id.0 as usize] {
            State::Done(value) => return value.clone(),
            State::InProgress | State::NotStarted => {}
        }
        let hir = self.hir;
        let def = hir.static_(id);
        let init = def.init?;
        let what = format!("the static `{}`", def.path);
        if !self.start(Item::Static(id), def.name_span, what.clone()) {
            return None;
        }
        self.statics[id.0 as usize] = State::InProgress;
        let value = self.mir_body(init).and_then(|body| {
            let origin = Origin {
                what,
                span: def.name_span,
            };
            eval::evaluate(self, body, origin)
        });
        self.finish(Item::Static(id));
        self.statics[id.0 as usize] = State::Done(value.clone());
        value
    }

    /// The alignment of a static: its type's, raised by `#[align(N)]`,
    /// which does not change its size (ADR 0030). `None` if its type has no
    /// layout, reported once. An `#[align(N)]` in error, reported once,
    /// leaves its type's alignment: it is part of no type, so nothing else
    /// is affected (ADR 0048).
    pub fn static_align(&mut self, id: StaticId) -> Option<u64> {
        if let Some(align) = self.static_aligns[id.0 as usize] {
            return align;
        }
        let hir = self.hir;
        let def = hir.static_(id);
        let ty = self.normalize(def.ty);
        let align = match self.layout_of(ty) {
            None if ty == hir::Ty::Error => None,
            None => {
                self.diags.push(
                    Diagnostic::error("ty::too-large", format!("`{ty}` is too large"))
                        .label(def.ty_span, "does not fit in the address space")
                        .help(format!(
                            "objects are limited to {MAX_SIZE} bytes on this target"
                        )),
                );
                None
            }
            Some(layout) => Some(match def.align {
                Some((len, span)) => self
                    .array_len(len)
                    .and_then(|wanted| {
                        self.checked_align(wanted, span, layout.align, &format!("`{ty}`"))
                    })
                    .unwrap_or(layout.align),
                None => layout.align,
            }),
        };
        self.static_aligns[id.0 as usize] = Some(align);
        align
    }

    /// `#[align(N)]` with `N` computed as `wanted`, for something whose
    /// natural alignment is `natural` (ADR 0022, 0030): `N` if it is a
    /// power of two, at most `MAX_ALIGN` and not below `natural`, else
    /// `None`, reported.
    fn checked_align(
        &mut self,
        wanted: u64,
        span: Span,
        natural: u64,
        subject: &str,
    ) -> Option<u64> {
        let problem = if !wanted.is_power_of_two() {
            "an alignment is a power of two".to_owned()
        } else if wanted > MAX_ALIGN {
            format!("alignments go up to {MAX_ALIGN}")
        } else if wanted < natural {
            format!("{subject} already needs alignment {natural}; `#[align]` only raises it")
        } else {
            return Some(wanted);
        };
        self.diags.push(
            Diagnostic::error("ty::invalid-align", format!("invalid alignment {wanted}"))
                .label(span, problem)
                .help(
                    "`#[align(N)]` takes a power of two not below the natural alignment (ADR 0022)",
                ),
        );
        None
    }

    /// Runs a `comptime { ... }` item.
    fn comptime_block(&mut self, id: FnId) {
        let f = self.hir.fn_(id);
        let span = f.name_span;
        if let Some(body) = self.mir_body(id) {
            let origin = Origin {
                what: "this `comptime` block".to_owned(),
                span,
            };
            eval::evaluate(self, body, origin);
        }
    }

    /// The value of `comptime expr` (ADR 0029), where `expr` is part of
    /// function `id`.
    pub fn comptime_value(&mut self, id: FnId, expr: &hir::Expr) -> Option<ConstValue> {
        if let Some(State::Done(value)) = self.comptime.get(&expr.id) {
            return value.clone();
        }
        let what = "this `comptime` expression".to_owned();
        if !self.start(Item::Comptime(expr.id), expr.span, what.clone()) {
            return None;
        }
        self.comptime.insert(expr.id, State::InProgress);
        let value = mir::build_expr_body(self, id, expr).and_then(|body| {
            let origin = Origin {
                what,
                span: expr.span,
            };
            eval::evaluate(self, Rc::new(body), origin)
        });
        self.finish(Item::Comptime(expr.id));
        self.comptime.insert(expr.id, State::Done(value.clone()));
        value
    }

    /// The value of an array length, repeat count or `#[align]` argument,
    /// or `None` if it failed (reported).
    pub fn array_len(&mut self, len: ArrayLen) -> Option<u64> {
        self.comptime_int(len, target::POINTER_BYTES)
    }

    /// The value of `len`, an integer of `bytes` bytes computed at compile
    /// time, or `None` if it failed (reported).
    fn comptime_int(&mut self, len: ArrayLen, bytes: u64) -> Option<u64> {
        let id = match len {
            ArrayLen::Known(len) => return Some(len),
            ArrayLen::Comptime(id) => id,
        };
        if let Some(value) = self.values.get(&id) {
            return *value;
        }
        let f = self.hir.fn_(id);
        let span = f.span;
        let what = describe(self, f);
        if !self.start(Item::Value(id), span, what.clone()) {
            return None;
        }
        let value = self.mir_body(id).and_then(|body| {
            let origin = Origin { what, span };
            eval::evaluate(self, body, origin)
        });
        self.finish(Item::Value(id));
        let value = value.map(|value| value.int(bytes as usize));
        self.values.insert(id, value);
        value
    }

    /// `ty` with every array length known, and every `Option(T)` and
    /// `Result(T, E)` the enum it is for its arguments, or `{error}` if a
    /// length failed (reported).
    pub fn normalize(&mut self, ty: hir::Ty) -> hir::Ty {
        if ty.is_normal() {
            return ty;
        }
        match ty {
            hir::Ty::Array(elem, len) => {
                let elem = self.normalize(*elem);
                match (self.array_len(len), elem) {
                    (_, hir::Ty::Error) | (None, _) => hir::Ty::Error,
                    (Some(len), elem) => hir::Ty::array(elem, len),
                }
            }
            hir::Ty::Ptr(ptr) => match self.normalize(*ptr.pointee) {
                hir::Ty::Error => hir::Ty::Error,
                pointee => hir::Ty::ptr(ptr.mutable, ptr.volatile, pointee),
            },
            hir::Ty::Slice(elem) => match self.normalize(*elem) {
                hir::Ty::Error => hir::Ty::Error,
                elem => hir::Ty::slice(elem),
            },
            hir::Ty::Applied(applied) => {
                let args: Vec<hir::Ty> = applied
                    .args
                    .iter()
                    .map(|&arg| self.normalize(arg))
                    .collect();
                if args.contains(&hir::Ty::Error) {
                    return hir::Ty::Error;
                }
                hir::Ty::Enum(self.instance(applied.builtin, args))
            }
            ty => ty,
        }
    }

    /// The enum `builtin` is with the normalized type arguments `args`,
    /// made the first time they are given (ADR 0045): two are one type
    /// when their arguments are.
    fn instance(&mut self, builtin: BuiltinEnum, args: Vec<hir::Ty>) -> hir::EnumTy {
        if let Some((_, _, def)) = self
            .instances
            .iter()
            .find(|(made, made_args, _)| *made == builtin && *made_args == args)
        {
            return def.ty;
        }
        let id = EnumId((self.hir.enums.len() + self.instances.len()) as u32);
        let def: &'static EnumDef = Box::leak(Box::new(builtin.def(id, &args)));
        self.instances.push((builtin, args, def));
        self.enum_layouts.push(State::NotStarted);
        self.mir_enums.push(None);
        def.ty
    }

    /// The definition of the enum `id`: a declared one, or an `Option(T)`
    /// or `Result(T, E)` made by `normalize` (ADR 0045).
    pub fn enum_def(&self, id: EnumId) -> &'h EnumDef {
        let hir = self.hir;
        match (id.0 as usize).checked_sub(hir.enums.len()) {
            Some(index) => self.instances[index].2,
            None => &hir.enums[id.0 as usize],
        }
    }

    /// Which of `Option` and `Result` the enum `id` is an instance of, if
    /// it is one (ADR 0045).
    pub fn builtin_enum(&self, id: EnumId) -> Option<BuiltinEnum> {
        let index = (id.0 as usize).checked_sub(self.hir.enums.len())?;
        Some(self.instances[index].0)
    }

    /// The enums whose values a `match` on a value of type `scrutinee`
    /// with these arms needs, each once, in the order first needed
    /// (ADR 0038, 0040): the type of each position at which some
    /// alternative is a variant or a constant, whatever its own type, and
    /// the scrutinee's for a `match` without arms. The positions inside a
    /// variant pattern have the types its declaration gives, those inside
    /// a `.V` pattern the types its position's enum gives them (ADR 0044),
    /// normalized, so that an `Option(T)` there is the enum it is
    /// (ADR 0045).
    pub fn enums_needed(&mut self, scrutinee: hir::Ty, arms: &[hir::Arm]) -> Vec<EnumId> {
        fn walk(
            q: &mut Queries,
            pattern: &hir::Pattern,
            ty: Option<hir::Ty>,
            out: &mut Vec<EnumId>,
        ) {
            let need = |out: &mut Vec<EnumId>| {
                if let Some(hir::Ty::Enum(e)) = ty
                    && !out.contains(&e.id)
                {
                    out.push(e.id);
                }
            };
            match &pattern.kind {
                PatternKind::Or(alternatives) => {
                    for alternative in alternatives {
                        walk(q, alternative, ty, out);
                    }
                }
                PatternKind::Const(_) => need(out),
                PatternKind::Variant(id, index, subs) => {
                    need(out);
                    let payload = &q.enum_def(*id).variants[*index as usize].payload;
                    payload_positions(q, Some(payload), subs.as_deref(), out);
                }
                // The enum of its position, whose variant `name`, if it
                // has one, gives its payload positions their types; without
                // it they have none, and need nothing (ADR 0044).
                PatternKind::ContextVariant(_, name, _, subs) => {
                    need(out);
                    let payload = match ty {
                        Some(hir::Ty::Enum(e)) => q
                            .enum_def(e.id)
                            .variants
                            .iter()
                            .find(|variant| variant.name == *name)
                            .map(|variant| variant.payload.as_slice()),
                        _ => None,
                    };
                    payload_positions(q, payload, subs.as_deref(), out);
                }
                _ => {}
            }
        }
        /// The patterns `subs` at the positions of the values `payload`
        /// types, if any.
        fn payload_positions(
            q: &mut Queries,
            payload: Option<&[hir::PayloadDef]>,
            subs: Option<&[hir::Pattern]>,
            out: &mut Vec<EnumId>,
        ) {
            for (i, sub) in subs.unwrap_or_default().iter().enumerate() {
                let value = payload.and_then(|payload| payload.get(i));
                let sub_ty = value.map(|value| q.normalize(value.ty));
                walk(q, sub, sub_ty, out);
            }
        }
        let mut out = Vec::new();
        if arms.is_empty()
            && let hir::Ty::Enum(e) = scrutinee
        {
            out.push(e.id);
        }
        for arm in arms {
            walk(self, &arm.pattern, Some(scrutinee), &mut out);
        }
        out
    }

    /// The `Option(T)` and `Result(T, E)` made so far, by their names, as
    /// dumps list them: the order they are made in follows the schedule
    /// (ADR 0028, 0045).
    pub fn instances_by_name(&self) -> Vec<EnumId> {
        let mut instances: Vec<&EnumDef> = self.instances.iter().map(|(_, _, def)| *def).collect();
        instances.sort_by(|a, b| a.name.cmp(&b.name));
        instances.into_iter().map(|def| def.ty.id).collect()
    }

    /// Reports each `Option(T)` and `Result(T, E)` written that is too
    /// large for the target while its arguments have layouts, where it is
    /// written: it has no declaration (ADR 0045). One whose argument has no
    /// layout reports nothing more, that argument having been reported.
    fn report_too_large_applications(&mut self) {
        let hir = self.hir;
        for &(span, ty) in &hir.applications {
            let hir::Ty::Enum(e) = self.normalize(ty) else {
                continue;
            };
            if self.enum_layout(e.id).is_none() && self.too_large_instances.contains(&e.id) {
                self.diags.push(
                    Diagnostic::error("ty::too-large", format!("`{}` is too large", e.name))
                        .label(span, "does not fit in the address space")
                        .help(format!(
                            "objects are limited to {MAX_SIZE} bytes on this target; an enum is as large as its largest variant, with its tag, rounded up to its alignment (ADR 0039, 0045)"
                        )),
                );
            }
        }
    }

    /// Read-only data for a value computed at compile time, stored once
    /// per source.
    pub fn const_data(&mut self, source: DataSource, value: &ConstValue, align: u64) -> DataId {
        if let Some(&id) = self.data_ids.get(&source) {
            return id;
        }
        let id = DataId(self.data.len() as u32);
        self.data.push(ConstData {
            bytes: value.bytes.clone(),
            align,
            relocations: value.relocations.clone(),
        });
        self.data_ids.insert(source, id);
        id
    }

    pub fn data(&self, id: DataId) -> &ConstData {
        &self.data[id.0 as usize]
    }

    /// The layout of `ty`, or `None` if it is larger than the target can
    /// hold. A struct or an enum with payloads without a layout, already
    /// reported, counts as empty.
    pub fn layout_of(&mut self, ty: hir::Ty) -> Option<Layout> {
        let ty = self.normalize(ty);
        let empty = Layout { size: 0, align: 1 };
        layout::of(ty, &mut |composite| {
            Some(self.composite_layout(composite).unwrap_or(empty))
        })
    }

    /// The layout of a struct or an enum with payloads; `None` if it has
    /// none, reported.
    fn composite_layout(&mut self, composite: Composite) -> Option<Layout> {
        match composite {
            Composite::Struct(id) => self.struct_layout(id).map(|s| s.layout),
            Composite::Enum(id) => self.enum_layout(id).map(|e| e.layout),
        }
    }

    /// Lays out a struct with C rules (ADR 0022), reporting structs that
    /// contain themselves, too large ones, and misused attributes. A struct
    /// whose `#[align(N)]` failed is laid out without it, and marked so.
    pub fn struct_layout(&mut self, id: StructId) -> Option<StructLayout> {
        if let State::Done(layout) = &self.layouts[id.0 as usize] {
            return layout.clone();
        }
        let hir = self.hir;
        let def = hir.struct_(id);
        let what = format!("the layout of `{}`", def.name);
        if !self.start(Item::Layout(id), def.name_span, what) {
            return None;
        }
        self.layouts[id.0 as usize] = State::InProgress;

        // Fields in declaration order, each at the next offset aligned for
        // it; the size is rounded up to the alignment. `#[packed]` drops
        // the padding and the alignment.
        let mut offsets = Vec::with_capacity(def.fields.len());
        let mut end = Some(0);
        let mut align = 1;
        let mut complete = true;
        let mut over_aligned = def.align.is_some();
        for field in &def.fields {
            let field_ty = self.normalize(field.ty);
            let mut inner_failed = false;
            let field_layout = layout::of(field_ty, &mut |composite| {
                let inner = self.composite_layout(composite);
                inner_failed |= inner.is_none();
                inner
            });
            let Some(field_layout) = field_layout else {
                // A type inside without a layout was reported; otherwise the
                // field alone is too large.
                if !inner_failed {
                    self.diags.push(
                        Diagnostic::error("ty::too-large", format!("`{field_ty}` is too large"))
                            .label(field.span, "this field does not fit in the address space")
                            .help(format!(
                                "objects are limited to {MAX_SIZE} bytes on this target"
                            )),
                    );
                }
                complete = false;
                continue;
            };
            if self.over_aligned(field_ty) {
                over_aligned = true;
                if def.packed {
                    self.diags.push(
                        Diagnostic::error(
                            "ty::aligned-in-packed",
                            format!("a packed struct cannot contain `{}`", field.ty),
                        )
                        .label(field.span, "asks for an alignment through `#[align]`")
                        .label(def.name_span, "packed: alignment 1")
                        .help("the alignment it asks for could not be kept (ADR 0022)"),
                    );
                }
            }
            let field_align = if def.packed { 1 } else { field_layout.align };
            align = align.max(field_align);
            let offset = end.and_then(|end| layout::align_up(end, field_align));
            offsets.push(offset.unwrap_or(0));
            end = offset.and_then(|offset| offset.checked_add(field_layout.size));
        }
        // An alignment in error, whatever the error, leaves the natural one,
        // which `#[align(N)]` could only have raised: what is computed from
        // it holds whatever `N` would have been (ADR 0048).
        let mut align_failed = false;
        if let Some((len, span)) = def.align {
            let subject = format!("`{}`", def.name);
            match self
                .array_len(len)
                .and_then(|wanted| self.checked_align(wanted, span, align, &subject))
            {
                Some(wanted) => align = wanted,
                None => align_failed = true,
            }
        }
        let size = end.and_then(|end| layout::align_up(end, align));

        self.finish(Item::Layout(id));
        let layout = match (complete, size) {
            (true, Some(size)) if size <= MAX_SIZE => Some(StructLayout {
                layout: Layout { size, align },
                offsets,
                over_aligned,
                align_failed,
            }),
            (true, _) => {
                self.diags.push(
                    Diagnostic::error("ty::too-large", format!("`{}` is too large", def.name))
                        .label(def.name_span, "does not fit in the address space")
                        .help(format!(
                            "objects are limited to {MAX_SIZE} bytes on this target"
                        )),
                );
                None
            }
            // A field without a layout was reported where it was found.
            (false, _) => None,
        };
        self.layouts[id.0 as usize] = State::Done(layout.clone());
        layout
    }

    /// Lays out an enum with payloads (ADR 0039), each variant as a struct
    /// of its tag and payload values, reporting payload types too large and
    /// enums that contain themselves or are too large. An `Option(T)` or
    /// `Result(T, E)` too large while its arguments have layouts is
    /// recorded instead, to be reported where it is written (ADR 0045).
    pub fn enum_layout(&mut self, id: EnumId) -> Option<EnumLayout> {
        if let State::Done(layout) = &self.enum_layouts[id.0 as usize] {
            return layout.clone();
        }
        let def = self.enum_def(id);
        let what = format!("the layout of `{}`", def.name);
        if !self.start_at(Item::EnumLayout(id), def.name_span, what) {
            return None;
        }
        self.enum_layouts[id.0 as usize] = State::InProgress;

        let mut variants = Vec::with_capacity(def.variants.len());
        let mut complete = true;
        // A payload value without a layout of its own, reported.
        let mut inner_failed_any = false;
        let mut over_aligned = false;
        for variant in &def.variants {
            let mut payload = Vec::with_capacity(variant.payload.len());
            for value in &variant.payload {
                let ty = self.normalize(value.ty);
                let mut inner_failed = false;
                let value_layout = layout::of(ty, &mut |composite| {
                    let inner = self.composite_layout(composite);
                    inner_failed |= inner.is_none();
                    inner
                });
                let Some(value_layout) = value_layout else {
                    // A type inside without a layout was reported; otherwise
                    // the payload value alone is too large.
                    inner_failed_any |= inner_failed;
                    if !inner_failed && let Some(span) = value.span {
                        self.diags.push(
                            Diagnostic::error("ty::too-large", format!("`{ty}` is too large"))
                                .label(span, "this payload value does not fit in the address space")
                                .help(format!(
                                    "objects are limited to {MAX_SIZE} bytes on this target"
                                )),
                        );
                    }
                    complete = false;
                    continue;
                };
                over_aligned |= self.over_aligned(ty);
                payload.push(value_layout);
            }
            variants.push(payload);
        }
        let tag = Layout {
            size: def.ty.int.bytes(),
            align: def.ty.int.bytes(),
        };
        let laid_out = layout::enum_layout(tag, &variants);

        self.finish(Item::EnumLayout(id));
        let layout = match (complete, laid_out, def.name_span) {
            (true, Some((layout, offsets)), _) => Some(EnumLayout {
                layout,
                offsets,
                over_aligned,
            }),
            // Too large on its own: its arguments have layouts (ADR 0045).
            (_, _, None) if !inner_failed_any => {
                self.too_large_instances.push(id);
                None
            }
            (true, None, Some(name_span)) => {
                self.diags.push(
                    Diagnostic::error("ty::too-large", format!("`{}` is too large", def.name))
                        .label(name_span, "does not fit in the address space")
                        .help(format!(
                            "objects are limited to {MAX_SIZE} bytes on this target; an enum is as large as its largest variant, with its tag, rounded up to its alignment (ADR 0039)"
                        )),
                );
                None
            }
            // A payload value without a layout was reported where it was
            // found.
            _ => None,
        };
        self.enum_layouts[id.0 as usize] = State::Done(layout.clone());
        layout
    }

    /// Whether `ty` contains a struct with `#[align(N)]`, among the structs
    /// and enums with payloads already laid out.
    fn over_aligned(&self, ty: hir::Ty) -> bool {
        match ty {
            hir::Ty::Struct(s) => matches!(
                &self.layouts[s.id.0 as usize],
                State::Done(Some(layout)) if layout.over_aligned
            ),
            hir::Ty::Enum(e) if e.payloads => matches!(
                &self.enum_layouts[e.id.0 as usize],
                State::Done(Some(layout)) if layout.over_aligned
            ),
            hir::Ty::Array(elem, _) => self.over_aligned(*elem),
            _ => false,
        }
    }

    /// Structs and enums with payloads found while laying themselves out
    /// contain each other by value; the first is the one found again. The
    /// cycle is shown from the first of them that is declared, which it is
    /// labelled at: an `Option(T)` or `Result(T, E)` is declared nowhere,
    /// and a cycle always goes through a struct or enum that is, its type
    /// arguments alone being finite (ADR 0045).
    fn report_containment_cycle(&mut self, cycle: &[Composite]) {
        let hir = self.hir;
        let name = |composite: &Composite| match *composite {
            Composite::Struct(id) => (
                hir.struct_(id).name.as_str(),
                Some(hir.struct_(id).name_span),
            ),
            Composite::Enum(id) => {
                let def = self.enum_def(id);
                (def.name.as_str(), def.name_span)
            }
        };
        let start = cycle
            .iter()
            .position(|composite| name(composite).1.is_some())
            .expect("a cycle goes through a declared struct or enum");
        let cycle = [&cycle[start..], &cycle[..start]].concat();
        let (first, first_span) = name(&cycle[0]);
        let first_span = first_span.expect("the cycle starts at a declared type");
        let message = if cycle.len() == 1 {
            format!("`{first}` contains itself")
        } else {
            let mut path: Vec<&str> = cycle.iter().map(|c| name(c).0).collect();
            path.push(first);
            let what = if cycle.iter().all(|c| matches!(c, Composite::Struct(_))) {
                "structs"
            } else {
                "types"
            };
            format!("these {what} contain each other: {}", path.join(" -> "))
        };
        let adrs = if cycle.iter().any(|c| name(c).1.is_none()) {
            "ADR 0021, 0039, 0045"
        } else if cycle.iter().any(|c| matches!(c, Composite::Enum(_))) {
            "ADR 0021, 0039"
        } else {
            "ADR 0021"
        };
        let mut diag = Diagnostic::error("ty::infinite-size", message)
            .label(first_span, "would have infinite size")
            .help(format!(
                "contain a pointer instead, such as `*mut {first}` ({adrs})"
            ));
        for other in &cycle[1..] {
            if let Some(span) = name(other).1 {
                diag = diag.label(span, "part of the cycle");
            }
        }
        self.diags.push(diag);
    }

    /// The MIR type of a struct, with its layout. Its fields are set after
    /// it exists, since they may point back to it.
    pub fn mir_struct(&mut self, id: StructId) -> StructRef {
        if let Some(s) = self.mir_structs[id.0 as usize] {
            return s;
        }
        let hir = self.hir;
        let def = hir.struct_(id);
        let layout = self
            .struct_layout(id)
            .expect("MIR is only built for well-typed programs");
        let s = StructRef::new(def.name.clone(), layout.layout, def.packed);
        self.mir_structs[id.0 as usize] = Some(s);
        let fields = def
            .fields
            .iter()
            .zip(&layout.offsets)
            .map(|(field, &offset)| Field {
                name: field.name.clone(),
                ty: self.mir_ty(field.ty),
                offset,
            })
            .collect();
        s.def().set_fields(fields);
        s
    }

    /// The MIR type of an enum with payloads, with its layout (ADR 0039).
    /// The payload values of its variants are set after it exists, since
    /// they may point back to it. The values of its variants are not part
    /// of it: they are asked for where they are needed, so that the type
    /// never waits for them (ADR 0038).
    pub fn mir_enum(&mut self, id: EnumId) -> EnumRef {
        if let Some(e) = self.mir_enums[id.0 as usize] {
            return e;
        }
        let def = self.enum_def(id);
        let layout = self
            .enum_layout(id)
            .expect("MIR is only built for well-typed programs");
        let e = EnumRef::new(
            mir::EnumId(id.0),
            def.name.clone(),
            def.ty.int,
            layout.layout,
        );
        self.mir_enums[id.0 as usize] = Some(e);
        let variants = def
            .variants
            .iter()
            .zip(&layout.offsets)
            .map(|(variant, offsets)| mir::Variant {
                name: variant.name.clone(),
                payload: variant
                    .payload
                    .iter()
                    .zip(offsets)
                    .enumerate()
                    .map(|(index, (value, &offset))| Field {
                        name: index.to_string(),
                        ty: self.mir_ty(value.ty),
                        offset,
                    })
                    .collect(),
            })
            .collect();
        e.def().set_variants(variants);
        e
    }

    /// The MIR type of a peripheral type (ADR 0058), with its registers'
    /// types. Its layout is set once it is computed, which may be after the
    /// type exists, and is never asked for here: the type may be made for a
    /// struct's field or a static's type while its layout is not computed,
    /// or is being computed, and asking would start that computation, or
    /// report a cycle, from wherever the type is first needed. Only a
    /// register's offset needs the layout, and a body reaching a register
    /// has an expression of the peripheral type, whose layout
    /// `needs_missing_layout` computes before the body is built. `into_mir`
    /// asks for every layout, so that code generation and dumps find them
    /// all.
    pub fn mir_peripheral_type(&mut self, id: PeripheralTypeId) -> PeripheralTypeRef {
        if let Some(p) = self.mir_peripheral_types[id.0 as usize] {
            return p;
        }
        let hir = self.hir;
        let def = hir.peripheral_type(id);
        // A register type is written as `u8`, `u16` or `u32`. Any other was
        // reported, and leaves the layout failed; no body that is kept
        // reaches it, so it stands as `()`, without being computed.
        let registers = (def.registers.iter())
            .map(|register| mir::Register {
                name: register.name.clone(),
                ty: match register.ty {
                    hir::Ty::Int(int) if layout::register_width(register.ty).is_some() => {
                        mir::Ty::Int(int)
                    }
                    _ => mir::Ty::Unit,
                },
            })
            .collect();
        let p = PeripheralTypeRef::new(def.path.clone(), registers);
        self.mir_peripheral_types[id.0 as usize] = Some(p);
        // Otherwise `peripheral_layout` sets it when it is computed.
        if let State::Done(Some(layout)) = &self.peripheral_layouts[id.0 as usize] {
            p.def().set_layout(layout.clone());
        }
        p
    }

    /// The MIR view of a peripheral (ADR 0058): its address, computed at
    /// compile time, and its type. MIR is built only for bodies that name
    /// no erroneous peripheral, so both exist.
    pub fn mir_peripheral(&mut self, id: PeripheralId) -> PeripheralRef {
        if let Some(p) = self.mir_peripherals[id.0 as usize] {
            return p;
        }
        let hir = self.hir;
        let def = hir.peripheral(id);
        let hir::Ty::Peripheral(ty) = def.ty else {
            unreachable!("MIR is only built for bodies naming no erroneous peripheral")
        };
        let address = self
            .array_len(def.address.0)
            .expect("MIR is only built for bodies naming no erroneous peripheral");
        let ty = self.mir_peripheral_type(ty.id);
        let p = PeripheralRef::new(def.path.clone(), address, ty);
        self.mir_peripherals[id.0 as usize] = Some(p);
        p
    }

    /// The MIR view of a static.
    pub fn mir_static(&mut self, id: StaticId) -> StaticRef {
        if let Some(s) = self.mir_statics[id.0 as usize] {
            return s;
        }
        let hir = self.hir;
        let def = hir.static_(id);
        let ty = self.mir_ty(def.ty);
        let s = StaticRef::new(
            mir::StaticId(id.0),
            def.symbol.clone(),
            ty,
            def.mutable,
            def.init.is_none(),
        );
        self.mir_statics[id.0 as usize] = Some(s);
        s
    }

    /// The byte string at `expr`, holding `bytes`: one object per
    /// occurrence (ADR 0031).
    pub fn mir_byte_string(&mut self, expr: ExprId, bytes: &[u8]) -> StaticRef {
        if let Some(&index) = self.byte_string_ids.get(&expr) {
            return self.byte_strings[index];
        }
        let index = self.byte_strings.len();
        let s = self.new_byte_string(bytes, expr.0 >= self.hir.core.expr);
        self.byte_string_ids.insert(expr, index);
        s
    }

    /// A new byte string holding `bytes`, of `core` or not (ADR 0052).
    fn new_byte_string(&mut self, bytes: &[u8], in_core: bool) -> StaticRef {
        let index = self.byte_strings.len();
        let id = mir::StaticId((self.hir.statics.len() + index) as u32);
        let s = StaticRef::byte_string(id, bytes.to_vec());
        self.byte_strings.push(s);
        self.byte_strings_in_core.push(in_core);
        s
    }

    /// The fields of the `core::PanicInfo` of the check that `cause` names,
    /// located at the first byte of `at`, but its message (ADR 0053).
    pub fn panic_fields(&mut self, cause: PanicCause, at: Span) -> PanicFields {
        let hir = self.hir;
        let (_, line, column) = hir.location(at);
        let reason = self.enum_def(hir.panic_reason);
        let index = reason
            .variants
            .iter()
            .position(|v| v.name == cause.reason());
        let index = index.expect("a variant of `core::PanicReason`") as u32;
        let tag = self
            .discriminant(hir.panic_reason, index)
            .expect("the variants of `core::PanicReason` have values");
        PanicFields {
            file: self.file_names[at.file.index()],
            line,
            column,
            reason: tag,
            reason_ty: reason.ty.int,
        }
    }

    /// Where the check that `cause` names is, located at the first byte of
    /// `at`, as the panic function learns it (ADR 0053): a `PanicInfo` in
    /// read-only data of its own, its `file` pointing to the byte string
    /// naming the file, its `message` empty, address 0 and length 0.
    pub fn panic_site(&mut self, cause: PanicCause, at: Span) -> PanicSite {
        let hir = self.hir;
        let fields = self.panic_fields(cause, at);
        let def = hir.struct_(hir.panic_info);
        let layout = self
            .struct_layout(hir.panic_info)
            .expect("`core::PanicInfo` has a layout");
        let offset = |name: &str| {
            let index = def.fields.iter().position(|field| field.name == name);
            layout.offsets[index.expect("a field of `core::PanicInfo`")] as usize
        };
        let tag_bytes = fields.reason_ty.bytes() as usize;

        let mut bytes = vec![0; layout.layout.size as usize];
        let mut put = |at: usize, value: &[u8]| bytes[at..at + value.len()].copy_from_slice(value);
        // A slice is its address, then its length (ADR 0031); the address
        // of the file's name is the linker's to fill in (ADR 0030).
        let name_len = u32::try_from(fields.name_len()).expect("file names are short");
        put(
            offset("file") + target::POINTER_BYTES as usize,
            &name_len.to_le_bytes(),
        );
        put(offset("line"), &fields.line.to_le_bytes());
        put(offset("column"), &fields.column.to_le_bytes());
        put(offset("reason"), &fields.reason.to_le_bytes()[..tag_bytes]);
        let info = DataId(self.data.len() as u32);
        self.data.push(ConstData {
            bytes,
            align: layout.layout.align,
            relocations: vec![mir::Relocation {
                offset: offset("file") as u32,
                target: fields.file.def().id,
                addend: 0,
            }],
        });
        PanicSite {
            info: PanicInfoAt::Data(info),
            file: fields.file,
            line: fields.line,
            column: fields.column,
        }
    }

    /// The static numbered `id` in MIR: a static, or a byte string.
    pub fn mir_static_by_id(&mut self, id: mir::StaticId) -> StaticRef {
        let statics = self.hir.statics.len();
        match (id.0 as usize).checked_sub(statics) {
            Some(index) => self.byte_strings[index],
            None => self.mir_static(StaticId(id.0)),
        }
    }

    pub fn mir_ty(&mut self, ty: hir::Ty) -> mir::Ty {
        match ty {
            hir::Ty::Unit => mir::Ty::Unit,
            hir::Ty::Never => mir::Ty::Never,
            hir::Ty::Bool => mir::Ty::Bool,
            hir::Ty::Int(int) => mir::Ty::Int(int),
            hir::Ty::Ptr(ptr) => mir::Ty::Ptr(Ptr {
                mutable: ptr.mutable,
                volatile: ptr.volatile,
                // Leaked like HIR pointee types; see `hir::Ty::ptr`.
                pointee: Box::leak(Box::new(self.mir_ty(*ptr.pointee))),
            }),
            hir::Ty::Array(elem, len) => {
                let len = self
                    .array_len(len)
                    .expect("MIR is only built for well-typed programs");
                mir::Ty::Array(Box::leak(Box::new(self.mir_ty(*elem))), len)
            }
            hir::Ty::Slice(elem) => mir::Ty::Slice(Box::leak(Box::new(self.mir_ty(*elem)))),
            hir::Ty::Struct(s) => mir::Ty::Struct(self.mir_struct(s.id)),
            hir::Ty::Enum(e) if e.payloads => mir::Ty::Enum(self.mir_enum(e.id)),
            // Stored as its integer type, which is all MIR sees (ADR 0035).
            hir::Ty::Enum(e) => mir::Ty::Int(e.int),
            // Stored as its integer type, which is all MIR sees: its fields
            // are shifts and masks of it (ADR 0059).
            hir::Ty::BitStruct(s) => mir::Ty::Int(s.int),
            hir::Ty::Applied(_) => {
                let ty = self.normalize(ty);
                self.mir_ty(ty)
            }
            hir::Ty::Peripheral(p) => mir::Ty::Peripheral(self.mir_peripheral_type(p.id)),
            hir::Ty::Error => unreachable!("MIR is only built for well-typed programs"),
        }
    }

    /// The MIR of a function, built from its checked HIR; `None` if it
    /// had errors, a check of ADR 0032 failed, or it needs a value that
    /// failed (all reported).
    pub fn mir_body(&mut self, id: FnId) -> Option<Rc<mir::Body>> {
        match &self.bodies[id.0 as usize] {
            State::Done(body) => return body.clone(),
            State::InProgress | State::NotStarted => {}
        }
        if !self.typeck_fn(id) {
            // Asked for while its own type checking is in progress, which
            // needed a constant that calls it: a cycle, reported. Once
            // checked, it is built like any other (ADR 0032).
            if let State::InProgress = self.checked[id.0 as usize] {
                return None;
            }
            self.bodies[id.0 as usize] = State::Done(None);
            return None;
        }
        if self.needs_missing_layout(id) {
            self.bodies[id.0 as usize] = State::Done(None);
            return None;
        }
        // A function's body may need a `comptime` value that needs the
        // function itself. A constant's body is only built for the constant,
        // which catches its own cycles.
        let f = self.hir.fn_(id);
        let tracked = f.comptime.is_none();
        if tracked
            && !self.start(
                Item::Body(id),
                f.name_span,
                format!("the function `{}`", f.path),
            )
        {
            return None;
        }
        self.bodies[id.0 as usize] = State::InProgress;
        let body = mir::build_body(self, id).map(Rc::new);
        if tracked {
            self.finish(Item::Body(id));
        }
        self.bodies[id.0 as usize] = State::Done(body.clone());
        body
    }

    /// Lowers every function that type checks to MIR, which checks the
    /// values known at compile time (ADR 0032), whatever errors other
    /// functions have.
    pub fn lower_all(&mut self) {
        for id in 0..self.hir.fns.len() {
            self.mir_body(FnId(id as u32));
        }
    }

    /// Whether lowering function `id` needs the layout of a struct that has
    /// none, or of a type that failed, both reported where they failed, or
    /// names an erroneous peripheral (ADR 0058). Like a body that does not
    /// type check, it is then left out without a new diagnostic.
    fn needs_missing_layout(&mut self, id: FnId) -> bool {
        let f = self.hir.fn_(id);
        let mut types = vec![f.ret];
        types.extend(f.locals.iter().map(|local| local.ty));
        let mut peripherals = Vec::new();
        if let Some(body) = &f.body {
            body.walk(&mut |e| {
                types.push(self.tables.expr_ty(e.id));
                match &e.kind {
                    ExprKind::SizeOf(ty) | ExprKind::AlignOf(ty) | ExprKind::OffsetOf(ty, _, _) => {
                        types.push(*ty);
                    }
                    ExprKind::Peripheral(id) => peripherals.push(*id),
                    _ => {}
                }
            });
        }
        let mut seen = Vec::new();
        types
            .into_iter()
            .any(|ty| self.lacks_layout(ty, &mut seen, PeripheralFailures::All))
            || (peripherals.into_iter()).any(|id| self.peripheral_erroneous(id))
    }

    /// Whether `ty` needs a struct or an enum with payloads without a
    /// layout, or a type that failed, as a payload of type `!` is
    /// (ADR 0034, 0039), or reaches a peripheral type or a bit-field struct
    /// that reaches a failed value (ADR 0058, 0059): all reported, so its
    /// uses report nothing more.
    pub fn is_erroneous(&mut self, ty: hir::Ty) -> bool {
        self.lacks_layout(ty, &mut Vec::new(), PeripheralFailures::All)
    }

    /// Whether the MIR type of `ty` needs a struct or an enum with payloads
    /// without a layout, or a type that failed; the types in `seen` were
    /// looked at already. The types of fields and payload values are not
    /// normalized, so an array length that failed is found here: `ty` is
    /// in error wherever `normalize` would make it `{error}`. So is a
    /// struct whose `#[align(N)]` failed (ADR 0048), a peripheral type
    /// that `peripherals` counts (ADR 0058), and a bit-field struct that
    /// reaches a failed value (ADR 0059).
    fn lacks_layout(
        &mut self,
        ty: hir::Ty,
        seen: &mut Vec<Composite>,
        peripherals: PeripheralFailures,
    ) -> bool {
        match ty {
            hir::Ty::Ptr(ptr) => self.lacks_layout(*ptr.pointee, seen, peripherals),
            // A struct or enum lays out a field whose length failed as
            // empty, keeping its own layout (see `layout::of`), so a
            // length is checked wherever it is reached, behind pointers
            // too.
            hir::Ty::Array(elem, len) => {
                self.lacks_layout(*elem, seen, peripherals) || self.array_len(len).is_none()
            }
            hir::Ty::Slice(elem) => self.lacks_layout(*elem, seen, peripherals),
            hir::Ty::Struct(s) => {
                if seen.contains(&Composite::Struct(s.id)) {
                    return false;
                }
                seen.push(Composite::Struct(s.id));
                // Its own `#[align(N)]` failed: it keeps a layout, like a
                // struct with a field whose length failed.
                match self.struct_layout(s.id) {
                    None => return true,
                    Some(layout) if layout.align_failed => return true,
                    Some(_) => {}
                }
                let hir = self.hir;
                hir.struct_(s.id)
                    .fields
                    .iter()
                    .any(|field| self.lacks_layout(field.ty, seen, peripherals))
            }
            hir::Ty::Enum(e) if e.payloads => {
                if seen.contains(&Composite::Enum(e.id)) {
                    return false;
                }
                seen.push(Composite::Enum(e.id));
                if self.enum_layout(e.id).is_none() {
                    return true;
                }
                self.enum_def(e.id)
                    .variants
                    .iter()
                    .flat_map(|variant| &variant.payload)
                    .any(|value| self.lacks_layout(value.ty, seen, peripherals))
            }
            hir::Ty::Applied(_) => {
                let ty = self.normalize(ty);
                self.lacks_layout(ty, seen, peripherals)
            }
            hir::Ty::Peripheral(p) => match peripherals {
                PeripheralFailures::All => self.peripheral_type_failed(p.id),
                PeripheralFailures::Explained => self.peripheral_explained(p.id),
                PeripheralFailures::Ignored => false,
            },
            hir::Ty::BitStruct(s) => self.bit_struct_failed(s.id),
            // Reported, such as a field of type `!` (ADR 0034).
            hir::Ty::Error => true,
            _ => false,
        }
    }

    /// What dumps leave out of the MIR that `into_mir` makes: the bodies,
    /// structs, enums, peripheral types, peripherals, statics and byte
    /// strings of `core` (ADR 0052).
    pub fn core_parts(&self) -> LeftOut {
        let hir = self.hir;
        LeftOut {
            bodies: hir.fns.iter().map(|f| hir.in_core(f.module)).collect(),
            structs: hir.structs.iter().map(|s| hir.in_core(s.module)).collect(),
            bit_structs: (hir.bit_structs.iter())
                .map(|s| hir.in_core(s.module))
                .collect(),
            enums: hir.enums.iter().map(|e| hir.in_core(e.module)).collect(),
            peripheral_types: (hir.peripheral_types.iter())
                .map(|p| hir.in_core(p.module))
                .collect(),
            peripherals: (hir.peripherals.iter())
                .map(|p| hir.in_core(p.module))
                .collect(),
            statics: (hir.statics.iter().map(|s| hir.in_core(s.module)))
                .chain(self.byte_strings_in_core.iter().copied())
                .collect(),
        }
    }

    /// The MIR of the whole program, in source order. The program must have
    /// been lowered without errors.
    pub fn into_mir(mut self) -> Mir {
        let structs = (0..self.hir.structs.len())
            .map(|id| self.mir_struct(StructId(id as u32)))
            .collect();
        for id in 0..self.hir.fns.len() {
            self.mir_body(FnId(id as u32));
        }
        // The declared enums, then the `Option(T)` and `Result(T, E)` the
        // program uses, by name (ADR 0045).
        let ids: Vec<EnumId> = (0..self.hir.enums.len())
            .map(|id| EnumId(id as u32))
            .chain(self.instances_by_name())
            .collect();
        let enums = ids
            .into_iter()
            .map(|id| {
                let def = self.enum_def(id);
                mir::Enum {
                    name: def.name.clone(),
                    int: def.ty.int,
                    def: def.ty.payloads.then(|| self.mir_enum(id)),
                    variants: def
                        .variants
                        .iter()
                        .enumerate()
                        .map(|(index, variant)| {
                            let value = self
                                .discriminant(id, index as u32)
                                .expect("every variant of a program without errors has a value");
                            (variant.name.clone(), value)
                        })
                        .collect(),
                }
            })
            .collect();
        let bit_structs = (0..self.hir.bit_structs.len())
            .map(|id| {
                let id = BitStructId(id as u32);
                let layout = self
                    .bit_layout(id)
                    .expect("every bit-field struct of a program without errors has its bits");
                let hir = self.hir;
                let def = hir.bit_struct(id);
                let fields = (def.fields.iter().zip(&layout.fields))
                    .map(|(field, bits)| {
                        let (low, high) =
                            bits.expect("every field of a program without errors has its bits");
                        let ty = self.normalize(field.ty);
                        mir::BitField {
                            name: field.name.clone(),
                            ty: self.mir_ty(ty),
                            low,
                            high,
                        }
                    })
                    .collect();
                mir::BitStruct {
                    name: def.path.clone(),
                    int: def.ty.int,
                    fields,
                }
            })
            .collect();
        let mut statics: Vec<mir::Static> = (0..self.hir.statics.len())
            .map(|id| {
                let id = StaticId(id as u32);
                // An `extern static` is only declared.
                let value = match self.hir.static_(id).init {
                    Some(_) => self
                        .static_value(id)
                        .expect("every static of a program without errors has a value"),
                    None => ConstValue {
                        bytes: Vec::new(),
                        relocations: Vec::new(),
                    },
                };
                mir::Static {
                    def: self.mir_static(id),
                    bytes: value.bytes,
                    align: self
                        .static_align(id)
                        .expect("every static of a program without errors has an alignment"),
                    relocations: value.relocations,
                }
            })
            .collect();
        // Numbered after the statics, so each is at the index of its id.
        statics.extend(self.byte_strings.iter().map(|&def| mir::Static {
            def,
            bytes: def.def().byte_string.clone().expect("a byte string"),
            align: 1,
            relocations: Vec::new(),
        }));
        let peripheral_types = (0..self.hir.peripheral_types.len())
            .map(|id| {
                let id = PeripheralTypeId(id as u32);
                self.peripheral_layout(id)
                    .expect("every peripheral type of a program without errors has a layout");
                self.mir_peripheral_type(id)
            })
            .collect();
        let peripherals = (0..self.hir.peripherals.len())
            .map(|id| self.mir_peripheral(PeripheralId(id as u32)))
            .collect();
        let memory = self.hir.memory.as_ref().map(|_| {
            self.memory_map()
                .expect("every value of a program without errors is computed")
        });
        Mir {
            structs,
            bit_structs,
            enums,
            peripheral_types,
            peripherals,
            data: self.data,
            statics,
            bodies: self
                .bodies
                .into_iter()
                .map(|body| match body {
                    State::Done(Some(body)) => Rc::unwrap_or_clone(body),
                    _ => unreachable!("every body of a well-typed program is built"),
                })
                .collect(),
            entry: BodyId(self.hir.entry.0),
            panic: BodyId(self.hir.panic.0),
            memory,
        }
    }
}
