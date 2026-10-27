//! Sizes, alignments and field offsets of types (ADR 0022). Layouts are a
//! function of types, not a pipeline stage (ADR 0033): these are the rules,
//! and `query` applies them on demand, laying out structs and enums with
//! payloads for type checking, MIR building and the evaluator.

use crate::hir::{BitStructTy, EnumId, EnumTy, IntTy, StructId, Ty};
use crate::target;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// In bytes; always a multiple of `align`.
    pub size: u64,
    /// In bytes; a power of two.
    pub align: u64,
}

/// The largest object the target can hold: half of its address space, so
/// that the distance between two bytes of an object fits in `isize`.
pub const MAX_SIZE: u64 = (1 << (target::POINTER_BITS - 1)) - 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructLayout {
    pub layout: Layout,
    /// The offset of each field, in declaration order.
    pub offsets: Vec<u64>,
    /// Asks for more than its natural alignment, through `#[align(N)]` on
    /// it or on something it contains; a packed struct could not keep it.
    pub over_aligned: bool,
    /// Its `#[align(N)]` failed, reported: the layout is the one it has
    /// without the attribute, a lower bound of any it could have had, and
    /// the struct is erroneous (ADR 0048).
    pub align_failed: bool,
}

/// The layout of an enum with payloads (ADR 0039).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnumLayout {
    pub layout: Layout,
    /// For each variant, the offset of each of its payload values, in
    /// order; the tag is at offset 0.
    pub offsets: Vec<Vec<u64>>,
    /// Asks for more than its natural alignment, through `#[align(N)]` on
    /// something a payload contains; a packed struct could not keep it.
    pub over_aligned: bool,
}

/// The layout of a peripheral type (ADR 0058): where its registers are, and
/// what a peripheral of it covers and must be aligned to. No value has a
/// peripheral type, so it has no size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeripheralLayout {
    /// The offset of each register, in declaration order.
    pub offsets: Vec<u64>,
    /// The end of its last register, `max(offset + width)`, and 0 with no
    /// register. Computed exactly: a `u32` at `0xFFFF_FFFC` gives 2³²,
    /// which is no `usize`.
    pub extent: u64,
    /// The width of its widest register, and 1 with no register.
    pub align: u64,
}

/// The largest alignment `#[align(N)]` accepts: the address space itself.
pub const MAX_ALIGN: u64 = 1 << (target::POINTER_BITS - 1);

/// The width in bytes of a register of type `ty`, if it is a register type
/// (ADR 0058): `u8`, `u16` or `u32`, an unsigned integer as wide as the
/// register and at most as wide as the target's widest single access
/// (ADR 0023).
pub fn register_width(ty: Ty) -> Option<u64> {
    const _: () = assert!(target::WIDEST_ACCESS_BITS >= 32);
    match ty {
        Ty::Int(int @ (IntTy::U8 | IntTy::U16 | IntTy::U32)) => Some(int.bytes()),
        _ => None,
    }
}

/// Lays out a peripheral type whose registers, in declaration order, have
/// the offsets and widths in `registers` (ADR 0058).
pub fn peripheral_layout(registers: &[(u64, u64)]) -> PeripheralLayout {
    PeripheralLayout {
        offsets: registers.iter().map(|&(offset, _)| offset).collect(),
        extent: (registers.iter())
            .map(|&(offset, width)| offset + width)
            .max()
            .unwrap_or(0),
        align: (registers.iter())
            .map(|&(_, width)| width)
            .max()
            .unwrap_or(1),
    }
}

/// A type laid out by rules of its own, once, on demand: a struct
/// (ADR 0022) or an enum with payloads (ADR 0039).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Composite {
    Struct(StructId),
    Enum(EnumId),
}

