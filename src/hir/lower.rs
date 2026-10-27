//! AST -> HIR lowering with name resolution.

use std::collections::HashMap;

use crate::diag::{Diagnostic, Diagnostics};
use crate::hir::{
    self, BuiltinEnum, ConstId, CoreStart, EnumDef, EnumId, EnumTy, ExprId, FileDef, Fn, FnId,
    FnKind, Hir, IntTy, Local, LocalId, ModuleDef, ModuleId, NeverInMemory, PeripheralByValue,
    PeripheralId, PeripheralTy, PeripheralTypeDef, PeripheralTypeId, StaticDef, StaticId,
    StructDef, StructId, StructTy, Ty, UnsizedType, Visibility, VolatilePeripheral,
};
use crate::span::Span;
use crate::syntax::ast;

use hardware::memory_map;

mod expr;
mod hardware;
mod item;
mod pattern;
mod resolve;
mod types;

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
        memories: Vec::new(),
        peripheral_types: Vec::new(),
        peripherals: Vec::new(),
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
        memories: memory_decls,
        peripheral_types: peripheral_type_decls,
        peripherals: peripheral_decls,
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

    // Peripheral types too (ADR 0058).
    let peripheral_tys = peripheral_type_decls
        .iter()
        .enumerate()
        .map(|(i, &(decl, module))| PeripheralTy {
            id: PeripheralTypeId(i as u32),
            name: Box::leak(item_path(&modules, module, &decl.name.name).into_boxed_str()),
        })
        .collect();

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
        peripheral_tys,
        peripheral_type_decls: &peripheral_type_decls,
        peripheral_decls: &peripheral_decls,
        uses,
        use_stack: Vec::new(),
        never_in_memory: Vec::new(),
        unsized_types: Vec::new(),
        peripherals_by_value: Vec::new(),
        volatile_peripherals: Vec::new(),
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
    let mut maps = Vec::new();
    let mut peripheral_types = Vec::new();
    let mut peripherals = Vec::new();
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
                peripherals_by_value: lowerer.peripherals_by_value.len(),
                volatile_peripherals: lowerer.volatile_peripherals.len(),
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
        for &(decl, module) in &memory_decls[from.memories..to.memories] {
            lowerer.module = module;
            maps.push((decl.span, lowerer.memory_decl(decl)));
        }
        for &(decl, module) in &peripheral_type_decls[from.peripheral_types..to.peripheral_types] {
            lowerer.module = module;
            peripheral_types.push(lowerer.peripheral_type_decl(decl));
        }
        for &(decl, module) in &peripheral_decls[from.peripherals..to.peripherals] {
            lowerer.module = module;
            peripherals.push(lowerer.peripheral_decl(decl));
        }
    }
    debug_assert_eq!(fns.len() as u32, lowerer.value_base);
    fns.append(&mut lowerer.values);
    let expr_count = lowerer.next_expr;
    let never_in_memory = lowerer.never_in_memory;
    let unsized_types = lowerer.unsized_types;
    let peripherals_by_value = lowerer.peripherals_by_value;
    let volatile_peripherals = lowerer.volatile_peripherals;
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
        &mut peripheral_types,
    );

    for diag in duplicates {
        diags.push(diag);
    }
    let memory = memory_map(maps, diags);
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
        memory,
        peripheral_types,
        peripherals,
        never_in_memory,
        unsized_types,
        peripherals_by_value,
        volatile_peripherals,
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
    memories: usize,
    peripheral_types: usize,
    peripherals: usize,
}

