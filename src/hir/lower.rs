//! AST -> HIR lowering with name resolution.

use std::collections::HashMap;

use crate::diag::{Diagnostic, Diagnostics};
use crate::hir::{
    self, Arm, ArrayLen, Block, BuiltinEnum, Comptime, ConstDef, ConstId, CoreStart, EnumDef,
    EnumId, EnumTy, Expr, ExprId, ExprKind, FieldDef, FileDef, Fn, FnId, FnKind, Hir, IntTy, Local,
    LocalId, ModuleDef, ModuleId, NeverInMemory, NeverPosition, Pattern, PatternId, PatternKind,
    PayloadDef, StaticDef, StaticId, Stmt, StructDef, StructId, StructTy, Ty, UnsizedPosition,
    UnsizedType, VariantDef, VariantValue, Visibility,
};
use crate::span::Span;
use crate::syntax::ast;
use crate::syntax::parser::builtin_variant_help;

/// Lowers a program, together with `core` (ADR 0052), whose root file is
/// `core_file`. `files` are the source files, by id, `core`'s from index
/// `core_files` on. Returns `None` only when no HIR can be built at all (no
/// entry or panic function); other errors are reported and lowering
/// continues.
pub fn lower(
    program: &ast::File,
    core_file: &ast::File,
    files: Vec<FileDef>,
    core_files: usize,
    diags: &mut Diagnostics,
) -> Option<Hir> {
    // Every item of every module is known before any is lowered, so that
    // items refer to each other in any order. `core` is a module tree of its
    // own, after the program's.
    let mut items = Items {
        modules: vec![ModuleDef {
            name: String::new(),
            path: String::new(),
            name_span: None,
            parent: None,
            vis: Visibility::Pub,
        }],
        namespaces: vec![HashMap::new()],
        core_fns: usize::MAX,
        core_fn_base: 0,
        fns: Vec::new(),
        structs: Vec::new(),
        enums: Vec::new(),
        consts: Vec::new(),
        statics: Vec::new(),
        comptimes: Vec::new(),
        mods: Vec::new(),
        use_decls: Vec::new(),
        uses: Vec::new(),
        duplicates: Vec::new(),
    };
    items.collect(&program.items, ModuleId::ROOT, false, diags);
    let program_end = items.counts();
    let core_root = ModuleId(items.modules.len() as u32);
    items.modules.push(ModuleDef {
        name: "core".to_owned(),
        path: "core".to_owned(),
        name_span: None,
        parent: None,
        vis: Visibility::Pub,
    });
    items.namespaces.push(HashMap::new());
    // Every body of the program, those run at compile time included, comes
    // before `core`'s, so that the program's ids are its own.
    items.core_fns = program_end.fns;
    items.core_fn_base = items.bodies(Counts::default(), program_end);
    items.collect(&core_file.items, core_root, true, diags);
    let core_end = items.counts();
    // Values computed at compile time get bodies after every other.
    let value_base = items.core_fn_base + items.bodies(program_end, core_end);
    items.declare_uses(diags);
    let Items {
        modules,
        namespaces,
        core_fns,
        core_fn_base,
        fns: fn_decls,
        structs: struct_decls,
        enums: enum_decls,
        consts: const_decls,
        statics: static_decls,
        comptimes: comptime_decls,
        mods: mod_decls,
        use_decls,
        uses,
        duplicates,
    } = items;

    // Structs are types, shown by their path (ADR 0049).
    let struct_tys = struct_decls
        .iter()
        .enumerate()
        .map(|(i, &(decl, module))| StructTy {
            id: StructId(i as u32),
            name: Box::leak(item_path(&modules, module, &decl.name.name).into_boxed_str()),
        })
        .collect();

    // Enums too, each with the integer type it is stored as (ADR 0035), or
    // its tag's, and whether a variant carries a payload (ADR 0039).
    let mut enum_tys = Vec::new();
    for (i, &(decl, module)) in enum_decls.iter().enumerate() {
        let int = match &decl.ty.kind {
            ast::TyKind::Named(path)
                if let Some(int) = path.as_single().and_then(|ty| IntTy::from_name(&ty.name)) =>
            {
                int
            }
            _ => {
                diags.push(
                    Diagnostic::error(
                        "resolve::enum-type",
                        "the type of an enum must be an integer type",
                    )
                    .label(decl.ty.span, "not an integer type")
                    .help("an enum is stored as one of `u8 u16 u32 u64 usize i8 i16 i32 i64 isize` (ADR 0035)"),
                );
                // Reported; any placeholder works since lowering errors
                // stop the pipeline.
                IntTy::U8
            }
        };
        enum_tys.push(EnumTy {
            id: EnumId(i as u32),
            name: Box::leak(item_path(&modules, module, &decl.name.name).into_boxed_str()),
            int,
            payloads: decl.variants.iter().any(|v| !v.payload.is_empty()),
        });
    }

    let mut lowerer = Lowerer {
        modules,
        namespaces,
        module: ModuleId::ROOT,
        core_root,
        fn_decls: &fn_decls,
        core_fns,
        core_fn_base,
        value_base,
        values: Vec::new(),
        comptime_floor: None,
        in_comptime: false,
        struct_tys,
        struct_decls: &struct_decls,
        enum_tys,
        enum_decls: &enum_decls,
        const_decls: &const_decls,
        static_decls: &static_decls,
        uses,
        use_stack: Vec::new(),
        never_in_memory: Vec::new(),
        unsized_types: Vec::new(),
        applications: Vec::new(),
        next_expr: 0,
        next_pattern: 0,
        diags,
        locals: Vec::new(),
        scopes: Vec::new(),
        loops: Vec::new(),
    };
    for &(decl, parent) in &mod_decls {
        lowerer.module = parent;
        lowerer.mod_decl(decl);
    }
    lowerer.use_decls(&use_decls);

    // The program's items, then `core`'s, each part lowered whole, so that
    // the program's expression ids are those of the program alone.
    let mut structs = Vec::new();
    let mut fns = Vec::new();
    let mut consts = Vec::new();
    let mut statics = Vec::new();
    let mut comptime_blocks = Vec::new();
    let mut enums = Vec::new();
    let mut core = None;
    for (from, to, in_core) in [
        (Counts::default(), program_end, false),
        (program_end, core_end, true),
    ] {
        if in_core {
            debug_assert_eq!(fns.len() as u32, core_fn_base);
            core = Some(CoreStart {
                module: core_root,
                expr: lowerer.next_expr,
                never_in_memory: lowerer.never_in_memory.len(),
                unsized_types: lowerer.unsized_types.len(),
                applications: lowerer.applications.len(),
                file: core_files,
            });
        }
        for &(decl, module) in &struct_decls[from.structs..to.structs] {
            lowerer.module = module;
            structs.push(lowerer.struct_decl(decl));
        }
        for &(decl, module) in &fn_decls[from.fns..to.fns] {
            lowerer.module = module;
            let mut f = lowerer.fn_decl(decl);
            // LLVM must not turn the loops of `core` into calls to `core`
            // (ADR 0020).
            f.runtime = in_core;
            fns.push(f);
        }
        // Bodies run at compile time follow the functions, so function ids
        // stay those of the declarations.
        for (i, &(decl, module)) in const_decls
            .iter()
            .enumerate()
            .take(to.consts)
            .skip(from.consts)
        {
            lowerer.module = module;
            let init = FnId(fns.len() as u32);
            let (def, body) = lowerer.const_decl(decl, ConstId(i as u32), init);
            consts.push(def);
            fns.push(body);
        }
        for (i, &(decl, module)) in static_decls
            .iter()
            .enumerate()
            .take(to.statics)
            .skip(from.statics)
        {
            lowerer.module = module;
            let init = FnId(fns.len() as u32);
            let (def, body) = lowerer.static_decl(decl, StaticId(i as u32), init);
            statics.push(def);
            fns.extend(body);
        }
        for &(decl, module) in &comptime_decls[from.comptimes..to.comptimes] {
            lowerer.module = module;
            comptime_blocks.push(FnId(fns.len() as u32));
            fns.push(lowerer.comptime_decl(decl));
        }
        for (i, &(decl, module)) in enum_decls
            .iter()
            .enumerate()
            .take(to.enums)
            .skip(from.enums)
        {
            lowerer.module = module;
            let (def, bodies) = lowerer.enum_decl(decl, EnumId(i as u32), fns.len() as u32);
            enums.push(def);
            fns.extend(bodies);
        }
    }
    debug_assert_eq!(fns.len() as u32, lowerer.value_base);
    fns.append(&mut lowerer.values);
    let expr_count = lowerer.next_expr;
    let never_in_memory = lowerer.never_in_memory;
    let unsized_types = lowerer.unsized_types;
    let applications = lowerer.applications;
    let modules = lowerer.modules;
    let namespaces = lowerer.namespaces;
    let uses = lowerer.uses;
    reachability(
        &modules,
        &namespaces,
        &uses,
        core_root,
        &mut structs,
        &mut enums,
    );

    for diag in duplicates {
        diags.push(diag);
    }
    check_start(&fns, &statics, diags);
    check_symbols(&fns, &statics, &namespaces, &uses, core_root, diags);
    // In any module, whatever their visibility (ADR 0049, 0050).
    let entry = find_unique(&fns, FnKind::Entry, diags);
    let panic = find_unique(&fns, FnKind::Panic, diags);
    // The items of `core`'s root that the compiler knows (ADR 0052).
    let core_item = |name: &str| match namespaces[core_root.0 as usize].get(name) {
        Some(Entry {
            binding: Binding::Def(def),
            ..
        }) => *def,
        _ => unreachable!("`core` declares `{name}` in its root"),
    };
    let Def::Struct(panic_info) = core_item("PanicInfo") else {
        unreachable!("`core::PanicInfo` is a struct")
    };
    let Def::Enum(panic_reason) = core_item("PanicReason") else {
        unreachable!("`core::PanicReason` is an enum")
    };

    Some(Hir {
        modules,
        structs,
        enums,
        fns,
        consts,
        statics,
        comptime_blocks,
        never_in_memory,
        unsized_types,
        applications,
        entry: entry?,
        panic: panic?,
        expr_count,
        core: core.expect("`core` is lowered after the program"),
        panic_info,
        panic_reason,
        files,
    })
}

/// How many items of each kind `Items` holds: where the program's end and
/// `core`'s start, or where `core`'s end.
#[derive(Clone, Copy, Default)]
struct Counts {
    structs: usize,
    enums: usize,
    fns: usize,
    consts: usize,
    statics: usize,
    comptimes: usize,
}

/// Fills in the `reachable` of every struct and enum: the modules where a
/// path written would name it and be usable (ADR 0050). Any path can be
/// written from the root module, through `crate`, and a segment is usable
/// from a module if it is `pub` or that module is within the one holding
/// it. So what is reachable from a module is what a walk of the namespaces
/// from the root finds through the names visible from there, `use`s
/// included. Every `use` is resolved by now; one that failed was reported.
/// Every module of the program also writes paths from `core`'s root,
/// `core_root`, and a module of `core` writes them from there only: `crate`
/// is `core`'s root there, and `core` cannot name the program (ADR 0052).
fn reachability(
    modules: &[ModuleDef],
    namespaces: &[HashMap<String, Entry>],
    uses: &[Use],
    core_root: ModuleId,
    structs: &mut [StructDef],
    enums: &mut [EnumDef],
) {
    for from in 0..modules.len() {
        let from = ModuleId(from as u32);
        let mut seen = vec![false; modules.len()];
        let mut pending = if from.0 >= core_root.0 {
            vec![core_root]
        } else {
            vec![core_root, ModuleId::ROOT]
        };
        for root in &pending {
            seen[root.0 as usize] = true;
        }
        while let Some(module) = pending.pop() {
            for entry in namespaces[module.0 as usize].values() {
                if entry.vis == Visibility::Private && !hir::is_within(modules, from, module) {
                    continue;
                }
                let def = match entry.binding {
                    Binding::Def(def) => def,
                    Binding::Use(id) => match uses[id].state {
                        UseState::Resolved(def) => def,
                        _ => continue,
                    },
                };
                // `from` goes in id order, so each list stays in order; a
                // second path to an item adds nothing.
                let reachable = match def {
                    Def::Module(id) => {
                        if !seen[id.0 as usize] {
                            seen[id.0 as usize] = true;
                            pending.push(id);
                        }
                        continue;
                    }
                    Def::Struct(id) => &mut structs[id.0 as usize].reachable,
                    Def::Enum(id) => &mut enums[id.0 as usize].reachable,
                    Def::Fn(_) | Def::Const(_) | Def::Static(_) => continue,
                };
                if reachable.last() != Some(&from) {
                    reachable.push(from);
                }
            }
        }
    }
}

