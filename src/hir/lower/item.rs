//! Lowering items: structs, functions, statics, constants, enums, modules,
//! `comptime` blocks, and their attributes.

use std::collections::HashMap;

use crate::diag::Diagnostic;
use crate::hir::{
    ArrayLen, BitFieldDef, BitStructDef, BitStructId, Block, Comptime, ConstDef, ConstId, EnumDef,
    EnumId, FieldDef, Fn, FnId, FnKind, HandlerVector, IntTy, ModuleDef, ModuleId, NeverPosition,
    PayloadDef, StaticDef, StaticId, StructDef, Ty, UnsizedPosition, VariantDef, VariantValue,
    Visibility,
};
use crate::span::Span;
use crate::syntax::ast;

use super::{Lowerer, Named, visibility, written};

/// The attributes of a struct, as `struct_attrs` found them.
struct StructAttrs {
    packed: bool,
    align: Option<(ArrayLen, Span)>,
    copy: Option<Span>,
    linear: Option<Span>,
}

impl Lowerer<'_> {
    pub(super) fn struct_decl(&mut self, decl: &ast::StructDecl) -> StructDef {
        self.duplicate_fields(decl.fields.iter().map(|field| &field.name));
        let attrs = self.struct_attrs(&decl.attrs);
        StructDef {
            packed: attrs.packed,
            align: attrs.align,
            copy: attrs.copy,
            linear: attrs.linear,
            name: self.path_of(self.module, &decl.name.name),
            name_span: decl.name.span,
            module: self.module,
            vis: visibility(decl.vis),
            // Filled in once every `use` is resolved, by `reachability`.
            reachable: Vec::new(),
            fields: decl
                .fields
                .iter()
                .map(|field| FieldDef {
                    name: field.name.name.clone(),
                    span: field.name.span,
                    vis: visibility(field.vis),
                    ty: self.memory_ty(&field.ty, NeverPosition::Field),
                })
                .collect(),
        }
    }

    /// `resolve::duplicate-field` for each field name of `names`, in source
    /// order, that an earlier one has (ADR 0021).
    fn duplicate_fields<'d>(&mut self, names: impl Iterator<Item = &'d ast::Ident>) {
        let mut seen: HashMap<&str, Span> = HashMap::new();
        for name in names {
            if let Some(&first) = seen.get(name.name.as_str()) {
                self.diags.push(
                    Diagnostic::error(
                        "resolve::duplicate-field",
                        format!("the field `{}` is declared more than once", name.name),
                    )
                    .label(name.span, "declared again here")
                    .label(first, "first declared here"),
                );
            }
            seen.entry(&name.name).or_insert(name.span);
        }
    }

    /// A bit-field struct (ADR 0059): no attribute applies to it, it has at
    /// least one field, each name once. A field's type is a type of memory;
    /// its bits are `u32`s and its default a value of its type, all
    /// computed at compile time in the struct's module; a field with a
    /// write effect has no default (ADR 0060). Effects are carried as
    /// written.
    pub(super) fn bit_struct_decl(
        &mut self,
        decl: &ast::BitStructDecl,
        id: BitStructId,
    ) -> BitStructDef {
        self.bit_struct_attrs(&decl.attrs);
        self.duplicate_fields(decl.fields.iter().map(|field| &field.name));
        let ty = self.bit_struct_tys[id.0 as usize];
        if decl.fields.is_empty() {
            self.diags.push(
                Diagnostic::error(
                    "resolve::empty-bit-struct",
                    format!("`{}` has no fields", ty.name),
                )
                .label(decl.name.span, "a bit-field struct needs at least one field")
                .help("a bit-field struct names bits of its integer type; without fields, use the integer type itself (ADR 0059)"),
            );
        }
        let mut fields = Vec::with_capacity(decl.fields.len());
        for (i, field) in decl.fields.iter().enumerate() {
            if let (Some((_, effect)), Some(default)) = (field.write_effect, &field.default) {
                self.diags.push(
                    Diagnostic::error(
                        "resolve::write-effect-default",
                        format!(
                            "the field `{}` has a write effect, so it has no default",
                            field.name.name
                        ),
                    )
                    .label(default.span, "a default")
                    .label(effect, "the write effect")
                    .help("`..` gives a field with a write effect the value whose write has no effect, all zeros for `write 1 ...` and all ones for `write 0 ...`, so that a literal never triggers an effect it does not name: remove the default (ADR 0059, 0060)"),
                );
            }
            let field_ty = self.memory_ty(&field.ty, NeverPosition::BitField);
            let bits = Comptime::Bits(id, i as u32);
            let low = (
                self.comptime_int(&field.low, "bits", IntTy::U32, bits),
                field.low.span,
            );
            let high = field
                .high
                .as_ref()
                .map(|high| (self.comptime_int(high, "bits", IntTy::U32, bits), high.span));
            let default = field.default.as_ref().map(|default| {
                let comptime = Comptime::Default(id, i as u32);
                let body = self.comptime_value(default, "default", field_ty, comptime);
                (body, default.span)
            });
            fields.push(BitFieldDef {
                name: field.name.name.clone(),
                span: field.name.span,
                vis: visibility(field.vis),
                ty: field_ty,
                ty_span: field.ty.span,
                low,
                high,
                read_effect: field.read_effect,
                write_effect: field.write_effect,
                access: field.access,
                default,
            });
        }
        BitStructDef {
            path: ty.name.to_owned(),
            name_span: decl.name.span,
            module: self.module,
            vis: visibility(decl.vis),
            // Filled in once every `use` is resolved, by `reachability`.
            reachable: Vec::new(),
            ty,
            int_span: decl.ty.span,
            fields,
        }
    }

    /// No attribute applies to a bit-field struct: its layout is its
    /// integer type's, which `#[packed]` and `#[align(N)]` would change
    /// (ADR 0059).
    fn bit_struct_attrs(&mut self, attrs: &[ast::Attr]) {
        for attr in attrs {
            match attr.name.name.as_str() {
                "packed" | "align" => self.diags.push(
                    Diagnostic::error(
                        "resolve::misplaced-attribute",
                        format!(
                            "`#[{}]` does not apply to a bit-field struct",
                            attr.name.name
                        ),
                    )
                    .label(attr.span, "not allowed on a bit-field struct")
                    .help("a bit-field struct has the size and alignment of its integer type, which it is stored as (ADR 0059)"),
                ),
                "export" | "naked" | "interrupts" | "copy" | "linear" => {
                    self.misplaced_attr(attr, "a bit-field struct")
                }
                _ => self.unknown_attr(attr),
            }
        }
    }

    /// Layout attributes (ADR 0022): `#[packed]` and `#[align(N)]`, at
    /// most once each, and not together; and `#[copy]` (ADR 0096) and
    /// `#[linear]` (ADR 0101), which go with them, once each and not
    /// together. Returns them.
    fn struct_attrs(&mut self, attrs: &[ast::Attr]) -> StructAttrs {
        let mut packed: Option<Span> = None;
        let mut align: Option<(ArrayLen, Span)> = None;
        let mut align_attr: Option<Span> = None;
        let mut copy: Option<Span> = None;
        let mut linear: Option<Span> = None;
        for attr in attrs {
            let first = match attr.name.name.as_str() {
                "copy" => {
                    self.flag_attr(attr, &mut copy);
                    None
                }
                "linear" => {
                    self.flag_attr(attr, &mut linear);
                    None
                }
                "packed" => {
                    if let Some(arg) = &attr.arg {
                        self.attr_error(arg.span, "`#[packed]` takes no argument", "remove this");
                    }
                    packed.replace(attr.span)
                }
                "align" => {
                    match &attr.arg {
                        Some(arg) => align = Some((self.array_len(arg), arg.span)),
                        None => self.attr_error(
                            attr.span,
                            "`#[align]` takes an alignment, such as `#[align(64)]`",
                            "no alignment",
                        ),
                    }
                    align_attr.replace(attr.span)
                }
                "export" | "naked" | "interrupts" => {
                    self.misplaced_attr(attr, "a struct");
                    None
                }
                _ => {
                    self.unknown_attr(attr);
                    None
                }
            };
            if let Some(first) = first {
                self.diags.push(
                    Diagnostic::error(
                        "resolve::duplicate-attribute",
                        format!("`#[{}]` is given more than once", attr.name.name),
                    )
                    .label(attr.span, "given again here")
                    .label(first, "first given here"),
                );
            }
        }
        if let (Some(packed), Some(align)) = (packed, align_attr) {
            self.diags.push(
                Diagnostic::error(
                    "resolve::conflicting-attributes",
                    "`#[packed]` and `#[align]` cannot be combined",
                )
                .label(packed, "alignment 1")
                .label(align, "a higher alignment")
                .help("`#[packed]` lowers alignment and `#[align]` raises it (ADR 0022)"),
            );
        }
        if let (Some(copy), Some(linear)) = (copy, linear) {
            self.diags.push(
                Diagnostic::error(
                    "resolve::conflicting-attributes",
                    "`#[copy]` and `#[linear]` cannot be combined",
                )
                .label(copy, "its values are copied")
                .label(linear, "its values must not be copied")
                .help("a linear value must be consumed exactly once, which a copy would defeat (ADR 0101)"),
            );
        }
        StructAttrs {
            packed: packed.is_some(),
            align,
            copy,
            linear,
        }
    }

    /// Items other than structs, enums, statics and functions with a body take
    /// no attributes; `what` names the item.
    pub(super) fn no_attrs(&mut self, attrs: &[ast::Attr], what: &str) {
        for attr in attrs {
            match attr.name.name.as_str() {
                "packed" | "align" | "export" | "naked" | "interrupts" | "copy" | "linear" => {
                    self.misplaced_attr(attr, what)
                }
                _ => self.unknown_attr(attr),
            }
        }
    }

    /// `#[interrupts]` (ADR 0089) and, on an enum with payloads, `#[copy]`
    /// (ADR 0096) are the attributes of an enum; the chip's list is found
    /// by `lower`, which is told whether the enum has the first. Returns the
    /// span of `#[copy]`.
    fn enum_attrs(&mut self, attrs: &[ast::Attr], payloads: bool) -> Option<Span> {
        let mut interrupts = None;
        let mut copy = None;
        for attr in attrs {
            match attr.name.name.as_str() {
                "interrupts" => self.flag_attr(attr, &mut interrupts),
                "copy" if payloads => self.flag_attr(attr, &mut copy),
                "copy" => self.misplaced_attr(attr, "an enum without payloads"),
                "packed" | "align" | "export" | "naked" | "linear" => {
                    self.misplaced_attr(attr, "an enum")
                }
                _ => self.unknown_attr(attr),
            }
        }
        copy
    }

    fn misplaced_attr(&mut self, attr: &ast::Attr, what: &str) {
        let applies_to = match attr.name.name.as_str() {
            "align" => "structs, statics and functions with a body",
            "export" => "functions and statics defined in the program",
            "naked" => {
                "functions with a body, other than the entry function, the panic function and interrupt handlers"
            }
            "interrupts" => "an enum declared in the program",
            "copy" => "structs, other than bit-field structs, and enums with payloads",
            "linear" => "structs, other than bit-field structs",
            _ => "structs",
        };
        let mut diag = Diagnostic::error(
            "resolve::misplaced-attribute",
            format!("`#[{}]` applies only to {applies_to}", attr.name.name),
        )
        .label(attr.span, format!("not allowed on {what}"));
        if attr.name.name == "export" {
            diag = diag.help("`#[export]` gives a function or static its own name as its symbol; an `extern` item always has its own name (ADR 0051)");
        }
        if attr.name.name == "naked" {
            diag = diag.help("`#[naked]` makes a function with a body that is one `asm` expression, with no code of the compiler's; the entry, the panic function and the interrupt handlers are written in Erebos, and an `extern fn` has no body here (ADR 0079, 0084)");
        }
        if attr.name.name == "interrupts" {
            diag = diag.help("`#[interrupts]` makes an enum the list of the chip's interrupts, whose variants are their numbers; it is not a property of other items (ADR 0089)");
        }
        if attr.name.name == "copy" {
            diag = diag.help("`#[copy]` lets the values of a struct or of an enum with payloads be copied; a bit-field struct and an enum without payloads are copy already, and no other item has values (ADR 0096)");
        }
        if attr.name.name == "linear" {
            diag = diag.help(if what.contains("enum") {
                "`#[linear]` would bind nobody on an enum: its variants are as visible as the enum, so any module that sees it could build a value and take it apart with `match`; an enum is linear by holding a linear value (ADR 0101)"
            } else {
                "`#[linear]` makes the values of a struct linear: each must be consumed before the function holding it returns, and none is overwritten; no other item has values (ADR 0101)"
            });
        }
        self.diags.push(diag);
    }

    /// An attribute that takes no argument and is given at most once
    /// (`#[export]`, ADR 0051; `#[naked]`, ADR 0079; `#[interrupts]`,
    /// ADR 0089; `#[copy]`, ADR 0096; `#[linear]`, ADR 0101); `given` holds the span
    /// of the one already given.
    fn flag_attr(&mut self, attr: &ast::Attr, given: &mut Option<Span>) {
        let name = &attr.name.name;
        if let Some(arg) = &attr.arg {
            self.attr_error(
                arg.span,
                &format!("`#[{name}]` takes no argument"),
                "remove this",
            );
        }
        if let Some(first) = given.replace(attr.span) {
            self.diags.push(
                Diagnostic::error(
                    "resolve::duplicate-attribute",
                    format!("`#[{name}]` is given more than once"),
                )
                .label(attr.span, "given again here")
                .label(first, "first given here"),
            );
        }
    }

    /// `#[export]`, `#[naked]` and `#[align(N)]` are the attributes of a
    /// function with a body; an `extern fn` takes none (ADR 0051, 0087), and
    /// the entry, the panic function and the interrupt handlers are not naked
    /// (ADR 0079, 0084). Returns
    /// whether it is exported, whether it is naked, and its alignment.
    fn fn_attrs(
        &mut self,
        attrs: &[ast::Attr],
        external: bool,
        kind: &ast::FnKind,
    ) -> (bool, bool, Option<(ArrayLen, Span)>) {
        if external {
            self.no_attrs(attrs, "an `extern fn`");
            return (false, false, None);
        }
        let mut export = None;
        let mut naked = None;
        let mut align: Option<(ArrayLen, Span)> = None;
        let mut align_attr: Option<Span> = None;
        for attr in attrs {
            match attr.name.name.as_str() {
                "export" => self.flag_attr(attr, &mut export),
                "naked" => match kind {
                    ast::FnKind::Plain => self.flag_attr(attr, &mut naked),
                    ast::FnKind::Entry => self.misplaced_attr(attr, "the entry function"),
                    ast::FnKind::Panic => self.misplaced_attr(attr, "the panic function"),
                    ast::FnKind::Interrupt(_) => self.misplaced_attr(attr, "an interrupt handler"),
                },
                "align" => {
                    match &attr.arg {
                        Some(arg) => align = Some((self.array_len(arg), arg.span)),
                        None => self.attr_error(
                            attr.span,
                            "`#[align]` takes an alignment, such as `#[align(64)]`",
                            "no alignment",
                        ),
                    }
                    if let Some(first) = align_attr.replace(attr.span) {
                        self.diags.push(
                            Diagnostic::error(
                                "resolve::duplicate-attribute",
                                "`#[align]` is given more than once",
                            )
                            .label(attr.span, "given again here")
                            .label(first, "first given here"),
                        );
                    }
                }
                "packed" | "interrupts" | "copy" | "linear" => {
                    self.misplaced_attr(attr, "a function")
                }
                _ => self.unknown_attr(attr),
            }
        }
        (export.is_some(), naked.is_some(), align)
    }

    /// `#[align(N)]` and `#[export]`, at most once each, are the attributes
    /// of a static; an `extern static` takes none, since its definition
    /// decides its alignment and its name is its symbol (ADR 0030, 0051).
    /// Returns the alignment, and whether it is exported.
    fn static_attrs(
        &mut self,
        attrs: &[ast::Attr],
        external: bool,
    ) -> (Option<(ArrayLen, Span)>, bool) {
        if external {
            self.no_attrs(attrs, "an `extern static`");
            return (None, false);
        }
        let mut align: Option<(ArrayLen, Span)> = None;
        let mut align_attr: Option<Span> = None;
        let mut export = None;
        for attr in attrs {
            match attr.name.name.as_str() {
                "align" => {
                    match &attr.arg {
                        Some(arg) => align = Some((self.array_len(arg), arg.span)),
                        None => self.attr_error(
                            attr.span,
                            "`#[align]` takes an alignment, such as `#[align(64)]`",
                            "no alignment",
                        ),
                    }
                    if let Some(first) = align_attr.replace(attr.span) {
                        self.diags.push(
                            Diagnostic::error(
                                "resolve::duplicate-attribute",
                                "`#[align]` is given more than once",
                            )
                            .label(attr.span, "given again here")
                            .label(first, "first given here"),
                        );
                    }
                }
                "export" => self.flag_attr(attr, &mut export),
                "packed" | "naked" | "interrupts" | "copy" | "linear" => {
                    self.misplaced_attr(attr, "a static")
                }
                _ => self.unknown_attr(attr),
            }
        }
        (align, export.is_some())
    }

    /// A static, and the body that computes its initial value, `init`; an
    /// `extern static` has none (ADR 0030).
    pub(super) fn static_decl(
        &mut self,
        decl: &ast::StaticDecl,
        id: StaticId,
        init: FnId,
    ) -> (StaticDef, Option<Fn>) {
        let (align, export) = self.static_attrs(&decl.attrs, decl.external);
        let ty = self.memory_ty(&decl.ty, NeverPosition::Static);
        let mut def = StaticDef {
            name: decl.name.name.clone(),
            path: self.path_of(self.module, &decl.name.name),
            symbol: if decl.external || export {
                decl.name.name.clone()
            } else {
                path_symbol(&self.modules, self.module, &decl.name.name)
            },
            name_span: decl.name.span,
            module: self.module,
            vis: visibility(decl.vis),
            mutable: decl.mutable,
            ty,
            ty_span: decl.ty.span,
            init: None,
            align,
        };
        let Some(value) = &decl.value else {
            return (def, None);
        };
        def.init = Some(init);
        let value = self.comptime_body(|this| this.expr(value));
        let body = Fn {
            name: decl.name.name.clone(),
            path: self.path_of(self.module, &decl.name.name),
            symbol: None,
            name_span: decl.name.span,
            module: self.module,
            vis: Visibility::Private,
            kind: FnKind::Plain,
            ret: ty,
            ret_span: Some(decl.ty.span),
            param_count: 0,
            locals: std::mem::take(&mut self.locals),
            body: Some(Block {
                stmts: Vec::new(),
                span: value.span,
                tail: Some(Box::new(value)),
            }),
            span: decl.span,
            runtime: false,
            comptime: Some(Comptime::Static(id)),
            align: None,
        };
        (def, Some(body))
    }

    /// A constant, and the body that computes its value (ADR 0029).
    pub(super) fn const_decl(
        &mut self,
        decl: &ast::ConstDecl,
        id: ConstId,
        init: FnId,
    ) -> (ConstDef, Fn) {
        self.no_attrs(&decl.attrs, "a constant");
        let ty = self.memory_ty(&decl.ty, NeverPosition::Const);
        let value = self.comptime_body(|this| this.expr(&decl.value));
        let def = ConstDef {
            name: decl.name.name.clone(),
            path: self.path_of(self.module, &decl.name.name),
            name_span: decl.name.span,
            module: self.module,
            vis: visibility(decl.vis),
            ty,
            init,
        };
        let body = Fn {
            name: decl.name.name.clone(),
            path: self.path_of(self.module, &decl.name.name),
            symbol: None,
            name_span: decl.name.span,
            module: self.module,
            vis: Visibility::Private,
            kind: FnKind::Plain,
            ret: ty,
            ret_span: Some(decl.ty.span),
            param_count: 0,
            locals: std::mem::take(&mut self.locals),
            body: Some(Block {
                stmts: Vec::new(),
                span: value.span,
                tail: Some(Box::new(value)),
            }),
            span: decl.span,
            runtime: false,
            comptime: Some(Comptime::Const(id)),
            align: None,
        };
        (def, body)
    }

    /// An enum, and the bodies computing the values of its variants, the
    /// first of which is `first_body` (ADR 0035). A payload type is a type
    /// of memory (ADR 0039).
    pub(super) fn enum_decl(
        &mut self,
        decl: &ast::EnumDecl,
        id: EnumId,
        first_body: u32,
    ) -> (EnumDef, Vec<Fn>) {
        let payloads = decl
            .variants
            .iter()
            .any(|variant| !variant.payload.is_empty());
        let copy = self.enum_attrs(&decl.attrs, payloads);
        let mut seen: HashMap<&str, Span> = HashMap::new();
        for variant in &decl.variants {
            if let Some(&first) = seen.get(variant.name.name.as_str()) {
                self.diags.push(
                    Diagnostic::error(
                        "resolve::duplicate-variant",
                        format!(
                            "the variant `{}` is declared more than once",
                            variant.name.name
                        ),
                    )
                    .label(variant.name.span, "declared again here")
                    .label(first, "first declared here"),
                );
            }
            seen.entry(&variant.name.name).or_insert(variant.name.span);
        }
        if decl.variants.is_empty() {
            self.diags.push(
                Diagnostic::error(
                    "resolve::empty-enum",
                    format!("`{}` has no variants", self.path_of(self.module, &decl.name.name)),
                )
                .label(decl.name.span, "an enum needs at least one variant")
                .help("an enum names values of its integer type; without variants, use the integer type itself (ADR 0035)"),
            );
        }
        let ty = self.enum_tys[id.0 as usize];
        let mut variants = Vec::new();
        let mut bodies = Vec::new();
        for (i, variant) in decl.variants.iter().enumerate() {
            let payload = variant
                .payload
                .iter()
                .map(|ty| PayloadDef {
                    span: Some(ty.span),
                    ty: self.memory_ty(ty, NeverPosition::Payload),
                })
                .collect();
            let value = self.comptime_body(|this| this.expr(&variant.value));
            let span = variant.name.span.to(value.span);
            variants.push(VariantDef {
                name: variant.name.name.clone(),
                span: Some(variant.name.span),
                payload,
                value: VariantValue::Comptime(FnId(first_body + i as u32)),
            });
            bodies.push(Fn {
                name: format!("{}::{}", decl.name.name, variant.name.name),
                path: format!("{}::{}", ty.name, variant.name.name),
                symbol: None,
                name_span: variant.name.span,
                module: self.module,
                vis: Visibility::Private,
                kind: FnKind::Plain,
                ret: Ty::Int(ty.int),
                ret_span: Some(decl.ty.span),
                param_count: 0,
                locals: std::mem::take(&mut self.locals),
                body: Some(Block {
                    stmts: Vec::new(),
                    span: value.span,
                    tail: Some(Box::new(value)),
                }),
                span,
                runtime: false,
                comptime: Some(Comptime::Discriminant(id, i as u32)),
                align: None,
            });
        }
        let def = EnumDef {
            name: ty.name.to_owned(),
            name_span: Some(decl.name.span),
            module: self.module,
            vis: visibility(decl.vis),
            // Filled in once every `use` is resolved, by `reachability`.
            reachable: Vec::new(),
            ty,
            variants,
            copy,
        };
        (def, bodies)
    }

    /// A `mod`: it takes no attributes. The driver has put the items of a
    /// module file in its body; one still without a body is a file the
    /// driver could not read and has reported (`resolve::module-file`,
    /// ADR 0049), and is an empty module here.
    pub(super) fn mod_decl(&mut self, decl: &ast::ModDecl) {
        self.no_attrs(&decl.attrs, "a module");
    }

    /// A `comptime { ... }` item, as a body returning `()`.
    pub(super) fn comptime_decl(&mut self, decl: &ast::ComptimeDecl) -> Fn {
        self.no_attrs(&decl.attrs, "a `comptime` block");
        let block = self.comptime_body(|this| this.block(&decl.block));
        Fn {
            name: "comptime".to_owned(),
            path: "comptime".to_owned(),
            symbol: None,
            name_span: Span::new(
                decl.span.file,
                decl.span.lo as usize,
                decl.span.lo as usize + 8,
            ),
            module: self.module,
            vis: Visibility::Private,
            kind: FnKind::Plain,
            ret: Ty::Unit,
            ret_span: None,
            param_count: 0,
            locals: std::mem::take(&mut self.locals),
            body: Some(block),
            span: decl.span,
            runtime: false,
            comptime: Some(Comptime::Block),
            align: None,
        }
    }

    /// An array length, its body, if any, named `length` in the dumps.
    pub(super) fn array_len(&mut self, expr: &ast::Expr) -> ArrayLen {
        self.comptime_usize(expr, "length")
    }

    /// A `usize` needed at compile time (ADR 0029): known for a literal, as
    /// most are, or computed by a body of its own, named `name` in the
    /// dumps.
    pub(super) fn comptime_usize(&mut self, expr: &ast::Expr, name: &str) -> ArrayLen {
        self.comptime_int(expr, name, IntTy::Usize, Comptime::Value)
    }

    /// A value of the integer type `int` needed at compile time (ADR 0029):
    /// known for a literal, as most are, or computed by a body of its own
    /// for `comptime`, named `name` in the dumps.
    fn comptime_int(
        &mut self,
        expr: &ast::Expr,
        name: &str,
        int: IntTy,
        comptime: Comptime,
    ) -> ArrayLen {
        // A rejected literal was reported by the lexer. One too large for
        // `int` is lowered as any other value, so that type checking
        // reports it as it does `const X: usize = 4294967296;`.
        if let ast::ExprKind::Int(value) = expr.kind
            && value.is_none_or(|value| value <= int.max())
        {
            return ArrayLen::Known(value.unwrap_or(0));
        }
        ArrayLen::Comptime(self.comptime_value(expr, name, Ty::Int(int), comptime))
    }

    /// A body of its own computing `expr`, a value of type `ret` that the
    /// program needs at compile time, for `comptime`, named `name` in the
    /// dumps.
    fn comptime_value(
        &mut self,
        expr: &ast::Expr,
        name: &str,
        ret: Ty,
        comptime: Comptime,
    ) -> FnId {
        let id = FnId(self.value_base + self.values.len() as u32);
        // Variables around it exist only at run time, and are reported as
        // such if used; its own are its body's.
        let locals = std::mem::take(&mut self.locals);
        let loops = std::mem::take(&mut self.loops);
        let floor = self.comptime_floor.replace(self.scopes.len());
        let was_comptime = std::mem::replace(&mut self.in_comptime, true);
        self.scopes.push(HashMap::new());
        let value = self.expr(expr);
        self.scopes.pop();
        self.in_comptime = was_comptime;
        self.comptime_floor = floor;
        self.loops = loops;
        let body_locals = std::mem::replace(&mut self.locals, locals);
        self.values.push(Fn {
            name: name.to_owned(),
            path: name.to_owned(),
            symbol: None,
            name_span: expr.span,
            module: self.module,
            vis: Visibility::Private,
            kind: FnKind::Plain,
            ret,
            ret_span: None,
            param_count: 0,
            locals: body_locals,
            body: Some(Block {
                stmts: Vec::new(),
                span: expr.span,
                tail: Some(Box::new(value)),
            }),
            span: expr.span,
            runtime: false,
            comptime: Some(comptime),
            align: None,
        });
        id
    }

    /// Lowers code run at compile time on its own: no variables, loops or
    /// function around it.
    fn comptime_body<T>(&mut self, lower: impl FnOnce(&mut Self) -> T) -> T {
        self.locals.clear();
        self.scopes.clear();
        self.loops.clear();
        self.scopes.push(HashMap::new());
        let was_comptime = std::mem::replace(&mut self.in_comptime, true);
        let result = lower(self);
        self.in_comptime = was_comptime;
        self.scopes.pop();
        result
    }

    fn unknown_attr(&mut self, attr: &ast::Attr) {
        self.diags.push(
            Diagnostic::error(
                "resolve::unknown-attribute",
                format!("unknown attribute `#[{}]`", attr.name.name),
            )
            .label(attr.name.span, "not an attribute")
            .help("the attributes are `#[packed]` and `#[align(N)]` on structs (ADR 0022), `#[align(N)]` on statics (ADR 0030) and on functions (ADR 0087), `#[copy]` on structs and on enums with payloads (ADR 0096), `#[linear]` on structs (ADR 0101), `#[export]` on functions and statics (ADR 0051), `#[naked]` on functions (ADR 0079), and `#[interrupts]` on an enum (ADR 0089)"),
        );
    }

    fn attr_error(&mut self, span: Span, message: &str, label: &str) {
        self.diags
            .push(Diagnostic::error("resolve::attribute-argument", message).label(span, label));
    }

    /// The vector an `interrupt fn` serves: the variant of `core::arch::Vector`
    /// or of an enum with `#[interrupts]` that `at` names, resolved from the
    /// module of the handler like any path. What it names otherwise is
    /// reported; a variant that `Vector` does not have, such as another
    /// target's, is `resolve::unknown-variant` (ADR 0080, 0084, 0089).
    ///
    /// Every enum with the attribute is the list here, not only the first:
    /// `resolve::duplicate-interrupts` reports the further ones, and a handler
    /// at one of their variants gets no second error for the same mistake.
    fn handler_vector(&mut self, at: &ast::Path) -> Option<HandlerVector> {
        let vector = self.vector;
        let not_a_vector = |this: &mut Self, what: String| {
            let name = this.enum_tys[vector.0 as usize].name;
            this.diags.push(
                Diagnostic::error(
                    "resolve::not-a-vector",
                    format!("`{}` is not a variant of `{name}`", written(at)),
                )
                .label(at.span, what)
                .help(format!(
                    "an interrupt handler names the vector it serves with a variant of `{name}`, as in `at Vector::SysTick` after `use core::arch::Vector;`, or, where the chip's interrupts are vectors, with a variant of the enum that has `#[interrupts]` (ADR 0084, 0089)"
                )),
            );
        };
        match self.lookup_path(at) {
            Named::Variant(id, variant) if id == vector || self.is_interrupt_list(id) => {
                Some(HandlerVector {
                    enum_id: id,
                    variant,
                    span: at.span,
                })
            }
            Named::Variant(id, _) => {
                let other = self.enum_tys[id.0 as usize].name;
                not_a_vector(self, format!("a variant of `{other}`"));
                None
            }
            Named::Item(def) => {
                not_a_vector(self, def.what().to_owned());
                None
            }
            Named::Missing(module) => {
                let diag = Diagnostic::error(
                    "resolve::unknown-path",
                    format!("cannot find `{}`", written(at)),
                );
                let diag = self.missing(diag, at, module, "not found");
                self.diags.push(diag);
                None
            }
            Named::Failed => None,
        }
    }

    /// Whether the enum `id` has `#[interrupts]` (ADR 0089). `core` declares
    /// no such enum, so the declarations of both parts can be asked.
    fn is_interrupt_list(&self, id: EnumId) -> bool {
        let (decl, _) = self.enum_decls[id.0 as usize];
        decl.attrs.iter().any(|attr| attr.name.name == "interrupts")
    }

    pub(super) fn fn_decl(&mut self, decl: &ast::FnDecl) -> Fn {
        let external = decl.body.is_none();
        let (export, naked, align) = self.fn_attrs(&decl.attrs, external, &decl.kind);
        let vector = match &decl.kind {
            ast::FnKind::Interrupt(at) => self.handler_vector(at),
            _ => None,
        };
        self.locals.clear();
        self.scopes.clear();
        self.loops.clear();
        let ret = decl
            .ret
            .as_ref()
            .map_or(Ty::Unit, |ty| self.ty(ty, UnsizedPosition::Return));
        self.scopes.push(HashMap::new());
        let mut param_names: HashMap<&str, Span> = HashMap::new();
        for param in &decl.params {
            if let Some(&first) = param_names.get(param.name.name.as_str()) {
                self.diags.push(
                    Diagnostic::error(
                        "resolve::duplicate-param",
                        format!(
                            "the parameter `{}` is declared more than once",
                            param.name.name
                        ),
                    )
                    .label(param.name.span, "declared again here")
                    .label(first, "first declared here"),
                );
            }
            param_names.insert(&param.name.name, param.name.span);
            if param.mutable && decl.body.is_none() {
                self.diags.push(
                    Diagnostic::error(
                        "resolve::mut-extern-param",
                        "a parameter of an `extern fn` cannot be `mut`",
                    )
                    .label(param.name.span, "has no body to change it in")
                    .help("remove `mut` (ADR 0026)"),
                );
            }
            let ty = self.ty(&param.ty, UnsizedPosition::Parameter);
            self.declare(&param.name, ty, param.mutable);
        }
        let body = decl.body.as_ref().map(|body| self.block(body));
        self.scopes.pop();
        let kind = match &decl.kind {
            ast::FnKind::Plain if naked => FnKind::Naked,
            ast::FnKind::Plain => FnKind::Plain,
            ast::FnKind::Entry => FnKind::Entry,
            ast::FnKind::Panic => FnKind::Panic,
            // A vector that could not be resolved was reported, and
            // lowering errors stop the pipeline: any kind works.
            ast::FnKind::Interrupt(_) => vector.map_or(FnKind::Plain, FnKind::Interrupt),
        };
        Fn {
            name: decl.name.name.clone(),
            path: self.path_of(self.module, &decl.name.name),
            symbol: Some(if external || export {
                decl.name.name.clone()
            } else {
                path_symbol(&self.modules, self.module, &decl.name.name)
            }),
            name_span: decl.name.span,
            module: self.module,
            vis: visibility(decl.vis),
            kind,
            ret,
            ret_span: decl.ret.as_ref().map(|ty| ty.span),
            param_count: decl.params.len(),
            locals: std::mem::take(&mut self.locals),
            body,
            span: decl.span,
            runtime: false,
            comptime: None,
            align,
        }
    }
}

/// The symbol of the function or static `name` of `module`, unless it is
/// `#[export]` or `extern` (ADR 0051): its path joined by dots, and its own
/// name in the root module. A module's path is its names joined by `::`,
/// which no name contains.
fn path_symbol(modules: &[ModuleDef], module: ModuleId, name: &str) -> String {
    let prefix = &modules[module.0 as usize].path;
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{}.{name}", prefix.replace("::", "."))
    }
}
