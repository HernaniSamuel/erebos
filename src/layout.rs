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

/// The layout of a peripheral type (ADR 0058, 0064): where its entries are,
/// and what a peripheral of it covers and must be aligned to. No value has a
/// peripheral type, so it has no size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeripheralLayout {
    /// The offset of each entry, in declaration order: of its first element.
    pub offsets: Vec<u64>,
    /// For each entry, in declaration order, its length and the distance in
    /// bytes between its elements if it is an array, `None` if not.
    pub arrays: Vec<Option<(u64, u64)>>,
    /// The end of the last byte any entry occupies, and 0 with none.
    /// Computed exactly up to 2³² (a `u32` at `0xFFFF_FFFC` gives 2³², which
    /// is no `usize`) and saturated above: [`PAST_ADDRESS_SPACE`] stands for
    /// any extent past 2³² (ADR 0064), so that nested arrays never overflow.
    pub extent: u64,
    /// The largest alignment of its entries' elements, and 1 with no entry:
    /// every entry counts, an array of 0 elements too.
    pub align: u64,
}

/// The extent of a peripheral type that is past 2³², whatever it is: an
/// extent is only compared with 2³² (ADR 0064), so this is the first value
/// that is not one that fits.
pub const PAST_ADDRESS_SPACE: u64 = (1 << target::POINTER_BITS) + 1;

/// Where an entry of a peripheral type is and what its elements are like,
/// for laying the type out (ADR 0064).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryPlace {
    /// Of the first element.
    pub offset: u64,
    /// The length and the step if the entry is an array.
    pub array: Option<(u64, u64)>,
    /// What one element occupies: a register's width, a cluster's extent.
    pub size: u64,
    /// A register's width, a cluster type's alignment.
    pub align: u64,
}

impl EntryPlace {
    /// The number of elements.
    pub fn len(&self) -> u64 {
        self.array.map_or(1, |(len, _)| len)
    }

    /// Whether it has no element.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The distance between elements: the step of an array, the size of a
    /// single element, and at least 1, so that it divides.
    fn distance(&self) -> u64 {
        self.array.map_or(self.size, |(_, step)| step).max(1)
    }

    /// The end of the last byte any element occupies, exactly: one past the
    /// last element's last byte, where nothing is occupied the offset.
    pub fn exact_end(&self) -> u128 {
        if self.is_empty() {
            return u128::from(self.offset);
        }
        let step = self.array.map_or(0, |(_, step)| step);
        u128::from(self.offset)
            + u128::from(self.len() - 1) * u128::from(step)
            + u128::from(self.size)
    }

    /// The end of the last byte any element occupies, at most
    /// [`PAST_ADDRESS_SPACE`]; 0 where nothing is occupied.
    pub fn end(&self) -> u64 {
        if self.is_empty() || self.size == 0 {
            return 0;
        }
        u64::try_from(self.exact_end())
            .map_or(PAST_ADDRESS_SPACE, |end| end.min(PAST_ADDRESS_SPACE))
    }
}

/// The first bytes two entries share, if they do (ADR 0064): the range
/// `[lo, hi)` shared by the lowest elements that meet. Both entries must have
/// elements that occupy bytes and do not overlap each other (a step of at
/// least their size). It walks the elements of the entry with fewer
/// elements and finds by division the elements of the other they may meet,
/// so it costs at most the smaller length; only elements starting below
/// [`PAST_ADDRESS_SPACE`] are looked at, since nothing past the address
/// space is ever placed.
pub fn first_shared(a: &EntryPlace, b: &EntryPlace) -> Option<(u128, u128)> {
    let (few, many) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let step = i128::from(few.distance());
    let (size, many_size) = (i128::from(few.size), i128::from(many.size));
    let (many_offset, many_step) = (i128::from(many.offset), i128::from(many.distance()));
    let reach = i128::from(PAST_ADDRESS_SPACE) - i128::from(few.offset);
    if reach <= 0 {
        return None;
    }
    let walked = i128::from(few.len()).min(reach / step + 1);
    for i in 0..walked {
        let start = i128::from(few.offset) + i * step;
        let end = start + size;
        // The elements `j` of `many` with `offset + j * step` below `end`
        // and `offset + j * step + size` above `start`.
        let first = (start - many_offset - many_size).div_euclid(many_step) + 1;
        let last = (end - many_offset - 1).div_euclid(many_step);
        let j = first.max(0);
        if j <= last.min(i128::from(many.len()) - 1) {
            let there = many_offset + j * many_step;
            return Some((
                start.max(there).unsigned_abs(),
                end.min(there + many_size).unsigned_abs(),
            ));
        }
    }
    None
}

