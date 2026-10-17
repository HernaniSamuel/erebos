//! On-demand compilation (ADR 0028). Type checking, layouts, MIR and
//! compile-time values are computed per item when first needed, remembered,
//! and protected against cycles; the driver then asks for every item, so
//! the whole program is still compiled.

use std::collections::HashMap;
use std::rc::Rc;

use crate::diag::{Diagnostic, Diagnostics};
use crate::eval::{self, ConstValue, Origin};
use crate::hir::{
    self, ArrayLen, Comptime, ConstId, EnumId, ExprId, ExprKind, FnId, Hir, StaticId, StructId,
};
use crate::layout::{self, Layout, MAX_ALIGN, MAX_SIZE, StructLayout};
use crate::mir::{self, BodyId, ConstData, DataId, Field, Mir, Ptr, StaticRef, StructRef};
use crate::span::Span;
use crate::ty::{self, Tables};

/// Where a piece of constant data comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DataSource {
    Const(ConstId),
    Comptime(ExprId),
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
    /// A `usize` computed at compile time, such as an array length.
    Value(FnId),
    /// The value of a variant (ADR 0035).
    Discriminant(EnumId, u32),
    Layout(StructId),
}

/// How diagnostics name a body.
fn describe(hir: &Hir, f: &hir::Fn) -> String {
    match f.comptime {
        Some(Comptime::Const(id)) => format!("the constant `{}`", hir.const_(id).name),
        Some(Comptime::Static(id)) => format!("the static `{}`", hir.static_(id).name),
        Some(Comptime::Block) => "this `comptime` block".to_owned(),
        Some(Comptime::Value) => "this value".to_owned(),
        Some(Comptime::Discriminant(id, index)) => variant_name(hir, id, index),
        None => format!("the function `{}`", f.name),
    }
}

/// How diagnostics name a variant: "the variant `Mode::Run`".
fn variant_name(hir: &Hir, id: EnumId, index: u32) -> String {
    let def = hir.enum_(id);
    format!(
        "the variant `{}::{}`",
        def.name, def.variants[index as usize].name
    )
}

/// An item being computed, as cycles report it.
struct InProgress {
    item: Item,
    span: Span,
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

    /// The MIR type of each struct.
    mir_structs: Vec<Option<StructRef>>,
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
    /// The alignment of each static; `None` if its type has no layout or
    /// its `#[align]` is invalid (reported).
    static_aligns: Vec<Option<Option<u64>>>,
    /// The MIR view of each static.
    mir_statics: Vec<Option<StaticRef>>,
    /// Each byte string used, as a static numbered after the statics, in
    /// the order MIR building meets them (ADR 0031).
    byte_strings: Vec<StaticRef>,
    /// The index in `byte_strings` of the byte string at each expression.
    byte_string_ids: HashMap<ExprId, usize>,
    /// The value of each `comptime` expression.
    comptime: HashMap<ExprId, State<Option<ConstValue>>>,
    /// The value of each array length, repeat count and `#[align]`
    /// argument computed at compile time.
    values: HashMap<FnId, Option<u64>>,
    /// Read-only data computed at compile time, and what it came from.
    data: Vec<ConstData>,
    data_ids: HashMap<DataSource, DataId>,
    /// Items being computed, outermost first.
    in_progress: Vec<InProgress>,
}

