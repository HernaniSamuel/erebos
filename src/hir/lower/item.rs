//! Lowering items: structs, functions, statics, constants, enums, modules,
//! `comptime` blocks, and their attributes.

use std::collections::HashMap;

use crate::diag::Diagnostic;
use crate::hir::{
    ArrayLen, Block, Comptime, ConstDef, ConstId, EnumDef, EnumId, FieldDef, Fn, FnId, FnKind,
    IntTy, ModuleDef, ModuleId, NeverPosition, PayloadDef, StaticDef, StaticId, StructDef, Ty,
    UnsizedPosition, VariantDef, VariantValue, Visibility,
};
use crate::span::Span;
use crate::syntax::ast;

use super::{Lowerer, visibility};

impl Lowerer<'_> {
    pub(super) fn struct_decl(&mut self, decl: &ast::StructDecl) -> StructDef {
        let mut seen: HashMap<&str, Span> = HashMap::new();
        for field in &decl.fields {
            if let Some(&first) = seen.get(field.name.name.as_str()) {
                self.diags.push(
                    Diagnostic::error(
                        "resolve::duplicate-field",
                        format!("the field `{}` is declared more than once", field.name.name),
                    )
                    .label(field.name.span, "declared again here")
                    .label(first, "first declared here"),
                );
            }
            seen.entry(&field.name.name).or_insert(field.name.span);
        }
        let (packed, align) = self.struct_attrs(&decl.attrs);
        StructDef {
            packed,
            align,
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

    /// Layout attributes (ADR 0022): `#[packed]` and `#[align(N)]`, at
    /// most once each, and not together. Returns them.
    fn struct_attrs(&mut self, attrs: &[ast::Attr]) -> (bool, Option<(ArrayLen, Span)>) {
        let mut packed: Option<Span> = None;
        let mut align: Option<(ArrayLen, Span)> = None;
        let mut align_attr: Option<Span> = None;
        for attr in attrs {
            let first = match attr.name.name.as_str() {
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
                "export" => {
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
        (packed.is_some(), align)
    }

    /// Items other than structs, statics and functions with a body take no
    /// attributes; `what` names the item.
    pub(super) fn no_attrs(&mut self, attrs: &[ast::Attr], what: &str) {
        for attr in attrs {
            match attr.name.name.as_str() {
                "packed" | "align" | "export" => self.misplaced_attr(attr, what),
                _ => self.unknown_attr(attr),
            }
        }
    }

    fn misplaced_attr(&mut self, attr: &ast::Attr, what: &str) {
        let applies_to = match attr.name.name.as_str() {
            "align" => "structs and statics",
            "export" => "functions and statics defined in the program",
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
        self.diags.push(diag);
    }

    /// `#[export]` (ADR 0051): no argument, at most once; `export` holds
    /// the span of the one already given.
    fn export_attr(&mut self, attr: &ast::Attr, export: &mut Option<Span>) {
        if let Some(arg) = &attr.arg {
            self.attr_error(arg.span, "`#[export]` takes no argument", "remove this");
        }
        if let Some(first) = export.replace(attr.span) {
            self.diags.push(
                Diagnostic::error(
                    "resolve::duplicate-attribute",
                    "`#[export]` is given more than once",
                )
                .label(attr.span, "given again here")
                .label(first, "first given here"),
            );
        }
    }

    /// `#[export]` is the only attribute of a function with a body; an
    /// `extern fn` takes none (ADR 0051). Returns whether it is exported.
    fn fn_attrs(&mut self, attrs: &[ast::Attr], external: bool) -> bool {
        if external {
            self.no_attrs(attrs, "an `extern fn`");
            return false;
        }
        let mut export = None;
        for attr in attrs {
            match attr.name.name.as_str() {
                "export" => self.export_attr(attr, &mut export),
                "packed" | "align" => self.misplaced_attr(attr, "a function"),
                _ => self.unknown_attr(attr),
            }
        }
        export.is_some()
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
                "export" => self.export_attr(attr, &mut export),
                "packed" => self.misplaced_attr(attr, "a static"),
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
        self.no_attrs(&decl.attrs, "an enum");
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
        // A rejected literal was reported by the lexer. One too large for
        // `usize` is lowered as any other length, so that type checking
        // reports it as it does `const X: usize = 4294967296;`.
        if let ast::ExprKind::Int(value) = expr.kind
            && value.is_none_or(|value| value <= IntTy::Usize.max())
        {
            return ArrayLen::Known(value.unwrap_or(0));
        }
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
            ret: Ty::Int(IntTy::Usize),
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
            comptime: Some(Comptime::Value),
        });
        ArrayLen::Comptime(id)
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
            .help("the attributes are `#[packed]` and `#[align(N)]` on structs (ADR 0022), `#[align(N)]` on statics (ADR 0030), and `#[export]` on functions and statics (ADR 0051)"),
        );
    }

    fn attr_error(&mut self, span: Span, message: &str, label: &str) {
        self.diags
            .push(Diagnostic::error("resolve::attribute-argument", message).label(span, label));
    }

    pub(super) fn fn_decl(&mut self, decl: &ast::FnDecl) -> Fn {
        let external = decl.body.is_none();
        let export = self.fn_attrs(&decl.attrs, external);
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
            kind: match decl.kind {
                ast::FnKind::Plain => FnKind::Plain,
                ast::FnKind::Entry => FnKind::Entry,
                ast::FnKind::Panic => FnKind::Panic,
            },
            ret,
            ret_span: decl.ret.as_ref().map(|ty| ty.span),
            param_count: decl.params.len(),
            locals: std::mem::take(&mut self.locals),
            body,
            span: decl.span,
            runtime: false,
            comptime: None,
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
