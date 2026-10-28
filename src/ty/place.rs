//! Checking places (ADR 0021): what an expression names in memory, through
//! pointers, fields and registers (ADR 0058), whether it may be written,
//! and `&raw const` / `&raw mut`.

use crate::diag::Diagnostic;
use crate::hir::{
    self, AccessMode, BitStructTy, EnumTy, Expr, ExprKind, Fn, Hir, IntTy, ModuleId, PeripheralTy,
    PeripheralTypeId, Ty,
};
use crate::span::Span;
use crate::target;

use super::{
    Checker, Immutable, NeverHelp, PlaceInfo, Register, never_in_memory, payload_values_help,
    whole_peripheral, whole_slice,
};

impl Checker<'_, '_> {
    /// The place a pointer of type `ty` leads to, for `*` and `[]`. A
    /// pointer of type `!` is never computed, so neither is the place.
    /// What a slice points to is a place of type `[T]`, which can only be
    /// indexed or passed to `&raw` (ADR 0031).
    fn through_pointer(&mut self, ty: Ty, pointer: &Expr, error: Diagnostic) -> PlaceInfo {
        match ty {
            Ty::Ptr(ptr) => PlaceInfo::through(ptr, pointer.span),
            Ty::Never => PlaceInfo {
                ty: Ty::Never,
                ..PlaceInfo::ERROR
            },
            Ty::Error => PlaceInfo::ERROR,
            _ => {
                self.q.diags.push(error);
                PlaceInfo::ERROR
            }
        }
    }

    /// Checks a read or write of a place, as `kind` says: `ReadWrite` for a
    /// compound assignment. A volatile access is exactly one instruction
    /// (ADR 0023), so it cannot be wider than the target's widest access,
    /// nor a whole aggregate. A register is volatile too, and must allow
    /// the access (ADR 0058), an assignment to one of its fields reading it
    /// as well (ADR 0060); its type was checked with its peripheral
    /// type, so it gets no volatile checks of its own. A bit field is
    /// accessed with the whole value of its bit-field struct, which the
    /// volatile checks see (ADR 0059).
    pub(super) fn access(&mut self, place: &PlaceInfo, span: Span, kind: AccessMode) {
        if let Some(register) = place.register {
            let field_assignment = place.bit_field.is_some() && kind == AccessMode::Write;
            self.register_access(register, span, kind, field_assignment);
            return;
        }
        let whole = PlaceInfo {
            ty: place.bit_field.map_or(place.ty, Ty::BitStruct),
            ..*place
        };
        let place = &whole;
        let volatile = place.via.is_some_and(|ptr| ptr.volatile);
        if volatile && !place.ty.is_aggregate() && self.may_be_misaligned(place) {
            self.q.diags.push(
                Diagnostic::error(
                    "ty::volatile-packed-field",
                    format!("a volatile `{}` here may be misaligned", place.ty),
                )
                .label(span, "reached through a field of a `#[packed]` struct")
                .help("it would be split into several accesses; register blocks use the default layout, with explicit padding fields (ADR 0022, 0023)"),
            );
        }
        if volatile && place.ty.slice_elem().is_some() {
            self.q.diags.push(
                Diagnostic::error(
                    "ty::volatile-aggregate",
                    format!("a volatile `{}` cannot be accessed as a whole", place.ty),
                )
                .label(span, "a slice is an address and a length, two accesses")
                .help("a slice is an aggregate, whose bus accesses would be up to the compiler (ADR 0023, 0031)"),
            );
        } else if volatile && place.ty.is_aggregate() {
            // An enum with payloads has no fields to access (ADR 0039).
            let (label, help) = match place.ty {
                Ty::Enum(_) => (
                    "an enum with payloads is an aggregate, without fields to access one at a time",
                    "the width and number of the bus accesses would be up to the compiler; device registers are described with structs, integers and enums without payloads (ADR 0023, 0039)",
                ),
                _ => (
                    "access its elements or fields one at a time",
                    "the width and number of the bus accesses would be up to the compiler (ADR 0023)",
                ),
            };
            self.q.diags.push(
                Diagnostic::error(
                    "ty::volatile-aggregate",
                    format!("a volatile `{}` cannot be accessed as a whole", place.ty),
                )
                .label(span, label)
                .help(help),
            );
        }
        // An enum without payloads and a bit-field struct are accessed as
        // their integer types (ADR 0023, 0035, 0059).
        if let (
            true,
            Ty::Int(int)
            | Ty::Enum(EnumTy {
                int,
                payloads: false,
                ..
            })
            | Ty::BitStruct(BitStructTy { int, .. }),
        ) = (volatile, place.ty)
            && int.bits() > target::WIDEST_ACCESS_BITS
        {
            let half = target::WIDEST_ACCESS_BITS;
            // Nothing converts to an enum (ADR 0035), so its halves would
            // not make one.
            let help = match place.ty {
                Ty::Enum(_) => format!(
                    "an enum is accessed as its integer type, `{int}`; declare it on an integer type of at most {half} bits if its values fit (ADR 0023, 0035)"
                ),
                // Its halves would not make one either: it converts only
                // from its own integer type (ADR 0059).
                Ty::BitStruct(_) => format!(
                    "a bit-field struct is accessed as its integer type, `{int}`, with all its fields at once; declare it on an integer type of at most {half} bits, or split its fields between two such structs (ADR 0023, 0059)"
                ),
                _ => format!(
                    "access it as `u{half}` halves, in the order the device needs (ADR 0023)"
                ),
            };
            self.q.diags.push(
                Diagnostic::error(
                    "ty::volatile-too-wide",
                    format!(
                        "a volatile `{}` cannot be accessed in one instruction",
                        place.ty
                    ),
                )
                .label(span, format!("the widest single access is {half} bits"))
                .help(help),
            );
        }
    }

