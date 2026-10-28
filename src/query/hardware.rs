//! The memory map (ADR 0057) and peripherals (ADR 0058): their checks,
//! layouts and placement.

use crate::diag::Diagnostic;
use crate::hir::{self, AccessMode, PeripheralId, PeripheralTypeId};
use crate::layout::{self, Composite, EntryPlace, PAST_ADDRESS_SPACE, PeripheralLayout};
use crate::memory::{MemoryMap, Region};
use crate::span::Span;
use crate::target;

use super::{Item, PeripheralFailures, Queries, State};

impl Queries<'_> {
    /// Checks the values of the program's `memory` block, if any
    /// (ADR 0057): no region empty or ending at 2³² or past it, no two
    /// sharing an address, the code's region starting at a multiple of the
    /// code alignment, and the stack's size a non-zero multiple of the
    /// stack alignment that fits in its region from its origin rounded up
    /// to that alignment. A check that needs a value that failed, which was
    /// reported, is skipped.
    pub(super) fn check_memory_map(&mut self) {
        let hir = self.hir;
        let Some(map) = &hir.memory else {
            return;
        };
        // Each region's origin and size, `None` where it failed, and its
        // range where both were computed.
        let values: Vec<(Option<u64>, Option<u64>)> = map
            .regions
            .iter()
            .map(|region| {
                (
                    self.array_len(region.origin.0),
                    self.array_len(region.size.0),
                )
            })
            .collect();
        let ranges: Vec<Option<(u64, u64)>> = values
            .iter()
            .map(|&(origin, size)| Some((origin?, origin? + size?)))
            .collect();

        let limit = 1u64 << target::POINTER_BITS;
        for ((region, &(_, size)), &range) in map.regions.iter().zip(&values).zip(&ranges) {
            if size == Some(0) {
                self.diags.push(
                    Diagnostic::error(
                        "ty::empty-region",
                        format!("the region `{}` is empty", region.name),
                    )
                    .label(region.size.1, "a size of 0")
                    .help("a region covers `[origin, origin + size)`: give it at least one byte (ADR 0057)"),
                );
            }
            if let Some((_, end)) = range
                && end >= limit
            {
                self.diags.push(
                    Diagnostic::error(
                        "ty::region-out-of-range",
                        format!("the region `{}` ends past the address space", region.name),
                    )
                    .label(region.name_span, format!("ends at {end:#x}"))
                    .help(format!(
                        "a region covers `[origin, origin + size)`, whose end must be below 2³² ({limit:#x}) so that the address one past it exists (ADR 0057)"
                    )),
                );
            }
        }

        // Once per pair, at the later region (ADR 0057).
        for (later, &later_range) in ranges.iter().enumerate() {
            let Some((later_lo, later_hi)) = later_range else {
                continue;
            };
            for (earlier, &earlier_range) in ranges[..later].iter().enumerate() {
                let Some((earlier_lo, earlier_hi)) = earlier_range else {
                    continue;
                };
                let lo = later_lo.max(earlier_lo);
                let hi = later_hi.min(earlier_hi);
                if lo >= hi {
                    continue;
                }
                let (later, earlier) = (&map.regions[later], &map.regions[earlier]);
                let shared = shared_bytes(lo.into(), hi.into());
                self.diags.push(
                    Diagnostic::error(
                        "ty::region-overlap",
                        format!(
                            "the regions `{}` and `{}` overlap",
                            earlier.name, later.name
                        ),
                    )
                    .label(
                        later.name_span,
                        format!("covers [{later_lo:#x}, {later_hi:#x})"),
                    )
                    .label(
                        earlier.name_span,
                        format!("covers [{earlier_lo:#x}, {earlier_hi:#x})"),
                    )
                    .help(format!(
                        "they share {shared}; the regions of a memory map are disjoint (ADR 0057)"
                    )),
                );
            }
        }

        let code = &map.regions[map.code.region];
        if let (Some(origin), _) = values[map.code.region]
            && origin % target::CODE_ALIGN != 0
        {
            self.diags.push(
                Diagnostic::error(
                    "ty::misaligned-code",
                    format!(
                        "the code's region `{}` starts at a misaligned address",
                        code.name
                    ),
                )
                .label(
                    code.origin.1,
                    format!("{origin:#x} is not a multiple of {}", target::CODE_ALIGN),
                )
                .label(
                    map.code.span,
                    format!("places the code, from the reset entry on, in `{}`", code.name),
                )
                .help(format!(
                    "the reset entry is at the origin of the code's region, which must therefore be a multiple of the target's code alignment, {} bytes (ADR 0057)",
                    target::CODE_ALIGN
                )),
            );
        }

        let (Some(size), size_span) = (self.array_len(map.stack_size.0), map.stack_size.1) else {
            return;
        };
        let align = target::STACK_ALIGN;
        let alignment_help = format!(
            "the stack's size is a non-zero multiple of the target's stack alignment, {align} bytes (ADR 0057)"
        );
        if size == 0 {
            self.diags.push(
                Diagnostic::error("ty::stack-size", "the stack has a size of 0")
                    .label(size_span, "a stack of 0 bytes")
                    .help(alignment_help),
            );
        } else if size % align != 0 {
            self.diags.push(
                Diagnostic::error(
                    "ty::stack-size",
                    format!("the stack's size, {size}, is not a multiple of {align}"),
                )
                .label(size_span, format!("not a multiple of {align}"))
                .help(alignment_help),
            );
        } else if let Some((origin, end)) = ranges[map.stack.region] {
            let start = origin.next_multiple_of(align);
            if start + size > end {
                let region = &map.regions[map.stack.region];
                self.diags.push(
                    Diagnostic::error(
                        "ty::stack-size",
                        format!("the stack does not fit in the region `{}`", region.name),
                    )
                    .label(
                        size_span,
                        format!(
                            "{size} bytes from {start:#x} end at {:#x}",
                            start + size
                        ),
                    )
                    .label(region.name_span, format!("ends at {end:#x}"))
                    .help(format!(
                        "the stack starts at its region's origin rounded up to the target's stack alignment, {align} bytes, and ends within the region (ADR 0057)"
                    )),
                );
            }
        }
    }

    /// The program's memory map with its values computed (ADR 0057);
    /// `None` without a `memory` block, or when one of its values failed,
    /// which was reported.
    pub fn memory_map(&mut self) -> Option<MemoryMap> {
        let hir = self.hir;
        let map = hir.memory.as_ref()?;
        let regions = map
            .regions
            .iter()
            .map(|region| {
                Some(Region {
                    name: region.name.clone(),
                    origin: self.array_len(region.origin.0)?,
                    size: self.array_len(region.size.0)?,
                    span: region.name_span,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(MemoryMap {
            regions,
            code: map.code.region,
            data: map.data.region,
            stack: map.stack.region,
            stack_size: self.array_len(map.stack_size.0)?,
        })
    }

    /// The layout of a peripheral type (ADR 0058, 0064), computed once from
    /// its entries' offsets, lengths, steps and types; `None` when it
    /// reaches a failed value: an offset, a step or a length that failed, a
    /// type that is in error or no register type, a cluster whose type has
    /// no layout, or a type that contains itself through clusters, all
    /// reported (by `check_peripherals` for the register types).
    pub fn peripheral_layout(&mut self, id: PeripheralTypeId) -> Option<PeripheralLayout> {
        if let State::Done(layout) = &self.peripheral_layouts[id.0 as usize] {
            return layout.clone();
        }
        let hir = self.hir;
        let def = hir.peripheral_type(id);
        let what = format!("the layout of `{}`", def.path);
        if !self.start(Item::PeripheralLayout(id), def.name_span, what) {
            return None;
        }
        self.peripheral_layouts[id.0 as usize] = State::InProgress;
        let mut places = Vec::with_capacity(def.entries.len());
        let mut failed = false;
        // Every entry is looked at, so that the layout of every cluster's
        // type is asked for, and a cycle through them found.
        for entry in &def.entries {
            match self.entry_geometry(entry).place() {
                Some(place) => places.push(place),
                None => failed = true,
            }
        }
        self.finish(Item::PeripheralLayout(id));
        let layout = (!failed).then(|| layout::peripheral_layout(&places));
        self.peripheral_layouts[id.0 as usize] = State::Done(layout.clone());
        // Its MIR type, made while the layout was computed, gets it now.
        if let (Some(p), Some(layout)) = (self.mir_peripheral_types[id.0 as usize], &layout) {
            p.def().set_layout(layout.clone());
        }
        layout
    }

    /// The values of an entry of a peripheral type, computed (ADR 0064): its
    /// offset, length and step, the type of its elements, and what one
    /// takes. A value that failed was reported. Asks for the layout of a
    /// cluster's type.
    fn entry_geometry(&mut self, entry: &hir::EntryDef) -> Geometry {
        let (elem, len) = split_entry_ty(entry.ty);
        let len = len.map(|len| self.array_len(len));
        let offset = self.array_len(entry.offset.0);
        // Written on an entry that is no array, a step was reported, and is
        // not looked at.
        let step = match (len, entry.step) {
            (Some(_), Some((step, _))) => Some(self.array_len(step)),
            _ => None,
        };
        let elem = self.normalize(elem);
        let unit = match (entry.is_cluster(), elem) {
            (true, hir::Ty::Peripheral(p)) => {
                (self.peripheral_layout(p.id)).map(|layout| (layout.extent, layout.align))
            }
            (true, _) => None,
            (false, elem) => layout::register_width(elem).map(|width| (width, width)),
        };
        Geometry {
            cluster: entry.is_cluster(),
            offset,
            len,
            step,
            elem,
            unit,
        }
    }

    /// Whether the peripheral type `id` reaches a failed value (ADR 0058).
    /// One whose layout is being computed is asked for again, which reports
    /// `ty::cycle`, as a struct's is (ADR 0028): an offset whose computation
    /// names the type, as through a pointer to it, needs itself, whichever
    /// of the layout and the offset is computed first.
    pub(super) fn peripheral_type_failed(&mut self, id: PeripheralTypeId) -> bool {
        self.peripheral_layout(id).is_none()
    }

    /// Whether the failure of the peripheral type `id` is explained by an
    /// error reported elsewhere, so that a register type reaching it stays
    /// silent, as a type reaching a failed value does (ADR 0058, as settled
    /// in M3.2 step 3).
    pub(super) fn peripheral_explained(&mut self, id: PeripheralTypeId) -> bool {
        if self.peripherals_explained.is_none() {
            self.explain_peripherals();
        }
        let explained = self.peripherals_explained.as_ref();
        explained.expect("computed above")[id.0 as usize]
    }

    /// Finds which peripheral types have an explained failure: those with a
    /// base failure (`peripheral_base_failure`), and those with a register
    /// whose type, no register type, reaches one already explained, so
    /// that the silence chains as far as the failure reaches. The least
    /// fixed point over every declaration: types failing only through
    /// registers that point to each other, with no base failure among
    /// them, are not explained, and those registers are reported, since
    /// their failure is their own. The result does not depend on the
    /// order the types are looked at in.
    fn explain_peripherals(&mut self) {
        let hir = self.hir;
        let base = (0..hir.peripheral_types.len())
            .map(|id| self.peripheral_base_failure(PeripheralTypeId(id as u32)))
            .collect();
        self.peripherals_explained = Some(base);
        // Each round marks at least one more type, or ends.
        loop {
            let mut changed = false;
            for (id, def) in hir.peripheral_types.iter().enumerate() {
                if self.peripherals_explained.as_ref().expect("set above")[id] {
                    continue;
                }
                let reaches = def.entries.iter().any(|entry| {
                    let elem = self.normalize(split_entry_ty(entry.ty).0);
                    (entry.is_cluster() || layout::register_width(elem).is_none())
                        && self.lacks_layout(elem, &mut Vec::new(), PeripheralFailures::Explained)
                });
                if reaches {
                    self.peripherals_explained.as_mut().expect("set above")[id] = true;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    /// Whether the peripheral type `id` has a base failure: it contains
    /// itself through clusters (`ty::infinite-size`), or an entry has an
    /// offset, a length or a step that failed, or is a cluster array without
    /// its step, or a cluster whose type is in error, or an element type in
    /// error without a peripheral type, which is in error in itself or is no
    /// register type and mentions no peripheral type. Its error is reported
    /// whatever points to the type. A failure owed only to registers that
    /// mention peripheral types may be one such register's own, and is no
    /// base failure. It depends only on the type's own declaration, and on
    /// whether it is in a cycle.
    fn peripheral_base_failure(&mut self, id: PeripheralTypeId) -> bool {
        let hir = self.hir;
        if self.in_cluster_cycle(id) {
            return true;
        }
        hir.peripheral_type(id).entries.iter().any(|entry| {
            let (elem, len) = split_entry_ty(entry.ty);
            if self.array_len(entry.offset.0).is_none() {
                return true;
            }
            if let Some(len) = len {
                let step_failed = match entry.step {
                    Some((step, _)) => self.array_len(step).is_none(),
                    // A cluster array without `step` was reported.
                    None => entry.is_cluster(),
                };
                if self.array_len(len).is_none() || step_failed {
                    return true;
                }
            }
            let elem = self.normalize(elem);
            if entry.is_cluster() {
                return !matches!(elem, hir::Ty::Peripheral(_));
            }
            layout::register_width(elem).is_none()
                && (self.lacks_layout(elem, &mut Vec::new(), PeripheralFailures::Ignored)
                    || !self.mentions_peripheral(elem, &mut Vec::new()))
        })
    }

    /// Whether the peripheral type `id` contains itself through clusters,
    /// directly or through other types (ADR 0064). Found from the
    /// declarations alone: whatever the lengths and steps are, a cluster
    /// holds its type.
    pub(super) fn in_cluster_cycle(&self, id: PeripheralTypeId) -> bool {
        let hir = self.hir;
        let mut seen = vec![false; hir.peripheral_types.len()];
        let mut pending = vec![id];
        while let Some(at) = pending.pop() {
            for entry in &hir.peripheral_type(at).entries {
                if let (true, hir::Ty::Peripheral(p)) =
                    (entry.is_cluster(), split_entry_ty(entry.ty).0)
                {
                    if p.id == id {
                        return true;
                    }
                    if !std::mem::replace(&mut seen[p.id.0 as usize], true) {
                        pending.push(p.id);
                    }
                }
            }
        }
        false
    }

    /// Reports peripheral types that contain each other through clusters,
    /// found while laying them out, the first being the one found again
    /// (ADR 0064). Every type of the cycle has no layout.
    pub(super) fn report_peripheral_cycle(&mut self, cycle: &[PeripheralTypeId]) {
        let hir = self.hir;
        let def = |id: &PeripheralTypeId| hir.peripheral_type(*id);
        let first = def(&cycle[0]);
        let message = if cycle.len() == 1 {
            format!("`{}` contains itself", first.path)
        } else {
            let mut path: Vec<&str> = cycle.iter().map(|id| def(id).path.as_str()).collect();
            path.push(&first.path);
            format!(
                "these peripheral types contain each other: {}",
                path.join(" -> ")
            )
        };
        let mut diag = Diagnostic::error("ty::infinite-size", message)
            .label(first.name_span, "would have infinite size")
            .help("a cluster places the registers of its type inside this one, so a type cannot hold itself: remove a cluster of the cycle, since a register cannot hold a pointer to its own type (ADR 0064)");
        for other in &cycle[1..] {
            diag = diag.label(def(other).name_span, "part of the cycle");
        }
        self.diags.push(diag);
    }

    /// Whether `ty` contains a peripheral type anywhere: behind pointers,
    /// in elements, fields, payload values and type arguments; the types
    /// in `seen` were looked at already.
    fn mentions_peripheral(&mut self, ty: hir::Ty, seen: &mut Vec<Composite>) -> bool {
        match ty {
            hir::Ty::Peripheral(_) => true,
            hir::Ty::Ptr(ptr) => self.mentions_peripheral(*ptr.pointee, seen),
            hir::Ty::Array(elem, _) | hir::Ty::Slice(elem) => self.mentions_peripheral(*elem, seen),
            hir::Ty::Struct(s) => {
                if seen.contains(&Composite::Struct(s.id)) {
                    return false;
                }
                seen.push(Composite::Struct(s.id));
                let hir = self.hir;
                (hir.struct_(s.id).fields.iter())
                    .any(|field| self.mentions_peripheral(field.ty, seen))
            }
            hir::Ty::Enum(e) if e.payloads => {
                if seen.contains(&Composite::Enum(e.id)) {
                    return false;
                }
                seen.push(Composite::Enum(e.id));
                (self.enum_def(e.id).variants.iter())
                    .flat_map(|variant| &variant.payload)
                    .any(|value| self.mentions_peripheral(value.ty, seen))
            }
            hir::Ty::Applied(applied) => {
                (applied.args.iter()).any(|&arg| self.mentions_peripheral(arg, seen))
            }
            _ => false,
        }
    }

    /// Whether the peripheral `id` is erroneous (ADR 0058): its type is no
    /// peripheral type or reaches a failed value, or its address failed,
    /// all reported.
    pub(super) fn peripheral_erroneous(&mut self, id: PeripheralId) -> bool {
        let def = self.hir.peripheral(id);
        let type_failed = match def.ty {
            hir::Ty::Peripheral(ty) => self.peripheral_type_failed(ty.id),
            _ => true,
        };
        type_failed || self.array_len(def.address.0).is_none()
    }

    /// The address of the peripheral `id` and the layout of its type;
    /// `None` when it is erroneous (ADR 0058).
    pub fn placed_peripheral(&mut self, id: PeripheralId) -> Option<(u64, PeripheralLayout)> {
        let def = self.hir.peripheral(id);
        let hir::Ty::Peripheral(ty) = def.ty else {
            return None;
        };
        let address = self.array_len(def.address.0)?;
        Some((address, self.peripheral_layout(ty.id)?))
    }

    /// Whether the reads of the register `index` of the peripheral type `id`
    /// have effects (ADR 0060): it declares a read effect, or its type does
    /// (`type_read_effect`). `Some` with the span of the effect that makes
    /// them so, the register's own first. The reads of a `write` register
    /// never count, since it is never read: its read effect only documents
    /// the device.
    pub fn register_read_effect(&mut self, id: PeripheralTypeId, index: usize) -> Option<Span> {
        let hir = self.hir;
        let register = &hir.peripheral_type(id).entries[index];
        if register.access() == Some(AccessMode::Write) {
            return None;
        }
        let ty = self.normalize(register.ty);
        (register.read_effect().map(|(_, span)| span)).or_else(|| self.type_read_effect(ty))
    }

    /// Whether a read of a volatile place of type `ty`, normalized, has
    /// effects (ADR 0060): `ty` is a bit-field struct with a field that
    /// declares a read effect. `Some` with the span of the first such
    /// effect. In ordinary memory effects mean nothing, so this is asked
    /// only for volatile places.
    pub fn type_read_effect(&self, ty: hir::Ty) -> Option<Span> {
        let hir::Ty::BitStruct(s) = ty else {
            return None;
        };
        (self.hir.bit_struct(s.id).fields.iter())
            .find_map(|field| field.read_effect.map(|(_, span)| span))
    }

    /// Checks every peripheral type and peripheral of the program, used or
    /// not, once their values are computed (ADR 0058): registers of a
    /// register type, at offsets aligned for their widths, sharing no byte
    /// unless one is `read` and the other `write`; peripherals at addresses
    /// aligned for their types, ending at 2³² or below, sharing no byte. A
    /// check that needs a value that failed, which was reported, is
    /// skipped; a register whose type is wrong takes part in no other
    /// check, and a peripheral of a type that reaches a failed value in
    /// none.
    pub(super) fn check_peripherals(&mut self) {
        let hir = self.hir;
        for id in 0..hir.peripheral_types.len() {
            self.check_peripheral_type(PeripheralTypeId(id as u32));
        }

        let placed: Vec<Option<(u64, PeripheralLayout)>> = (0..hir.peripherals.len())
            .map(|id| self.placed_peripheral(PeripheralId(id as u32)))
            .collect();
        let limit = 1u64 << target::POINTER_BITS;
        for (def, placed) in hir.peripherals.iter().zip(&placed) {
            let Some((address, layout)) = placed else {
                continue;
            };
            if address % layout.align != 0 {
                self.diags.push(
                    Diagnostic::error(
                        "ty::misaligned-peripheral",
                        format!("the peripheral `{}` is at a misaligned address", def.path),
                    )
                    .label(
                        def.address.1,
                        format!("{address:#x} is not a multiple of {}", layout.align),
                    )
                    .label(
                        def.ty_span,
                        format!("`{}` has alignment {}", def.ty, layout.align),
                    )
                    .help("a peripheral's address is a multiple of its type's alignment, the width of its widest register, so that every register is accessed aligned (ADR 0058)"),
                );
            }
            let end = address + layout.extent;
            if end > limit {
                // An extent past 2³² is saturated (ADR 0064), and shown so.
                let covers = if layout.extent >= PAST_ADDRESS_SPACE {
                    format!("covers [{address:#x}, past 2³²)")
                } else {
                    format!("covers [{address:#x}, {end:#x})")
                };
                self.diags.push(
                    Diagnostic::error(
                        "ty::peripheral-out-of-range",
                        format!("the peripheral `{}` ends past the address space", def.path),
                    )
                    .label(def.name_span, covers)
                    .help(format!(
                        "a peripheral covers `[address, address + extent)`, its type's extent being the end of the last byte of its entries, and ends at 2³² ({limit:#x}) or below (ADR 0058, 0064)"
                    )),
                );
            }
        }

        // Once per pair, at the later peripheral: in the order of files,
        // numbered depth first, then of positions in a file (ADR 0057,
        // 0058), which is not the order peripherals are gathered in.
        let ranges: Vec<Option<(u64, u64)>> = (placed.iter())
            .map(|placed| {
                let (address, layout) = placed.as_ref()?;
                Some((*address, address + layout.extent))
            })
            .collect();
        let mut order: Vec<usize> = (0..hir.peripherals.len()).collect();
        order.sort_by_key(|&i| {
            let span = hir.peripherals[i].name_span;
            (span.file, span.lo)
        });
        for (k, &later) in order.iter().enumerate() {
            let Some((later_lo, later_hi)) = ranges[later] else {
                continue;
            };
            for &earlier in &order[..k] {
                let Some((earlier_lo, earlier_hi)) = ranges[earlier] else {
                    continue;
                };
                let lo = later_lo.max(earlier_lo);
                let hi = later_hi.min(earlier_hi);
                if lo >= hi {
                    continue;
                }
                let (later, earlier) = (&hir.peripherals[later], &hir.peripherals[earlier]);
                self.diags.push(
                    Diagnostic::error(
                        "ty::peripheral-overlap",
                        format!(
                            "the peripherals `{}` and `{}` overlap",
                            earlier.path, later.path
                        ),
                    )
                    .label(
                        later.name_span,
                        format!("covers [{later_lo:#x}, {})", range_end(later_lo, later_hi)),
                    )
                    .label(
                        earlier.name_span,
                        format!(
                            "covers [{earlier_lo:#x}, {})",
                            range_end(earlier_lo, earlier_hi)
                        ),
                    )
                    .help(format!(
                        "they share {}; two peripherals cannot share a byte (ADR 0058)",
                        if saturated(later_lo, later_hi) || saturated(earlier_lo, earlier_hi) {
                            format!("the bytes from {lo:#x} on")
                        } else {
                            shared_bytes(lo.into(), hi.into())
                        }
                    )),
                );
            }
        }
    }

    /// Checks the entries of the peripheral type `id` (ADR 0058, 0064): the
    /// types of registers, the alignment of offsets and steps, the steps
    /// against the size of the elements, and the overlaps, once per pair of
    /// one alternate group or both of none (ADR 0061), at the later entry.
    fn check_peripheral_type(&mut self, id: PeripheralTypeId) {
        let hir = self.hir;
        let def = hir.peripheral_type(id);
        // Asks for the layout, which reports a type that contains itself.
        self.peripheral_layout(id);
        // Each entry's values, `None` in `placed` where one failed or the
        // type is wrong, all reported.
        let mut placed: Vec<Option<EntryPlace>> = Vec::with_capacity(def.entries.len());
        for entry in &def.entries {
            let geometry = self.entry_geometry(entry);
            let elem = geometry.elem;
            let array = geometry.len.is_some();
            // A type in error was reported, and so was what explains the
            // failure of a peripheral type it reaches. One reaching only
            // peripheral types whose failure is not explained is reported
            // here: their failure may be this very register, or registers
            // that point back to its type.
            if !entry.is_cluster()
                && layout::register_width(elem).is_none()
                && !self.lacks_layout(elem, &mut Vec::new(), PeripheralFailures::Explained)
            {
                let what = match elem {
                    hir::Ty::BitStruct(s) => format!(", a bit-field struct on `{}`", s.int),
                    hir::Ty::Enum(e) if e.payloads => ", an enum with payloads".to_owned(),
                    hir::Ty::Enum(e) => format!(", an enum on `{}`", e.int),
                    _ => String::new(),
                };
                let subject = if array {
                    format!("the element type of the register array `{}`", entry.name)
                } else {
                    format!("the type of the register `{}`", entry.name)
                };
                let help = match elem {
                    hir::Ty::Array(..) => {
                        "a register repeated in two dimensions is a register array in a cluster array: declare the inner array in a peripheral type of its own, and the outer one as a cluster array of it, with the distance between its elements as its `step` (ADR 0064)"
                    }
                    hir::Ty::Peripheral(_) => {
                        "a cluster has no access mode: write `NAME: Type at OFFSET`, with a peripheral type, without `read` or `write`; a register is a `u8`, a `u16` or a `u32`, or a bit-field struct or an enum without payloads on one of them (ADR 0058, 0060, 0064)"
                    }
                    _ => {
                        "a register is a `u8`, a `u16` or a `u32`, or a bit-field struct or an enum without payloads on one of them: as wide as that integer type, and read and written in one access (ADR 0058, 0060)"
                    }
                };
                self.diags.push(
                    Diagnostic::error(
                        "ty::register-type",
                        format!("`{elem}` is not a register type"),
                    )
                    .label(entry.ty_span, format!("{subject}{what}"))
                    .help(help),
                );
            }
            placed.push(geometry.place());

            // The offset and the step against what an element needs.
            let Some((size, align)) = geometry.unit else {
                continue;
            };
            let what = if entry.is_cluster() {
                "cluster"
            } else {
                "register"
            };
            let needs = if entry.is_cluster() {
                format!("`{elem}` has alignment {align}")
            } else {
                format!("`{elem}` is {size} bytes wide")
            };
            if let Some(offset) = geometry.offset
                && offset % align != 0
            {
                let help = if entry.is_cluster() {
                    "a cluster's offset is a multiple of its type's alignment, so that every register of it is accessed aligned once the peripheral is (ADR 0058, 0064)"
                } else {
                    "a register's offset is a multiple of its width, so that each access to it, one instruction, is aligned (ADR 0023, 0058)"
                };
                self.diags.push(
                    Diagnostic::error(
                        "ty::misaligned-register",
                        format!("the {what} `{}` is at a misaligned offset", entry.name),
                    )
                    .label(
                        entry.offset.1,
                        format!("{offset:#x} is not a multiple of {align}"),
                    )
                    .label(entry.ty_span, needs.clone())
                    .help(help),
                );
            }
            // Neither is a failed value: the type keeps its layout.
            if let (Some(Some(step)), Some((_, step_span))) = (geometry.step, entry.step) {
                if step % align != 0 {
                    self.diags.push(
                        Diagnostic::error(
                            "ty::misaligned-step",
                            format!("the step of the {what} array `{}` is misaligned", entry.name),
                        )
                        .label(step_span, format!("{step:#x} is not a multiple of {align}"))
                        .label(entry.ty_span, needs.clone())
                        .help("the distance between the elements of an array is a multiple of the alignment of its element, so that every element is accessed aligned once the first is (ADR 0064)"),
                    );
                }
                if step < size {
                    // A saturated extent (ADR 0064) is shown as such.
                    let (takes, less) = if size >= PAST_ADDRESS_SPACE {
                        (
                            "more than 2³² bytes".to_owned(),
                            "the size of an element, which is more than 2³² bytes".to_owned(),
                        )
                    } else {
                        (format!("{size} bytes"), size.to_string())
                    };
                    self.diags.push(
                        Diagnostic::error(
                            "ty::step-too-small",
                            format!("the step of the {what} array `{}` is smaller than its elements", entry.name),
                        )
                        .label(step_span, format!("{step:#x} is less than {less}"))
                        .label(entry.ty_span, format!("each element of `{elem}` takes {takes}"))
                        .help("the elements of an array do not overlap: the step is at least the size of an element, and 0 only for elements that occupy nothing (ADR 0064)"),
                    );
                }
            }
        }

        // Only entries whose elements occupy bytes and keep clear of each
        // other take part in the overlap check (ADR 0064).
        let occupying: Vec<Option<EntryPlace>> = placed
            .into_iter()
            .map(|place| {
                place.filter(|place| {
                    !place.is_empty()
                        && place.size > 0
                        && place.array.is_none_or(|(_, step)| step >= place.size)
                })
            })
            .collect();

        // In source order, which entries are kept in.
        for (later, &later_placed) in occupying.iter().enumerate() {
            let Some(later_place) = later_placed else {
                continue;
            };
            for (earlier, &earlier_placed) in occupying[..later].iter().enumerate() {
                let Some(earlier_place) = earlier_placed else {
                    continue;
                };
                let (later, earlier) = (&def.entries[later], &def.entries[earlier]);
                // A read can only mean one and a write the other.
                let apart = matches!(
                    (earlier.access(), later.access()),
                    (Some(AccessMode::Read), Some(AccessMode::Write))
                        | (Some(AccessMode::Write), Some(AccessMode::Read))
                );
                // Entries of different alternate groups, or of a group and
                // none, belong to different modes of the device, of which one
                // is in use at a time (ADR 0061).
                let other_mode = (earlier.group.as_ref()).map(|(group, _)| group)
                    != (later.group.as_ref()).map(|(group, _)| group);
                if apart || other_mode {
                    continue;
                }
                let Some((lo, hi)) = layout::first_shared(&earlier_place, &later_place) else {
                    continue;
                };
                let registers = !later.is_cluster() && !earlier.is_cluster();
                let covers = |entry: &hir::EntryDef, place: &EntryPlace| {
                    let kind = match entry.access() {
                        Some(access) => format!("`{}`", access.words()),
                        None => "a cluster".to_owned(),
                    };
                    let array = match place.array {
                        Some((len, _)) => format!(" array of {len}"),
                        None => String::new(),
                    };
                    // A saturated size (ADR 0064) is shown as such.
                    let end = place.exact_end();
                    let end = if end >= u128::from(PAST_ADDRESS_SPACE) {
                        "past 2³²".to_owned()
                    } else {
                        format!("{end:#x}")
                    };
                    format!("{kind}{array}, covers [{:#x}, {end})", place.offset)
                };
                let (message, help) = if registers {
                    (
                        format!(
                            "the registers `{}` and `{}` overlap",
                            earlier.name, later.name
                        ),
                        format!(
                            "they share {}; two registers of one mode share a byte only when one is `read` and the other `write`, so that a read means one and a write the other; registers of another mode of the device go in an alternate group, `in GROUP` after the offset, and are checked only against the registers of their group (ADR 0058, 0061)",
                            shared_bytes(lo, hi)
                        ),
                    )
                } else {
                    (
                        format!(
                            "the entries `{}` and `{}` overlap",
                            earlier.name, later.name
                        ),
                        format!(
                            "they share {}; two entries share a byte only when both are registers, one `read` and the other `write`, so that a read means one and a write the other; a cluster has no mode, and occupies its whole extent, the gaps between its registers included; entries of another mode of the device go in an alternate group, `in GROUP` after the offset, and are checked only against the entries of their group (ADR 0058, 0061, 0064)",
                            if hi >= u128::from(PAST_ADDRESS_SPACE) {
                                format!("the bytes from {lo:#x} on")
                            } else {
                                shared_bytes(lo, hi)
                            }
                        ),
                    )
                };
                self.diags.push(
                    Diagnostic::error("ty::register-overlap", message)
                        .label(later.span, covers(later, &later_place))
                        .label(earlier.span, covers(earlier, &earlier_place))
                        .help(help),
                );
            }
        }
    }
}

/// An entry's type split into the type of its elements and, for an array,
/// its length (ADR 0064). An array of arrays is an entry whose elements are
/// arrays, which is no register type.
fn split_entry_ty(ty: hir::Ty) -> (hir::Ty, Option<hir::ArrayLen>) {
    match ty {
        hir::Ty::Array(elem, len) => (*elem, Some(len)),
        ty => (ty, None),
    }
}

/// An entry's values once computed (ADR 0064).
struct Geometry {
    cluster: bool,
    /// `None` if it failed.
    offset: Option<u64>,
    /// For an entry whose type is an array, its length; `None` inside if it
    /// failed.
    len: Option<Option<u64>>,
    /// For an array with a `step`, its value; `None` inside if it failed.
    step: Option<Option<u64>>,
    /// The normalized type of one element.
    elem: hir::Ty,
    /// What one element takes, its size and its alignment; `None` if its
    /// type is no register type, or a cluster's type has no layout.
    unit: Option<(u64, u64)>,
}

impl Geometry {
    /// Where the entry is, if every value is known. A register array
    /// without a `step` has its width; a cluster array without one failed
    /// (it was reported).
    fn place(&self) -> Option<EntryPlace> {
        let (size, align) = self.unit?;
        let array = match self.len {
            None => None,
            Some(len) => {
                let step = match self.step {
                    Some(step) => step?,
                    None if self.cluster => return None,
                    None => size,
                };
                Some((len?, step))
            }
        };
        Some(EntryPlace {
            offset: self.offset?,
            array,
            size,
            align,
        })
    }
}

/// The end of a peripheral's range as diagnostics show it: a saturated
/// extent (ADR 0064) is past 2³².
fn range_end(start: u64, end: u64) -> String {
    if saturated(start, end) {
        "past 2³²".to_owned()
    } else {
        format!("{end:#x}")
    }
}

/// Whether the range `[start, end)` of a peripheral is that of a saturated
/// extent (ADR 0064).
fn saturated(start: u64, end: u64) -> bool {
    end - start >= PAST_ADDRESS_SPACE
}

/// How diagnostics name the bytes `[lo, hi)` two ranges share: "the byte
/// at 0x10", "4 bytes from 0x10".
fn shared_bytes(lo: u128, hi: u128) -> String {
    match hi - lo {
        1 => format!("the byte at {lo:#x}"),
        n => format!("{n} bytes from {lo:#x}"),
    }
}