/// What a name in a module's namespace is (ADR 0049).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Def {
    Fn(FnId),
    Struct(StructId),
    Enum(EnumId),
    Const(ConstId),
    Static(StaticId),
    Module(ModuleId),
}

impl Def {
    /// How diagnostics name what it is.
    fn what(self) -> &'static str {
        match self {
            Def::Fn(_) => "a function",
            Def::Struct(_) => "a struct",
            Def::Enum(_) => "an enum",
            Def::Const(_) => "a constant",
            Def::Static(_) => "a static",
            Def::Module(_) => "a module",
        }
    }
}

/// What a name in a module's namespace stands for: an item or a child
/// module, or what a `use` names, resolved when first needed (ADR 0049).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Binding {
    Def(Def),
    /// The index of the `use` in `Lowerer::uses`.
    Use(usize),
}

/// A name in a module's namespace: an item, a child module or a name a
/// `use` brings in.
#[derive(Clone, Copy, Debug)]
struct Entry {
    binding: Binding,
    vis: Visibility,
    /// The span of its name where it is declared.
    span: Span,
}

/// A name a `use` brings in (ADR 0049): one per path of the declaration.
struct Use {
    /// The path from the module of the `use`, the prefix before braces
    /// included.
    path: ast::Path,
    /// The path as written: between the braces, without the prefix.
    span: Span,
    /// The name it brings in: the one after `as`, or the path's last.
    name: ast::Ident,
    module: ModuleId,
    vis: Visibility,
    /// Declared in `core` (ADR 0052).
    in_core: bool,
    state: UseState,
}

/// How far a `use` is resolved; each is resolved once, when first needed
/// (ADR 0049).
#[derive(Clone, Copy)]
enum UseState {
    Unresolved,
    /// Being resolved: met again then, it is in a cycle.
    Resolving,
    Resolved(Def),
    /// Reported, or naming what was.
    Failed,
}