    /// Checks that `register`, accessed at `span` as `kind` says, allows
    /// it: a `write` register is never read and a `read` one never written,
    /// and a compound assignment does both (ADR 0058), as does a
    /// `field_assignment`, an assignment to one of its bit fields, which
    /// reads the register to keep its other bits (ADR 0060).
    fn register_access(
        &mut self,
        register: Register,
        span: Span,
        kind: AccessMode,
        field_assignment: bool,
    ) {
        let def = self.hir.peripheral_type(register.id);
        let reg = &def.registers[register.index];
        let reads = field_assignment || !matches!(kind, AccessMode::Write);
        let writes = !matches!(kind, AccessMode::Read);
        let label = match kind {
            AccessMode::Read => "read here",
            AccessMode::Write if field_assignment => "read and written by this field assignment",
            AccessMode::Write => "written here",
            AccessMode::ReadWrite => "read and written by this compound assignment",
        };
        if reads && reg.access == AccessMode::Write {
            let help = if field_assignment {
                "a field assignment reads the register to keep its other bits, and a `write` register is never read: assign it a whole value, as a literal with `..`; declare it `read write` if the device also lets it be read (ADR 0058, 0060)"
            } else {
                "a `write` register can only be assigned with `=`, since a compound assignment reads it first; declare it `read write` if the device also lets it be read (ADR 0058)"
            };
            self.q.diags.push(
                Diagnostic::error(
                    "ty::read-of-write-only",
                    format!(
                        "the register `{}` of `{}` is write-only",
                        reg.name, def.path
                    ),
                )
                .label(span, label)
                .label(reg.span, "declared `write`")
                .help(help),
            );
        }
        if writes && reg.access == AccessMode::Read {
            self.q.diags.push(
                Diagnostic::error(
                    "ty::write-to-read-only",
                    format!("the register `{}` of `{}` is read-only", reg.name, def.path),
                )
                .label(span, label)
                .label(reg.span, "declared `read`")
                .help("a `read` register can only be read; declare it `read write` if the device also lets it be written (ADR 0058)"),
            );
        }
    }