/// The layout of `ty`, given the layout of each struct and each enum with
/// payloads.
pub fn of(ty: Ty, composite: &mut impl FnMut(Composite) -> Option<Layout>) -> Option<Layout> {
    let layout = match ty {
        // `!` has no values; `{error}` was reported.
        Ty::Unit | Ty::Never | Ty::Error => Layout { size: 0, align: 1 },
        Ty::Bool => Layout { size: 1, align: 1 },
        Ty::Enum(EnumTy {
            id, payloads: true, ..
        }) => composite(Composite::Enum(id))?,
        // An enum without payloads is stored as its integer type
        // (ADR 0035), and so is a bit-field struct (ADR 0059).
        Ty::Int(int) | Ty::Enum(EnumTy { int, .. }) | Ty::BitStruct(BitStructTy { int, .. }) => {
            Layout {
                size: int.bytes(),
                align: int.bytes(),
            }
        }
        // A slice is an address and a length (ADR 0031).
        Ty::Ptr(_) if ty.slice_elem().is_some() => Layout {
            size: 2 * target::POINTER_BYTES,
            align: target::POINTER_BYTES,
        },
        Ty::Ptr(_) => Layout {
            size: target::POINTER_BYTES,
            align: target::POINTER_BYTES,
        },
        // Unsized: only the pointee of a slice, never laid out (ADR 0031).
        Ty::Slice(_) => return None,
        // No size: only the type of a peripheral and the pointee of a
        // pointer, never laid out as a value (ADR 0058).
        Ty::Peripheral(_) => return None,
        Ty::Array(elem, len) => {
            let elem = of(*elem, composite)?;
            Layout {
                size: elem.size.checked_mul(len.known())?,
                align: elem.align,
            }
        }
        Ty::Struct(s) => composite(Composite::Struct(s.id))?,
        Ty::Applied(_) => unreachable!("types are normalized before they are laid out"),
    };
    (layout.size <= MAX_SIZE).then_some(layout)
}

/// The size, alignment and payload offsets of an enum with payloads whose
/// tag has layout `tag` and whose variants have payload values of the
/// layouts in `variants` (ADR 0039): each variant is laid out as a struct
/// of the tag followed by its payload values, and the enum is as aligned
/// as the most aligned of these structs, and as large as the largest,
/// rounded up to its alignment. `None` if it does not fit in the target.
pub fn enum_layout(tag: Layout, variants: &[Vec<Layout>]) -> Option<(Layout, Vec<Vec<u64>>)> {
    let mut align = tag.align;
    let mut end = tag.size;
    let mut offsets = Vec::with_capacity(variants.len());
    for payload in variants {
        let mut at = tag.size;
        let mut variant_offsets = Vec::with_capacity(payload.len());
        for value in payload {
            let offset = align_up(at, value.align)?;
            variant_offsets.push(offset);
            at = offset.checked_add(value.size)?;
            align = align.max(value.align);
        }
        end = end.max(at);
        offsets.push(variant_offsets);
    }
    let size = align_up(end, align)?;
    (size <= MAX_SIZE).then_some((Layout { size, align }, offsets))
}

