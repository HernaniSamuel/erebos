//! Lowering `memory` blocks (ADR 0057) and peripherals (ADR 0058).

use std::collections::HashMap;

use crate::diag::{Diagnostic, Diagnostics};
use crate::hir::{
    ArrayLen, EntryDef, EntryKind, IntTy, MemoryMap, NeverPosition, PeripheralDef, PeripheralId,
    PeripheralTypeDef, Placement, RegionDef, Ty,
};
use crate::span::Span;
use crate::syntax::ast;

use super::{Def, Lowerer, Named, visibility, written};

impl Lowerer<'_> {
    /// A `memory` block (ADR 0057), checked on its own: each region name
    /// once and not `INCLUDE`, each of `code`, `data` and `stack` placed
    /// exactly once in a region of the block, the stack outside the code's
    /// region. "Second" is later in the source: the error points there and
    /// labels the first. Every value is lowered, those of a repeated region
    /// or placement too, so that the errors in them are reported. Returns
    /// the map, unless a placement is missing or names no region.
    pub(super) fn memory_decl(&mut self, decl: &ast::MemoryDecl) -> Option<MemoryMap> {
        self.no_attrs(&decl.attrs, "a `memory` block");
        let mut regions: Vec<RegionDef> = Vec::new();
        let mut by_name: HashMap<&str, usize> = HashMap::new();
        for region in &decl.regions {
            let name = region.name.name.as_str();
            if name == "INCLUDE" {
                self.diags.push(
                    Diagnostic::error(
                        "resolve::reserved-name",
                        "`INCLUDE` is a reserved name",
                    )
                    .label(region.name.span, "a command of the linker script language")
                    .help("the generated linker script names each region as the source does, and would read `INCLUDE` as a command there: give the region another name (ADR 0057)"),
                );
            }
            let origin = (
                self.comptime_usize(&region.origin, "origin"),
                region.origin.span,
            );
            let size = (self.comptime_usize(&region.size, "size"), region.size.span);
            if let Some(&first) = by_name.get(name) {
                self.diags.push(
                    Diagnostic::error(
                        "resolve::duplicate-region",
                        format!("the region `{name}` is declared more than once"),
                    )
                    .label(region.name.span, "declared again here")
                    .label(regions[first].name_span, "first declared here"),
                );
                continue;
            }
            by_name.insert(name, regions.len());
            regions.push(RegionDef {
                name: name.to_owned(),
                name_span: region.name.span,
                origin,
                size,
                span: region.span,
            });
        }

        // By `code`, `data` and `stack`, in the order of `PLACEMENTS`: the
        // first placement of each, with its region if the block has one of
        // that name.
        let mut placed: [Option<(Option<usize>, Span)>; PLACEMENTS.len()] =
            [None; PLACEMENTS.len()];
        let mut stack_size = None;
        for placement in &decl.placements {
            let slot = match &placement.kind {
                ast::PlacementKind::Code => 0,
                ast::PlacementKind::Data => 1,
                ast::PlacementKind::Stack { .. } => 2,
            };
            let word = PLACEMENTS[slot].0;
            let span = Span::new(
                placement.span.file,
                placement.span.lo as usize,
                placement.region.span.hi as usize,
            );
            let name = &placement.region;
            let region = by_name.get(name.name.as_str()).copied();
            if region.is_none() {
                self.diags.push(
                    Diagnostic::error(
                        "resolve::unknown-region",
                        format!("no region named `{}` in this `memory` block", name.name),
                    )
                    .label(name.span, "not a region of this block")
                    .help("a placement names a region declared in its `memory` block; regions are not items, so a constant or a module of that name does not count (ADR 0057)"),
                );
            }
            if let ast::PlacementKind::Stack { size } = &placement.kind {
                let size = (self.comptime_usize(size, "stack_size"), size.span);
                stack_size.get_or_insert(size);
            }
            if let Some((_, first)) = placed[slot] {
                self.diags.push(
                    Diagnostic::error(
                        "resolve::duplicate-placement",
                        format!("`{word}` is placed more than once"),
                    )
                    .label(span, "placed again here")
                    .label(first, "first placed here")
                    .help("each of `code`, `data` and `stack` is placed exactly once (ADR 0057)"),
                );
                continue;
            }
            placed[slot] = Some((region, span));
        }
        for (slot, (word, example)) in PLACEMENTS.into_iter().enumerate() {
            if placed[slot].is_none() {
                self.diags.push(
                    Diagnostic::error(
                        "resolve::missing-placement",
                        format!("the `memory` block does not place `{word}`"),
                    )
                    .label(memory_word(decl.span), format!("no `{word} in`"))
                    .help(format!("each of `code`, `data` and `stack` is placed exactly once: add `{example}` (ADR 0057)")),
                );
            }
        }
        if let [
            Some((Some(code), code_span)),
            _,
            Some((Some(stack), stack_span)),
        ] = placed
            && code == stack
        {
            let name = &regions[code].name;
            let stack_label = (stack_span, format!("the stack, in `{name}`"));
            let code_label = (code_span, format!("the code, in `{name}`"));
            let (again, first) = if stack_span.lo > code_span.lo {
                (stack_label, code_label)
            } else {
                (code_label, stack_label)
            };
            self.diags.push(
                Diagnostic::error(
                    "resolve::stack-with-code",
                    "the stack cannot share the code's region",
                )
                .label(again.0, again.1)
                .label(first.0, first.1)
                .help("the reset entry is at the origin of the code's region, where the stack would be reserved: place the stack in another region (ADR 0057)"),
            );
        }

        let [Some(code), Some(data), Some(stack)] = placed.map(|p| {
            p.and_then(|(region, span)| {
                Some(Placement {
                    region: region?,
                    span,
                })
            })
        }) else {
            return None;
        };
        Some(MemoryMap {
            span: decl.span,
            regions,
            code,
            data,
            stack,
            stack_size: stack_size.expect("a placed stack has a size"),
        })
    }

    /// A peripheral type (ADR 0058, 0064): it takes no attributes, and each
    /// entry name, of a register, register array or cluster, is declared
    /// once; a register's type is a type of memory or an array of one, a
    /// cluster's a peripheral type or an array of one, and offsets, steps
    /// and array lengths are `usize`s computed at compile time in the
    /// type's module. A register's read effect and an entry's group are
    /// carried as written (ADR 0060, 0061).
    pub(super) fn peripheral_type_decl(
        &mut self,
        decl: &ast::PeripheralTypeDecl,
    ) -> PeripheralTypeDef {
        self.no_attrs(&decl.attrs, "a peripheral type");
        let mut seen: HashMap<&str, Span> = HashMap::new();
        for entry in &decl.entries {
            let name = match entry {
                ast::EntryDecl::Register(register) => &register.name,
                ast::EntryDecl::Cluster(cluster) => &cluster.name,
            };
            if let Some(&first) = seen.get(name.name.as_str()) {
                self.diags.push(
                    Diagnostic::error(
                        "resolve::duplicate-register",
                        format!("the entry `{}` is declared more than once", name.name),
                    )
                    .label(name.span, "declared again here")
                    .label(first, "first declared here")
                    .help("each entry of a peripheral type, register, register array or cluster, has a name of its own; two registers at one offset, as a `read` one and a `write` one, take two names (ADR 0058, 0064)"),
                );
            }
            seen.entry(&name.name).or_insert(name.span);
        }
        let entries = decl
            .entries
            .iter()
            .map(|entry| match entry {
                ast::EntryDecl::Register(register) => self.register_entry(register),
                ast::EntryDecl::Cluster(cluster) => self.cluster_entry(cluster),
            })
            .collect();
        PeripheralTypeDef {
            path: self.path_of(self.module, &decl.name.name),
            name_span: decl.name.span,
            module: self.module,
            vis: visibility(decl.vis),
            // Filled in once every `use` is resolved, by `reachability`.
            reachable: Vec::new(),
            entries,
        }
    }

    /// A register (ADR 0058, 0060, 0064). Its `step` is for a register
    /// array only: on a register of any other type it is
    /// `resolve::misplaced-step`.
    fn register_entry(&mut self, register: &ast::RegisterDecl) -> EntryDef {
        let ty = self.register_ty(&register.ty);
        let offset = (
            self.comptime_usize(&register.offset, "offset"),
            register.offset.span,
        );
        let step = self.entry_step(register.step.as_ref());
        if let (Some(step), false) = (&register.step, is_array(&register.ty)) {
            self.misplaced_step(step, &register.ty, "a register", &register.name);
        }
        EntryDef {
            name: register.name.name.clone(),
            span: register.name.span,
            vis: visibility(register.vis),
            kind: EntryKind::Register {
                access: register.access,
                read_effect: register.read_effect,
            },
            ty,
            ty_span: register.ty.span,
            offset,
            step,
            // A label, resolved nowhere (ADR 0061).
            group: (register.group.as_ref()).map(|group| (group.name.clone(), group.span)),
        }
    }

    /// A cluster (ADR 0064). A cluster array needs a `step`
    /// (`resolve::missing-step`), and a single cluster takes none
    /// (`resolve::misplaced-step`); a cluster whose type is no peripheral
    /// type, reported, has neither error.
    fn cluster_entry(&mut self, cluster: &ast::ClusterDecl) -> EntryDef {
        let (ty, judged) = self.cluster_ty(cluster);
        let offset = (
            self.comptime_usize(&cluster.offset, "offset"),
            cluster.offset.span,
        );
        let step = self.entry_step(cluster.step.as_ref());
        if judged {
            match (&cluster.step, is_array(&cluster.ty)) {
                (None, true) => self.diags.push(
                    Diagnostic::error(
                        "resolve::missing-step",
                        format!("the cluster array `{}` has no `step`", cluster.name.name),
                    )
                    .label(cluster.ty.span, "an array of clusters")
                    .label(cluster.offset.span, "`step DISTANCE` goes after the offset")
                    .help("the distance between the elements of a cluster array is written: a peripheral type's extent grows when a register is added at its end, and a distance taken from it would move every element but the first (ADR 0064)"),
                ),
                (Some(step), false) => {
                    self.misplaced_step(step, &cluster.ty, "a cluster", &cluster.name);
                }
                _ => {}
            }
        }
        EntryDef {
            name: cluster.name.name.clone(),
            span: cluster.name.span,
            vis: visibility(cluster.vis),
            kind: EntryKind::Cluster,
            ty,
            ty_span: cluster.ty.span,
            offset,
            step,
            group: (cluster.group.as_ref()).map(|group| (group.name.clone(), group.span)),
        }
    }

    /// An entry's `step`, a `usize` computed at compile time, with the span
    /// of its expression.
    fn entry_step(&mut self, step: Option<&ast::Expr>) -> Option<(ArrayLen, Span)> {
        step.map(|step| (self.comptime_usize(step, "step"), step.span))
    }

    /// `resolve::misplaced-step`: `step` written on `what`, `name`, whose
    /// type `ty` is no array.
    fn misplaced_step(&mut self, step: &ast::Expr, ty: &ast::Ty, what: &str, name: &ast::Ident) {
        self.diags.push(
            Diagnostic::error(
                "resolve::misplaced-step",
                format!("`{}` has a `step` but is not an array", name.name),
            )
            .label(step.span, "a `step`")
            .label(ty.span, format!("{what} that is no array"))
            .help("`step` is the distance between the elements of a register array or a cluster array: remove it, or make the type an array (ADR 0064)"),
        );
    }

    /// The type of a register: a type of memory, or an array of one. A
    /// peripheral type is allowed at the end of the arrays, as it is as the
    /// type of a register, where the type checker rejects it with the
    /// other types that are no register type (ADR 0058, 0064).
    fn register_ty(&mut self, ty: &ast::Ty) -> Ty {
        match &ty.kind {
            ast::TyKind::Array { elem, len } => {
                let elem = self.register_ty(elem);
                let len = self.array_len(len);
                match elem {
                    Ty::Error => Ty::Error,
                    elem => Ty::array_of(elem, len),
                }
            }
            _ => self.memory_ty(ty, NeverPosition::Register),
        }
    }

    /// The type of a cluster (ADR 0064) must be a peripheral type or an
    /// array of one: any other is `resolve::not-a-peripheral-type`, lowered
    /// as `{error}`, and then the entry's only error. One that a register
    /// may have, or an array of one, is most likely a register written
    /// without its access mode, which the help says. Returns the type and
    /// whether it was judged to be one (`{error}` already reported, as for
    /// an unknown name, counts). The array of a peripheral type is not
    /// recorded for the by-value check of the type checker, which a pointee
    /// `*const [P; 2]` still gets: the cluster is where the array is a type.
    fn cluster_ty(&mut self, cluster: &ast::ClusterDecl) -> (Ty, bool) {
        let (elem, len) = match &cluster.ty.kind {
            ast::TyKind::Array { elem, len } => (&**elem, Some(&**len)),
            _ => (&cluster.ty, None),
        };
        let (what, register) = match &elem.kind {
            ast::TyKind::Named(path) => match self.named_ty(path) {
                ty @ (Ty::Peripheral(_) | Ty::Error) => {
                    return (self.cluster_array(ty, len), true);
                }
                Ty::Struct(_) => ("a struct", false),
                Ty::BitStruct(b) => ("a bit-field struct", register_int(b.int)),
                Ty::Enum(e) => ("an enum", !e.payloads && register_int(e.int)),
                Ty::Int(int) => ("a built-in type", register_int(int)),
                _ => ("a built-in type", false),
            },
            ast::TyKind::Applied(path, args) => match self.applied_ty(elem, path, args) {
                Ty::Error => return (self.cluster_array(Ty::Error, len), true),
                _ => ("an enum", false),
            },
            ast::TyKind::Ptr { .. } => ("a pointer type", false),
            ast::TyKind::Array { .. } => ("arrays", false),
            ast::TyKind::Slice(_) => ("a slice type", false),
            ast::TyKind::Never | ast::TyKind::Unit => ("a built-in type", false),
        };
        // Judged bad; the length is still computed, for its own errors.
        self.cluster_array(Ty::Error, len);
        let mut diag = Diagnostic::error(
            "resolve::not-a-peripheral-type",
            format!(
                "the type of the cluster `{}` is not a peripheral type",
                cluster.name.name
            ),
        )
        .label(
            cluster.ty.span,
            if len.is_some() {
                format!("an array of {what}, not of a peripheral type")
            } else {
                format!("{what}, not a peripheral type")
            },
        );
        diag = if register {
            diag.help(format!(
                "an entry with no access mode is a cluster, whose type is a peripheral type; a register starts with its access mode, `read`, `write` or `read write`, as in `read {}: ... at ...` (ADR 0064)",
                cluster.name.name
            ))
        } else {
            diag.help("a cluster places the registers of a peripheral type at an offset: its type is a peripheral type, declared as `peripheral NAME { registers }`, or an array of one (ADR 0064)")
        };
        self.diags.push(diag);
        (Ty::Error, false)
    }

    /// `elem`, or the array of `len` elements of it when the cluster's type
    /// is written as one: `{error}` if `elem` is.
    fn cluster_array(&mut self, elem: Ty, len: Option<&ast::Expr>) -> Ty {
        let Some(len) = len else {
            return elem;
        };
        let len = self.array_len(len);
        match elem {
            Ty::Error => Ty::Error,
            elem => Ty::array_of(elem, len),
        }
    }

    /// A peripheral (ADR 0058): it takes no attributes, its type names a
    /// peripheral type, and its address is a `usize` computed at compile
    /// time in its module.
    pub(super) fn peripheral_decl(
        &mut self,
        decl: &ast::PeripheralDecl,
        id: PeripheralId,
    ) -> PeripheralDef {
        self.no_attrs(&decl.attrs, "a peripheral");
        let ty = self.peripheral_of(decl);
        let address = (
            self.comptime_usize(&decl.address, "address"),
            decl.address.span,
        );
        let over = (decl.over.as_ref()).and_then(|path| self.over_of(path, id));
        PeripheralDef {
            path: self.path_of(self.module, &decl.name.name),
            name_span: decl.name.span,
            module: self.module,
            vis: visibility(decl.vis),
            ty,
            ty_span: decl.ty.span,
            address,
            over,
        }
    }

    /// The peripheral `path` names after `over` (ADR 0066), which must be
    /// one other than the peripheral `id` being declared. A name of nothing
    /// is `resolve::unknown-name`, as where a value is named; of anything
    /// else `resolve::not-a-peripheral`; `id` itself `resolve::over-itself`.
    fn over_of(&mut self, path: &ast::Path, id: PeripheralId) -> Option<(PeripheralId, Span)> {
        let not_a_peripheral = |what: &str| {
            Diagnostic::error(
                "resolve::not-a-peripheral",
                format!("`{}` is not a peripheral", written(path)),
            )
            .label(path.span, format!("{what}, not a peripheral"))
            .help("`over` names the peripheral whose device this one is another view of, as in `peripheral RNG: Rng at 0x6002_6000 over APB_CTRL;` (ADR 0066)")
        };
        let diag = match self.lookup_path(path) {
            Named::Item(Def::Peripheral(over)) if over == id => Diagnostic::error(
                "resolve::over-itself",
                format!("the peripheral `{}` is declared over itself", written(path)),
            )
            .label(path.span, "names the peripheral being declared")
            .help("`over` names another peripheral, whose device this one is another view of; a peripheral is not checked for overlap with itself (ADR 0066)"),
            Named::Item(Def::Peripheral(over)) => return Some((over, path.span)),
            Named::Failed => return None,
            Named::Item(Def::Module(_)) => {
                self.misused_module(path, "a peripheral");
                return None;
            }
            Named::Item(def) => not_a_peripheral(def.what()),
            Named::Variant(..) => not_a_peripheral("a variant"),
            Named::Missing(module) => {
                let message = if path.segments.len() == 1 {
                    format!("cannot find `{}` in this scope", written(path))
                } else {
                    format!("cannot find `{}`", written(path))
                };
                let diag = Diagnostic::error("resolve::unknown-name", message);
                self.missing(diag, path, module, "not found")
            }
        };
        self.diags.push(diag);
        None
    }

    /// The type of the peripheral `decl`: a peripheral type, or `{error}`
    /// once reported. A path that names no type, or a type written with
    /// the wrong type arguments, is reported as anywhere; any other type is
    /// `resolve::not-a-peripheral-type`. The parts of a pointer, array or
    /// slice type are not lowered: whatever they are, the type is no
    /// peripheral type.
    fn peripheral_of(&mut self, decl: &ast::PeripheralDecl) -> Ty {
        let what = match &decl.ty.kind {
            ast::TyKind::Named(path) => match self.named_ty(path) {
                ty @ (Ty::Peripheral(_) | Ty::Error) => return ty,
                Ty::Struct(_) => "a struct",
                Ty::BitStruct(_) => "a bit-field struct",
                Ty::Enum(_) => "an enum",
                _ => "a built-in type",
            },
            ast::TyKind::Applied(path, args) => match self.applied_ty(&decl.ty, path, args) {
                Ty::Error => return Ty::Error,
                _ => "an enum",
            },
            ast::TyKind::Ptr { .. } => "a pointer type",
            ast::TyKind::Array { .. } => "an array type",
            ast::TyKind::Slice(_) => "a slice type",
            ast::TyKind::Never | ast::TyKind::Unit => "a built-in type",
        };
        self.diags.push(
            Diagnostic::error(
                "resolve::not-a-peripheral-type",
                format!(
                    "the type of the peripheral `{}` is not a peripheral type",
                    self.path_of(self.module, &decl.name.name)
                ),
            )
            .label(decl.ty.span, format!("{what}, not a peripheral type"))
            .help("a peripheral places a device's registers at an address: its type is declared as `peripheral NAME { registers }`, and a pointer to one is written in code, as `*mut NAME` (ADR 0058)"),
        );
        Ty::Error
    }
}