    /// Types a place expression: a local, a static, a peripheral, a byte
    /// string, `*pointer`, `base[index]`, `base.field` or `base.register`.
    /// Returns `None`, having typed nothing, if `expr` is not a place.
    pub(super) fn place_info(&mut self, f: &Fn, expr: &Expr) -> Option<PlaceInfo> {
        let info = match &expr.kind {
            ExprKind::Local(id) => {
                let local = f.local(*id);
                PlaceInfo {
                    ty: self.norm(local.ty),
                    immutable: (!local.mutable).then_some(Immutable::Local(*id)),
                    via: None,
                    misaligned: false,
                    constant: false,
                    register: None,
                    bit_field: None,
                }
            }
            ExprKind::ByteString(bytes) => PlaceInfo {
                ty: Ty::array(Ty::Int(IntTy::U8), bytes.len() as u64),
                immutable: Some(Immutable::ByteString(expr.span)),
                via: None,
                misaligned: false,
                constant: false,
                register: None,
                bit_field: None,
            },
            ExprKind::Static(id) => {
                let def = self.hir.static_(*id);
                PlaceInfo {
                    ty: self.norm(def.ty),
                    immutable: (!def.mutable).then_some(Immutable::Static(*id)),
                    via: None,
                    misaligned: false,
                    constant: false,
                    register: None,
                    bit_field: None,
                }
            }
            // Not `mut`: what may be written is its registers' business, so
            // `&raw mut` of it is allowed (ADR 0058).
            ExprKind::Peripheral(id) => PlaceInfo {
                ty: self.hir.peripheral(*id).ty,
                immutable: None,
                via: None,
                misaligned: false,
                constant: false,
                register: None,
                bit_field: None,
            },
            ExprKind::Deref(pointer) => {
                let ty = self.check(f, pointer, None);
                let error = Diagnostic::error(
                    "ty::deref-non-pointer",
                    format!("cannot dereference `{ty}`"),
                )
                .label(pointer.span, "not a pointer");
                self.through_pointer(ty, pointer, error)
            }
            ExprKind::Field(base, name, name_span) => {
                let (ty, base_place) = self.base(f, base);
                self.field(f, expr, ty, base_place, name, *name_span)
            }
            ExprKind::Index(base, index, _) => {
                // An array is indexed where it is; anything else is read as
                // a value, which must be a pointer.
                let base_place = self.place_info(f, base);
                let ty = match base_place {
                    Some(info) => info.ty,
                    None => self.check(f, base, None),
                };
                self.check(f, index, Some(Ty::Int(IntTy::Usize)));
                if let Ty::Array(elem, _) = ty {
                    let base_place = base_place.unwrap_or_else(|| PlaceInfo::temporary(ty, base));
                    PlaceInfo {
                        ty: *elem,
                        ..base_place
                    }
                } else if let (Ty::Slice(elem), Some(base_place)) = (ty, base_place) {
                    // `(*s)[i]`, checked against the length (ADR 0031): the
                    // element is reached through the slice, so it is
                    // volatile if the slice is.
                    if self.q.layout_of(*elem).is_none() {
                        self.too_large(
                            *elem,
                            base.span,
                            "the distance between elements does not fit in the address space",
                        );
                    }
                    PlaceInfo {
                        ty: *elem,
                        ..base_place
                    }
                } else if let Ty::Ptr(ptr) = ty
                    && let Ty::Slice(elem) = *ptr.pointee
                {
                    if let Some(info) = base_place {
                        self.access(&info, base.span, AccessMode::Read);
                    }
                    // Indexing a pointer steps over whole pointees, and `[T]`
                    // has no size (ADR 0021, 0031).
                    let first = Ty::ptr(ptr.mutable, ptr.volatile, *elem);
                    self.q.diags.push(
                        Diagnostic::error(
                            "ty::slice-pointer-index",
                            format!("cannot index the slice `{ty}` itself"),
                        )
                        .label(base.span, "a slice, not a pointer to its elements")
                        .help(format!(
                            "index what it points to, checked against its length: `(*s)[i]`; or, unchecked, a pointer to its first element: `(s as {first})[i]` (ADR 0031)"
                        )),
                    );
                    PlaceInfo::ERROR
                } else if let Ty::Ptr(ptr) = ty
                    && let Ty::Peripheral(_) = *ptr.pointee
                {
                    if let Some(info) = base_place {
                        self.access(&info, base.span, AccessMode::Read);
                    }
                    // Indexing steps over whole pointees, and a peripheral
                    // type has no size (ADR 0058).
                    self.q.diags.push(
                        Diagnostic::error(
                            "ty::peripheral-index",
                            format!("cannot index `{ty}`, a pointer to a peripheral type"),
                        )
                        .label(base.span, format!("`{}` has no size, so no stride", ptr.pointee))
                        .help(format!(
                            "a pointer to a peripheral type reaches one device, whose registers are reached as in `(*u).NAME`; for another device of type `{}`, use a pointer to it (ADR 0058)",
                            ptr.pointee
                        )),
                    );
                    PlaceInfo::ERROR
                } else {
                    // A peripheral is not read to be indexed: it is no
                    // value, and no array either.
                    if let Some(info) = base_place
                        && !matches!(info.ty, Ty::Peripheral(_))
                    {
                        self.access(&info, base.span, AccessMode::Read);
                    }
                    if let Ty::Ptr(ptr) = ty
                        && self.q.layout_of(*ptr.pointee).is_none()
                    {
                        self.too_large(
                            *ptr.pointee,
                            base.span,
                            "the distance between elements does not fit in the address space",
                        );
                    }
                    let error = Diagnostic::error(
                        "ty::cannot-index",
                        format!("cannot index a value of type `{ty}`"),
                    )
                    .label(base.span, "not an array or a pointer");
                    // Unchecked, as in C (ADR 0021).
                    self.through_pointer(ty, base, error)
                }
            }
            _ => return None,
        };
        self.q.tables.expr_types[expr.id.0 as usize] = info.ty;
        Some(info)
    }

    /// The type of the base of a field access or of indexing, and its
    /// place: the base's own if it is a place expression, or else a
    /// temporary holding its value.
    fn base(&mut self, f: &Fn, base: &Expr) -> (Ty, PlaceInfo) {
        match self.place_info(f, base) {
            Some(info) => (info.ty, info),
            None => {
                let ty = self.check(f, base, None);
                (ty, PlaceInfo::temporary(ty, base))
            }
        }
    }