/// Fills in the `reachable` of every struct, enum and peripheral type: the
/// modules where a path written would name it and be usable (ADR 0050,
/// 0058). Any path can be written from the root module, through `crate`,
/// and a segment is usable from a module if it is `pub` or that module is
/// within the one holding it. So what is reachable from a module is what a walk of the namespaces
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
    peripheral_types: &mut [PeripheralTypeDef],
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
                    Def::PeripheralType(id) => &mut peripheral_types[id.0 as usize].reachable,
                    Def::Fn(_) | Def::Const(_) | Def::Static(_) | Def::Peripheral(_) => continue,
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
    PeripheralType(PeripheralTypeId),
    Peripheral(PeripheralId),
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
            Def::PeripheralType(_) => "a peripheral type",
            Def::Peripheral(_) => "a peripheral",
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
    memories: Vec<(&'a ast::MemoryDecl, ModuleId)>,
    peripheral_types: Vec<(&'a ast::PeripheralTypeDecl, ModuleId)>,
    peripherals: Vec<(&'a ast::PeripheralDecl, ModuleId)>,
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
            memories: self.memories.len(),
            peripheral_types: self.peripheral_types.len(),
            peripherals: self.peripherals.len(),
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
                // Not lowered yet (M3.3, step 2).
                ast::Item::BitStruct(_) => continue,
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
                ast::Item::Memory(decl) => {
                    self.memories.push((decl, module));
                    continue;
                }
                ast::Item::PeripheralType(decl) => {
                    self.peripheral_types.push((decl, module));
                    let id = PeripheralTypeId(self.peripheral_types.len() as u32 - 1);
                    (&decl.name, decl.vis, Def::PeripheralType(id))
                }
                ast::Item::Peripheral(decl) => {
                    self.peripherals.push((decl, module));
                    let id = PeripheralId(self.peripherals.len() as u32 - 1);
                    (&decl.name, decl.vis, Def::Peripheral(id))
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
        // `use u8;` names a built-in by its own name: it brings nothing in,
        // and is reported once, as naming a built-in (`resolve_use`).
        if let Binding::Use(id) = binding
            && plain_name(&self.uses[id].path).is_some_and(|single| single.name == name)
            && builtin_what(name).is_some()
        {
            return;
        }
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
            } else if name == "rt"
                && module == ModuleId::ROOT
                && matches!(binding, Binding::Def(Def::Module(_)))
            {
                Some((
                    "reserved for `core` in the root module".to_owned(),
                    "the files of `core` are named `rt/core.ers` and `rt/builtins.ers`, so the files of this module could be named like them: give it another name, or put it inside another module (ADR 0056)".to_owned(),
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
            "functions, structs, enums, constants, statics, peripheral types and peripherals share one namespace (ADR 0029, 0030, 0035, 0058)"
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
    peripheral_tys: Vec<PeripheralTy>,
    peripheral_type_decls: &'a [(&'a ast::PeripheralTypeDecl, ModuleId)],
    peripheral_decls: &'a [(&'a ast::PeripheralDecl, ModuleId)],
    /// Each name a `use` brings in (ADR 0049).
    uses: Vec<Use>,
    /// The `use`s being resolved, each through the next.
    use_stack: Vec<usize>,
    /// Each `!` written where memory would have its type (ADR 0034).
    never_in_memory: Vec<NeverInMemory>,
    /// Each `[T]` written where a size is needed (ADR 0031).
    unsized_types: Vec<UnsizedType>,
    /// Each peripheral type written where a value would have it, and each
    /// pointer to one written `volatile` (ADR 0058).
    peripherals_by_value: Vec<PeripheralByValue>,
    volatile_peripherals: Vec<VolatilePeripheral>,
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

/// Name resolution: paths (ADR 0049) and visibility (ADR 0050).
#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::SourceMap;
    use crate::syntax::{lexer, parser};

    /// The codes `program` reports when lowered with `core` as its `core`.
    fn lower_codes(program: &str, core: &str) -> Vec<&'static str> {
        let mut sources = SourceMap::default();
        let mut diags = Diagnostics::default();
        let mut parse = |name: &str, text: &str, diags: &mut Diagnostics| {
            let file = sources.add(name, text);
            let tokens = lexer::lex(file, text, diags);
            (file, parser::parse(text, &tokens, diags))
        };
        let (_, program) = parse("main.ers", program, &mut diags);
        let (core_file, core) = parse("rt/core.ers", core, &mut diags);
        let files = sources
            .files()
            .map(|file| FileDef::new(&file.name, &file.text))
            .collect();
        lower(&program, &core, files, core_file.index(), &mut diags);
        diags.into_vec().iter().map(|d| d.code).collect()
    }

    /// Inside `core`, at its root and in its modules, `crate` is `core`'s
    /// root, not the program's (ADR 0049, 0052): the program has no `LIMIT`.
    #[test]
    fn crate_in_core_is_core() {
        let core = "
            pub struct PanicInfo { pub line: u32 }
            pub enum PanicReason: u8 { DivisionByZero = 0 }
            const LIMIT: u32 = 3;
            pub fn limit() -> u32 { crate::LIMIT }
            mod inner {
                fn limit() -> u32 { crate::LIMIT }
            }
        ";
        let program = "
            entry fn main() -> ! { loop {} }
            panic fn on_panic() -> ! { loop {} }
        ";
        assert_eq!(lower_codes(program, core), Vec::<&str>::new());
        // The same paths in the program are reported.
        let program = "
            entry fn main() -> ! { loop {} }
            panic fn on_panic() -> ! { loop {} }
            fn limit() -> u32 { crate::LIMIT }
        ";
        assert_eq!(lower_codes(program, core), ["resolve::unknown-name"]);
    }
}
