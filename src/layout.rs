//! Sizes, alignments and field offsets of types (ADR 0022): the rules of the
//! layout part of the "const evaluation" layer of ADR 0003. Structs are laid
//! out on demand by `query`, which applies them.

use crate::hir::{StructId, Ty};
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
}

/// The largest alignment `#[align(N)]` accepts: the address space itself.
pub const MAX_ALIGN: u64 = 1 << (target::POINTER_BITS - 1);

/// The layout of `ty`, given the layout of each struct.
pub fn of(ty: Ty, struct_layout: &mut impl FnMut(StructId) -> Option<Layout>) -> Option<Layout> {
    let layout = match ty {
        // `!` has no values; `{error}` was reported.
        Ty::Unit | Ty::Never | Ty::Error => Layout { size: 0, align: 1 },
        Ty::Bool => Layout { size: 1, align: 1 },
        Ty::Int(int) => Layout {
            size: int.bytes(),
            align: int.bytes(),
        },
        Ty::Ptr(_) => Layout {
            size: target::POINTER_BYTES,
            align: target::POINTER_BYTES,
        },
        Ty::Array(elem, len) => {
            let elem = of(*elem, struct_layout)?;
            Layout {
                size: elem.size.checked_mul(len.known())?,
                align: elem.align,
            }
        }
        Ty::Struct(s) => struct_layout(s.id)?,
    };
    (layout.size <= MAX_SIZE).then_some(layout)
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
        of(ty, &mut |_| unreachable!("no structs"))
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
    fn align_up_rounds_to_powers_of_two() {
        assert_eq!(align_up(0, 4), Some(0));
        assert_eq!(align_up(1, 4), Some(4));
        assert_eq!(align_up(8, 8), Some(8));
        assert_eq!(align_up(u64::MAX, 2), None);
    }
}
