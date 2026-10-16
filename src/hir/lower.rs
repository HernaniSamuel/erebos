//! AST -> HIR lowering with name resolution.

use std::collections::HashMap;

use crate::diag::{Diagnostic, Diagnostics};
use crate::hir::{
    ArrayLen, Block, Comptime, ConstDef, ConstId, Expr, ExprId, ExprKind, FieldDef, Fn, FnId,
    FnKind, Hir, IntTy, Local, LocalId, StaticDef, StaticId, Stmt, StructDef, StructId, StructTy,
    Ty,
};
use crate::span::Span;
use crate::syntax::ast;

/// Lowers a program, together with the runtime support functions when
/// given (ADR 0020). Returns `None` only when no HIR can be built at all (no
/// entry or panic function); other errors are reported and lowering
/// continues.
pub fn lower(
    program: &ast::File,
    runtime: Option<&ast::File>,
    diags: &mut Diagnostics,
) -> Option<Hir> {
    let items = || {
        program
            .items
            .iter()
            .chain(runtime.into_iter().flat_map(|rt| &rt.items))
    };
    let decls: Vec<&ast::FnDecl> = items()
        .filter_map(|item| match item {
            ast::Item::Fn(decl) => Some(decl),
            _ => None,
        })
        .collect();
    let struct_decls: Vec<&ast::StructDecl> = items()
        .filter_map(|item| match item {
            ast::Item::Struct(decl) => Some(decl),
            _ => None,
        })
        .collect();
    let const_decls: Vec<&ast::ConstDecl> = items()
        .filter_map(|item| match item {
            ast::Item::Const(decl) => Some(decl),
            _ => None,
        })
        .collect();
    let static_decls: Vec<&ast::StaticDecl> = items()
        .filter_map(|item| match item {
            ast::Item::Static(decl) => Some(decl),
            _ => None,
        })
        .collect();
    let comptime_decls: Vec<&ast::ComptimeDecl> = items()
        .filter_map(|item| match item {
            ast::Item::Comptime(decl) => Some(decl),
            _ => None,
        })
        .collect();
    let program_fns = program
        .items
        .iter()
        .filter(|item| matches!(item, ast::Item::Fn(_)))
        .count();

    // Names starting with `__` belong to the runtime (ADR 0020), and so do
    // the C names it defines, such as `memcpy`.
    let runtime_names: Vec<&str> = runtime
        .into_iter()
        .flat_map(|rt| &rt.items)
        .filter_map(item_name)
        .map(|name| name.name.as_str())
        .collect();
    for item in &program.items {
        let Some(ident) = item_name(item) else {
            continue;
        };
        let name = ident.name.as_str();
        let (label, help) = if name == "assert" {
            (
                "a built-in function has this name".to_owned(),
                "`assert(cond)` is built in (ADR 0029)".to_owned(),
            )
        } else if name.starts_with("__") {
            (
                "names starting with `__` are reserved".to_owned(),
                "the runtime support functions use them (ADR 0020)".to_owned(),
            )
        } else if runtime_names.contains(&name) {
            (
                "the runtime defines this function".to_owned(),
                format!("call the runtime's `{name}` instead of defining it (ADR 0020)"),
            )
        } else {
            continue;
        };
        diags.push(
            Diagnostic::error(
                "resolve::reserved-name",
                format!("`{name}` is a reserved name"),
            )
            .label(ident.span, label)
            .help(help),
        );
    }

    // Structs are global too, and their names are types.
    let mut struct_ids: HashMap<String, StructId> = HashMap::new();
    let mut struct_tys = Vec::new();
    for (i, decl) in struct_decls.iter().enumerate() {
        let name = &decl.name.name;
        let builtin = name == "bool" || IntTy::from_name(name).is_some();
        if builtin {
            diags.push(
                Diagnostic::error(
                    "resolve::duplicate-type",
                    format!("`{name}` is a built-in type"),
                )
                .label(decl.name.span, "cannot be redefined"),
            );
        } else {
            // A repeated name keeps its first definition and is reported
            // with the other items below.
            struct_ids.entry(name.clone()).or_insert(StructId(i as u32));
        }
        struct_tys.push(StructTy {
            id: StructId(i as u32),
            name: Box::leak(name.clone().into_boxed_str()),
        });
    }

    // Functions are global: any body may call any function, declared before
    // or after it. A repeated name keeps its first definition.
    let mut fn_ids = HashMap::new();
    for (i, decl) in decls.iter().enumerate() {
        fn_ids
            .entry(decl.name.name.clone())
            .or_insert((FnId(i as u32), decl.kind));
    }

    // Constants too; a repeated name keeps its first definition.
    let mut const_ids = HashMap::new();
    for (i, decl) in const_decls.iter().enumerate() {
        const_ids
            .entry(decl.name.name.clone())
            .or_insert(ConstId(i as u32));
    }

    // Statics too, in the same namespace.
    let mut static_ids = HashMap::new();
    for (i, decl) in static_decls.iter().enumerate() {
        static_ids
            .entry(decl.name.name.clone())
            .or_insert(StaticId(i as u32));
    }

    let mut lowerer = Lowerer {
        fn_ids,
        const_ids,
        static_ids,
        // Values computed at compile time get bodies after every other.
        value_base: (decls.len()
            + const_decls.len()
            + static_decls.iter().filter(|decl| !decl.external).count()
            + comptime_decls.len()) as u32,
        values: Vec::new(),
        comptime_floor: None,
        in_comptime: false,
        struct_ids,
        struct_tys,
        struct_decls: &struct_decls,
        next_expr: 0,
        diags,
        locals: Vec::new(),
        scopes: Vec::new(),
        loops: Vec::new(),
    };
    let structs: Vec<StructDef> = struct_decls
        .iter()
        .map(|decl| lowerer.struct_decl(decl))
        .collect();
    let mut fns: Vec<Fn> = decls.iter().map(|decl| lowerer.fn_decl(decl)).collect();
    for f in &mut fns[program_fns..] {
        f.runtime = true;
    }
    // Bodies run at compile time follow the functions, so function ids
    // stay those of the declarations.
    let mut consts = Vec::new();
    for (i, decl) in const_decls.iter().enumerate() {
        let init = FnId(fns.len() as u32);
        let id = ConstId(i as u32);
        let (def, body) = lowerer.const_decl(decl, id, init);
        consts.push(def);
        fns.push(body);
    }
    let mut statics = Vec::new();
    for (i, decl) in static_decls.iter().enumerate() {
        let init = FnId(fns.len() as u32);
        let (def, body) = lowerer.static_decl(decl, StaticId(i as u32), init);
        statics.push(def);
        fns.extend(body);
    }
    let mut comptime_blocks = Vec::new();
    for decl in &comptime_decls {
        comptime_blocks.push(FnId(fns.len() as u32));
        fns.push(lowerer.comptime_decl(decl));
    }
    debug_assert_eq!(fns.len() as u32, lowerer.value_base);
    fns.append(&mut lowerer.values);
    let expr_count = lowerer.next_expr;

    check_duplicate_items(program, runtime, diags);
    check_start(&fns, &statics, diags);
    let entry = find_unique(&fns, FnKind::Entry, diags);
    let panic = find_unique(&fns, FnKind::Panic, diags);

    Some(Hir {
        structs,
        fns,
        consts,
        statics,
        comptime_blocks,
        entry: entry?,
        panic: panic?,
        expr_count,
    })
}