/// Rounds `offset` up to a multiple of `align`, a power of two.
pub fn align_up(offset: u64, align: u64) -> Option<u64> {
    Some(offset.checked_add(align - 1)? & !(align - 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hir::IntTy;

    fn scalar(ty: Ty) -> Option<Layout> {
        of(ty, &mut |_| {
            unreachable!("no structs or enums with payloads")
        })
    }

    fn int(size: u64) -> Layout {
        Layout { size, align: size }
    }

    #[test]
    fn scalars_and_arrays() {
        let u64_ = Ty::Int(IntTy::U64);
        assert_eq!(scalar(u64_), Some(Layout { size: 8, align: 8 }));
        assert_eq!(scalar(Ty::Bool), Some(Layout { size: 1, align: 1 }));
        assert_eq!(
            scalar(Ty::array(u64_, 3)),
            Some(Layout { size: 24, align: 8 })
        );
        assert_eq!(
            scalar(Ty::array(u64_, 0)),
            Some(Layout { size: 0, align: 8 })
        );
        assert_eq!(scalar(Ty::array(Ty::Bool, MAX_SIZE + 1)), None);
        assert_eq!(scalar(Ty::array(Ty::array(u64_, u64::MAX), 2)), None);
    }

    #[test]
    fn slices_are_an_address_and_a_length() {
        let u8_ = Ty::Int(IntTy::U8);
        let slice = Ty::ptr(false, false, Ty::slice(u8_));
        assert_eq!(scalar(slice), Some(Layout { size: 8, align: 4 }));
        assert_eq!(scalar(Ty::slice(u8_)), None);
    }

    /// The example of ADR 0039: `enum Msg: u8 { Nop = 0, Led(bool) = 1,
    /// Write(u8, u16) = 2, Wide(u32) = 3 }`.
    #[test]
    fn enums_with_payloads_lay_out_each_variant_as_a_struct() {
        let variants = [vec![], vec![int(1)], vec![int(1), int(2)], vec![int(4)]];
        let (layout, offsets) = enum_layout(int(1), &variants).unwrap();
        assert_eq!(layout, Layout { size: 8, align: 4 });
        assert_eq!(offsets, [vec![], vec![1], vec![1, 2], vec![4]]);
        // A payload starts right after the tag, not at a common offset.
        let (layout, offsets) = enum_layout(int(2), &[vec![int(1), int(8)]]).unwrap();
        assert_eq!(layout, Layout { size: 16, align: 8 });
        assert_eq!(offsets, [vec![2, 8]]);
        // Zero-sized payloads take no bytes, but raise the alignment.
        let empty = Layout { size: 0, align: 8 };
        let (layout, offsets) = enum_layout(int(1), &[vec![int(1), empty]]).unwrap();
        assert_eq!(layout, Layout { size: 8, align: 8 });
        assert_eq!(offsets, [vec![1, 8]]);
    }

    #[test]
    fn enums_with_payloads_fit_in_the_target() {
        let bytes = |size| Layout { size, align: 1 };
        // Over the limit only through the tag.
        assert_eq!(enum_layout(int(1), &[vec![bytes(MAX_SIZE)]]), None);
        // Over it only through the rounding.
        let variants = [vec![bytes(MAX_SIZE - 1)], vec![int(4)]];
        assert_eq!(enum_layout(int(1), &variants), None);
        let fits = enum_layout(int(1), &[vec![bytes(MAX_SIZE - 1)]]).unwrap();
        assert_eq!(fits.0.size, MAX_SIZE);
        assert_eq!(enum_layout(int(1), &[vec![bytes(u64::MAX)]]), None);
    }

    /// The 16550 UART of ADR 0058, an empty peripheral type, and a register
    /// ending at 2³².
    #[test]
    fn peripheral_types_reach_to_their_last_register() {
        let uart = peripheral_layout(&[(0, 1), (0, 1), (1, 1), (5, 1)]);
        assert_eq!(uart.offsets, [0, 0, 1, 5]);
        assert_eq!((uart.extent, uart.align), (6, 1));
        let empty = peripheral_layout(&[]);
        assert_eq!((empty.extent, empty.align), (0, 1));
        // The widest register aligns it, the last to end gives its extent,
        // in whatever order they are declared.
        let mixed = peripheral_layout(&[(8, 1), (0, 4), (2, 2)]);
        assert_eq!((mixed.extent, mixed.align), (9, 4));
        let top = peripheral_layout(&[(0, 1), (0xFFFF_FFFC, 4)]);
        assert_eq!((top.extent, top.align), (1 << 32, 4));
    }

    #[test]
    fn registers_are_u8_u16_or_u32() {
        assert_eq!(register_width(Ty::Int(IntTy::U8)), Some(1));
        assert_eq!(register_width(Ty::Int(IntTy::U16)), Some(2));
        assert_eq!(register_width(Ty::Int(IntTy::U32)), Some(4));
        for int in [IntTy::U64, IntTy::Usize, IntTy::I8, IntTy::I32] {
            assert_eq!(register_width(Ty::Int(int)), None);
        }
        assert_eq!(register_width(Ty::Bool), None);
    }

    #[test]
    fn align_up_rounds_to_powers_of_two() {
        assert_eq!(align_up(0, 4), Some(0));
        assert_eq!(align_up(1, 4), Some(4));
        assert_eq!(align_up(8, 8), Some(8));
        assert_eq!(align_up(u64::MAX, 2), None);
    }
}