    /// `base.name`, where `base` has type `ty` and place `base_place`, in
    /// the body `f`, whose module must see the field (ADR 0050), or the
    /// register if `ty` is a peripheral type (ADR 0058).
    pub(super) fn field(
        &mut self,
        f: &Fn,
        expr: &Expr,
        ty: Ty,
        base_place: PlaceInfo,
        name: &str,
        name_span: Span,
    ) -> PlaceInfo {
        let hir = self.hir;
        let s = match ty {
            Ty::Struct(s) => s,
            Ty::BitStruct(s) => return self.bit_field(f, expr, s, base_place, name, name_span),
            Ty::Never => {
                return PlaceInfo {
                    ty: Ty::Never,
                    ..PlaceInfo::ERROR
                };
            }
            Ty::Error => return PlaceInfo::ERROR,
            Ty::Peripheral(p) => return self.register(f, expr, p, base_place, name, name_span),
            other => {
                let mut diag =
                    Diagnostic::error("ty::no-fields", format!("`{other}` has no fields"))
                        .label(name_span, "no such field");
                if let Ty::Ptr(ptr) = other
                    && let Ty::Struct(_) | Ty::BitStruct(_) | Ty::Peripheral(_) = *ptr.pointee
                {
                    diag = diag.help(format!(
                        "dereference the pointer first: `(*p).{name}` (ADR 0021)"
                    ));
                }
                if (other.slice_elem().is_some() || matches!(other, Ty::Slice(_))) && name == "len"
                {
                    diag = diag.help("the length of a slice `s` is `len(s)` (ADR 0031)");
                }
                self.q.diags.push(payload_values_help(diag, other));
                return PlaceInfo::ERROR;
            }
        };
        let def = hir.struct_(s.id);
        let fields = Fields::of_struct(hir, s.id, f.module);
        let Some(index) = self.named_field(&fields, name, name_span) else {
            return PlaceInfo::ERROR;
        };
        self.q.tables.fields.insert(expr.id, index as u32);
        PlaceInfo {
            ty: self.norm(def.fields[index].ty),
            misaligned: base_place.misaligned || def.packed,
            ..base_place
        }
    }

    /// The index of the field `name` of `fields`, named at `name_span`:
    /// `ty::unknown-field` if there is none, and `ty::private-field` if the
    /// module the fields are named in does not see it (ADR 0050). A field
    /// that is not visible is still the one named, so nothing more is
    /// reported about it.
    fn named_field(&mut self, fields: &Fields, name: &str, name_span: Span) -> Option<usize> {
        let Some(index) = fields.fields.iter().position(|field| field.name == name) else {
            self.q.diags.push(unknown_field(fields, name, name_span));
            return None;
        };
        if !fields.fields[index].visible {
            self.q
                .diags
                .push(private_field(self.hir, fields, index, name_span));
        }
        Some(index)
    }

    /// `base.name`, where `base` is a value of the bit-field struct `s` at
    /// `base_place` (ADR 0059): the field `name`, which the module of `f`
    /// must see, as any field (ADR 0050). The place is the field, of its
    /// type, read and written with the whole value: it can be written
    /// where `base_place` can, and has no address.
    fn bit_field(
        &mut self,
        f: &Fn,
        expr: &Expr,
        s: BitStructTy,
        base_place: PlaceInfo,
        name: &str,
        name_span: Span,
    ) -> PlaceInfo {
        let hir = self.hir;
        let fields = Fields::of_bit_struct(hir, s.id, f.module);
        let Some(index) = self.named_field(&fields, name, name_span) else {
            return PlaceInfo::ERROR;
        };
        self.q
            .tables
            .bit_fields
            .insert(expr.id, (s.id, index as u32));
        PlaceInfo {
            ty: self.norm(hir.bit_struct(s.id).fields[index].ty),
            bit_field: Some(s),
            ..base_place
        }
    }

    /// `offset_of(S, name)` where `S` is the bit-field struct `s`: a bit
    /// field has bits, not a byte offset (ADR 0059). An unknown or private
    /// field is reported as for any `offset_of`.
    pub(super) fn bit_field_offset(&mut self, f: &Fn, s: BitStructTy, name: &str, name_span: Span) {
        let hir = self.hir;
        let fields = Fields::of_bit_struct(hir, s.id, f.module);
        let Some(index) = self.named_field(&fields, name, name_span) else {
            return;
        };
        let field = &hir.bit_struct(s.id).fields[index];
        self.q.diags.push(
            Diagnostic::error(
                "ty::bit-field-offset",
                format!("the field `{name}` of `{}` has no byte offset", s.name),
            )
            .label(name_span, "a bit field")
            .label(field.bits_span(), format!("its bits in `{}`", s.int))
            .help(format!(
                "a field of a bit-field struct is a range of bits of its `{}`, not a place in memory; the struct's size and alignment are its integer's (ADR 0059)",
                s.int
            )),
        );
    }