fn item_name(item: &ast::Item) -> Option<&ast::Ident> {
    match item {
        ast::Item::Fn(decl) => Some(&decl.name),
        ast::Item::Struct(decl) => Some(&decl.name),
        ast::Item::Const(decl) => Some(&decl.name),
        ast::Item::Static(decl) => Some(&decl.name),
        ast::Item::Comptime(_) => None,
    }
}

/// Functions, structs, constants and statics share one namespace
/// (ADR 0029, 0030).
fn check_duplicate_items(
    program: &ast::File,
    runtime: Option<&ast::File>,
    diags: &mut Diagnostics,
) {
    let items = program.items.iter().map(|item| (item, false)).chain(
        runtime
            .into_iter()
            .flat_map(|rt| &rt.items)
            .map(|item| (item, true)),
    );
    let mut seen: HashMap<&str, (Span, bool)> = HashMap::new();
    for (item, in_runtime) in items {
        let Some(name) = item_name(item) else {
            continue;
        };
        match seen.get(name.name.as_str()) {
            // A program item named like a runtime one was reported as a
            // reserved name; the two live in different files.
            Some(&(_, first_in_runtime)) if first_in_runtime != in_runtime => {}
            Some(&(first, _)) => diags.push(
                Diagnostic::error(
                    "resolve::duplicate-item",
                    format!("`{}` is defined more than once", name.name),
                )
                .label(name.span, "redefined here")
                .label(first, "first defined here")
                .help("functions, structs, constants and statics share one namespace (ADR 0029, 0030)"),
            ),
            None => {
                seen.insert(&name.name, (name.span, in_runtime));
            }
        }
    }
}

