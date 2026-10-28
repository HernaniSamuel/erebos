//! Lowering `memory` blocks (ADR 0057) and peripherals (ADR 0058).

use std::collections::HashMap;

use crate::diag::{Diagnostic, Diagnostics};
use crate::hir::{
    IntTy, MemoryMap, NeverPosition, PeripheralDef, PeripheralTypeDef, Placement, RegionDef,
    RegisterDef, Ty,
};
use crate::span::Span;
use crate::syntax::ast;

use super::{Lowerer, visibility};

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

    /// A peripheral type (ADR 0058): it takes no attributes, and each
    /// register name is declared once; a register's type is a type of
    /// memory, and its offset a `usize` computed at compile time in the
    /// type's module. Its read effect and its group are carried as written
    /// (ADR 0060, 0061).
    pub(super) fn peripheral_type_decl(
        &mut self,
        decl: &ast::PeripheralTypeDecl,
    ) -> PeripheralTypeDef {
        self.no_attrs(&decl.attrs, "a peripheral type");
        let mut seen: HashMap<&str, Span> = HashMap::new();
        // Clusters are checked for their type only; step 2 gives them a place
        // in the HIR, with their steps, and counts them in the names.
        let mut registers_decl = Vec::new();
        for entry in &decl.entries {
            match entry {
                ast::EntryDecl::Register(register) => registers_decl.push(register),
                ast::EntryDecl::Cluster(cluster) => self.cluster_ty(cluster),
            }
        }
        for &register in &registers_decl {
            if let Some(&first) = seen.get(register.name.name.as_str()) {
                self.diags.push(
                    Diagnostic::error(
                        "resolve::duplicate-register",
                        format!(
                            "the register `{}` is declared more than once",
                            register.name.name
                        ),
                    )
                    .label(register.name.span, "declared again here")
                    .label(first, "first declared here")
                    .help("each register of a peripheral type has a name of its own; two registers at one offset, as a `read` one and a `write` one, take two names (ADR 0058)"),
                );
            }
            seen.entry(&register.name.name)
                .or_insert(register.name.span);
        }
        let registers = registers_decl
            .iter()
            .map(|&register| RegisterDef {
                name: register.name.name.clone(),
                span: register.name.span,
                vis: visibility(register.vis),
                access: register.access,
                ty: self.memory_ty(&register.ty, NeverPosition::Register),
                ty_span: register.ty.span,
                offset: (
                    self.comptime_usize(&register.offset, "offset"),
                    register.offset.span,
                ),
                read_effect: register.read_effect,
                // A label, resolved nowhere (ADR 0061).
                group: (register.group.as_ref()).map(|group| (group.name.clone(), group.span)),
            })
            .collect();
        PeripheralTypeDef {
            path: self.path_of(self.module, &decl.name.name),
            name_span: decl.name.span,
            module: self.module,
            vis: visibility(decl.vis),
            // Filled in once every `use` is resolved, by `reachability`.
            reachable: Vec::new(),
            registers,
        }
    }

    /// The type of a cluster (ADR 0064) must be a peripheral type or an
    /// array of one: any other is `resolve::not-a-peripheral-type`. One
    /// that a register may have, or an array of one, is most likely a
    /// register written without its access mode, which the help says. Its
    /// parts, the array lengths, the offset, the step and the group, are
    /// lowered by step 2; a cluster of a peripheral type is skipped here.
    fn cluster_ty(&mut self, cluster: &ast::ClusterDecl) {
        let mut inner = &cluster.ty;
        while let ast::TyKind::Array { elem, .. } = &inner.kind {
            inner = elem;
        }
        let (what, register) = match &inner.kind {
            ast::TyKind::Named(path) => match self.named_ty(path) {
                Ty::Peripheral(_) | Ty::Error => return,
                Ty::Struct(_) => ("a struct", false),
                Ty::BitStruct(b) => ("a bit-field struct", register_int(b.int)),
                Ty::Enum(e) => ("an enum", !e.payloads && register_int(e.int)),
                Ty::Int(int) => ("a built-in type", register_int(int)),
                _ => ("a built-in type", false),
            },
            ast::TyKind::Applied(path, args) => match self.applied_ty(inner, path, args) {
                Ty::Error => return,
                _ => ("an enum", false),
            },
            ast::TyKind::Ptr { .. } => ("a pointer type", false),
            // Arrays were stripped above; only a slice is left.
            ast::TyKind::Array { .. } | ast::TyKind::Slice(_) => ("a slice type", false),
            ast::TyKind::Never | ast::TyKind::Unit => ("a built-in type", false),
        };
        let mut diag = Diagnostic::error(
            "resolve::not-a-peripheral-type",
            format!(
                "the type of the cluster `{}` is not a peripheral type",
                cluster.name.name
            ),
        )
        .label(
            cluster.ty.span,
            if matches!(cluster.ty.kind, ast::TyKind::Array { .. }) {
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
    }

    /// A peripheral (ADR 0058): it takes no attributes, its type names a
    /// peripheral type, and its address is a `usize` computed at compile
    /// time in its module.
    pub(super) fn peripheral_decl(&mut self, decl: &ast::PeripheralDecl) -> PeripheralDef {
        self.no_attrs(&decl.attrs, "a peripheral");
        let ty = self.peripheral_of(decl);
        PeripheralDef {
            path: self.path_of(self.module, &decl.name.name),
            name_span: decl.name.span,
            module: self.module,
            vis: visibility(decl.vis),
            ty,
            ty_span: decl.ty.span,
            address: (
                self.comptime_usize(&decl.address, "address"),
                decl.address.span,
            ),
            // Step 2 resolves `over` (decl.over), with its errors.
        }
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