/// The largest alignment `#[align(N)]` accepts: the address space itself.
pub const MAX_ALIGN: u64 = 1 << (target::POINTER_BITS - 1);

/// The width in bytes of a register of type `ty`, if it is a register type
/// (ADR 0058, 0060): `u8`, `u16` or `u32`, an unsigned integer as wide as
/// the register and at most as wide as the target's widest single access
/// (ADR 0023), or a bit-field struct or an enum without payloads stored as
/// one of them, whose width is its integer's.
pub fn register_width(ty: Ty) -> Option<u64> {
    const _: () = assert!(target::WIDEST_ACCESS_BITS >= 32);
    match ty {
        Ty::Int(int)
        | Ty::BitStruct(BitStructTy { int, .. })
        | Ty::Enum(EnumTy {
            int,
            payloads: false,
            ..
        }) if matches!(int, IntTy::U8 | IntTy::U16 | IntTy::U32) => Some(int.bytes()),
        _ => None,
    }
}

/// The most bits a bit field of type `ty` may have (ADR 0059): one for a
/// `bool`, which takes exactly one; the width of `u8`, `u16`, `u32` and
/// `u64`, and of the integer type of an enum without payloads on one of
/// them, whose variants must also fit in the field's bits (`bits_needed`).
/// `None` if `ty` is no bit field type.
pub fn bit_field_holds(ty: Ty) -> Option<u32> {
    match ty {
        Ty::Bool => Some(1),
        Ty::Int(int)
        | Ty::Enum(EnumTy {
            int,
            payloads: false,
            ..
        }) if matches!(int, IntTy::U8 | IntTy::U16 | IntTy::U32 | IntTy::U64) => Some(int.bits()),
        _ => None,
    }
}

/// The fewest bits that hold `value`, an unsigned integer: a field of `n`
/// bits holds the values from 0 to 2ⁿ − 1 (ADR 0059).
pub fn bits_needed(value: u64) -> u32 {
    u64::BITS - value.leading_zeros()
}

/// The bits of the fields of a bit-field struct (ADR 0059), computed and
/// checked once: its layout is its integer type's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BitLayout {
    /// Each field's lowest and highest bit, in declaration order; `None`
    /// for a field that is a failed value, which was reported: an end that
    /// failed, bits reversed or past the integer type, a type in error or
    /// no bit field type, or a width that does not suit the type or cannot
    /// be checked against it, an enum variant's value having failed.
    pub fields: Vec<Option<(u32, u32)>>,
}

impl BitLayout {
    /// Whether the struct reaches a failed value (ADR 0048, 0059): a field
    /// cannot be read or written as declared.
    pub fn failed(&self) -> bool {
        self.fields.iter().any(Option::is_none)
    }
}