/// The compiler defines `_start` itself (ADR 0012). The bodies include
/// those computing constants and statics, named after them.
fn check_start(fns: &[Fn], statics: &[StaticDef], diags: &mut Diagnostics) {
    let bodies = fns.iter().map(|f| (f.name.as_str(), f.name_span));
    // An `extern static` has no body.
    let externs = statics
        .iter()
        .filter(|s| s.init.is_none())
        .map(|s| (s.name.as_str(), s.name_span));
    for (_, span) in bodies.chain(externs).filter(|(name, _)| *name == "_start") {
        diags.push(
            Diagnostic::error("resolve::reserved-name", "`_start` is a reserved name")
                .label(span, "reserved for the startup code")
                .help("the compiler generates `_start`, which sets up the stack and jumps to the entry function (ADR 0012)"),
        );
    }
}

/// Finds the single function of a special kind (entry or panic), reporting
/// a missing or repeated one.
fn find_unique(fns: &[Fn], kind: FnKind, diags: &mut Diagnostics) -> Option<FnId> {
    let (what, example) = match kind {
        FnKind::Entry => ("entry", "entry fn main() -> ! { ... }"),
        FnKind::Panic => ("panic", "panic fn on_panic() -> ! { loop {} }"),
        FnKind::Plain => unreachable!("plain functions are not unique"),
    };
    let mut found = fns
        .iter()
        .enumerate()
        .filter(|(_, f)| f.kind == kind)
        .map(|(i, f)| (FnId(i as u32), f));

    let Some((id, first)) = found.next() else {
        diags.push(
            Diagnostic::error(
                if kind == FnKind::Entry {
                    "resolve::missing-entry"
                } else {
                    "resolve::missing-panic"
                },
                format!("no {what} function"),
            )
            .help(format!("every program declares exactly one: `{example}`")),
        );
        return None;
    };
    for (_, extra) in found {
        diags.push(
            Diagnostic::error(
                if kind == FnKind::Entry {
                    "resolve::multiple-entries"
                } else {
                    "resolve::multiple-panics"
                },
                format!("a program has exactly one {what} function"),
            )
            .label(extra.name_span, format!("second {what} function"))
            .label(first.name_span, format!("first {what} function")),
        );
    }
    Some(id)
}

struct Lowerer<'a> {
    /// Every function's id and kind, by name.
    fn_ids: HashMap<String, (FnId, ast::FnKind)>,
    /// Every constant's id, by name.
    const_ids: HashMap<String, ConstId>,
    /// Every static's id, by name.
    static_ids: HashMap<String, StaticId>,
    /// Inside `comptime expr`, the number of scopes outside it: their
    /// variables exist only at run time (ADR 0029).
    comptime_floor: Option<usize>,
    /// Lowering code that runs at compile time, where `return` has no
    /// function to leave.
    in_comptime: bool,
    /// The id of the first body of a value computed at compile time, and
    /// those bodies.
    value_base: u32,
    values: Vec<Fn>,
    /// Every struct's id, by name.
    struct_ids: HashMap<String, StructId>,
    struct_tys: Vec<StructTy>,
    struct_decls: &'a [&'a ast::StructDecl],
    next_expr: u32,
    diags: &'a mut Diagnostics,
    /// Locals of the function being lowered.
    locals: Vec<Local>,
    /// Innermost scope last. A `let` shadows earlier names, as in Rust.
    scopes: Vec<HashMap<String, LocalId>>,
    /// Enclosing loops, innermost last: the expression id and whether it is
    /// a `while` (whose `break` carries no value).
    loops: Vec<(ExprId, bool)>,
}