    /// `base.name`, where `base` is a place of the peripheral type `p`,
    /// reached from a peripheral or through a pointer (ADR 0058): the
    /// register `name`, which the module of `f` must see, as a field
    /// (ADR 0050). The place is the register, of its type, with the
    /// mutability of `base_place`: through a `*const` pointer it cannot be
    /// written.
    fn register(
        &mut self,
        f: &Fn,
        expr: &Expr,
        p: PeripheralTy,
        base_place: PlaceInfo,
        name: &str,
        name_span: Span,
    ) -> PlaceInfo {
        let hir = self.hir;
        let def = hir.peripheral_type(p.id);
        let Some(index) = def.registers.iter().position(|reg| reg.name == name) else {
            self.q
                .diags
                .push(unknown_register(hir, p.id, name, name_span, f.module));
            return PlaceInfo::ERROR;
        };
        // A register that is not visible is still the one named, so nothing
        // more is reported about it.
        if !hir.register_visible(p.id, index, f.module) {
            self.q
                .diags
                .push(private_register(hir, p.id, index, f.module, name_span));
        }
        self.q.tables.fields.insert(expr.id, index as u32);
        PlaceInfo {
            ty: self.norm(def.registers[index].ty),
            misaligned: false,
            register: Some(Register { id: p.id, index }),
            ..base_place
        }
    }

    /// Checks `expr`, written where a place is needed and found not to be
    /// one, with no type expected. A `.V` is not checked: with no type
    /// expected it would also be `ty::untyped-variant`, and that it is no
    /// place is enough.
    fn check_not_a_place(&mut self, f: &Fn, expr: &Expr) {
        if !matches!(expr.kind, ExprKind::ContextVariant(..)) {
            self.check(f, expr, None);
        }
    }

    /// Checks that `place` can be assigned to, by `=` or, with `kind`
    /// `ReadWrite`, by a compound assignment, and returns its type.
    pub(super) fn place(&mut self, f: &Fn, place: &Expr, kind: AccessMode) -> Ty {
        let Some(info) = self.place_info(f, place) else {
            self.check_not_a_place(f, place);
            self.q.diags.push(
                Diagnostic::error("ty::not-assignable", "cannot assign to this expression")
                    .label(place.span, "not a variable, `*pointer` or `a[index]`"),
            );
            return Ty::Error;
        };
        if let Ty::Slice(_) = info.ty {
            self.q
                .diags
                .push(whole_slice(info.ty, place.span, "written"));
            return Ty::Error;
        }
        if let Ty::Peripheral(_) = info.ty {
            let accessed = match kind {
                AccessMode::ReadWrite => "read and written",
                _ => "written",
            };
            self.q
                .diags
                .push(whole_peripheral(info.ty, place.span, accessed));
            return Ty::Error;
        }
        match info.immutable {
            Some(Immutable::Local(id)) => {
                let local = f.local(id);
                self.q.diags.push(
                    Diagnostic::error(
                        "ty::assign-immutable",
                        format!("cannot assign to `{}`, which is not `mut`", local.name),
                    )
                    .label(place.span, "assigned here")
                    .label(
                        local.span,
                        format!("help: declare it `let mut {}`", local.name),
                    ),
                );
            }
            Some(Immutable::ConstPointer(ptr, pointer)) => {
                self.q.diags.push(
                    Diagnostic::error(
                        "ty::write-through-const",
                        "cannot write through a `*const` pointer",
                    )
                    .label(pointer, format!("has type `{}`", Ty::Ptr(ptr)))
                    .help("only `*mut` pointers can be written through"),
                );
            }
            Some(Immutable::Temporary(base)) => {
                self.q.diags.push(
                    Diagnostic::error(
                        "ty::not-assignable",
                        "cannot assign to part of a temporary value",
                    )
                    .label(base, "this value lives in no variable")
                    .help("store it in a `let mut` first"),
                );
            }
            Some(Immutable::Static(id)) => {
                let def = self.hir.static_(id);
                self.q.diags.push(
                    Diagnostic::error(
                        "ty::assign-immutable",
                        format!("cannot assign to `{}`, which is not `mut`", def.path),
                    )
                    .label(place.span, "assigned here")
                    .label(
                        def.name_span,
                        format!("help: declare it `{}`", mutable_static(def)),
                    ),
                );
            }
            Some(Immutable::ByteString(span)) => {
                self.q.diags.push(
                    Diagnostic::error("ty::assign-immutable", "cannot assign to a byte string")
                        .label(span, "in read-only memory")
                        .help("copy it into a `let mut` to change it (ADR 0031)"),
                );
            }
            None => {}
        }
        self.access(&info, place.span, kind);
        self.assignment_effects(place, &info, kind);
        info.ty
    }