/// The items of every module, each with the module it is declared in, and
/// each module's namespace (ADR 0049), gathered before any item is lowered.
struct Items<'a> {
    modules: Vec<ModuleDef>,
    namespaces: Vec<HashMap<String, Entry>>,
    /// The index in `fns` of `core`'s first function, and its id: every
    /// body of the program comes before `core`'s (ADR 0052), those run at
    /// compile time included, so `core`'s functions are not numbered by
    /// their index.
    core_fns: usize,
    core_fn_base: u32,
    fns: Vec<(&'a ast::FnDecl, ModuleId)>,
    structs: Vec<(&'a ast::StructDecl, ModuleId)>,
    enums: Vec<(&'a ast::EnumDecl, ModuleId)>,
    consts: Vec<(&'a ast::ConstDecl, ModuleId)>,
    statics: Vec<(&'a ast::StaticDecl, ModuleId)>,
    comptimes: Vec<(&'a ast::ComptimeDecl, ModuleId)>,
    /// Each `mod`, with the module that declares it.
    mods: Vec<(&'a ast::ModDecl, ModuleId)>,
    /// Each `use` declaration, with its module, and each name they bring in.
    use_decls: Vec<(&'a ast::UseDecl, ModuleId)>,
    uses: Vec<Use>,
    /// Each `resolve::duplicate-item`, reported after the items are
    /// lowered, as they were with one namespace.
    duplicates: Vec<Diagnostic>,
}

impl<'a> Items<'a> {
    /// How many items of each kind are gathered so far.
    fn counts(&self) -> Counts {
        Counts {
            structs: self.structs.len(),
            enums: self.enums.len(),
            fns: self.fns.len(),
            consts: self.consts.len(),
            statics: self.statics.len(),
            comptimes: self.comptimes.len(),
        }
    }

    /// How many bodies the items from `from` to `to` have: one per
    /// function, constant, static with an initializer, `comptime` block
    /// and variant.
    fn bodies(&self, from: Counts, to: Counts) -> u32 {
        let statics = self.statics[from.statics..to.statics]
            .iter()
            .filter(|(decl, _)| !decl.external)
            .count();
        let variants: usize = self.enums[from.enums..to.enums]
            .iter()
            .map(|(decl, _)| decl.variants.len())
            .sum();
        let count = (to.fns - from.fns)
            + (to.consts - from.consts)
            + statics
            + (to.comptimes - from.comptimes)
            + variants;
        count as u32
    }

    /// Gathers `items`, declared in `module`, and those of the modules
    /// among them, depth first in the order written; `in_core` if they are
    /// `core`'s (ADR 0052).
    fn collect(
        &mut self,
        items: &'a [ast::Item],
        module: ModuleId,
        in_core: bool,
        diags: &mut Diagnostics,
    ) {
        for item in items {
            let (name, vis, def) = match item {
                ast::Item::Use(decl) => {
                    self.use_decl(decl, module, in_core);
                    continue;
                }
                ast::Item::Fn(decl) => {
                    self.fns.push((decl, module));
                    let index = self.fns.len() - 1;
                    let id = match index.checked_sub(self.core_fns) {
                        Some(i) => FnId(self.core_fn_base + i as u32),
                        None => FnId(index as u32),
                    };
                    (&decl.name, decl.vis, Def::Fn(id))
                }
                ast::Item::Struct(decl) => {
                    self.structs.push((decl, module));
                    let id = StructId(self.structs.len() as u32 - 1);
                    (&decl.name, decl.vis, Def::Struct(id))
                }
                ast::Item::Enum(decl) => {
                    self.enums.push((decl, module));
                    let id = EnumId(self.enums.len() as u32 - 1);
                    (&decl.name, decl.vis, Def::Enum(id))
                }
                ast::Item::Const(decl) => {
                    self.consts.push((decl, module));
                    let id = ConstId(self.consts.len() as u32 - 1);
                    (&decl.name, decl.vis, Def::Const(id))
                }
                ast::Item::Static(decl) => {
                    self.statics.push((decl, module));
                    let id = StaticId(self.statics.len() as u32 - 1);
                    (&decl.name, decl.vis, Def::Static(id))
                }
                ast::Item::Comptime(decl) => {
                    self.comptimes.push((decl, module));
                    continue;
                }
                ast::Item::Mod(decl) => {
                    let id = ModuleId(self.modules.len() as u32);
                    self.modules.push(ModuleDef {
                        name: decl.name.name.clone(),
                        path: item_path(&self.modules, module, &decl.name.name),
                        name_span: Some(decl.name.span),
                        parent: Some(module),
                        vis: visibility(decl.vis),
                    });
                    self.namespaces.push(HashMap::new());
                    self.mods.push((decl, module));
                    self.declare(
                        module,
                        &decl.name,
                        visibility(decl.vis),
                        Binding::Def(Def::Module(id)),
                        in_core,
                        diags,
                    );
                    if let Some(items) = &decl.items {
                        self.collect(items, id, in_core, diags);
                    }
                    continue;
                }
            };
            self.declare(
                module,
                name,
                visibility(vis),
                Binding::Def(def),
                in_core,
                diags,
            );
        }
    }

    /// Gathers the names the `use` declaration `decl`, of `module`, brings
    /// in; they are declared after every item (`declare_uses`).
    fn use_decl(&mut self, decl: &'a ast::UseDecl, module: ModuleId, in_core: bool) {
        self.use_decls.push((decl, module));
        for name in &decl.names {
            let path = match &decl.prefix {
                Some(prefix) => ast::Path {
                    segments: prefix
                        .segments
                        .iter()
                        .chain(&name.path.segments)
                        .cloned()
                        .collect(),
                    span: prefix.span.to(name.path.span),
                },
                None => name.path.clone(),
            };
            let last = &name.path.segments[name.path.segments.len() - 1];
            self.uses.push(Use {
                path,
                span: name.path.span,
                name: name.alias.as_ref().unwrap_or(last).clone(),
                module,
                vis: visibility(decl.vis),
                in_core,
                state: UseState::Unresolved,
            });
        }
    }

    /// Adds the name of each `use` to the namespace of its module, after
    /// every item, so that an item keeps its name against a `use` whichever
    /// is written first (ADR 0049).
    fn declare_uses(&mut self, diags: &mut Diagnostics) {
        for id in 0..self.uses.len() {
            let u = &self.uses[id];
            let (module, name, vis, in_core) = (u.module, u.name.clone(), u.vis, u.in_core);
            self.declare(module, &name, vis, Binding::Use(id), in_core, diags);
        }
    }

    /// Adds `name` to the namespace of `module`, unless it is taken: one
    /// name per module for items, modules and `use`s alike (ADR 0029,
    /// 0049). No item, module or `use` of the program takes a built-in
    /// name, a name starting with `__` (ADR 0020) or `core` (ADR 0052), in
    /// any module; no module of the root is named `llvm` (ADR 0051). The
    /// items of `core` (`in_core`) are not checked: they are the compiler's.
    fn declare(
        &mut self,
        module: ModuleId,
        ident: &ast::Ident,
        vis: Visibility,
        binding: Binding,
        in_core: bool,
        diags: &mut Diagnostics,
    ) {
        let name = ident.name.as_str();
        let is_use = matches!(binding, Binding::Use(_));
        if !in_core {
            let reserved = if let Some(help) = builtin_fn(name) {
                Some((
                    "a built-in function has this name".to_owned(),
                    help.to_owned(),
                ))
            } else if name.starts_with("__") {
                Some((
                    "names starting with `__` are reserved".to_owned(),
                    "the runtime support functions of `core` use them (ADR 0020, 0052)".to_owned(),
                ))
            } else if name == "core" {
                Some((
                    "the name of the module `core`".to_owned(),
                    "every module names the module `core`, shipped with the compiler, by this name, as in `core::memcpy`, so no item, module or `use` of the program can take it (ADR 0052)".to_owned(),
                ))
            } else if name == "llvm"
                && module == ModuleId::ROOT
                && matches!(binding, Binding::Def(Def::Module(_)))
            {
                Some((
                    "reserved for LLVM in the root module".to_owned(),
                    "the symbols of its items would start with `llvm.`, which LLVM reserves for its intrinsics (ADR 0051)".to_owned(),
                ))
            } else {
                None
            };
            if let Some((label, help)) = reserved {
                diags.push(
                    Diagnostic::error(
                        "resolve::reserved-name",
                        format!("`{name}` is a reserved name"),
                    )
                    .label(ident.span, label)
                    .help(help),
                );
            }
        }
        // The built-in types are in every module (ADR 0029, 0049): in M6,
        // `Option(u32)` is a call resolved there (ADR 0045).
        if is_builtin_type(name) {
            diags.push(
                Diagnostic::error(
                    "resolve::duplicate-type",
                    format!("`{name}` is a built-in type"),
                )
                .label(ident.span, "cannot be redefined"),
            );
        }
        let entry = Entry {
            binding,
            vis,
            span: ident.span,
        };
        let namespace = &mut self.namespaces[module.0 as usize];
        let Some(first) = namespace.get(name) else {
            namespace.insert(name.to_owned(), entry);
            return;
        };
        let help = if matches!(first.binding, Binding::Use(_)) || is_use {
            "a module's items, its modules and the names its `use` declarations bring in share one namespace (ADR 0049)"
        } else if matches!(first.binding, Binding::Def(Def::Module(_)))
            || matches!(binding, Binding::Def(Def::Module(_)))
        {
            "the items and modules of a module share one namespace (ADR 0049)"
        } else {
            "functions, structs, enums, constants and statics share one namespace (ADR 0029, 0030, 0035)"
        };
        // A repeated name keeps its first definition, but an item keeps it
        // against a `use`, even one written before it: the labels follow
        // the source.
        let (again, before) = if (ident.span.file, ident.span.lo) < (first.span.file, first.span.lo)
        {
            (first.span, ident.span)
        } else {
            (ident.span, first.span)
        };
        self.duplicates.push(
            Diagnostic::error(
                "resolve::duplicate-item",
                format!("`{name}` is defined more than once"),
            )
            .label(again, "redefined here")
            .label(before, "first defined here")
            .help(help),
        );
    }
}

fn visibility(vis: ast::Visibility) -> Visibility {
    match vis {
        ast::Visibility::Private => Visibility::Private,
        ast::Visibility::Pub => Visibility::Pub,
    }
}

/// The path of the item `name` of `module`, as diagnostics and dumps show
/// it: from the root module, and only its name in the root (ADR 0049).
fn item_path(modules: &[ModuleDef], module: ModuleId, name: &str) -> String {
    let prefix = &modules[module.0 as usize].path;
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}::{name}")
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

/// "1 type argument", or as many, or "no type arguments".
fn type_arguments(count: usize) -> String {
    match count {
        0 => "no type arguments".to_owned(),
        1 => "1 type argument".to_owned(),
        n => format!("{n} type arguments"),
    }
}

/// Whether `name` is a built-in type: `bool`, an integer type, `Option`
/// or `Result` (ADR 0029, 0045).
fn is_builtin_type(name: &str) -> bool {
    name == "bool" || IntTy::from_name(name).is_some() || BuiltinEnum::from_name(name).is_some()
}

/// What the built-in name `name` is, as diagnostics say it, if it is one.
fn builtin_what(name: &str) -> Option<&'static str> {
    if is_builtin_type(name) {
        Some("a built-in type")
    } else if builtin_fn(name).is_some() {
        Some("a built-in function")
    } else {
        None
    }
}

/// The built-in function named `name`, as a help saying how it is called
/// and which ADRs define it. A call by that name is always the built-in,
/// so no item can have it.
fn builtin_fn(name: &str) -> Option<&'static str> {
    match name {
        "assert" => Some("`assert(cond)` and `assert(cond, msg)` are built in (ADR 0029, 0031)"),
        "len" => Some("`len(s)` is built in (ADR 0031)"),
        "slice" => Some("`slice(p, n)` is built in (ADR 0031)"),
        _ => None,
    }
}

/// A path as written, `uart::Mode::Run`.
fn written(path: &ast::Path) -> String {
    written_segments(&path.segments)
}

/// Segments of a path as written, separated by `::`.
fn written_segments(segments: &[ast::Ident]) -> String {
    let names: Vec<&str> = segments.iter().map(|s| s.name.as_str()).collect();
    names.join("::")
}

/// The name a path is, if it is one name other than `crate` and `super`:
/// one that may be a local, a built-in or an item of the current module.
fn plain_name(path: &ast::Path) -> Option<&ast::Ident> {
    path.as_single()
        .filter(|name| !matches!(name.name.as_str(), "crate" | "super"))
}

/// What a path names (ADR 0049).
enum Named {
    Item(Def),
    Variant(EnumId, u32),
    /// The last segment names nothing in this module; the caller reports
    /// it, as the error of its position.
    Missing(ModuleId),
    /// Reported.
    Failed,
}

/// Where the segments of a path read so far lead.
#[derive(Clone, Copy)]
enum Prefix {
    Module(ModuleId),
    Enum(EnumId),
}

/// What a name is in a module's namespace, a `use` resolved to what it
/// names.
enum Lookup {
    Found(Def),
    Missing,
    /// A `use` that failed, which was reported.
    Failed,
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

/// A function or static with a symbol, as `check_symbols` compares them.
struct Symbol<'a> {
    symbol: &'a str,
    path: &'a str,
    span: Span,
    external: bool,
    def: Def,
    /// An item of `core` (ADR 0052).
    in_core: bool,
}

/// Every symbol of the program and of `core` is unique (ADR 0051, 0052):
/// two functions or statics with one, in any modules, are
/// `resolve::duplicate-symbol`, each later one reported against the first.
/// An item that did not get its name in its module's namespace was reported
/// as `resolve::duplicate-item`, and an item of the program named with `__`
/// as `resolve::reserved-name` (ADR 0020): both are left out, the latter
/// since `core` gives such names as symbols. `core`'s files come after the
/// program's, so an item of the program taking a symbol of `core` is
/// reported at the program's item, with a help naming what to call instead
/// (ADR 0052).
fn check_symbols(
    fns: &[Fn],
    statics: &[StaticDef],
    namespaces: &[HashMap<String, Entry>],
    uses: &[Use],
    core_root: ModuleId,
    diags: &mut Diagnostics,
) {
    let in_core = |module: ModuleId| module.0 >= core_root.0;
    // Whether the item holds its name in its module, and is not an item of
    // the program with a reserved name.
    let checked = |module: ModuleId, name: &str, def: Def| {
        (in_core(module) || !name.starts_with("__"))
            && namespaces[module.0 as usize]
                .get(name)
                .is_some_and(|entry| entry.binding == Binding::Def(def))
    };
    let mut items: Vec<Symbol> = Vec::new();
    for (i, f) in fns.iter().enumerate() {
        let Some(symbol) = &f.symbol else { continue };
        let def = Def::Fn(FnId(i as u32));
        if checked(f.module, &f.name, def) {
            let item = Symbol {
                symbol,
                path: &f.path,
                span: f.name_span,
                external: f.body.is_none(),
                def,
                in_core: in_core(f.module),
            };
            items.push(item);
        }
    }
    for (i, s) in statics.iter().enumerate() {
        let def = Def::Static(StaticId(i as u32));
        if checked(s.module, &s.name, def) {
            let item = Symbol {
                symbol: &s.symbol,
                path: &s.path,
                span: s.name_span,
                external: s.init.is_none(),
                def,
                in_core: in_core(s.module),
            };
            items.push(item);
        }
    }
    // In source order, so that the first is the one declared first.
    items.sort_by_key(|item| (item.span.file, item.span.lo));

    let mut firsts: HashMap<&str, &Symbol> = HashMap::new();
    for again in &items {
        let Some(&first) = firsts.get(again.symbol) else {
            firsts.insert(again.symbol, again);
            continue;
        };
        let symbol = again.symbol;
        let diag = Diagnostic::error(
            "resolve::duplicate-symbol",
            format!("the symbol `{symbol}` is given to more than one item"),
        );
        let diag = if again.in_core || first.in_core {
            let (item, core_item) = if again.in_core {
                (first, again)
            } else {
                (again, first)
            };
            let call = core_path(namespaces, uses, core_root, core_item);
            diag.label(item.span, format!("given here, to `{}`", item.path))
                .label(
                    core_item.span,
                    format!("and here, in `core`, to `{}`", core_item.path),
                )
                .help(format!(
                    "`{call}` has this symbol (ADR 0052): call it rather than declaring another, or give this item another name (ADR 0051)"
                ))
        } else {
            let help = if again.external && first.external {
                "an `extern` item has its own name as its symbol in any module: declare it in one module, and bring it into the others with `use` or name it by its path (ADR 0049, 0051)"
            } else {
                "an item of the root module, an `#[export]` item and an `extern` item each have their own name as their symbol: rename one of the two (ADR 0051)"
            };
            diag.label(again.span, format!("given again here, to `{}`", again.path))
                .label(first.span, format!("first given here, to `{}`", first.path))
                .help(help)
        };
        diags.push(diag);
    }
}

/// How a program names `item`, an item of `core`: by the `pub` name of
/// `core`'s root that stands for it, such as `core::memcpy`, which a `pub
/// use` offers, or else by its path (ADR 0052).
fn core_path(
    namespaces: &[HashMap<String, Entry>],
    uses: &[Use],
    core_root: ModuleId,
    item: &Symbol,
) -> String {
    namespaces[core_root.0 as usize]
        .iter()
        .filter(|(_, entry)| entry.vis == Visibility::Pub)
        .filter(|(_, entry)| match entry.binding {
            Binding::Def(def) => def == item.def,
            Binding::Use(id) => {
                matches!(uses[id].state, UseState::Resolved(def) if def == item.def)
            }
        })
        .map(|(name, _)| format!("core::{name}"))
        .min()
        .unwrap_or_else(|| item.path.to_owned())
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

/// A variable a pattern binds, as lowering collects them (ADR 0040, 0041).
#[derive(Clone)]
struct Bound {
    name: String,
    local: LocalId,
    mutable: bool,
    /// Its name, for `resolve::duplicate-binding`.
    name_span: Span,
    /// Its whole `let`, for `resolve::inconsistent-binding`.
    let_span: Span,
    /// Whether an or-pattern reported it as `resolve::inconsistent-binding`:
    /// the or-patterns around it report nothing more about it.
    reported: bool,
}

/// An or-pattern around a pattern that is in one of its alternatives
/// after the first (ADR 0041).
#[derive(Clone, Copy)]
struct Later<'b> {
    /// How many variables the arm's pattern binds before the or-pattern.
    before: usize,
    /// The variables its first alternative binds. A later alternative's
    /// `let` of one of these names is that variable. The first alternative
    /// was compared with the variables bound before the or-pattern, so a
    /// `let` of such a name repeating one of those was reported there as
    /// `resolve::duplicate-binding`, and is not reported again.
    first: &'b [Bound],
}

struct Lowerer<'a> {
    /// The module tree (ADR 0049).
    modules: Vec<ModuleDef>,
    /// Each module's namespace, by module id.
    namespaces: Vec<HashMap<String, Entry>>,
    /// The module of the item being lowered, where its names are looked up
    /// and from which visibility is judged (ADR 0049, 0050).
    module: ModuleId,
    /// The root of `core` (ADR 0052); the modules from it on are `core`'s.
    core_root: ModuleId,
    /// Every function's declaration, the program's then `core`'s, with its
    /// module; `fn_decl` finds one by id.
    fn_decls: &'a [(&'a ast::FnDecl, ModuleId)],
    /// The index in `fn_decls` of `core`'s first function, and its id.
    core_fns: usize,
    core_fn_base: u32,
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
    struct_tys: Vec<StructTy>,
    struct_decls: &'a [(&'a ast::StructDecl, ModuleId)],
    enum_tys: Vec<EnumTy>,
    enum_decls: &'a [(&'a ast::EnumDecl, ModuleId)],
    const_decls: &'a [(&'a ast::ConstDecl, ModuleId)],
    static_decls: &'a [(&'a ast::StaticDecl, ModuleId)],
    /// Each name a `use` brings in (ADR 0049).
    uses: Vec<Use>,
    /// The `use`s being resolved, each through the next.
    use_stack: Vec<usize>,
    /// Each `!` written where memory would have its type (ADR 0034).
    never_in_memory: Vec<NeverInMemory>,
    /// Each `[T]` written where a size is needed (ADR 0031).
    unsized_types: Vec<UnsizedType>,
    /// Each `Option(T)` and `Result(T, E)` written (ADR 0045).
    applications: Vec<(Span, Ty)>,
    next_expr: u32,
    /// The next `.V` pattern's id (ADR 0044).
    next_pattern: u32,
    diags: &'a mut Diagnostics,
    /// Locals of the function being lowered.
    locals: Vec<Local>,
    /// Innermost scope last. A `let` shadows earlier names, as in Rust.
    scopes: Vec<HashMap<String, LocalId>>,
    /// Enclosing loops, innermost last: the expression id and whether it is
    /// a `while` (whose `break` carries no value).
    loops: Vec<(ExprId, bool)>,
}