impl Lowerer<'_> {
    fn struct_decl(&mut self, decl: &ast::StructDecl) -> StructDef {
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
            name: decl.name.name.clone(),
            name_span: decl.name.span,
            fields: decl
                .fields
                .iter()
                .map(|field| FieldDef {
                    name: field.name.name.clone(),
                    span: field.name.span,
                    ty: self.ty(&field.ty),
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

    /// Items other than structs and statics take no attributes yet; `what`
    /// names the item.
    fn no_attrs(&mut self, attrs: &[ast::Attr], what: &str) {
        for attr in attrs {
            match attr.name.name.as_str() {
                "packed" | "align" => self.misplaced_attr(attr, what),
                _ => self.unknown_attr(attr),
            }
        }
    }

    fn misplaced_attr(&mut self, attr: &ast::Attr, what: &str) {
        let applies_to = if attr.name.name == "align" {
            "structs and statics"
        } else {
            "structs"
        };
        self.diags.push(
            Diagnostic::error(
                "resolve::misplaced-attribute",
                format!("`#[{}]` applies only to {applies_to}", attr.name.name),
            )
            .label(attr.span, format!("not allowed on {what}")),
        );
    }

    /// `#[align(N)]`, at most once, is the only attribute of a static; an
    /// `extern static` takes none, since its definition decides its
    /// alignment (ADR 0030). Returns it.
    fn static_attrs(&mut self, attrs: &[ast::Attr], external: bool) -> Option<(ArrayLen, Span)> {
        if external {
            self.no_attrs(attrs, "an `extern static`");
            return None;
        }
        let mut align: Option<(ArrayLen, Span)> = None;
        let mut align_attr: Option<Span> = None;
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
                "packed" => self.misplaced_attr(attr, "a static"),
                _ => self.unknown_attr(attr),
            }
        }
        align
    }

    /// A static, and the body that computes its initial value, `init`; an
    /// `extern static` has none (ADR 0030).
    fn static_decl(
        &mut self,
        decl: &ast::StaticDecl,
        id: StaticId,
        init: FnId,
    ) -> (StaticDef, Option<Fn>) {
        let align = self.static_attrs(&decl.attrs, decl.external);
        let ty = self.ty(&decl.ty);
        let mut def = StaticDef {
            name: decl.name.name.clone(),
            name_span: decl.name.span,
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
            name_span: decl.name.span,
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
    fn const_decl(&mut self, decl: &ast::ConstDecl, id: ConstId, init: FnId) -> (ConstDef, Fn) {
        self.no_attrs(&decl.attrs, "a constant");
        let ty = self.ty(&decl.ty);
        let value = self.comptime_body(|this| this.expr(&decl.value));
        let def = ConstDef {
            name: decl.name.name.clone(),
            name_span: decl.name.span,
            ty,
            init,
        };
        let body = Fn {
            name: decl.name.name.clone(),
            name_span: decl.name.span,
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

    /// A `comptime { ... }` item, as a body returning `()`.
    fn comptime_decl(&mut self, decl: &ast::ComptimeDecl) -> Fn {
        self.no_attrs(&decl.attrs, "a `comptime` block");
        let block = self.comptime_body(|this| this.block(&decl.block));
        Fn {
            name: "comptime".to_owned(),
            name_span: Span::new(
                decl.span.file,
                decl.span.lo as usize,
                decl.span.lo as usize + 8,
            ),
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

    /// A `usize` needed at compile time (ADR 0029): known for a literal, as
    /// most are, or computed by a body of its own.
    fn array_len(&mut self, expr: &ast::Expr) -> ArrayLen {
        if let ast::ExprKind::Int(value) = expr.kind {
            // A rejected literal was reported by the lexer.
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
            name: "length".to_owned(),
            name_span: expr.span,
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
            .help("the attributes are `#[packed]` and `#[align(N)]` on structs (ADR 0022), and `#[align(N)]` on statics (ADR 0030)"),
        );
    }

    fn attr_error(&mut self, span: Span, message: &str, label: &str) {
        self.diags
            .push(Diagnostic::error("resolve::attribute-argument", message).label(span, label));
    }

    /// `Name { field: value, ... }`: every field exactly once, in any order.
    fn struct_lit(
        &mut self,
        expr: &ast::Expr,
        name: &ast::Ident,
        inits: &[ast::FieldInit],
    ) -> ExprKind {
        let id = self.struct_ids.get(&name.name).copied();
        let fields: Vec<(Option<u32>, Expr)> = inits
            .iter()
            .map(|init| {
                let decl = id.map(|id| self.struct_decls[id.0 as usize]);
                let index = decl.and_then(|decl| {
                    decl.fields
                        .iter()
                        .position(|f| f.name.name == init.name.name)
                });
                (index.map(|i| i as u32), self.expr(&init.value))
            })
            .collect();
        let Some(id) = id else {
            self.diags.push(
                Diagnostic::error(
                    "resolve::unknown-type",
                    format!("unknown struct `{}`", name.name),
                )
                .label(name.span, "not a struct"),
            );
            return ExprKind::Unit;
        };
        let decl = self.struct_decls[id.0 as usize];
        let mut given: Vec<Option<Span>> = vec![None; decl.fields.len()];
        for (init, (index, _)) in inits.iter().zip(&fields) {
            match index {
                None => self.diags.push(
                    Diagnostic::error(
                        "resolve::unknown-field",
                        format!("`{}` has no field `{}`", name.name, init.name.name),
                    )
                    .label(init.name.span, "unknown field")
                    .label(decl.name.span, format!("`{}` is declared here", name.name)),
                ),
                Some(i) => match given[*i as usize] {
                    Some(first) => self.diags.push(
                        Diagnostic::error(
                            "resolve::duplicate-field",
                            format!("the field `{}` is given more than once", init.name.name),
                        )
                        .label(init.name.span, "given again here")
                        .label(first, "first given here"),
                    ),
                    None => given[*i as usize] = Some(init.name.span),
                },
            }
        }
        // A field declared twice was reported; only its first counts.
        let missing: Vec<&str> = decl
            .fields
            .iter()
            .zip(&given)
            .enumerate()
            .filter(|(i, (field, given))| {
                given.is_none()
                    && decl.fields[..*i]
                        .iter()
                        .all(|earlier| earlier.name.name != field.name.name)
            })
            .map(|(_, (field, _))| field.name.name.as_str())
            .collect();
        if !missing.is_empty() {
            let list = missing
                .iter()
                .map(|name| format!("`{name}`"))
                .collect::<Vec<_>>()
                .join(", ");
            self.diags.push(
                Diagnostic::error(
                    "resolve::missing-fields",
                    format!("missing fields in `{}`: {list}", name.name),
                )
                .label(expr.span, "every field must be given (ADR 0021)"),
            );
        }
        ExprKind::StructLit(
            self.struct_tys[id.0 as usize],
            fields
                .into_iter()
                .filter_map(|(index, value)| Some((index?, value)))
                .collect(),
        )
    }

    fn fn_decl(&mut self, decl: &ast::FnDecl) -> Fn {
        self.no_attrs(&decl.attrs, "a function");
        self.locals.clear();
        self.scopes.clear();
        self.loops.clear();
        let ret = decl.ret.as_ref().map_or(Ty::Unit, |ty| self.ty(ty));
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
            let ty = self.ty(&param.ty);
            self.declare(&param.name, ty, param.mutable);
        }
        let body = decl.body.as_ref().map(|body| self.block(body));
        self.scopes.pop();
        Fn {
            name: decl.name.name.clone(),
            name_span: decl.name.span,
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

    fn ty(&mut self, ty: &ast::Ty) -> Ty {
        match &ty.kind {
            ast::TyKind::Never => Ty::Never,
            ast::TyKind::Unit => Ty::Unit,
            ast::TyKind::Ptr {
                mutable,
                volatile,
                pointee,
            } => {
                let pointee = self.ty(pointee);
                Ty::ptr(*mutable, *volatile, pointee)
            }
            ast::TyKind::Array { elem, len } => {
                let elem = self.ty(elem);
                let len = self.array_len(len);
                Ty::array_of(elem, len)
            }
            ast::TyKind::Named(name) if name.name == "bool" => Ty::Bool,
            ast::TyKind::Named(name) => match IntTy::from_name(&name.name) {
                Some(int) => Ty::Int(int),
                None if self.struct_ids.contains_key(&name.name) => {
                    Ty::Struct(self.struct_tys[self.struct_ids[&name.name].0 as usize])
                }
                None => {
                    self.diags.push(
                        Diagnostic::error(
                            "resolve::unknown-type",
                            format!("unknown type `{}`", name.name),
                        )
                        .label(name.span, "not a type"),
                    );
                    Ty::Error
                }
            },
        }
    }

    fn block(&mut self, block: &ast::Block) -> Block {
        self.scopes.push(HashMap::new());
        let stmts = block.stmts.iter().map(|stmt| self.stmt(stmt)).collect();
        let tail = block.tail.as_ref().map(|e| Box::new(self.expr(e)));
        self.scopes.pop();
        Block {
            stmts,
            tail,
            span: block.span,
        }
    }

    fn stmt(&mut self, stmt: &ast::Stmt) -> Stmt {
        match stmt {
            ast::Stmt::Let(let_) => {
                // The initializer is resolved before the new name exists, so
                // `let x: u32 = x;` refers to an outer `x`.
                let init = self.expr(&let_.init);
                let ty = self.ty(&let_.ty);
                let local = self.declare(&let_.name, ty, let_.mutable);
                Stmt::Let { local, init }
            }
            ast::Stmt::Expr { expr, semi } => Stmt::Expr {
                expr: self.expr(expr),
                semi: *semi,
            },
        }
    }

    /// Adds a local to the innermost scope, shadowing earlier ones.
    fn declare(&mut self, name: &ast::Ident, ty: Ty, mutable: bool) -> LocalId {
        let local = LocalId(self.locals.len() as u32);
        self.locals.push(Local {
            name: name.name.clone(),
            ty,
            mutable,
            span: name.span,
        });
        self.scopes
            .last_mut()
            .expect("locals are declared inside a scope")
            .insert(name.name.clone(), local);
        local
    }

    fn find_local(&self, name: &str) -> Option<LocalId> {
        self.find_local_scope(name).map(|(local, _)| local)
    }

    /// The local `name` refers to, and the index of its scope.
    fn find_local_scope(&self, name: &str) -> Option<(LocalId, usize)> {
        self.scopes
            .iter()
            .enumerate()
            .rev()
            .find_map(|(depth, scope)| scope.get(name).map(|&local| (local, depth)))
    }

    /// A name used as a value: a local, shadowing an item of the same name,
    /// or a constant or static.
    fn resolve(&mut self, name: &ast::Ident) -> Option<ExprKind> {
        if let Some((local, depth)) = self.find_local_scope(&name.name) {
            if self.comptime_floor.is_some_and(|floor| depth < floor) {
                self.diags.push(
                    Diagnostic::error(
                        "ty::not-comptime",
                        format!("`{}` is only known at run time", name.name),
                    )
                    .label(name.span, "a variable of the running program, used at compile time")
                    .help("compile-time code can use constants, literals and function calls (ADR 0029)"),
                );
                return None;
            }
            return Some(ExprKind::Local(local));
        }
        if let Some(&id) = self.const_ids.get(&name.name) {
            return Some(ExprKind::Const(id));
        }
        if let Some(&id) = self.static_ids.get(&name.name) {
            return Some(ExprKind::Static(id));
        }
        let mut diag = Diagnostic::error(
            "resolve::unknown-name",
            format!("cannot find `{}` in this scope", name.name),
        );
        if self.fn_ids.contains_key(&name.name) {
            diag = diag
                .label(name.span, "a function, which is not a value")
                .help(format!("call it: `{}(...)`", name.name));
        } else {
            diag = diag.label(name.span, "not found");
        }
        self.diags.push(diag);
        None
    }

    /// Resolves the callee of a call. Locals shadow functions, as in Rust.
    fn resolve_fn(&mut self, name: &ast::Ident) -> Option<FnId> {
        if let Some(local) = self.find_local(&name.name) {
            let declared = self.locals[local.0 as usize].span;
            self.diags.push(
                Diagnostic::error(
                    "resolve::not-a-function",
                    format!("`{}` is a variable, not a function", name.name),
                )
                .label(name.span, "called here")
                .label(declared, "declared here"),
            );
            return None;
        }
        match self.fn_ids.get(&name.name) {
            Some(&(_, ast::FnKind::Entry)) => {
                self.diags.push(
                    Diagnostic::error("resolve::call-entry", "the entry function cannot be called")
                        .label(name.span, "called here")
                        .help("execution starts there; it is not a function to call"),
                );
                None
            }
            Some(&(id, _)) => Some(id),
            None => {
                self.diags.push(
                    Diagnostic::error(
                        "resolve::unknown-function",
                        format!("cannot find function `{}`", name.name),
                    )
                    .label(name.span, "not found"),
                );
                None
            }
        }
    }

    fn expr(&mut self, expr: &ast::Expr) -> Expr {
        let id = ExprId(self.next_expr);
        self.next_expr += 1;
        let kind = match &expr.kind {
            // Rejected literals were reported by the lexer, and the pipeline
            // stops before HIR when there are syntax errors.
            ast::ExprKind::Int(value) => ExprKind::Int(value.unwrap_or(0)),
            ast::ExprKind::Byte(value) => ExprKind::Byte(value.unwrap_or(0)),
            ast::ExprKind::Bool(value) => ExprKind::Bool(*value),
            ast::ExprKind::Unit => ExprKind::Unit,
            ast::ExprKind::Array(elems) => {
                ExprKind::Array(elems.iter().map(|e| self.expr(e)).collect())
            }
            ast::ExprKind::Repeat(value, len) => {
                let value = self.boxed(value);
                ExprKind::Repeat(value, self.array_len(len))
            }
            ast::ExprKind::Name(name) => match self.resolve(name) {
                Some(kind) => kind,
                // Reported; any placeholder works since lowering errors stop
                // the pipeline.
                None => ExprKind::Unit,
            },
            ast::ExprKind::Call(name, args) if name.name == "assert" => {
                let mut args: Vec<Expr> = args.iter().map(|arg| self.expr(arg)).collect();
                if args.len() != 1 {
                    self.diags.push(
                        Diagnostic::error(
                            "ty::wrong-arg-count",
                            format!("`assert` takes 1 argument, but {} were given", args.len()),
                        )
                        .label(expr.span, "in this call")
                        .help("`assert(cond)` checks one `bool` (ADR 0029)"),
                    );
                    return Expr {
                        id,
                        kind: ExprKind::Unit,
                        span: expr.span,
                    };
                }
                ExprKind::Assert(Box::new(args.remove(0)))
            }
            ast::ExprKind::Call(name, args) => {
                let callee = self.resolve_fn(name);
                let args = args.iter().map(|arg| self.expr(arg)).collect();
                match callee {
                    Some(callee) => ExprKind::Call { callee, args },
                    // Reported; lowering errors stop the pipeline.
                    None => ExprKind::Unit,
                }
            }
            ast::ExprKind::Unary(op, operand) => ExprKind::Unary(*op, self.boxed(operand)),
            ast::ExprKind::Deref(operand) => ExprKind::Deref(self.boxed(operand)),
            ast::ExprKind::StructLit(name, fields) => self.struct_lit(expr, name, fields),
            ast::ExprKind::Field(base, field) => {
                ExprKind::Field(self.boxed(base), field.name.clone(), field.span)
            }
            ast::ExprKind::SizeOf(ty) => ExprKind::SizeOf(self.ty(ty)),
            ast::ExprKind::AlignOf(ty) => ExprKind::AlignOf(self.ty(ty)),
            ast::ExprKind::OffsetOf(ty, field) => {
                ExprKind::OffsetOf(self.ty(ty), field.name.clone(), field.span)
            }
            ast::ExprKind::Index(base, index) => {
                ExprKind::Index(self.boxed(base), self.boxed(index))
            }
            ast::ExprKind::AddrOf { mutable, place } => ExprKind::AddrOf {
                mutable: *mutable,
                place: self.boxed(place),
            },
            ast::ExprKind::Binary(op, lhs, rhs) => {
                ExprKind::Binary(*op, self.boxed(lhs), self.boxed(rhs))
            }
            ast::ExprKind::Logical(op, lhs, rhs) => {
                ExprKind::Logical(*op, self.boxed(lhs), self.boxed(rhs))
            }
            ast::ExprKind::Cast(operand, ty) => {
                ExprKind::Cast(self.boxed(operand), self.ty(ty), ty.span)
            }
            ast::ExprKind::Assign(place, value) => {
                ExprKind::Assign(self.boxed(place), self.boxed(value))
            }
            ast::ExprKind::CompoundAssign(op, place, value) => {
                ExprKind::CompoundAssign(*op, self.boxed(place), self.boxed(value))
            }
            ast::ExprKind::Block(block) => ExprKind::Block(self.block(block)),
            ast::ExprKind::If { cond, then, else_ } => ExprKind::If {
                cond: self.boxed(cond),
                then: self.block(then),
                else_: else_.as_ref().map(|e| self.boxed(e)),
            },
            ast::ExprKind::While { cond, body } => {
                let cond = self.boxed(cond);
                self.loops.push((id, true));
                let body = self.block(body);
                self.loops.pop();
                ExprKind::While { cond, body }
            }
            ast::ExprKind::Loop(body) => {
                self.loops.push((id, false));
                let body = self.block(body);
                self.loops.pop();
                ExprKind::Loop(body)
            }
            ast::ExprKind::Break(value) => {
                let target = self.loop_target(expr.span, "break");
                if let (Some((_, true)), Some(value)) = (self.loops.last(), value) {
                    self.diags.push(
                        Diagnostic::error(
                            "resolve::break-value-in-while",
                            "`break` with a value is only allowed in `loop`",
                        )
                        .label(value.span, "a `while` loop has no value")
                        .help("use `loop` with an `if` to break with a value"),
                    );
                }
                ExprKind::Break {
                    target,
                    value: value.as_ref().map(|e| self.boxed(e)),
                }
            }
            ast::ExprKind::Continue => ExprKind::Continue {
                target: self.loop_target(expr.span, "continue"),
            },
            ast::ExprKind::Return(value) => {
                if self.in_comptime {
                    self.diags.push(
                        Diagnostic::error(
                            "resolve::return-in-comptime",
                            "`return` cannot leave code that runs at compile time",
                        )
                        .label(expr.span, "no function to return from")
                        .help(
                            "compile-time code gives its value as its last expression (ADR 0029)",
                        ),
                    );
                }
                ExprKind::Return(value.as_ref().map(|e| self.boxed(e)))
            }
            ast::ExprKind::Comptime(inner) => {
                // The enclosing function's variables and loops exist only at
                // run time (ADR 0029).
                let floor = self.comptime_floor.replace(self.scopes.len());
                let loops = std::mem::take(&mut self.loops);
                let was_comptime = std::mem::replace(&mut self.in_comptime, true);
                self.scopes.push(HashMap::new());
                let inner = self.boxed(inner);
                self.scopes.pop();
                self.in_comptime = was_comptime;
                self.loops = loops;
                self.comptime_floor = floor;
                ExprKind::Comptime(inner)
            }
        };
        Expr {
            id,
            kind,
            span: expr.span,
        }
    }

    /// The innermost enclosing loop. Outside a loop this reports an error and
    /// returns a placeholder, since lowering errors stop the pipeline.
    fn loop_target(&mut self, span: Span, keyword: &str) -> ExprId {
        match self.loops.last() {
            Some(&(id, _)) => id,
            None => {
                self.diags.push(
                    Diagnostic::error(
                        "resolve::outside-loop",
                        format!("`{keyword}` outside of a loop"),
                    )
                    .label(span, "not inside `loop` or `while`"),
                );
                ExprId(0)
            }
        }
    }

    fn boxed(&mut self, expr: &ast::Expr) -> Box<Expr> {
        Box::new(self.expr(expr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::SourceMap;
    use crate::syntax::{lexer, parser};

    fn parse(
        sources: &mut SourceMap,
        name: &str,
        text: &str,
        diags: &mut Diagnostics,
    ) -> ast::File {
        let file = sources.add(name, text);
        let tokens = lexer::lex(file, text, diags);
        parser::parse(text, &tokens, diags)
    }

    /// Dumps leave the runtime out, so snapshot tests cannot show this.
    #[test]
    fn runtime_names_are_reserved_in_the_program_only() {
        let mut sources = SourceMap::default();
        let mut diags = Diagnostics::default();
        let program = parse(
            &mut sources,
            "main.ers",
            "entry fn main() -> ! { loop {} }
             panic fn on_panic() -> ! { loop {} }
             fn memcpy() {}
             fn __helper() {}
",
            &mut diags,
        );
        let runtime = parse(
            &mut sources,
            "rt.ers",
            "fn memcpy() {}
",
            &mut diags,
        );
        let main = program_file(&program);
        lower(&program, Some(&runtime), &mut diags);

        let reported: Vec<_> = diags
            .into_vec()
            .into_iter()
            .map(|d| (d.code, d.labels.iter().all(|l| l.span.file == main)))
            .collect();
        assert_eq!(
            reported,
            [
                ("resolve::reserved-name", true),
                ("resolve::reserved-name", true)
            ]
        );
    }

    fn program_file(file: &ast::File) -> crate::span::FileId {
        item_name(&file.items[0])
            .expect("the first item has a name")
            .span
            .file
    }
}