    /// Checks the effects of assigning `place`, at `info`, as `kind` says,
    /// in a volatile place: a register, or a place reached through a
    /// volatile pointer (ADR 0060). In ordinary memory effects mean nothing.
    /// A compound assignment, and an assignment to a bit field, which keeps
    /// the other bits, read the place: `ty::read-with-effects` if its reads
    /// have effects. A compound assignment to a bit field with a write effect
    /// writes back the bits it read: `ty::compound-write-effect`.
    fn assignment_effects(&mut self, place: &Expr, info: &PlaceInfo, kind: AccessMode) {
        let field = self.q.tables.bit_field(place.id);
        let compound = kind == AccessMode::ReadWrite;
        let volatile = info.register.is_some() || info.via.is_some_and(|ptr| ptr.volatile);
        if !volatile || !(compound || field.is_some()) {
            return;
        }
        let hir = self.hir;
        let read_by = if compound {
            "read by this compound assignment"
        } else {
            "read by this field assignment"
        };
        // A register's reads, or those of the whole value through the
        // pointer: a bit field is read with the whole bit-field struct.
        let (effect, read) = match info.register {
            Some(register) => {
                let def = hir.peripheral_type(register.id);
                let reg = &def.registers[register.index];
                (
                    self.q.register_read_effect(register.id, register.index),
                    format!("the register `{}` of `{}`", reg.name, def.path),
                )
            }
            None => {
                let whole = info.bit_field.map_or(info.ty, Ty::BitStruct);
                (
                    self.q.type_read_effect(whole),
                    format!("`{whole}` through a volatile pointer"),
                )
            }
        };
        if let Some(effect) = effect {
            self.q.diags.push(
                Diagnostic::error("ty::read-with-effects", format!("reading {read} has effects"))
                    .label(place.span, read_by)
                    .label(effect, "a read has effects")
                    .help("the read would go unseen, and what it returned lost; read the value into a `let` first, which shows the read, then write the whole value meant (ADR 0060)"),
            );
        }
        if compound && let Some((s, index)) = field {
            let def = hir.bit_struct(s);
            let field = &def.fields[index as usize];
            if let Some((effect, effect_span)) = field.write_effect {
                let action = effect.action.word();
                let bit = effect.bit;
                let place_words = match info.register {
                    Some(_) => "read and written back to the register",
                    None => "read and written back through a volatile pointer",
                };
                self.q.diags.push(
                    Diagnostic::error(
                        "ty::compound-write-effect",
                        format!(
                            "cannot compound-assign the field `{}` of `{}`, which has a write effect",
                            field.name, def.path
                        ),
                    )
                    .label(place.span, place_words)
                    .label(effect_span, "declared here")
                    .help(format!(
                        "every `{bit}` written to it {action} its bit, so writing back the bits read {action} each one read as `{bit}`; assign the field the bits meant with `=` (ADR 0060)"
                    )),
                );
            }
        }
    }

    /// A place inside a packed struct whose type needs alignment.
    fn may_be_misaligned(&mut self, place: &PlaceInfo) -> bool {
        place.misaligned && self.q.layout_of(place.ty).is_some_and(|l| l.align > 1)
    }