/// What a call's path names: a function, or a variant built with its
/// payload's values (ADR 0039).
enum Callee {
    Fn(FnId),
    Variant(EnumId, u32),
}

/// Name resolution: paths (ADR 0049) and visibility (ADR 0050).
impl Lowerer<'_> {
    /// How diagnostics name a module: "module `uart::regs`", or "the root
    /// module".
    fn module_name(&self, module: ModuleId) -> String {
        self.modules[module.0 as usize].describe()
    }

    /// The path of the item `name` of `module`, as diagnostics show it.
    fn path_of(&self, module: ModuleId, name: &str) -> String {
        item_path(&self.modules, module, name)
    }

    /// Whether `inner` is `outer` or a module inside it, where the private
    /// items of `outer` are visible (ADR 0050).
    fn is_within(&self, inner: ModuleId, outer: ModuleId) -> bool {
        hir::is_within(&self.modules, inner, outer)
    }

    /// Whether `module` is a module of `core` (ADR 0052).
    fn in_core(&self, module: ModuleId) -> bool {
        module.0 >= self.core_root.0
    }

    /// The root of the tree `module` is in, which `crate` names there: the
    /// root module, or `core`'s root in `core` (ADR 0049, 0052).
    fn crate_root(&self, module: ModuleId) -> ModuleId {
        if self.in_core(module) {
            self.core_root
        } else {
            ModuleId::ROOT
        }
    }

    /// The declaration of the function `id`, with its module.
    fn fn_decl_of(&self, id: FnId) -> (&ast::FnDecl, ModuleId) {
        let index = match id.0.checked_sub(self.core_fn_base) {
            Some(i) => self.core_fns + i as usize,
            None => id.0 as usize,
        };
        self.fn_decls[index]
    }

    /// The segment `name` in the namespace of `module`. Naming an item that
    /// is not visible from the current module is reported (ADR 0050); it
    /// is still what the name means, so nothing more is reported about it.
    /// A name a `use` brings in is what the `use` names (ADR 0049).
    fn lookup(&mut self, module: ModuleId, name: &ast::Ident) -> Lookup {
        let Some(&entry) = self.namespaces[module.0 as usize].get(&name.name) else {
            return Lookup::Missing;
        };
        if entry.vis == Visibility::Private && !self.is_within(self.module, module) {
            // The program cannot change `core`, only use what it offers.
            let remedy = if self.in_core(module) {
                "the program uses what `core` declares `pub` (ADR 0050, 0052)"
            } else {
                "declare it `pub` to use it elsewhere (ADR 0050)"
            };
            self.diags.push(
                Diagnostic::error(
                    "resolve::private-item",
                    format!("`{}` is private", self.path_of(module, &name.name)),
                )
                .label(name.span, "not visible here")
                .label(entry.span, "declared here without `pub`")
                .help(format!(
                    "a private item of {} is visible only there and in the modules inside it; {remedy}",
                    self.module_name(module)
                )),
            );
        }
        match self.binding_def(entry.binding) {
            Some(def) => Lookup::Found(def),
            None => Lookup::Failed,
        }
    }

    /// What `binding` stands for; `None` for a `use` that failed, which was
    /// reported.
    fn binding_def(&mut self, binding: Binding) -> Option<Def> {
        match binding {
            Binding::Def(def) => Some(def),
            Binding::Use(id) => self.resolve_use(id),
        }
    }

    /// What the `use` `id` names, resolved the first time it is needed, as
    /// any path is from the module of the `use` (ADR 0049). `None` if that
    /// failed, which is reported: a path in error, a variant, or a cycle
    /// of `use`s.
    fn resolve_use(&mut self, id: usize) -> Option<Def> {
        match self.uses[id].state {
            UseState::Resolved(def) => return Some(def),
            UseState::Failed => return None,
            UseState::Resolving => {
                self.use_cycle(id);
                return None;
            }
            UseState::Unresolved => {}
        }
        self.uses[id].state = UseState::Resolving;
        self.use_stack.push(id);
        let saved = std::mem::replace(&mut self.module, self.uses[id].module);
        let path = self.uses[id].path.clone();
        let state = if let Some(single) = plain_name(&path)
            && let Some(what) = builtin_what(&single.name)
        {
            // Without `as`, the name is the built-in's, which `declare`
            // reported.
            if self.uses[id].name.name != single.name {
                self.use_builtin(single, what);
            }
            UseState::Failed
        } else {
            self.use_state(id, &path)
        };
        self.module = saved;
        self.use_stack.pop();
        self.uses[id].state = state;
        match state {
            UseState::Resolved(def) => Some(def),
            _ => None,
        }
    }

    /// `resolve::unknown-path` for `name`, a built-in name a `use` names,
    /// which is `what`: not an item, and in every module already
    /// (ADR 0049).
    fn use_builtin(&mut self, name: &ast::Ident, what: &str) {
        self.diags.push(
            Diagnostic::error(
                "resolve::unknown-path",
                format!(
                    "`{}` is {what}, not an item, and `use` brings in items and modules",
                    name.name
                ),
            )
            .label(name.span, what)
            .help(format!(
                "the built-in names are in every module already: write `{}` itself (ADR 0049)",
                name.name
            )),
        );
    }

    /// What the path of the `use` `id` names, from its module, as the state
    /// it leaves the `use` in; errors are reported.
    fn use_state(&mut self, id: usize, path: &ast::Path) -> UseState {
        match self.lookup_path(path) {
            // In a cycle, which was reported.
            _ if matches!(self.uses[id].state, UseState::Failed) => UseState::Failed,
            Named::Item(def) => UseState::Resolved(def),
            Named::Variant(enum_id, _) => {
                self.use_variant(id, enum_id);
                UseState::Failed
            }
            Named::Missing(module) => {
                let diag = Diagnostic::error(
                    "resolve::unknown-path",
                    format!("cannot find `{}`", written(path)),
                );
                let diag = self.missing(diag, path, module, "not found");
                self.diags.push(diag);
                UseState::Failed
            }
            Named::Failed => UseState::Failed,
        }
    }

    /// `resolve::use-variant` for the `use` `id`, which names a variant of
    /// the enum `enum_id` (ADR 0049).
    fn use_variant(&mut self, id: usize, enum_id: EnumId) {
        let u = &self.uses[id];
        let path = &u.path;
        let (variant, prefix) = path
            .segments
            .split_last()
            .expect("a variant follows its enum");
        let enum_name = self.enum_tys[enum_id.0 as usize].name;
        let written_enum = written_segments(prefix);
        let as_used = &prefix[prefix.len() - 1].name;
        self.diags.push(
            Diagnostic::error(
                "resolve::use-variant",
                format!("`{}` is a variant, which `use` cannot bring in", written(path)),
            )
            .label(u.span, format!("a variant of `{enum_name}`"))
            .help(format!(
                "a variant is written with its enum, or as `.{}` where the enum is the expected type: bring in the enum with `use {written_enum};` and write `{as_used}::{}` (ADR 0044, 0049)",
                variant.name, variant.name
            )),
        );
    }

    /// `resolve::use-cycle` for the `use`s from `id` to the last one being
    /// resolved, which names `id` again, each through the next (ADR 0049).
    /// Each of them fails, so the cycle is reported once, at the first of
    /// them in file order, then offset; the labels name the others.
    fn use_cycle(&mut self, id: usize) {
        let at = self
            .use_stack
            .iter()
            .position(|&u| u == id)
            .expect("a `use` being resolved is on the stack");
        let mut cycle = self.use_stack[at..].to_vec();
        for &u in &cycle {
            self.uses[u].state = UseState::Failed;
        }
        let first = (0..cycle.len())
            .min_by_key(|&i| {
                let span = self.uses[cycle[i]].span;
                (span.file, span.lo)
            })
            .expect("a cycle has a `use`");
        cycle.rotate_left(first);
        // Each by the path of the name it brings in.
        let names: Vec<String> = cycle
            .iter()
            .map(|&u| {
                let u = &self.uses[u];
                format!("`{}`", item_path(&self.modules, u.module, &u.name.name))
            })
            .collect();
        let mut diag = if let [name] = names.as_slice() {
            let head = &self.uses[cycle[0]];
            let saved = std::mem::replace(&mut self.module, head.module);
            let help = match head.path.as_single() {
                Some(single) => self.parent_help(0, &single.name),
                None => None,
            };
            self.module = saved;
            let head = &self.uses[cycle[0]];
            Diagnostic::error(
                "resolve::use-cycle",
                format!("the `use` bringing in {name} can only be resolved through itself"),
            )
            .label(head.span, format!("brings in {name} through itself"))
            .help(help.unwrap_or_else(|| {
                format!(
                    "a `use` path is looked up as any path is, from the module of the `use`, and this one leads to `{}`, the name this `use` brings in: write the path to where the item is declared (ADR 0049)",
                    head.name.name
                )
            }))
        } else {
            let (last, others) = names.split_last().expect("a cycle has a `use`");
            Diagnostic::error(
                "resolve::use-cycle",
                format!(
                    "the `use` declarations bringing in {} and {last} can only be resolved through each other",
                    others.join(", ")
                ),
            )
            .help("each of these `use` declarations finds what it names through the next, and none where it is declared: write one of their paths to the item itself (ADR 0049)")
        };
        if names.len() > 1 {
            for (i, &u) in cycle.iter().enumerate() {
                let next = &names[(i + 1) % names.len()];
                diag = diag.label(
                    self.uses[u].span,
                    format!("brings in {} through {next}", names[i]),
                );
            }
        }
        self.diags.push(diag);
    }

    /// Every `use` declaration: its attributes, none of which applies, and
    /// each name it brings in, resolved now if no item needed it yet, so
    /// that every error is reported; a `pub use` offers only a `pub` item
    /// (ADR 0049, 0050).
    fn use_decls(&mut self, decls: &[(&ast::UseDecl, ModuleId)]) {
        for &(decl, _) in decls {
            self.no_attrs(&decl.attrs, "a `use`");
        }
        for id in 0..self.uses.len() {
            let Some(def) = self.resolve_use(id) else {
                continue;
            };
            let u = &self.uses[id];
            if u.vis == Visibility::Private {
                continue;
            }
            let (span, module) = (u.span, u.module);
            let (vis, declared_in, name_span, path) = self.declared(def);
            // Where the item is not visible, naming it was reported as
            // `resolve::private-item`.
            if vis == Visibility::Private && self.is_within(module, declared_in) {
                self.diags.push(
                    Diagnostic::error(
                        "resolve::private-reexport",
                        format!("`{path}` is private, so `pub use` cannot offer it"),
                    )
                    .label(span, "offered here with `pub use`")
                    .label(name_span, "declared here without `pub`")
                    .help("a `pub use` makes the name it brings in `pub`, which cannot widen what the item declared: declare the item `pub`, or the `use` without `pub` (ADR 0050)"),
                );
            }
        }
    }

    /// How the item or module `def` is declared: its visibility, its
    /// module, the span of its name and its path.
    fn declared(&self, def: Def) -> (Visibility, ModuleId, Span, String) {
        let item = |vis: ast::Visibility, module: ModuleId, name: &ast::Ident| {
            (
                visibility(vis),
                module,
                name.span,
                item_path(&self.modules, module, &name.name),
            )
        };
        match def {
            Def::Fn(id) => {
                let (decl, module) = self.fn_decl_of(id);
                item(decl.vis, module, &decl.name)
            }
            Def::Struct(id) => {
                let (decl, module) = self.struct_decls[id.0 as usize];
                item(decl.vis, module, &decl.name)
            }
            Def::Enum(id) => {
                let (decl, module) = self.enum_decls[id.0 as usize];
                item(decl.vis, module, &decl.name)
            }
            Def::Const(id) => {
                let (decl, module) = self.const_decls[id.0 as usize];
                item(decl.vis, module, &decl.name)
            }
            Def::Static(id) => {
                let (decl, module) = self.static_decls[id.0 as usize];
                item(decl.vis, module, &decl.name)
            }
            Def::Module(id) => {
                let def = &self.modules[id.0 as usize];
                (
                    def.vis,
                    def.parent.expect("no name stands for the root module"),
                    def.name_span
                        .expect("a module other than the root has a name"),
                    def.path.clone(),
                )
            }
        }
    }

    /// The module that `super`, written at `segment`, names after a path
    /// that led to `module`: its parent. The root module has none, which is
    /// reported.
    fn parent(&mut self, module: ModuleId, segment: &ast::Ident) -> Option<ModuleId> {
        let parent = self.modules[module.0 as usize].parent;
        if parent.is_none() {
            self.diags.push(
                Diagnostic::error("resolve::super-at-root", "the root module has no parent")
                    .label(segment.span, "`super` of the root module")
                    .help("`super` names the module around the current one; the root module's items are named directly, or from anywhere through `crate` (ADR 0049)"),
            );
        }
        parent
    }

    /// What `path` names, from the current module (ADR 0049): each segment
    /// before the last names a module or an enum, and is looked up in the
    /// module named before it, the first in the current one; after an enum
    /// comes a variant, which ends the path. Errors of a segment before the
    /// last are reported; a last segment that names nothing is left to the
    /// caller, as the error of its position. Locals and the built-in names
    /// of a single name are the caller's to try first.
    fn lookup_path(&mut self, path: &ast::Path) -> Named {
        let (last, init) = path
            .segments
            .split_last()
            .expect("a path has at least one segment");
        let mut at = Prefix::Module(self.module);
        for (i, segment) in init.iter().enumerate() {
            let module = match at {
                Prefix::Module(module) => module,
                // `E::V::x`: a variant ends a path.
                Prefix::Enum(id) => {
                    if self.variant_index(id, segment).is_some() {
                        self.not_an_enum(&path.segments[..=i], "a variant");
                    }
                    return Named::Failed;
                }
            };
            at = match segment.name.as_str() {
                "crate" => Prefix::Module(self.crate_root(module)),
                // `core` names `core` in every module, `core`'s included;
                // no item can take the name (ADR 0052).
                "core" if i == 0 => Prefix::Module(self.core_root),
                "super" => match self.parent(module, segment) {
                    Some(parent) => Prefix::Module(parent),
                    None => return Named::Failed,
                },
                name if i == 0
                    && let Some(builtin) = BuiltinEnum::from_name(name) =>
                {
                    // Not items: their variants are named by the expected
                    // type (ADR 0045).
                    self.diags.push(
                        Diagnostic::error(
                            "resolve::not-an-enum",
                            format!("`{name}` is not an enum"),
                        )
                        .label(
                            segment.span,
                            format!(
                                "an enum only with its type arguments, as in `{}`",
                                builtin.written()
                            ),
                        )
                        .help(builtin_variant_help(builtin.name(), &path.segments[1].name)),
                    );
                    return Named::Failed;
                }
                name if i == 0 && (is_builtin_type(name) || builtin_fn(name).is_some()) => {
                    let what = if is_builtin_type(name) {
                        "a built-in type"
                    } else {
                        "a built-in function"
                    };
                    self.not_an_enum(&path.segments[..=i], what);
                    return Named::Failed;
                }
                _ => match self.lookup(module, segment) {
                    Lookup::Found(Def::Module(module)) => Prefix::Module(module),
                    Lookup::Found(Def::Enum(id)) => Prefix::Enum(id),
                    Lookup::Found(def) => {
                        self.not_an_enum(&path.segments[..=i], def.what());
                        return Named::Failed;
                    }
                    Lookup::Failed => return Named::Failed,
                    Lookup::Missing => {
                        let diag = Diagnostic::error(
                            "resolve::unknown-path",
                            format!("cannot find `{}`", written_segments(&path.segments[..=i])),
                        )
                        .label(
                            segment.span,
                            format!("not found in {}", self.module_name(module)),
                        );
                        let diag = match self.parent_help(i, &segment.name) {
                            Some(help) => diag.help(help),
                            None => diag,
                        };
                        self.diags.push(diag);
                        return Named::Failed;
                    }
                },
            };
        }
        match at {
            Prefix::Enum(id) => match self.variant_index(id, last) {
                Some(index) => Named::Variant(id, index),
                None => Named::Failed,
            },
            Prefix::Module(module) => match last.name.as_str() {
                "crate" => Named::Item(Def::Module(self.crate_root(module))),
                "core" if init.is_empty() => Named::Item(Def::Module(self.core_root)),
                "super" => match self.parent(module, last) {
                    Some(parent) => Named::Item(Def::Module(parent)),
                    None => Named::Failed,
                },
                _ => match self.lookup(module, last) {
                    Lookup::Found(def) => Named::Item(def),
                    Lookup::Missing => Named::Missing(module),
                    Lookup::Failed => Named::Failed,
                },
            },
        }
    }

    /// `resolve::not-an-enum` for the last of `segments`, followed by `::`
    /// in a path, which is `what` (ADR 0035, 0049).
    fn not_an_enum(&mut self, segments: &[ast::Ident], what: &str) {
        let segment = &segments[segments.len() - 1];
        self.diags.push(
            Diagnostic::error(
                "resolve::not-an-enum",
                format!(
                    "`{}` is neither a module nor an enum",
                    written_segments(segments)
                ),
            )
            .label(segment.span, what)
            .help("`::` follows a module, to name one of its items, or an enum, to name one of its variants, as in `Mode::Run` (ADR 0035, 0049)"),
        );
    }

    /// A help for `name`, the segment `index` of a path, not found in the
    /// current module when a module around it has the name: a module does
    /// not see the names of its parent (ADR 0049).
    fn parent_help(&self, index: usize, name: &str) -> Option<String> {
        if index > 0 {
            return None;
        }
        let mut supers = String::new();
        let mut at = self.modules[self.module.0 as usize].parent;
        while let Some(module) = at {
            supers.push_str("super::");
            if self.namespaces[module.0 as usize].contains_key(name) {
                return Some(format!(
                    "a module does not see the names of the modules around it: the `{name}` of {} is written `{supers}{name}` here (ADR 0049)",
                    self.module_name(module)
                ));
            }
            at = self.modules[module.0 as usize].parent;
        }
        None
    }

    /// `diag`, for the last segment of `path` naming nothing in `module`,
    /// labeled at that segment. A name of the root module written alone
    /// keeps `label`, as one-file programs had it; any other names the
    /// module it was looked up in (ADR 0049).
    fn missing(
        &self,
        diag: Diagnostic,
        path: &ast::Path,
        module: ModuleId,
        label: &str,
    ) -> Diagnostic {
        let last = &path.segments[path.segments.len() - 1];
        let single = path.segments.len() == 1;
        if single && module == ModuleId::ROOT {
            return diag.label(last.span, label);
        }
        let diag = diag.label(
            last.span,
            format!("not found in {}", self.module_name(module)),
        );
        match self.parent_help(path.segments.len() - 1, &last.name) {
            Some(help) => diag.help(help),
            None => diag,
        }
    }

    /// `resolve::misused-module` for `path`, which names a module where
    /// `expected` is (ADR 0049).
    fn misused_module(&mut self, path: &ast::Path, expected: &str) {
        let written = written(path);
        self.diags.push(
            Diagnostic::error(
                "resolve::misused-module",
                format!("`{written}` is a module, not {expected}"),
            )
            .label(path.span, "a module")
            .help(format!(
                "a module holds items, each named by a path through it, as in `{written}::name` (ADR 0049)"
            )),
        );
    }

    /// The index of the variant `variant` of the enum `id`; one it does not
    /// have is reported (ADR 0035).
    fn variant_index(&mut self, id: EnumId, variant: &ast::Ident) -> Option<u32> {
        let decl = self.enum_decls[id.0 as usize].0;
        // A variant declared twice was reported; its first counts.
        if let Some(index) = decl
            .variants
            .iter()
            .position(|v| v.name.name == variant.name)
        {
            return Some(index as u32);
        }
        // A variant declared twice is listed once.
        let mut names: Vec<String> = Vec::new();
        for v in &decl.variants {
            let name = format!("`{}`", v.name.name);
            if !names.contains(&name) {
                names.push(name);
            }
        }
        let enum_name = self.enum_tys[id.0 as usize].name;
        let mut diag = Diagnostic::error(
            "resolve::unknown-variant",
            format!("`{enum_name}` has no variant `{}`", variant.name),
        )
        .label(variant.span, "unknown variant")
        .label(decl.name.span, format!("`{enum_name}` is declared here"));
        if !names.is_empty() {
            diag = diag.help(format!("its variants are {}", names.join(", ")));
        }
        self.diags.push(diag);
        None
    }
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
    fn no_attrs(&mut self, attrs: &[ast::Attr], what: &str) {
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
    fn static_decl(
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
    fn const_decl(&mut self, decl: &ast::ConstDecl, id: ConstId, init: FnId) -> (ConstDef, Fn) {
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
    fn enum_decl(
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
    fn mod_decl(&mut self, decl: &ast::ModDecl) {
        self.no_attrs(&decl.attrs, "a module");
    }

    /// A `comptime { ... }` item, as a body returning `()`.
    fn comptime_decl(&mut self, decl: &ast::ComptimeDecl) -> Fn {
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

    /// A `usize` needed at compile time (ADR 0029): known for a literal, as
    /// most are, or computed by a body of its own.
    fn array_len(&mut self, expr: &ast::Expr) -> ArrayLen {
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
            name: "length".to_owned(),
            path: "length".to_owned(),
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

    /// `Name { field: value, ... }`: every field exactly once, in any order.
    fn struct_lit(
        &mut self,
        expr: &ast::Expr,
        path: &ast::Path,
        inits: &[ast::FieldInit],
    ) -> ExprKind {
        let id = match self.lookup_path(path) {
            Named::Item(Def::Struct(id)) => Some(id),
            Named::Item(Def::Module(_)) => {
                self.misused_module(path, "a struct");
                None
            }
            Named::Failed => None,
            named => {
                let diag = Diagnostic::error(
                    "resolve::unknown-type",
                    format!("unknown struct `{}`", written(path)),
                );
                let diag = match named {
                    Named::Missing(module) => self.missing(diag, path, module, "not a struct"),
                    Named::Item(def) => {
                        diag.label(path.span, format!("{}, not a struct", def.what()))
                    }
                    _ => diag.label(path.span, "a variant, not a struct"),
                };
                self.diags.push(diag);
                None
            }
        };
        let fields: Vec<(Option<u32>, Expr)> = inits
            .iter()
            .map(|init| {
                let decl = id.map(|id| self.struct_decls[id.0 as usize].0);
                let index = decl.and_then(|decl| {
                    decl.fields
                        .iter()
                        .position(|f| f.name.name == init.name.name)
                });
                (index.map(|i| i as u32), self.expr(&init.value))
            })
            .collect();
        let Some(id) = id else {
            return ExprKind::Unit;
        };
        let (decl, module) = self.struct_decls[id.0 as usize];
        let name = self.struct_tys[id.0 as usize].name;
        let mut given: Vec<Option<Span>> = vec![None; decl.fields.len()];
        for (init, (index, _)) in inits.iter().zip(&fields) {
            match index {
                None => self.diags.push(
                    Diagnostic::error(
                        "resolve::unknown-field",
                        format!("`{name}` has no field `{}`", init.name.name),
                    )
                    .label(init.name.span, "unknown field")
                    .label(decl.name.span, format!("`{name}` is declared here")),
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
        // A literal gives every field (ADR 0021), so outside the visibility
        // of a private field it cannot be written, whether it gives that
        // field or not (ADR 0050); what it misses is not reported then.
        // A private struct is named from outside its module only by a path
        // already reported as private (ADR 0050); its fields are not
        // reported again.
        if !self.is_within(self.module, module) && decl.vis != ast::Visibility::Private {
            // A field declared twice was reported; only its first counts.
            let private: Vec<(&ast::FieldDecl, Option<Span>)> = decl
                .fields
                .iter()
                .zip(&given)
                .enumerate()
                .filter(|(i, (field, _))| {
                    field.vis == ast::Visibility::Private
                        && decl.fields[..*i]
                            .iter()
                            .all(|earlier| earlier.name.name != field.name.name)
                })
                .map(|(_, (field, &given))| (field, given))
                .collect();
            if !private.is_empty() {
                self.private_fields(path, name, module, &private);
                return ExprKind::Unit;
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
                    format!("missing fields in `{name}`: {list}"),
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

    /// `resolve::private-field` for a literal, written at `path`, of the
    /// struct `name` of `module`, outside the visibility of its fields
    /// `private`, each with where the literal gives it, if it does
    /// (ADR 0050).
    fn private_fields(
        &mut self,
        path: &ast::Path,
        name: &str,
        module: ModuleId,
        private: &[(&ast::FieldDecl, Option<Span>)],
    ) {
        let names: Vec<String> = private
            .iter()
            .map(|(field, _)| format!("`{}`", field.name.name))
            .collect();
        let message = match names.split_last() {
            Some((last, [])) => format!("the field {last} of `{name}` is private"),
            Some((last, others)) => format!(
                "the fields {} and {last} of `{name}` are private",
                others.join(", ")
            ),
            None => unreachable!("a struct with a private field"),
        };
        let mut diag = Diagnostic::error("resolve::private-field", message)
            .label(path.span, "cannot be built here");
        for (field, given) in private {
            if let Some(given) = given {
                diag = diag.label(*given, "not visible here");
            }
            diag = diag.label(field.name.span, "declared here without `pub`");
        }
        self.diags.push(diag.help(format!(
            "a struct literal gives every field (ADR 0021), and a private field is visible only in {} and the modules inside it: build the struct with a function of that module, or declare the field `pub` (ADR 0050)",
            self.module_name(module)
        )));
    }

    fn fn_decl(&mut self, decl: &ast::FnDecl) -> Fn {
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

    /// A type that memory has: `!` there is `{error}`, recorded for the
    /// type checker (ADR 0034).
    fn memory_ty(&mut self, ty: &ast::Ty, position: NeverPosition) -> Ty {
        if let ast::TyKind::Never = ty.kind {
            self.never_in_memory.push(NeverInMemory {
                span: ty.span,
                position,
            });
            return Ty::Error;
        }
        self.ty(ty, UnsizedPosition::Memory(position))
    }

    /// `[elem]` written where a size is needed, at `position`: `{error}`,
    /// recorded for the type checker unless its element type failed
    /// already, which was reported (ADR 0031, 0034).
    fn unsized_ty(&mut self, span: Span, elem: &ast::Ty, position: UnsizedPosition) -> Ty {
        let elem = self.memory_ty(elem, NeverPosition::SliceElement);
        if elem != Ty::Error {
            self.unsized_types.push(UnsizedType {
                span,
                ty: Ty::slice(elem),
                position,
            });
        }
        Ty::Error
    }

    /// A written type, at `position`, where `[T]` has no size. One
    /// containing `{error}` is `{error}` as a whole.
    fn ty(&mut self, ty: &ast::Ty, position: UnsizedPosition) -> Ty {
        match &ty.kind {
            ast::TyKind::Never => Ty::Never,
            ast::TyKind::Unit => Ty::Unit,
            ast::TyKind::Slice(elem) => self.unsized_ty(ty.span, elem, position),
            // Behind a pointer, `[T]` is the pointee of a slice (ADR 0031).
            ast::TyKind::Ptr {
                mutable,
                volatile,
                pointee,
            } => {
                let pointee = match &pointee.kind {
                    ast::TyKind::Slice(elem) => {
                        match self.memory_ty(elem, NeverPosition::SliceElement) {
                            Ty::Error => Ty::Error,
                            elem => Ty::slice(elem),
                        }
                    }
                    _ => self.memory_ty(pointee, NeverPosition::Pointee),
                };
                match pointee {
                    Ty::Error => Ty::Error,
                    pointee => Ty::ptr(*mutable, *volatile, pointee),
                }
            }
            ast::TyKind::Array { elem, len } => {
                let elem = self.memory_ty(elem, NeverPosition::Element);
                let len = self.array_len(len);
                match elem {
                    Ty::Error => Ty::Error,
                    elem => Ty::array_of(elem, len),
                }
            }
            ast::TyKind::Applied(path, args) => self.applied_ty(ty, path, args),
            ast::TyKind::Named(path) => self.named_ty(path),
        }
    }

    /// A type written by its path, without type arguments.
    fn named_ty(&mut self, path: &ast::Path) -> Ty {
        if let Some(name) = plain_name(path) {
            if name.name == "bool" {
                return Ty::Bool;
            }
            if let Some(builtin) = BuiltinEnum::from_name(&name.name) {
                self.diags.push(
                    Diagnostic::error(
                        "resolve::type-argument-count",
                        format!(
                            "`{}` takes {}, written in parentheses",
                            name.name,
                            type_arguments(builtin.arity())
                        ),
                    )
                    .label(name.span, "written without them")
                    .help(format!(
                        "write the types it holds, as in `{}` (ADR 0045)",
                        builtin.written()
                    )),
                );
                return Ty::Error;
            }
            if let Some(int) = IntTy::from_name(&name.name) {
                return Ty::Int(int);
            }
        }
        match self.lookup_path(path) {
            Named::Item(Def::Struct(id)) => Ty::Struct(self.struct_tys[id.0 as usize]),
            Named::Item(Def::Enum(id)) => Ty::Enum(self.enum_tys[id.0 as usize]),
            named => {
                self.not_a_type(path, named);
                Ty::Error
            }
        }
    }

    /// `name(args)`, written as `ty`: `Option(T)` or `Result(T, E)`, with
    /// as many type arguments as it takes, each a type of memory, as a
    /// payload type is (ADR 0045). Any other type takes none. One whose
    /// arguments are in error, reported, is `{error}` as a whole.
    fn applied_ty(&mut self, ty: &ast::Ty, path: &ast::Path, args: &[ast::Ty]) -> Ty {
        let lowered: Vec<Ty> = args
            .iter()
            .map(|arg| self.memory_ty(arg, NeverPosition::TypeArgument))
            .collect();
        let builtin = plain_name(path).and_then(|name| BuiltinEnum::from_name(&name.name));
        if builtin.is_none() && !self.names_type(path) {
            return Ty::Error;
        }
        let name = written(path);
        let takes = builtin.map_or(0, BuiltinEnum::arity);
        let Some(builtin) = builtin.filter(|_| args.len() == takes) else {
            let given = match args.len() {
                0 => "none were given".to_owned(),
                1 => "1 was given".to_owned(),
                n => format!("{n} were given"),
            };
            let help = match builtin {
                Some(builtin) => format!("it is written `{}` (ADR 0045)", builtin.written()),
                None => {
                    "only `Option(T)` and `Result(T, E)` take type arguments (ADR 0045)".to_owned()
                }
            };
            // `u32()`: no arguments, but parentheses all the same.
            let message = if builtin.is_none() && args.is_empty() {
                format!("`{name}` takes no type arguments, and is written without parentheses")
            } else {
                format!("`{name}` takes {}, but {given}", type_arguments(takes))
            };
            self.diags.push(
                Diagnostic::error("resolve::type-argument-count", message)
                    .label(ty.span, "in this type")
                    .help(help),
            );
            return Ty::Error;
        };
        if lowered.contains(&Ty::Error) {
            return Ty::Error;
        }
        let applied = Ty::applied(builtin, lowered);
        self.applications.push((ty.span, applied));
        applied
    }

    /// Whether `path` names a type that takes no type arguments: `bool`,
    /// an integer type, a struct or an enum. Anything else is reported.
    fn names_type(&mut self, path: &ast::Path) -> bool {
        if let Some(name) = plain_name(path)
            && (name.name == "bool" || IntTy::from_name(&name.name).is_some())
        {
            return true;
        }
        match self.lookup_path(path) {
            Named::Item(Def::Struct(_) | Def::Enum(_)) => true,
            named => {
                self.not_a_type(path, named);
                false
            }
        }
    }

    /// Reports `path`, which `named` shows not to be a type, unless that
    /// was reported.
    fn not_a_type(&mut self, path: &ast::Path, named: Named) {
        let diag = Diagnostic::error(
            "resolve::unknown-type",
            format!("unknown type `{}`", written(path)),
        );
        let diag = match named {
            Named::Failed => return,
            Named::Item(Def::Module(_)) => return self.misused_module(path, "a type"),
            Named::Missing(module) => self.missing(diag, path, module, "not a type"),
            Named::Item(def) => diag.label(path.span, format!("{}, not a type", def.what())),
            Named::Variant(..) => diag.label(path.span, "a variant, not a type"),
        };
        self.diags.push(diag);
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
                let ty = self.ty(&let_.ty, UnsizedPosition::Variable);
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
        let local = self.new_local(name, ty, mutable);
        self.scopes
            .last_mut()
            .expect("locals are declared inside a scope")
            .insert(name.name.clone(), local);
        local
    }

    /// A local, not yet visible in any scope.
    fn new_local(&mut self, name: &ast::Ident, ty: Ty, mutable: bool) -> LocalId {
        let local = LocalId(self.locals.len() as u32);
        self.locals.push(Local {
            name: name.name.clone(),
            ty,
            mutable,
            span: name.span,
        });
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

    /// A path used as a value: a local, shadowing an item of the same name,
    /// a constant, a static, or `Enum::Variant` (ADR 0035).
    fn resolve(&mut self, path: &ast::Path) -> Option<ExprKind> {
        if let Some(name) = plain_name(path)
            && let Some((local, depth)) = self.find_local_scope(&name.name)
        {
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
        let named = self.lookup_path(path);
        let message = if path.segments.len() == 1 {
            format!("cannot find `{}` in this scope", written(path))
        } else {
            format!("cannot find `{}`", written(path))
        };
        let diag = Diagnostic::error("resolve::unknown-name", message);
        let diag = match named {
            Named::Item(Def::Const(id)) => return Some(ExprKind::Const(id)),
            Named::Item(Def::Static(id)) => return Some(ExprKind::Static(id)),
            Named::Variant(id, index) => return Some(ExprKind::Variant(id, index, None)),
            Named::Failed => return None,
            Named::Item(Def::Module(_)) => {
                self.misused_module(path, "a value");
                return None;
            }
            Named::Item(Def::Fn(_)) => diag
                .label(path.span, "a function, which is not a value")
                .help(format!("call it: `{}(...)`", written(path))),
            Named::Item(def) => {
                diag.label(path.span, format!("{}, which is not a value", def.what()))
            }
            Named::Missing(module) => self.missing(diag, path, module, "not found"),
        };
        self.diags.push(diag);
        None
    }

    /// Resolves the callee of a call: a function, or a variant built with
    /// its payload's values (ADR 0039). Locals shadow functions, as in
    /// Rust.
    fn resolve_fn(&mut self, path: &ast::Path) -> Option<Callee> {
        if let Some(name) = plain_name(path)
            && let Some(local) = self.find_local(&name.name)
        {
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
        let diag = Diagnostic::error(
            "resolve::unknown-function",
            format!("cannot find function `{}`", written(path)),
        );
        let diag = match self.lookup_path(path) {
            Named::Item(Def::Fn(id)) => {
                if self.fn_decl_of(id).0.kind == ast::FnKind::Entry {
                    self.diags.push(
                        Diagnostic::error(
                            "resolve::call-entry",
                            "the entry function cannot be called",
                        )
                        .label(path.span, "called here")
                        .help("execution starts there; it is not a function to call"),
                    );
                    return None;
                }
                return Some(Callee::Fn(id));
            }
            Named::Variant(id, index) => return Some(Callee::Variant(id, index)),
            Named::Failed => return None,
            Named::Item(Def::Module(_)) => {
                self.misused_module(path, "a function");
                return None;
            }
            Named::Item(def) => diag.label(path.span, format!("{}, not a function", def.what())),
            Named::Missing(module) => self.missing(diag, path, module, "not found"),
        };
        self.diags.push(diag);
        None
    }

    fn expr(&mut self, expr: &ast::Expr) -> Expr {
        let id = ExprId(self.next_expr);
        self.next_expr += 1;
        let kind = match &expr.kind {
            // Rejected literals were reported by the lexer, and the pipeline
            // stops before HIR when there are syntax errors.
            ast::ExprKind::Int(value) => ExprKind::Int(value.unwrap_or(0)),
            ast::ExprKind::Byte(value) => ExprKind::Byte(value.unwrap_or(0)),
            // A rejected one was reported by the lexer.
            ast::ExprKind::ByteString(bytes) => {
                ExprKind::ByteString(bytes.clone().unwrap_or_default())
            }
            ast::ExprKind::Bool(value) => ExprKind::Bool(*value),
            ast::ExprKind::Unit => ExprKind::Unit,
            ast::ExprKind::Array(elems) => {
                ExprKind::Array(elems.iter().map(|e| self.expr(e)).collect())
            }
            ast::ExprKind::Repeat(value, len) => {
                let value = self.boxed(value);
                ExprKind::Repeat(value, self.array_len(len))
            }
            ast::ExprKind::Name(path) => match self.resolve(path) {
                Some(kind) => kind,
                // Reported; any placeholder works since lowering errors
                // stop the pipeline.
                None => ExprKind::Unit,
            },
            // Its enum is the expected type, known to type checking
            // (ADR 0044).
            ast::ExprKind::ContextVariant(variant, args) => ExprKind::ContextVariant(
                variant.name.clone(),
                variant.span,
                args.as_ref()
                    .map(|args| args.iter().map(|arg| self.expr(arg)).collect()),
            ),
            ast::ExprKind::Call(path, args)
                if let Some(name) = plain_name(path)
                    && name.name == "assert" =>
            {
                let mut args: Vec<Expr> = args.iter().map(|arg| self.expr(arg)).collect();
                if !matches!(args.len(), 1 | 2) {
                    let given = if args.len() == 1 { "was" } else { "were" };
                    self.diags.push(
                        Diagnostic::error(
                            "ty::wrong-arg-count",
                            format!(
                                "`assert` takes 1 or 2 arguments, but {} {given} given",
                                args.len()
                            ),
                        )
                        .label(expr.span, "in this call")
                        .help(
                            "`assert(cond)` checks one `bool`, and `assert(cond, msg)` adds a message, a `*const [u8]` (ADR 0029, 0031)",
                        ),
                    );
                    return Expr {
                        id,
                        kind: ExprKind::Unit,
                        span: expr.span,
                    };
                }
                let message = (args.len() == 2).then(|| Box::new(args.remove(1)));
                ExprKind::Assert(Box::new(args.remove(0)), message, name.span)
            }
            // Built in, whatever else has the name (ADR 0031).
            ast::ExprKind::Call(path, args)
                if let Some(name) = plain_name(path)
                    && matches!(name.name.as_str(), "len" | "slice") =>
            {
                let mut args: Vec<Expr> = args.iter().map(|arg| self.expr(arg)).collect();
                let (params, help) = if name.name == "len" {
                    (1, "`len(s)` gives the length of one slice (ADR 0031)")
                } else {
                    (2, "`slice(p, n)` takes a pointer and a length (ADR 0031)")
                };
                if args.len() != params {
                    let plural = if params == 1 { "" } else { "s" };
                    let given = if args.len() == 1 { "was" } else { "were" };
                    self.diags.push(
                        Diagnostic::error(
                            "ty::wrong-arg-count",
                            format!(
                                "`{}` takes {params} argument{plural}, but {} {given} given",
                                name.name,
                                args.len()
                            ),
                        )
                        .label(expr.span, "in this call")
                        .help(help),
                    );
                    return Expr {
                        id,
                        kind: ExprKind::Unit,
                        span: expr.span,
                    };
                }
                let first = Box::new(args.remove(0));
                match args.pop() {
                    Some(len) => ExprKind::Slice(first, Box::new(len)),
                    None => ExprKind::Len(first),
                }
            }
            // A function's arguments, or the values of a variant's payload
            // (ADR 0039), which type checking counts.
            ast::ExprKind::Call(path, args) => {
                let callee = self.resolve_fn(path);
                let args = args.iter().map(|arg| self.expr(arg)).collect();
                match callee {
                    Some(Callee::Fn(callee)) => ExprKind::Call { callee, args },
                    Some(Callee::Variant(id, index)) => ExprKind::Variant(id, index, Some(args)),
                    // Reported; lowering errors stop the pipeline.
                    None => ExprKind::Unit,
                }
            }
            ast::ExprKind::Unary(op, operand) => ExprKind::Unary(*op, self.boxed(operand)),
            ast::ExprKind::Deref(operand) => ExprKind::Deref(self.boxed(operand)),
            ast::ExprKind::StructLit(path, fields) => self.struct_lit(expr, path, fields),
            ast::ExprKind::Field(base, field) => {
                ExprKind::Field(self.boxed(base), field.name.clone(), field.span)
            }
            ast::ExprKind::SizeOf(ty) => {
                ExprKind::SizeOf(self.memory_ty(ty, NeverPosition::LayoutQuery("size_of")))
            }
            ast::ExprKind::AlignOf(ty) => {
                ExprKind::AlignOf(self.memory_ty(ty, NeverPosition::LayoutQuery("align_of")))
            }
            ast::ExprKind::OffsetOf(ty, field) => {
                let ty = self.memory_ty(ty, NeverPosition::LayoutQuery("offset_of"));
                ExprKind::OffsetOf(ty, field.name.clone(), field.span)
            }
            ast::ExprKind::Index(base, index, open) => {
                ExprKind::Index(self.boxed(base), self.boxed(index), *open)
            }
            ast::ExprKind::AddrOf { mutable, place } => ExprKind::AddrOf {
                mutable: *mutable,
                place: self.boxed(place),
            },
            ast::ExprKind::Binary(op, lhs, rhs, op_span) => {
                ExprKind::Binary(*op, self.boxed(lhs), self.boxed(rhs), *op_span)
            }
            ast::ExprKind::Logical(op, lhs, rhs) => {
                ExprKind::Logical(*op, self.boxed(lhs), self.boxed(rhs))
            }
            ast::ExprKind::Cast(operand, ty) => {
                let operand = self.boxed(operand);
                ExprKind::Cast(operand, self.ty(ty, UnsizedPosition::Cast), ty.span)
            }
            ast::ExprKind::Assign(place, value) => {
                ExprKind::Assign(self.boxed(place), self.boxed(value))
            }
            ast::ExprKind::CompoundAssign(op, place, value, op_span) => {
                ExprKind::CompoundAssign(*op, self.boxed(place), self.boxed(value), *op_span)
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
            ast::ExprKind::Match {
                scrutinee,
                arms,
                keyword,
            } => {
                let scrutinee = self.boxed(scrutinee);
                let arms = arms.iter().map(|arm| self.arm(arm)).collect();
                ExprKind::Match {
                    scrutinee,
                    arms,
                    keyword: *keyword,
                }
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
            // It may return from the enclosing function, as `return` does
            // (ADR 0046).
            ast::ExprKind::Try(operand, question) => {
                if self.in_comptime {
                    self.diags.push(
                        Diagnostic::error(
                            "resolve::return-in-comptime",
                            "`?` cannot leave code that runs at compile time",
                        )
                        .label(expr.span, "returns on a failure, with no function to return from")
                        .help(
                            "compile-time code gives its value as its last expression; take the value apart with a `match`, or call a function that uses `?` (ADR 0029, 0046)",
                        ),
                    );
                }
                ExprKind::Try(self.boxed(operand), *question)
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

    /// An arm of a `match`, in a scope of its own: the variables its
    /// pattern binds exist only in its expression (ADR 0037, 0040).
    fn arm(&mut self, arm: &ast::Arm) -> Arm {
        self.scopes.push(HashMap::new());
        let mut bound = Vec::new();
        let pattern = self.pattern(&arm.pattern, &[], &mut bound);
        // The variables exist in the arm's expression, not in the rest of
        // the pattern, whose names resolve as outside it.
        let scope = self
            .scopes
            .last_mut()
            .expect("an arm has a scope of its own");
        for bound in bound {
            scope.insert(bound.name, bound.local);
        }
        let body = self.expr(&arm.body);
        self.scopes.pop();
        Arm { pattern, body }
    }

    /// A pattern. Each variable it binds goes in `bound`, after those
    /// bound before it in the arm's pattern, to be visible in the arm's
    /// expression; one name bound twice is reported against the others
    /// (ADR 0040). The alternatives of an or-pattern bind one variable
    /// per name, which the first declares (ADR 0041); `later` are the
    /// or-patterns around the pattern that it is in a later alternative
    /// of, innermost last.
    fn pattern(
        &mut self,
        pattern: &ast::Pattern,
        later: &[Later],
        bound: &mut Vec<Bound>,
    ) -> Pattern {
        let kind = match &pattern.kind {
            ast::PatternKind::Wildcard => PatternKind::Wildcard,
            ast::PatternKind::Binding { mutable, name, ty } => {
                // A `let` the first alternative of an or-pattern around it
                // compared with the same variable was reported there.
                let reported = |index: usize| {
                    later
                        .iter()
                        .any(|or| index < or.before && or.first.iter().any(|v| v.name == name.name))
                };
                if let Some((_, first)) = bound
                    .iter()
                    .enumerate()
                    .find(|&(index, bound)| bound.name == name.name && !reported(index))
                {
                    self.diags.push(
                        Diagnostic::error(
                            "resolve::duplicate-binding",
                            format!("`{}` is bound twice in this pattern", name.name),
                        )
                        .label(first.name_span, "first bound here")
                        .label(name.span, "bound again here")
                        .help("each variable of a pattern has a name of its own (ADR 0040)"),
                    );
                }
                let local_ty = self.ty(ty, UnsizedPosition::Variable);
                // A later alternative's `let` is the variable the first
                // alternative declared, whose type type checking compares
                // with the one written here (ADR 0041).
                let shared = later
                    .iter()
                    .rev()
                    .find_map(|or| or.first.iter().find(|v| v.name == name.name));
                let local = match shared {
                    Some(shared) => shared.local,
                    None => self.new_local(name, local_ty, *mutable),
                };
                bound.push(Bound {
                    name: name.name.clone(),
                    local,
                    mutable: *mutable,
                    name_span: name.span,
                    let_span: pattern.span,
                    reported: false,
                });
                PatternKind::Binding {
                    local,
                    ty: local_ty,
                    ty_span: ty.span,
                }
            }
            // Rejected literals were reported by the lexer, and the pipeline
            // stops before HIR when there are syntax errors.
            ast::PatternKind::Int { value, negated } => PatternKind::Int {
                value: value.unwrap_or(0),
                negated: *negated,
            },
            ast::PatternKind::Byte(value) => PatternKind::Byte(value.unwrap_or(0)),
            ast::PatternKind::Bool(value) => PatternKind::Bool(*value),
            ast::PatternKind::Name(path) => self.pattern_path(path),
            ast::PatternKind::PathArgs(path, patterns) => {
                let found = self.pattern_variant(path);
                let patterns = patterns
                    .iter()
                    .map(|sub| self.pattern(sub, later, bound))
                    .collect();
                match found {
                    Some((id, index)) => PatternKind::Variant(id, index, Some(patterns)),
                    // Reported; lowering errors stop the pipeline.
                    None => PatternKind::Wildcard,
                }
            }
            // Its enum is the type of its position, known to type checking
            // (ADR 0044); what it binds, it binds as a variant pattern.
            ast::PatternKind::ContextVariant(variant, patterns) => {
                let id = PatternId(self.next_pattern);
                self.next_pattern += 1;
                let patterns = patterns.as_ref().map(|patterns| {
                    patterns
                        .iter()
                        .map(|sub| self.pattern(sub, later, bound))
                        .collect()
                });
                PatternKind::ContextVariant(id, variant.name.clone(), variant.span, patterns)
            }
            ast::PatternKind::Or(alternatives) => {
                PatternKind::Or(self.or_pattern(alternatives, later, bound))
            }
        };
        Pattern {
            kind,
            span: pattern.span,
        }
    }

    /// The alternatives of an or-pattern (ADR 0041). Each starts from the
    /// variables bound before the or-pattern; the first declares one
    /// variable per name it binds, and the others' `let`s of those names
    /// are those variables. A name that some alternatives do not bind, or
    /// bind with `let mut` while others bind it with `let`, is
    /// `resolve::inconsistent-binding`, once per name, unless an inner
    /// or-pattern reported it; either way it counts as bound by the
    /// or-pattern, which adds to `bound` each name that some alternative
    /// binds, as the first one binding it does.
    fn or_pattern(
        &mut self,
        alternatives: &[ast::Pattern],
        later: &[Later],
        bound: &mut Vec<Bound>,
    ) -> Vec<Pattern> {
        let before = bound.len();
        let mut lowered = Vec::with_capacity(alternatives.len());
        // The variables each alternative binds, in the order written.
        let mut per_alternative: Vec<Vec<Bound>> = Vec::with_capacity(alternatives.len());
        let mut first = Vec::new();
        for (i, alternative) in alternatives.iter().enumerate() {
            let mut alternative_bound = bound.clone();
            let pattern = if i == 0 {
                self.pattern(alternative, later, &mut alternative_bound)
            } else {
                let mut inner = later.to_vec();
                inner.push(Later {
                    before,
                    first: &first,
                });
                self.pattern(alternative, &inner, &mut alternative_bound)
            };
            lowered.push(pattern);
            let variables = alternative_bound.split_off(before);
            if i == 0 {
                first = variables.clone();
            }
            per_alternative.push(variables);
        }
        // Each name once, as the first alternative binding it binds it.
        let mut names: Vec<Bound> = Vec::new();
        for variable in per_alternative.iter().flatten() {
            if !names.iter().any(|named| named.name == variable.name) {
                names.push(variable.clone());
            }
        }
        for named in &mut names {
            let found: Vec<Option<&Bound>> = per_alternative
                .iter()
                .map(|variables| variables.iter().find(|v| v.name == named.name))
                .collect();
            if found.iter().flatten().any(|v| v.reported) {
                named.reported = true;
                continue;
            }
            let name = &named.name;
            if let Some(missing) = found.iter().position(Option::is_none) {
                let binding = found
                    .iter()
                    .position(Option::is_some)
                    .expect("some alternative binds each name");
                self.diags.push(
                    Diagnostic::error(
                        "resolve::inconsistent-binding",
                        format!("`{name}` is not bound by every alternative"),
                    )
                    .label(alternatives[binding].span, format!("binds `{name}`"))
                    .label(alternatives[missing].span, format!("does not bind `{name}`"))
                    .help(format!(
                        "the arm's expression has `{name}` whichever alternative matched, so every alternative binds it, with the same type (ADR 0041)"
                    )),
                );
                named.reported = true;
                continue;
            }
            let first = found[0].expect("every alternative binds the name");
            if let Some(other) = found.iter().flatten().find(|v| v.mutable != first.mutable) {
                let how = |v: &Bound| {
                    if v.mutable {
                        "bound with `let mut` here"
                    } else {
                        "bound with `let` here"
                    }
                };
                self.diags.push(
                    Diagnostic::error(
                        "resolve::inconsistent-binding",
                        format!("`{name}` is bound with `let mut` by only some alternatives"),
                    )
                    .label(first.let_span, how(first))
                    .label(other.let_span, how(other))
                    .help(format!(
                        "the alternatives bind one variable `{name}`, which is mutable or not; write `let mut` in every alternative or in none (ADR 0041)"
                    )),
                );
                named.reported = true;
            }
        }
        bound.extend(names);
        lowered
    }

    /// A path in a pattern without patterns of its own: a constant or a
    /// variant, resolved as in an expression (ADR 0037, 0049).
    fn pattern_path(&mut self, path: &ast::Path) -> PatternKind {
        if let Some(name) = plain_name(path) {
            return match self.pattern_const(name) {
                Some(id) => PatternKind::Const(id),
                // Reported; lowering errors stop the pipeline.
                None => PatternKind::Wildcard,
            };
        }
        match self.lookup_path(path) {
            Named::Variant(id, index) => return PatternKind::Variant(id, index, None),
            Named::Item(Def::Const(id)) => return PatternKind::Const(id),
            Named::Failed => {}
            Named::Item(Def::Module(_)) => self.misused_module(path, "a constant"),
            Named::Item(def) => {
                let diag = self.not_a_constant(path, def);
                self.diags.push(diag);
            }
            Named::Missing(module) => {
                let diag = Diagnostic::error(
                    "resolve::unknown-name",
                    format!("cannot find `{}`", written(path)),
                );
                let diag = self.missing(diag, path, module, "not found");
                self.diags.push(diag);
            }
        }
        // Reported; lowering errors stop the pipeline.
        PatternKind::Wildcard
    }

    /// The variant named by the path of `path(p, ...)`, a pattern with
    /// patterns of its own, which only a variant has (ADR 0040).
    fn pattern_variant(&mut self, path: &ast::Path) -> Option<(EnumId, u32)> {
        let diag = match self.lookup_path(path) {
            Named::Variant(id, index) => return Some((id, index)),
            Named::Failed => return None,
            Named::Item(Def::Module(_)) => {
                self.misused_module(path, "a variant");
                return None;
            }
            Named::Item(def) => Diagnostic::error(
                "resolve::unknown-variant",
                format!("`{}` is not a variant", written(path)),
            )
            .label(path.span, format!("{}, not a variant", def.what()))
            .help("patterns in parentheses match the values of a variant's payload, as in `E::V(p)` (ADR 0040)"),
            Named::Missing(module) => {
                let diag = Diagnostic::error(
                    "resolve::unknown-name",
                    format!("cannot find `{}`", written(path)),
                );
                self.missing(diag, path, module, "not found")
            }
        };
        self.diags.push(diag);
        None
    }

    /// `resolve::not-a-constant` for `path` in a pattern, which names
    /// `def` (ADR 0037).
    fn not_a_constant(&self, path: &ast::Path, def: Def) -> Diagnostic {
        let written = written(path);
        let diag = Diagnostic::error(
            "resolve::not-a-constant",
            format!("`{written}` is {}, not a constant", def.what()),
        )
        .label(path.span, "a name in a pattern is a constant");
        // To compare with anything else, the value is bound and tested.
        let compare = format!(
            "to compare with it, bind the value and test it: `let x: T => if x == {written} {{ ... }}` (ADR 0037)"
        );
        match def {
            Def::Static(_) => diag.help(format!(
                "a static is memory, read at run time (ADR 0030); {compare}"
            )),
            Def::Enum(id) => {
                // An empty enum was reported; any name shows the form.
                let variant = self.enum_decls[id.0 as usize]
                    .0
                    .variants
                    .first()
                    .map_or("Variant", |v| v.name.name.as_str());
                diag.help(format!(
                    "a variant is matched with its enum's name, as in `{written}::{variant}` (ADR 0035, 0037)"
                ))
            }
            _ => diag.help("a pattern matches a value: a literal, a constant or `_` (ADR 0037)"),
        }
    }

    /// A bare name in a pattern, which is always a constant (ADR 0037). It
    /// is looked up as in an expression, so a local variable shadows a
    /// constant of the same name. `core` names `core`'s root, as in a path
    /// (ADR 0052).
    fn pattern_const(&mut self, name: &ast::Ident) -> Option<ConstId> {
        let path = ast::Path {
            segments: vec![ast::Ident {
                name: name.name.clone(),
                span: name.span,
            }],
            span: name.span,
        };
        let binding = if name.name == "core" {
            Some(Binding::Def(Def::Module(self.core_root)))
        } else {
            self.namespaces[self.module.0 as usize]
                .get(&name.name)
                .map(|entry| entry.binding)
        };
        let def = match binding {
            Some(binding) => match self.binding_def(binding) {
                Some(def) => Lookup::Found(def),
                None => Lookup::Failed,
            },
            None => Lookup::Missing,
        };
        let diag = if let Some(local) = self.find_local(&name.name) {
            let declared = self.locals[local.0 as usize].span;
            let compare = format!(
                "to compare with it, bind the value and test it: `let x: T => if x == {} {{ ... }}` (ADR 0037)",
                name.name
            );
            let diag = Diagnostic::error(
                "resolve::not-a-constant",
                format!("`{}` is a variable, not a constant", name.name),
            )
            .label(name.span, "a name in a pattern is a constant")
            .label(declared, "the variable is declared here");
            if matches!(def, Lookup::Found(Def::Const(_))) {
                diag.help(format!(
                    "the variable shadows the constant `{}` here (ADR 0029); {compare}",
                    name.name
                ))
            } else {
                diag.help(compare)
            }
        } else {
            match def {
                Lookup::Found(Def::Const(id)) => return Some(id),
                Lookup::Found(Def::Module(_)) => {
                    self.misused_module(&path, "a constant");
                    return None;
                }
                Lookup::Found(def) => self.not_a_constant(&path, def),
                Lookup::Failed => return None,
                Lookup::Missing => {
                    let diag = Diagnostic::error(
                        "resolve::unknown-name",
                        format!("cannot find `{}` in this scope", name.name),
                    )
                    .help(format!(
                        "a name in a pattern is always a constant; to bind the value to a new variable, write `let {}: T` (ADR 0037)",
                        name.name
                    ));
                    self.missing(diag, &path, self.module, "not found")
                }
            }
        };
        self.diags.push(diag);
        None
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
