//! Name resolution: looking names and paths up in the namespaces of
//! modules, and resolving `use` declarations (ADR 0050).

use crate::builtin::builtin_variant_help;
use crate::diag::Diagnostic;
use crate::hir::{self, BuiltinEnum, EnumId, FnId, ModuleId, Visibility};
use crate::span::Span;
use crate::syntax::ast;

use super::{
    Binding, Def, Lookup, Lowerer, Named, UseState, builtin_fn, builtin_what, is_builtin_type,
    item_path, plain_name, visibility, written, written_segments,
};

impl Lowerer<'_> {
    /// How diagnostics name a module: "module `uart::regs`", or "the root
    /// module".
    pub(super) fn module_name(&self, module: ModuleId) -> String {
        self.modules[module.0 as usize].describe()
    }

    /// The path of the item `name` of `module`, as diagnostics show it.
    pub(super) fn path_of(&self, module: ModuleId, name: &str) -> String {
        item_path(&self.modules, module, name)
    }

    /// Whether `inner` is `outer` or a module inside it, where the private
    /// items of `outer` are visible (ADR 0050).
    pub(super) fn is_within(&self, inner: ModuleId, outer: ModuleId) -> bool {
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
    pub(super) fn fn_decl_of(&self, id: FnId) -> (&ast::FnDecl, ModuleId) {
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
    pub(super) fn binding_def(&mut self, binding: Binding) -> Option<Def> {
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
            self.use_builtin(single, what);
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
    pub(super) fn use_decls(&mut self, decls: &[(&ast::UseDecl, ModuleId)]) {
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
            Def::BitStruct(id) => {
                let (decl, module) = self.bit_struct_decls[id.0 as usize];
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
            Def::PeripheralType(id) => {
                let (decl, module) = self.peripheral_type_decls[id.0 as usize];
                item(decl.vis, module, &decl.name)
            }
            Def::Peripheral(id) => {
                let (decl, module) = self.peripheral_decls[id.0 as usize];
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
    pub(super) fn lookup_path(&mut self, path: &ast::Path) -> Named {
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
    pub(super) fn missing(
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
    pub(super) fn misused_module(&mut self, path: &ast::Path, expected: &str) {
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

/// Where the segments of a path read so far lead.
#[derive(Clone, Copy)]
enum Prefix {
    Module(ModuleId),
    Enum(EnumId),
}