    /// `&raw const place` or `&raw mut place` (ADR 0021). The address of a
    /// place is taken without accessing it.
    pub(super) fn addr_of(&mut self, f: &Fn, mutable: bool, place: &Expr) -> Ty {
        let not_a_place = Diagnostic::error("ty::not-a-place", "`&raw` needs a place in memory")
            .label(place.span, "a value, not a place")
            .help("take the address of a variable, `*pointer` or `a[index]`; to point at a value, store it in a `let` first");
        let Some(info) = self.place_info(f, place) else {
            self.check_not_a_place(f, place);
            self.q.diags.push(not_a_place);
            return Ty::Error;
        };
        // Bits have no address, in a register too, which is then not also
        // reported: what is addressed is the bit field (ADR 0059).
        if let Some(s) = info.bit_field {
            self.q.diags.push(
                Diagnostic::error(
                    "ty::bit-field-address",
                    format!("cannot take the address of a field of `{}`", s.name),
                )
                .label(place.span, "a bit field, not a place in memory")
                .help(format!(
                    "a field of a bit-field struct is a range of bits of its `{}`; copy it into a `let` and take that address, or take the address of the whole value (ADR 0059)",
                    s.int
                )),
            );
            return Ty::Error;
        }
        // The pointer would reach the register without its access mode
        // (ADR 0058).
        if let Some(register) = info.register {
            let def = self.hir.peripheral_type(register.id);
            let reg = &def.registers[register.index];
            self.q.diags.push(
                Diagnostic::error(
                    "ty::register-address",
                    format!(
                        "cannot take the address of the register `{}` of `{}`",
                        reg.name, def.path
                    ),
                )
                .label(place.span, "a register")
                .help(format!(
                    "a pointer to it would reach it without its access mode, `{}`; read or write it where it is named, or pass the whole device with a pointer to its peripheral, as `&raw mut P` (ADR 0058)",
                    reg.access.words()
                )),
            );
            return Ty::Error;
        }
        // Its result would be a `*const !` (ADR 0034).
        if info.ty == Ty::Never {
            self.q.diags.push(never_in_memory(
                place.span,
                "the type of a place under `&raw`",
                NeverHelp::Unit,
            ));
            return Ty::Error;
        }
        // An element or field of a constant is read where the constant's
        // data is, but has no address of its own (ADR 0029).
        if info.constant {
            self.q.diags.push(not_a_place);
            return Ty::Error;
        }
        if mutable {
            match info.immutable {
                Some(Immutable::Local(id)) => {
                    let local = f.local(id);
                    self.q.diags.push(
                        Diagnostic::error(
                            "ty::address-of-immutable",
                            format!(
                                "cannot take a `*mut` address of `{}`, which is not `mut`",
                                local.name
                            ),
                        )
                        .label(place.span, "address taken here")
                        .label(
                            local.span,
                            format!("help: declare it `let mut {}`", local.name),
                        ),
                    );
                }
                Some(Immutable::ConstPointer(ptr, pointer)) => {
                    self.q.diags.push(
                        Diagnostic::error(
                            "ty::address-of-immutable",
                            "cannot take a `*mut` address through a `*const` pointer",
                        )
                        .label(pointer, format!("has type `{}`", Ty::Ptr(ptr)))
                        .help("use `&raw const`, or a `*mut` pointer"),
                    );
                }
                Some(Immutable::Temporary(base)) => {
                    self.q.diags.push(
                        Diagnostic::error(
                            "ty::address-of-immutable",
                            "cannot take a `*mut` address of part of a temporary value",
                        )
                        .label(base, "this value lives in no variable")
                        .help("store it in a `let mut` first"),
                    );
                }
                Some(Immutable::Static(id)) => {
                    let def = self.hir.static_(id);
                    self.q.diags.push(
                        Diagnostic::error(
                            "ty::address-of-immutable",
                            format!(
                                "cannot take a `*mut` address of `{}`, which is not `mut`",
                                def.path
                            ),
                        )
                        .label(place.span, "address taken here")
                        .label(
                            def.name_span,
                            format!("help: declare it `{}`", mutable_static(def)),
                        ),
                    );
                }
                Some(Immutable::ByteString(span)) => {
                    self.q.diags.push(
                        Diagnostic::error(
                            "ty::address-of-immutable",
                            "cannot take a `*mut` address of a byte string",
                        )
                        .label(span, "in read-only memory")
                        .help("use `&raw const`, or copy it into a `let mut` (ADR 0031)"),
                    );
                }
                None => {}
            }
        }
        if self.may_be_misaligned(&info) {
            self.q.diags.push(
                Diagnostic::error(
                    "ty::packed-field-address",
                    format!("cannot take the address of a `{}` that may be misaligned", info.ty),
                )
                .label(place.span, "reached through a field of a `#[packed]` struct")
                .help(format!(
                    "a `*const {}` promises alignment, and dereferencing a misaligned one may trap; copy the value into a `let` first (ADR 0022)",
                    info.ty
                )),
            );
        }
        match info.ty {
            Ty::Error => Ty::Error,
            ty => Ty::ptr(mutable, false, ty),
        }
    }
}

/// The fields of a struct or a bit-field struct (ADR 0059), as a field
/// access or `offset_of` written in `module` names them (ADR 0050).
struct Fields<'h> {
    /// The struct's path.
    owner: &'h str,
    owner_span: Span,
    owner_module: ModuleId,
    /// The module the fields are named in.
    module: ModuleId,
    /// In declaration order.
    fields: Vec<FieldView<'h>>,
}

struct FieldView<'h> {
    name: &'h str,
    span: Span,
    vis: hir::Visibility,
    /// Whether `Fields::module` sees it.
    visible: bool,
}