/// Lays out a peripheral type whose entries, in declaration order, are
/// placed as `entries` say (ADR 0058, 0064).
pub fn peripheral_layout(entries: &[EntryPlace]) -> PeripheralLayout {
    PeripheralLayout {
        offsets: entries.iter().map(|entry| entry.offset).collect(),
        arrays: entries.iter().map(|entry| entry.array).collect(),
        extent: entries.iter().map(EntryPlace::end).max().unwrap_or(0),
        align: entries.iter().map(|entry| entry.align).max().unwrap_or(1),
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
    use crate::hir::{BitStructId, IntTy};

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

    fn register(offset: u64, width: u64) -> EntryPlace {
        EntryPlace {
            offset,
            array: None,
            size: width,
            align: width,
        }
    }

    fn array(offset: u64, len: u64, step: u64, size: u64, align: u64) -> EntryPlace {
        EntryPlace {
            offset,
            array: Some((len, step)),
            size,
            align,
        }
    }

    /// The 16550 UART of ADR 0058, an empty peripheral type, and a register
    /// ending at 2³².
    #[test]
    fn peripheral_types_reach_to_their_last_register() {
        let uart = peripheral_layout(&[
            register(0, 1),
            register(0, 1),
            register(1, 1),
            register(5, 1),
        ]);
        assert_eq!(uart.offsets, [0, 0, 1, 5]);
        assert_eq!((uart.extent, uart.align), (6, 1));
        let empty = peripheral_layout(&[]);
        assert_eq!((empty.extent, empty.align), (0, 1));
        // The widest register aligns it, the last to end gives its extent,
        // in whatever order they are declared.
        let mixed = peripheral_layout(&[register(8, 1), register(0, 4), register(2, 2)]);
        assert_eq!((mixed.extent, mixed.align), (9, 4));
        let top = peripheral_layout(&[register(0, 1), register(0xFFFF_FFFC, 4)]);
        assert_eq!((top.extent, top.align), (1 << 32, 4));
    }

    /// Arrays reach to their last element, an empty one occupies nothing but
    /// still aligns the type, and an extent past 2³² saturates (ADR 0064).
    #[test]
    fn arrays_reach_to_their_last_element() {
        let plic = peripheral_layout(&[array(0, 96, 4, 4, 4), array(0x20_0000, 2, 0x1000, 8, 4)]);
        assert_eq!(plic.arrays, [Some((96, 4)), Some((2, 0x1000))]);
        assert_eq!((plic.extent, plic.align), (0x20_1008, 4));
        // 0 elements: no bytes, but the alignment of the element counts.
        let empty = peripheral_layout(&[register(0, 1), array(0x100, 0, 4, 4, 4)]);
        assert_eq!((empty.extent, empty.align), (1, 4));
        // Elements of size 0 occupy nothing either.
        let nothing = peripheral_layout(&[array(0x10, 5, 0, 0, 2)]);
        assert_eq!((nothing.extent, nothing.align), (0, 2));
        // Exactly 2³² is exact, a byte more and anything beyond is one value.
        let exact = peripheral_layout(&[array(0, 2, 0x8000_0000, 0x8000_0000, 4)]);
        assert_eq!(exact.extent, 1 << 32);
        let past = peripheral_layout(&[array(1, 2, 0x8000_0000, 0x8000_0000, 4)]);
        assert_eq!(past.extent, PAST_ADDRESS_SPACE);
        let huge = peripheral_layout(&[array(u64::MAX, u64::MAX, u64::MAX, u64::MAX, 1)]);
        assert_eq!(huge.extent, PAST_ADDRESS_SPACE);
    }

    #[test]
    fn entries_share_the_first_bytes_where_their_elements_meet() {
        // Two interleaved arrays never meet; moved so that the first's tenth
        // element is the second's first, they do.
        let a = array(0, 4, 8, 4, 4);
        let b = array(4, 4, 8, 4, 4);
        assert_eq!(first_shared(&a, &b), None);
        let a = array(0, 100, 4, 4, 4);
        let b = array(36, 100, 8, 4, 4);
        assert_eq!(first_shared(&a, &b), Some((36, 40)));
        // A register in a gap of a cluster's range, and an array's range.
        let cluster = array(0x100, 2, 0x100, 0x20, 4);
        assert_eq!(first_shared(&cluster, &register(0x1FC, 4)), None);
        assert_eq!(
            first_shared(&register(0x11C, 4), &cluster),
            Some((0x11C, 0x120))
        );
        assert_eq!(
            first_shared(&register(0x20, 1), &register(0x20, 1)),
            Some((0x20, 0x21))
        );
        assert_eq!(first_shared(&register(0x20, 1), &register(0x21, 1)), None);
        // An empty array shares nothing; the shorter array is the one walked.
        assert_eq!(first_shared(&array(0, 0, 4, 4, 4), &register(0, 4)), None);
        let long = array(4, 1 << 20, 8, 4, 4);
        let long_too = array(0, 1 << 20, 8, 4, 4);
        assert_eq!(first_shared(&long, &long_too), None);
        // Past the address space nothing is looked at.
        let far = array(1 << 40, u64::MAX, 4, 4, 4);
        assert_eq!(first_shared(&far, &far), None);
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

    /// A bit-field struct or an enum without payloads is a register as wide
    /// as its integer type, when that is one (ADR 0060).
    #[test]
    fn registers_are_bit_field_structs_or_enums_on_u8_u16_or_u32() {
        let bits_on = |int| {
            Ty::BitStruct(BitStructTy {
                id: BitStructId(0),
                name: "B",
                int,
            })
        };
        let enum_on = |int, payloads| {
            Ty::Enum(EnumTy {
                id: EnumId(0),
                name: "E",
                int,
                payloads,
            })
        };
        for (int, width) in [(IntTy::U8, 1), (IntTy::U16, 2), (IntTy::U32, 4)] {
            assert_eq!(register_width(bits_on(int)), Some(width));
            assert_eq!(register_width(enum_on(int, false)), Some(width));
            assert_eq!(register_width(enum_on(int, true)), None);
        }
        assert_eq!(register_width(bits_on(IntTy::U64)), None);
        for int in [IntTy::U64, IntTy::Usize, IntTy::I8, IntTy::I32] {
            assert_eq!(register_width(enum_on(int, false)), None);
        }
    }

    #[test]
    fn bit_fields_are_bools_unsigned_integers_or_enums_on_them() {
        let enum_on = |int, payloads| {
            Ty::Enum(EnumTy {
                id: EnumId(0),
                name: "E",
                int,
                payloads,
            })
        };
        assert_eq!(bit_field_holds(Ty::Bool), Some(1));
        assert_eq!(bit_field_holds(Ty::Int(IntTy::U8)), Some(8));
        assert_eq!(bit_field_holds(Ty::Int(IntTy::U16)), Some(16));
        assert_eq!(bit_field_holds(Ty::Int(IntTy::U32)), Some(32));
        assert_eq!(bit_field_holds(Ty::Int(IntTy::U64)), Some(64));
        assert_eq!(bit_field_holds(enum_on(IntTy::U8, false)), Some(8));
        assert_eq!(bit_field_holds(enum_on(IntTy::U64, false)), Some(64));
        for int in [IntTy::Usize, IntTy::I8, IntTy::I64, IntTy::Isize] {
            assert_eq!(bit_field_holds(Ty::Int(int)), None);
            assert_eq!(bit_field_holds(enum_on(int, false)), None);
        }
        assert_eq!(bit_field_holds(enum_on(IntTy::U8, true)), None);
        let u8_ = Ty::Int(IntTy::U8);
        assert_eq!(bit_field_holds(Ty::ptr(false, false, u8_)), None);
        assert_eq!(bit_field_holds(Ty::array(u8_, 1)), None);
        assert_eq!(bit_field_holds(Ty::Unit), None);
    }

    #[test]
    fn values_need_their_highest_bit() {
        assert_eq!(bits_needed(0), 0);
        assert_eq!(bits_needed(1), 1);
        assert_eq!(bits_needed(3), 2);
        assert_eq!(bits_needed(4), 3);
        assert_eq!(bits_needed(15), 4);
        assert_eq!(bits_needed(16), 5);
        assert_eq!(bits_needed(u64::MAX), 64);
    }

    #[test]
    fn align_up_rounds_to_powers_of_two() {
        assert_eq!(align_up(0, 4), Some(0));
        assert_eq!(align_up(1, 4), Some(4));
        assert_eq!(align_up(8, 8), Some(8));
        assert_eq!(align_up(u64::MAX, 2), None);
    }
}