/// Whether the written type `ty` is an array.
fn is_array(ty: &ast::Ty) -> bool {
    matches!(ty.kind, ast::TyKind::Array { .. })
}

/// Whether a register may be an integer of type `int` (ADR 0058).
fn register_int(int: IntTy) -> bool {
    matches!(int, IntTy::U8 | IntTy::U16 | IntTy::U32)
}

/// The program's memory map, out of its `memory` blocks, each with its
/// span and its map if it resolved (ADR 0057): the first block in the order
/// of files, then of positions (ADR 0049), which is not the order modules
/// are gathered in, since a `mod` before a block reaches a later file. Each
/// other block is a `resolve::duplicate-memory`.
pub(super) fn memory_map(
    mut maps: Vec<(Span, Option<MemoryMap>)>,
    diags: &mut Diagnostics,
) -> Option<MemoryMap> {
    maps.sort_by_key(|(span, _)| (span.file, span.lo));
    let mut maps = maps.into_iter();
    let (first, map) = maps.next()?;
    for (again, _) in maps {
        diags.push(
            Diagnostic::error(
                "resolve::duplicate-memory",
                "the program has more than one `memory` block",
            )
            .label(memory_word(again), "another `memory` block")
            .label(memory_word(first), "the first one")
            .help("a program has one memory map, whose `memory` block may sit in any module (ADR 0057)"),
        );
    }
    map
}

/// The word `memory` that starts the block spanning `block`, where the
/// errors about the whole block point.
fn memory_word(block: Span) -> Span {
    let lo = block.lo as usize;
    Span::new(block.file, lo, lo + "memory".len())
}

/// The words a `memory` block places, each with the placement a block
/// missing it is told to add.
const PLACEMENTS: [(&str, &str); 3] = [
    ("code", "code in REGION;"),
    ("data", "data in REGION;"),
    ("stack", "stack in REGION { size: SIZE }"),
];