impl<'h> Fields<'h> {
    fn of_struct(hir: &'h Hir, id: hir::StructId, module: ModuleId) -> Fields<'h> {
        let def = hir.struct_(id);
        Fields {
            owner: &def.name,
            owner_span: def.name_span,
            owner_module: def.module,
            module,
            fields: (def.fields.iter().enumerate())
                .map(|(index, field)| FieldView {
                    name: &field.name,
                    span: field.span,
                    vis: field.vis,
                    visible: hir.field_visible(id, index, module),
                })
                .collect(),
        }
    }

    fn of_bit_struct(hir: &'h Hir, id: hir::BitStructId, module: ModuleId) -> Fields<'h> {
        let def = hir.bit_struct(id);
        Fields {
            owner: &def.path,
            owner_span: def.name_span,
            owner_module: def.module,
            module,
            fields: (def.fields.iter().enumerate())
                .map(|(index, field)| FieldView {
                    name: &field.name,
                    span: field.span,
                    vis: field.vis,
                    visible: hir.bit_field_visible(id, index, module),
                })
                .collect(),
        }
    }
}

/// `ty::unknown-field` for `name`, written at `span`, which `fields` has
/// not. Only the fields visible there are listed (ADR 0050).
fn unknown_field(fields: &Fields, name: &str, span: Span) -> Diagnostic {
    let owner = fields.owner;
    let names: Vec<String> = (fields.fields.iter())
        .filter(|field| field.visible)
        .map(|field| format!("`{}`", field.name))
        .collect();
    let diag = Diagnostic::error(
        "ty::unknown-field",
        format!("`{owner}` has no field `{name}`"),
    )
    .label(span, "unknown field");
    if fields.fields.is_empty() {
        diag.help(format!("`{owner}` has no fields"))
    } else if names.is_empty() {
        diag.help("none of its fields is visible here (ADR 0050)")
    } else {
        diag.help(format!("its fields are {}", names.join(", ")))
    }
}

/// `ty::private-field` for the field `index` of `fields`, named at `span`
/// in a module that does not see it (ADR 0050): a private field outside
/// its struct's module, or a `pub` one where the struct cannot be named.
fn private_field(hir: &Hir, fields: &Fields, index: usize, span: Span) -> Diagnostic {
    let owner = fields.owner;
    let field = &fields.fields[index];
    match field.vis {
        hir::Visibility::Private => Diagnostic::error(
            "ty::private-field",
            format!("the field `{}` of `{owner}` is private", field.name),
        )
        .label(span, "not visible here")
        .label(field.span, "declared here without `pub`")
        .help(format!(
            "a private field is visible only in {} and the modules inside it; declare it `pub` to use it elsewhere (ADR 0050)",
            hir.module(fields.owner_module).describe()
        )),
        hir::Visibility::Pub => Diagnostic::error(
            "ty::private-field",
            format!("the field `{}` of `{owner}` is not visible here", field.name),
        )
        .label(span, "not visible here")
        .label(fields.owner_span, format!("`{owner}` is declared here"))
        .help(format!(
            "a `pub` field is visible wherever a path can name its struct, and none written in {} can name `{owner}`: offer the struct with `pub` or `pub use` (ADR 0050)",
            hir.module(fields.module).describe(),
        )),
    }
}

/// `ty::unknown-register` for `name`, written at `span` in `module` on a
/// place of the peripheral type `id`, which has no such register
/// (ADR 0058). The help lists, once each, the registers visible there, as
/// for fields (ADR 0050).
fn unknown_register(
    hir: &Hir,
    id: PeripheralTypeId,
    name: &str,
    span: Span,
    module: ModuleId,
) -> Diagnostic {
    let def = hir.peripheral_type(id);
    let mut names: Vec<String> = Vec::new();
    for (index, reg) in def.registers.iter().enumerate() {
        let listed = format!("`{}`", reg.name);
        if hir.register_visible(id, index, module) && !names.contains(&listed) {
            names.push(listed);
        }
    }
    let diag = Diagnostic::error(
        "ty::unknown-register",
        format!("`{}` has no register `{name}`", def.path),
    )
    .label(span, "unknown register");
    if def.registers.is_empty() {
        diag.help(format!("`{}` has no registers", def.path))
    } else if names.is_empty() {
        diag.help("none of its registers is visible here (ADR 0050, 0058)")
    } else {
        diag.help(format!("its registers are {}", names.join(", ")))
    }
}

/// `ty::private-register` for the register `index` of the peripheral type
/// `id`, named at `span` in `module`, which does not see it: a private
/// register outside its type's module, or a `pub` one where the type
/// cannot be named, whatever peripheral it is seen through (ADR 0050,
/// 0058).
fn private_register(
    hir: &Hir,
    id: PeripheralTypeId,
    index: usize,
    module: ModuleId,
    span: Span,
) -> Diagnostic {
    let def = hir.peripheral_type(id);
    let reg = &def.registers[index];
    match reg.vis {
        hir::Visibility::Private => Diagnostic::error(
            "ty::private-register",
            format!("the register `{}` of `{}` is private", reg.name, def.path),
        )
        .label(span, "not visible here")
        .label(reg.span, "declared here without `pub`")
        .help(format!(
            "a private register is visible only in {} and the modules inside it; declare it `pub` to use it elsewhere, or offer functions that use it (ADR 0050, 0058)",
            hir.module(def.module).describe()
        )),
        hir::Visibility::Pub => Diagnostic::error(
            "ty::private-register",
            format!(
                "the register `{}` of `{}` is not visible here",
                reg.name, def.path
            ),
        )
        .label(span, "not visible here")
        .label(def.name_span, format!("`{}` is declared here", def.path))
        .help(format!(
            "a `pub` register is visible wherever a path can name its peripheral type, whatever peripheral it is reached from, and none written in {} can name `{}`: offer the type with `pub` or `pub use` (ADR 0050, 0058)",
            hir.module(module).describe(),
            def.path
        )),
    }
}

/// How a static would be declared to be written: `static mut NAME`, or
/// `extern static mut NAME`.
fn mutable_static(def: &hir::StaticDef) -> String {
    let external = if def.init.is_none() { "extern " } else { "" };
    format!("{external}static mut {}", def.name)
}
