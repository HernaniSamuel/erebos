//! Sizes and alignments of types (ADR 0022): the layout part of the
//! "const evaluation" layer of ADR 0003. The type checker uses it to reject
//! types too large for the address space, and MIR building to record the
//! layouts that code generation follows.

use crate::hir::Ty;
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

/// The layout of `ty`, or `None` if it is larger than [`MAX_SIZE`].
pub fn of(ty: Ty) -> Option<Layout> {
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
            let elem = of(*elem)?;
            Layout {
                size: elem.size.checked_mul(len)?,
                align: elem.align,
            }
        }
    };
    (layout.size <= MAX_SIZE).then_some(layout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hir::IntTy;

    #[test]
    fn scalars_and_arrays() {
        let u64_ = Ty::Int(IntTy::U64);
        assert_eq!(of(u64_), Some(Layout { size: 8, align: 8 }));
        assert_eq!(of(Ty::Bool), Some(Layout { size: 1, align: 1 }));
        assert_eq!(of(Ty::array(u64_, 3)), Some(Layout { size: 24, align: 8 }));
        assert_eq!(of(Ty::array(u64_, 0)), Some(Layout { size: 0, align: 8 }));
        assert_eq!(of(Ty::array(Ty::Bool, MAX_SIZE + 1)), None);
        assert_eq!(of(Ty::array(Ty::array(u64_, u64::MAX), 2)), None);
    }
}
