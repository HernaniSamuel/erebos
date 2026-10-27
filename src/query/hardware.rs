//! The memory map (ADR 0057) and peripherals (ADR 0058): their checks,
//! layouts and placement.

use crate::diag::Diagnostic;
use crate::hir::{self, AccessMode, PeripheralId, PeripheralTypeId};
use crate::layout::{self, Composite, PeripheralLayout};
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
                let shared = shared_bytes(lo, hi);
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

    /// The layout of a peripheral type (ADR 0058), computed once from its
    /// registers' offsets and types; `None` when it reaches a failed value:
    /// an offset that failed, or a register type that is in error or no
    /// register type, all reported (by `check_peripherals` for the last).
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
        let mut registers = Vec::with_capacity(def.registers.len());
        let mut failed = false;
        for register in &def.registers {
            let offset = self.array_len(register.offset.0);
            let ty = self.normalize(register.ty);
            match (offset, layout::register_width(ty)) {
                (Some(offset), Some(width)) => registers.push((offset, width)),
                _ => failed = true,
            }
        }
        self.finish(Item::PeripheralLayout(id));
        let layout = (!failed).then(|| layout::peripheral_layout(&registers));
        self.peripheral_layouts[id.0 as usize] = State::Done(layout.clone());
        // Its MIR type, made while the layout was computed, gets it now.
        if let (Some(p), Some(layout)) = (self.mir_peripheral_types[id.0 as usize], &layout) {
            p.def().set_layout(layout.clone());
        }
        layout
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
                let reaches = def.registers.iter().any(|register| {
                    let ty = self.normalize(register.ty);
                    layout::register_width(ty).is_none()
                        && self.lacks_layout(
                            register.ty,
                            &mut Vec::new(),
                            PeripheralFailures::Explained,
                        )
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

    /// Whether the peripheral type `id` has a base failure: an offset that
    /// failed, or a register type in error without a peripheral type,
    /// which is in error in itself or is no register type and mentions no
    /// peripheral type. Its error is reported whatever points to the type.
    /// A failure owed only to registers that mention peripheral types may
    /// be one such register's own, and is no base failure. It depends only
    /// on the type's own declaration.
    fn peripheral_base_failure(&mut self, id: PeripheralTypeId) -> bool {
        let hir = self.hir;
        hir.peripheral_type(id).registers.iter().any(|register| {
            if self.array_len(register.offset.0).is_none() {
                return true;
            }
            let ty = self.normalize(register.ty);
            layout::register_width(ty).is_none()
                && (self.lacks_layout(register.ty, &mut Vec::new(), PeripheralFailures::Ignored)
                    || !self.mentions_peripheral(register.ty, &mut Vec::new()))
        })
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
        let register = &hir.peripheral_type(id).registers[index];
        if register.access == AccessMode::Write {
            return None;
        }
        let ty = self.normalize(register.ty);
        (register.read_effect.map(|(_, span)| span)).or_else(|| self.type_read_effect(ty))
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
                self.diags.push(
                    Diagnostic::error(
                        "ty::peripheral-out-of-range",
                        format!("the peripheral `{}` ends past the address space", def.path),
                    )
                    .label(def.name_span, format!("covers [{address:#x}, {end:#x})"))
                    .help(format!(
                        "a peripheral covers `[address, address + extent)`, its type's extent being the end of its last register, and ends at 2³² ({limit:#x}) or below (ADR 0058)"
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
                        format!("covers [{later_lo:#x}, {later_hi:#x})"),
                    )
                    .label(
                        earlier.name_span,
                        format!("covers [{earlier_lo:#x}, {earlier_hi:#x})"),
                    )
                    .help(format!(
                        "they share {}; two peripherals cannot share a byte (ADR 0058)",
                        shared_bytes(lo, hi)
                    )),
                );
            }
        }
    }

    /// Checks the registers of the peripheral type `id` (ADR 0058): their
    /// types, the alignment of their offsets, and their overlaps, once per
    /// pair, at the later register.
    fn check_peripheral_type(&mut self, id: PeripheralTypeId) {
        let hir = self.hir;
        let def = hir.peripheral_type(id);
        // Each register's offset and width, `None` when its offset failed
        // or its type is wrong, both reported.
        let mut placed: Vec<Option<(u64, u64)>> = Vec::with_capacity(def.registers.len());
        for register in &def.registers {
            let ty = self.normalize(register.ty);
            let width = layout::register_width(ty);
            // A type in error was reported, and so was what explains the
            // failure of a peripheral type it reaches. One reaching only
            // peripheral types whose failure is not explained is reported
            // here: their failure may be this very register, or registers
            // that point back to its type.
            if width.is_none()
                && !self.lacks_layout(register.ty, &mut Vec::new(), PeripheralFailures::Explained)
            {
                let what = match ty {
                    hir::Ty::BitStruct(s) => format!(", a bit-field struct on `{}`", s.int),
                    hir::Ty::Enum(e) if e.payloads => ", an enum with payloads".to_owned(),
                    hir::Ty::Enum(e) => format!(", an enum on `{}`", e.int),
                    _ => String::new(),
                };
                self.diags.push(
                    Diagnostic::error("ty::register-type", format!("`{ty}` is not a register type"))
                        .label(register.ty_span, format!("the type of the register `{}`{what}", register.name))
                        .help("a register is a `u8`, a `u16` or a `u32`, or a bit-field struct or an enum without payloads on one of them: as wide as that integer type, and read and written in one access (ADR 0058, 0060)"),
                );
            }
            placed.push(self.array_len(register.offset.0).zip(width));
        }

        for (register, &placed) in def.registers.iter().zip(&placed) {
            let Some((offset, width)) = placed else {
                continue;
            };
            if offset % width != 0 {
                let ty = self.normalize(register.ty);
                self.diags.push(
                    Diagnostic::error(
                        "ty::misaligned-register",
                        format!("the register `{}` is at a misaligned offset", register.name),
                    )
                    .label(
                        register.offset.1,
                        format!("{offset:#x} is not a multiple of {width}"),
                    )
                    .label(register.ty_span, format!("`{ty}` is {width} bytes wide"))
                    .help("a register's offset is a multiple of its width, so that each access to it, one instruction, is aligned (ADR 0023, 0058)"),
                );
            }
        }

        // In source order, which registers are kept in.
        for (later, &later_placed) in placed.iter().enumerate() {
            let Some((later_lo, later_width)) = later_placed else {
                continue;
            };
            let later_hi = later_lo + later_width;
            for (earlier, &earlier_placed) in placed[..later].iter().enumerate() {
                let Some((earlier_lo, earlier_width)) = earlier_placed else {
                    continue;
                };
                let earlier_hi = earlier_lo + earlier_width;
                let lo = later_lo.max(earlier_lo);
                let hi = later_hi.min(earlier_hi);
                let (later, earlier) = (&def.registers[later], &def.registers[earlier]);
                // A read can only mean one and a write the other.
                let apart = matches!(
                    (earlier.access, later.access),
                    (AccessMode::Read, AccessMode::Write) | (AccessMode::Write, AccessMode::Read)
                );
                if lo >= hi || apart {
                    continue;
                }
                self.diags.push(
                    Diagnostic::error(
                        "ty::register-overlap",
                        format!(
                            "the registers `{}` and `{}` overlap",
                            earlier.name, later.name
                        ),
                    )
                    .label(
                        later.span,
                        format!(
                            "`{}`, covers [{later_lo:#x}, {later_hi:#x})",
                            later.access.words()
                        ),
                    )
                    .label(
                        earlier.span,
                        format!(
                            "`{}`, covers [{earlier_lo:#x}, {earlier_hi:#x})",
                            earlier.access.words()
                        ),
                    )
                    .help(format!(
                        "they share {}; two registers share a byte only when one is `read` and the other `write`, so that a read means one and a write the other (ADR 0058)",
                        shared_bytes(lo, hi)
                    )),
                );
            }
        }
    }
}

/// How diagnostics name the bytes `[lo, hi)` two ranges share: "the byte
/// at 0x10", "4 bytes from 0x10".
fn shared_bytes(lo: u64, hi: u64) -> String {
    match hi - lo {
        1 => format!("the byte at {lo:#x}"),
        n => format!("{n} bytes from {lo:#x}"),
    }
}