impl<'h> Queries<'h> {
    pub fn new(hir: &'h Hir) -> Queries<'h> {
        Queries {
            hir,
            diags: Diagnostics::default(),
            tables: Tables::new(hir.expr_count),
            checked: hir.fns.iter().map(|_| State::NotStarted).collect(),
            layouts: hir.structs.iter().map(|_| State::NotStarted).collect(),

            mir_structs: vec![None; hir.structs.len()],
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
            byte_string_ids: HashMap::new(),
            comptime: HashMap::new(),
            values: HashMap::new(),
            data: Vec::new(),
            data_ids: HashMap::new(),
            in_progress: Vec::new(),
        }
    }

    /// Checks every item, in source order, whether or not something else
    /// needed it.
    pub fn check_all(&mut self) {
        let hir = self.hir;
        ty::report_never_in_memory(self);
        ty::report_unsized_types(self);
        for id in 0..hir.structs.len() {
            self.struct_layout(StructId(id as u32));
        }
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
        if !self.start(Item::Check(id), f.name_span, describe(hir, f)) {
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
        let Some(at) = self.in_progress.iter().position(|p| p.item == item) else {
            self.in_progress.push(InProgress {
                item,
                span,
                what,
                cycle_reported: false,
            });
            return true;
        };
        // Structs laid out inside each other, with no evaluation between:
        // containment by value (ADR 0021).
        let structs: Option<Vec<StructId>> = self.in_progress[at..]
            .iter()
            .map(|p| match p.item {
                Item::Layout(id) => Some(id),
                _ => None,
            })
            .collect();
        if let Some(structs) = structs {
            self.report_containment_cycle(&structs);
            return false;
        }
        // Asked for again within the cycle reported already.
        if std::mem::replace(&mut self.in_progress[at].cycle_reported, true) {
            return false;
        }
        let InProgress {
            span: first,
            what: first_what,
            ..
        } = &self.in_progress[at];
        let mut diag = Diagnostic::error(
            "ty::cycle",
            format!("{first_what} depends on its own value"),
        )
        .label(*first, "needed while computing itself");
        // Checking a body and computing its value share a span.
        let mut seen = vec![*first];
        for InProgress { span, what, .. } in &self.in_progress[at + 1..] {
            if span.file == first.file && !seen.contains(span) {
                seen.push(*span);
                diag = diag.label(*span, format!("through {what}"));
            }
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
        let what = format!("the constant `{}`", def.name);
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
        let slot = (id.0 as usize, index as usize);
        if let State::Done(value) = self.discriminants[slot.0][slot.1] {
            return value;
        }
        let hir = self.hir;
        let def = hir.enum_(id);
        let variant = &def.variants[slot.1];
        let what = variant_name(hir, id, index);
        if !self.start(Item::Discriminant(id, index), variant.span, what.clone()) {
            return None;
        }
        self.discriminants[slot.0][slot.1] = State::InProgress;
        let value = self.mir_body(variant.value).and_then(|body| {
            let origin = Origin {
                what,
                span: variant.span,
            };
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
        if let Some(ok) = self.enums_ok[id.0 as usize] {
            return ok;
        }
        let hir = self.hir;
        let def = hir.enum_(id);
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
                    self.diags.push(
                        Diagnostic::error(
                            "ty::duplicate-discriminant",
                            format!(
                                "`{0}::{1}` has the same value as `{0}::{2}`",
                                def.name, variant.name, first.name
                            ),
                        )
                        .label(variant.span, format!("is {shown}"))
                        .label(first.span, format!("is {shown} too"))
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
        let what = format!("the static `{}`", def.name);
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
    /// layout or the alignment is invalid, reported once.
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
            Some(layout) => match def.align {
                Some((len, span)) => self.array_len(len).and_then(|wanted| {
                    self.checked_align(wanted, span, layout.align, &format!("`{ty}`"))
                }),
                None => Some(layout.align),
            },
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
        let id = match len {
            ArrayLen::Known(len) => return Some(len),
            ArrayLen::Comptime(id) => id,
        };
        if let Some(value) = self.values.get(&id) {
            return *value;
        }
        let span = self.hir.fn_(id).span;
        let what = "this value".to_owned();
        if !self.start(Item::Value(id), span, what.clone()) {
            return None;
        }
        let value = self.mir_body(id).and_then(|body| {
            let origin = Origin { what, span };
            eval::evaluate(self, body, origin)
        });
        self.finish(Item::Value(id));
        let value = value.map(|value| value.int(crate::target::POINTER_BYTES as usize));
        self.values.insert(id, value);
        value
    }

    /// `ty` with every array length known, or `{error}` if one failed
    /// (reported).
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
            ty => ty,
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
    /// hold. A struct without a layout, already reported, counts as empty.
    pub fn layout_of(&mut self, ty: hir::Ty) -> Option<Layout> {
        let ty = self.normalize(ty);
        let empty = Layout { size: 0, align: 1 };
        layout::of(ty, &mut |id| {
            Some(self.struct_layout(id).map_or(empty, |s| s.layout))
        })
    }

    /// Lays out a struct with C rules (ADR 0022), reporting structs that
    /// contain themselves, too large ones, and misused attributes.
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
            let field_layout = layout::of(field_ty, &mut |id| {
                let inner = self.struct_layout(id).map(|s| s.layout);
                inner_failed |= inner.is_none();
                inner
            });
            let Some(field_layout) = field_layout else {
                // A struct inside without a layout was reported; otherwise
                // the field alone is too large.
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
        let wanted = match def.align {
            Some((len, span)) => self.array_len(len).map(|wanted| (wanted, span)),
            None => None,
        };
        if let Some((wanted, span)) = wanted
            && let Some(wanted) =
                self.checked_align(wanted, span, align, &format!("`{}`", def.name))
        {
            align = wanted;
        }
        let size = end.and_then(|end| layout::align_up(end, align));

        self.finish(Item::Layout(id));
        let layout = match (complete, size) {
            (true, Some(size)) if size <= MAX_SIZE => Some(StructLayout {
                layout: Layout { size, align },
                offsets,
                over_aligned,
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

    /// Whether `ty` contains a struct with `#[align(N)]`, among the structs
    /// already laid out.
    fn over_aligned(&self, ty: hir::Ty) -> bool {
        match ty {
            hir::Ty::Struct(s) => matches!(
                &self.layouts[s.id.0 as usize],
                State::Done(Some(layout)) if layout.over_aligned
            ),
            hir::Ty::Array(elem, _) => self.over_aligned(*elem),
            _ => false,
        }
    }

    /// Structs found while laying themselves out contain each other by
    /// value; the first is the one found again.
    fn report_containment_cycle(&mut self, cycle: &[StructId]) {
        let hir = self.hir;
        let first = hir.struct_(cycle[0]);
        let message = if cycle.len() == 1 {
            format!("`{}` contains itself", first.name)
        } else {
            let mut path: Vec<&str> = cycle
                .iter()
                .map(|s| hir.struct_(*s).name.as_str())
                .collect();
            path.push(&first.name);
            format!("these structs contain each other: {}", path.join(" -> "))
        };
        let mut diag = Diagnostic::error("ty::infinite-size", message)
            .label(first.name_span, "would have infinite size")
            .help(format!(
                "contain a pointer instead, such as `*mut {}` (ADR 0021)",
                first.name
            ));
        for other in &cycle[1..] {
            diag = diag.label(hir.struct_(*other).name_span, "part of the cycle");
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
            def.name.clone(),
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
        let id = mir::StaticId((self.hir.statics.len() + index) as u32);
        let s = StaticRef::byte_string(id, bytes.to_vec());
        self.byte_strings.push(s);
        self.byte_string_ids.insert(expr, index);
        s
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
            // Stored as its integer type, which is all MIR sees (ADR 0035).
            hir::Ty::Enum(e) => mir::Ty::Int(e.int),
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
                format!("the function `{}`", f.name),
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
    /// none, or of a type that failed, both reported where they failed.
    /// Like a body that does not type check, it is then left out without a
    /// new diagnostic.
    fn needs_missing_layout(&mut self, id: FnId) -> bool {
        let f = self.hir.fn_(id);
        let mut types = vec![f.ret];
        types.extend(f.locals.iter().map(|local| local.ty));
        if let Some(body) = &f.body {
            body.walk(&mut |e| {
                types.push(self.tables.expr_ty(e.id));
                if let ExprKind::SizeOf(ty) | ExprKind::AlignOf(ty) | ExprKind::OffsetOf(ty, _, _) =
                    &e.kind
                {
                    types.push(*ty);
                }
            });
        }
        let mut seen = Vec::new();
        types.into_iter().any(|ty| self.lacks_layout(ty, &mut seen))
    }

    /// Whether the MIR type of `ty` needs a struct without a layout or a
    /// type that failed; the structs in `seen` were looked at already.
    fn lacks_layout(&mut self, ty: hir::Ty, seen: &mut Vec<StructId>) -> bool {
        match ty {
            hir::Ty::Ptr(ptr) => self.lacks_layout(*ptr.pointee, seen),
            hir::Ty::Array(elem, _) | hir::Ty::Slice(elem) => self.lacks_layout(*elem, seen),
            hir::Ty::Struct(s) => {
                if seen.contains(&s.id) {
                    return false;
                }
                seen.push(s.id);
                if self.struct_layout(s.id).is_none() {
                    return true;
                }
                let hir = self.hir;
                hir.struct_(s.id)
                    .fields
                    .iter()
                    .any(|field| self.lacks_layout(field.ty, seen))
            }
            // Reported, such as a field of type `!` (ADR 0034).
            hir::Ty::Error => true,
            _ => false,
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
        let hir = self.hir;
        let enums = hir
            .enums
            .iter()
            .enumerate()
            .map(|(id, def)| mir::Enum {
                name: def.name.clone(),
                int: def.ty.int,
                variants: def
                    .variants
                    .iter()
                    .enumerate()
                    .map(|(index, variant)| {
                        let value = self
                            .discriminant(EnumId(id as u32), index as u32)
                            .expect("every variant of a program without errors has a value");
                        (variant.name.clone(), value)
                    })
                    .collect(),
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
        Mir {
            structs,
            enums,
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
        }
    }
}
