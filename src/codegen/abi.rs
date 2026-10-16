//! How values cross calls in the riscv32 C ABI (ADR 0006, 0025): the
//! integer calling convention of the RISC-V psABI for ilp32, as clang
//! implements it for `-mabi=ilp32`.

use crate::mir::{Body, Ty};
use crate::target;

/// How one parameter or return value crosses a call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PassMode {
    /// Not passed at all: `()`, `!` and zero-sized aggregates.
    Ignore,
    /// A scalar, as itself.
    Direct,
    /// An aggregate in registers: its bytes, as laid out in memory, read as
    /// the given integer type.
    Cast(Cast),
    /// An aggregate in memory. An argument is a pointer to a copy that the
    /// caller makes; a return value is written to memory the caller
    /// provides, whose address is a hidden first argument.
    Indirect,
}

/// The register form of an aggregate of at most two registers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cast {
    /// One register: `i32`.
    Word,
    /// A register pair holding an aggregate aligned to 8: `i64`.
    Long,
    /// A register pair: `[2 x i32]`.
    Pair,
}

/// The table of ADR 0025.
pub fn classify(ty: Ty) -> PassMode {
    if !ty.is_aggregate() {
        return match ty {
            Ty::Unit | Ty::Never => PassMode::Ignore,
            _ => PassMode::Direct,
        };
    }
    let xlen = target::POINTER_BYTES;
    let layout = ty.layout();
    match layout.size {
        0 => PassMode::Ignore,
        size if size <= xlen => PassMode::Cast(Cast::Word),
        size if size <= 2 * xlen && layout.align == 2 * xlen => PassMode::Cast(Cast::Long),
        size if size <= 2 * xlen => PassMode::Cast(Cast::Pair),
        _ => PassMode::Indirect,
    }
}

/// How a function's return value and parameters cross a call.
pub struct FnAbi {
    pub ret: Ty,
    pub ret_mode: PassMode,
    pub params: Vec<(Ty, PassMode)>,
}

impl FnAbi {
    pub fn of(body: &Body) -> FnAbi {
        FnAbi {
            ret: body.ret,
            ret_mode: classify(body.ret),
            params: body
                .params()
                .iter()
                .map(|param| (param.ty, classify(param.ty)))
                .collect(),
        }
    }

    /// The LLVM parameter that carries MIR parameter `i`, if it is passed:
    /// the hidden return pointer comes first, and ignored parameters take
    /// no LLVM parameter.
    pub fn llvm_param(&self, i: usize) -> Option<u32> {
        if self.params[i].1 == PassMode::Ignore {
            return None;
        }
        let hidden = usize::from(self.ret_mode == PassMode::Indirect);
        let before = self.params[..i]
            .iter()
            .filter(|(_, mode)| *mode != PassMode::Ignore)
            .count();
        Some((hidden + before) as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::int::IntTy;

    fn bytes(len: u64) -> Ty {
        Ty::Array(Box::leak(Box::new(Ty::Int(IntTy::U8))), len)
    }

    fn words(ty: IntTy, len: u64) -> Ty {
        Ty::Array(Box::leak(Box::new(Ty::Int(ty))), len)
    }

    #[test]
    fn classification_follows_the_psabi() {
        assert_eq!(classify(Ty::Int(IntTy::U64)), PassMode::Direct);
        assert_eq!(classify(Ty::Unit), PassMode::Ignore);
        assert_eq!(classify(bytes(0)), PassMode::Ignore);
        assert_eq!(classify(bytes(1)), PassMode::Cast(Cast::Word));
        assert_eq!(classify(bytes(3)), PassMode::Cast(Cast::Word));
        assert_eq!(classify(bytes(4)), PassMode::Cast(Cast::Word));
        assert_eq!(classify(bytes(5)), PassMode::Cast(Cast::Pair));
        assert_eq!(classify(words(IntTy::U32, 2)), PassMode::Cast(Cast::Pair));
        assert_eq!(classify(words(IntTy::U64, 1)), PassMode::Cast(Cast::Long));
        assert_eq!(classify(bytes(9)), PassMode::Indirect);
        assert_eq!(classify(words(IntTy::U64, 2)), PassMode::Indirect);
    }

    /// As the C struct `{ const T *ptr; size_t len; }` (ADR 0031).
    #[test]
    fn slices_cross_calls_in_two_registers() {
        let slice = Ty::Ptr(crate::mir::Ptr {
            mutable: false,
            volatile: false,
            pointee: Box::leak(Box::new(Ty::Slice(Box::leak(Box::new(Ty::Int(
                IntTy::U64,
            )))))),
        });
        assert_eq!(classify(slice), PassMode::Cast(Cast::Pair));
    }
}
