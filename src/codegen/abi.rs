//! How values cross calls in the C ABI of each target (ADR 0006): the
//! integer calling convention of the RISC-V psABI for ilp32, as clang
//! implements it for `-mabi=ilp32` (ADR 0025), and the AAPCS of ARMv7-M as
//! clang implements it for `thumbv7m-none-eabi` (ADR 0075).

use crate::mir::{Body, Ty};
use crate::target::{self, Arch};

/// How one parameter or return value crosses a call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PassMode {
    /// Not passed at all: `()`, `!` and zero-sized aggregates.
    Ignore,
    /// A scalar, as itself.
    Direct,
    /// An aggregate in registers: its bytes, as laid out in memory, read as
    /// the given integer type or array of integers.
    Cast(Cast),
    /// An aggregate in memory. An argument is a pointer to a copy that the
    /// caller makes; a return value is written to memory the caller
    /// provides, whose address is a hidden first argument.
    Indirect,
    /// An argument aggregate that the callee receives as its own copy, which
    /// the call makes in the argument area (LLVM's `byval`), aligned to
    /// `align` bytes there (ADR 0075). Never a return value.
    ByVal { align: u32 },
}

/// The register form of an aggregate: an integer, or an array of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cast {
    /// One byte, `i8`: a returned aggregate of size 1 on ARMv7-M.
    Byte,
    /// Two bytes, `i16`: a returned aggregate of size 2 on ARMv7-M.
    Half,
    /// One register: `i32`.
    Word,
    /// A register pair holding an aggregate aligned to 8: `i64`.
    Long,
    /// `[N x i32]`: `N` registers or stack words.
    Words(u32),
    /// `[N x i64]`: `N` register pairs or double words, the first at an even
    /// register (ADR 0075).
    Longs(u32),
}

/// Whether a value is a parameter or a return value: ARMv7-M passes an
/// aggregate in one form and returns it in another (ADR 0075).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
    Param,
    Return,
}

/// How a value of type `ty` crosses a call on `arch`, as a parameter or as
/// the return value: scalars as themselves, `()` and `!` not at all,
/// aggregates by the architecture's table (ADR 0073).
pub fn classify(arch: Arch, ty: Ty, position: Position) -> PassMode {
    if !ty.is_aggregate() {
        return match ty {
            Ty::Unit | Ty::Never => PassMode::Ignore,
            _ => PassMode::Direct,
        };
    }
    match arch {
        Arch::Riscv32 => classify_riscv32(ty),
        Arch::Armv7m => classify_armv7m(ty, position),
    }
}

/// The table of ADR 0025 for an aggregate, which covers enums with
/// payloads as aggregates (ADR 0039).
fn classify_riscv32(ty: Ty) -> PassMode {
    let xlen = target::POINTER_BYTES;
    let layout = ty.layout();
    match layout.size {
        0 => PassMode::Ignore,
        size if size <= xlen => PassMode::Cast(Cast::Word),
        size if size <= 2 * xlen && layout.align == 2 * xlen => PassMode::Cast(Cast::Long),
        size if size <= 2 * xlen => PassMode::Cast(Cast::Words(2)),
        _ => PassMode::Indirect,
    }
}

/// The table of ADR 0075 for an aggregate.
fn classify_armv7m(ty: Ty, position: Position) -> PassMode {
    let size = ty.layout().size;
    let align = natural_align(ty);
    match (size, position) {
        (0, _) => PassMode::Ignore,
        (1, Position::Return) => PassMode::Cast(Cast::Byte),
        (2, Position::Return) => PassMode::Cast(Cast::Half),
        (3 | 4, Position::Return) => PassMode::Cast(Cast::Word),
        (_, Position::Return) => PassMode::Indirect,
        (size, Position::Param) if size > ARMV7M_REGISTER_FORM_MAX => PassMode::ByVal {
            align: align.clamp(4, 8) as u32,
        },
        (size, Position::Param) if align >= 8 => PassMode::Cast(Cast::Longs(units(size, 8))),
        (size, Position::Param) => PassMode::Cast(Cast::Words(units(size, 4))),
    }
}

/// The largest aggregate AAPCS passes as an array of integers on ARMv7-M:
/// 16 words (ADR 0075).
const ARMV7M_REGISTER_FORM_MAX: u64 = 64;

/// How many units of `unit` bytes hold `size` bytes, a size of at most
/// [`ARMV7M_REGISTER_FORM_MAX`].
fn units(size: u64, unit: u64) -> u32 {
    size.div_ceil(unit) as u32
}

/// The natural alignment of an aggregate on ARMv7-M (ADR 0075): the largest
/// alignment of its fields, elements or variants, each with its own
/// `#[align(N)]` but without the aggregate's own, and 1 for a `#[packed]`
/// struct. For every other type, its alignment.
fn natural_align(ty: Ty) -> u64 {
    match ty {
        Ty::Struct(s) if s.def().packed => 1,
        Ty::Struct(s) => s
            .def()
            .fields()
            .iter()
            .map(|field| field.ty.layout().align)
            .max()
            .unwrap_or(1),
        // An array's alignment is its element's, an enum's its tag's or
        // its payloads': neither has an `#[align(N)]` of its own.
        _ => ty.layout().align,
    }
}

/// How a function's return value and parameters cross a call.
pub struct FnAbi {
    pub ret: Ty,
    pub ret_mode: PassMode,
    pub params: Vec<(Ty, PassMode)>,
}

impl FnAbi {
    pub fn of(arch: Arch, body: &Body) -> FnAbi {
        FnAbi {
            ret: body.ret,
            ret_mode: classify(arch, body.ret, Position::Return),
            params: body
                .params()
                .iter()
                .map(|param| (param.ty, classify(arch, param.ty, Position::Param)))
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
    use crate::layout::Layout;
    use crate::mir::{Field, Ptr, StructRef};

    /// `super::classify` on riscv32 as a parameter, which is also how it
    /// returns; it shadows the glob import.
    fn classify(ty: Ty) -> PassMode {
        super::classify(Arch::Riscv32, ty, Position::Param)
    }

    fn bytes(len: u64) -> Ty {
        Ty::Array(Box::leak(Box::new(Ty::Int(IntTy::U8))), len)
    }

    fn words(ty: IntTy, len: u64) -> Ty {
        Ty::Array(Box::leak(Box::new(Ty::Int(ty))), len)
    }

    /// A struct of the given size and alignment (its own `#[align(N)]`
    /// included) whose fields have the given types, at offsets that do not
    /// matter to the ABI.
    fn strukt(fields: &[Ty], size: u64, align: u64, packed: bool) -> Ty {
        let def = StructRef::new("S".to_owned(), Layout { size, align }, packed, true, false);
        def.def().set_fields(
            fields
                .iter()
                .enumerate()
                .map(|(i, &ty)| Field {
                    name: format!("f{i}"),
                    ty,
                    offset: 0,
                })
                .collect(),
        );
        Ty::Struct(def)
    }

    fn slice() -> Ty {
        Ty::Ptr(Ptr {
            mutable: false,
            volatile: false,
            pointee: Box::leak(Box::new(Ty::Slice(Box::leak(Box::new(Ty::Int(
                IntTy::U64,
            )))))),
        })
    }

    #[test]
    fn classification_follows_the_psabi() {
        assert_eq!(classify(Ty::Int(IntTy::U64)), PassMode::Direct);
        assert_eq!(classify(Ty::Unit), PassMode::Ignore);
        assert_eq!(classify(bytes(0)), PassMode::Ignore);
        assert_eq!(classify(bytes(1)), PassMode::Cast(Cast::Word));
        assert_eq!(classify(bytes(3)), PassMode::Cast(Cast::Word));
        assert_eq!(classify(bytes(4)), PassMode::Cast(Cast::Word));
        assert_eq!(classify(bytes(5)), PassMode::Cast(Cast::Words(2)));
        assert_eq!(
            classify(words(IntTy::U32, 2)),
            PassMode::Cast(Cast::Words(2))
        );
        assert_eq!(classify(words(IntTy::U64, 1)), PassMode::Cast(Cast::Long));
        assert_eq!(classify(bytes(9)), PassMode::Indirect);
        assert_eq!(classify(words(IntTy::U64, 2)), PassMode::Indirect);
    }

    /// As the C struct `{ const T *ptr; size_t len; }` (ADR 0031).
    #[test]
    fn slices_cross_calls_in_two_registers() {
        assert_eq!(classify(slice()), PassMode::Cast(Cast::Words(2)));
    }

    /// riscv32 passes and returns an aggregate the same way.
    #[test]
    fn riscv32_does_not_tell_parameters_from_returns() {
        for ty in [
            bytes(0),
            bytes(3),
            bytes(8),
            words(IntTy::U64, 1),
            bytes(20),
        ] {
            assert_eq!(
                super::classify(Arch::Riscv32, ty, Position::Param),
                super::classify(Arch::Riscv32, ty, Position::Return)
            );
        }
    }

    /// The pass modes of an aggregate on ARMv7-M, as a parameter and as the
    /// return value.
    fn armv7m(ty: Ty) -> (PassMode, PassMode) {
        (
            super::classify(Arch::Armv7m, ty, Position::Param),
            super::classify(Arch::Armv7m, ty, Position::Return),
        )
    }

    fn words_of(n: u32) -> PassMode {
        PassMode::Cast(Cast::Words(n))
    }

    fn longs_of(n: u32) -> PassMode {
        PassMode::Cast(Cast::Longs(n))
    }

    /// Every row of the table in ADR 0075's context, which are clang's
    /// forms for the C twin of each type.
    #[test]
    fn armv7m_follows_the_aapcs_rows() {
        let ignore = (PassMode::Ignore, PassMode::Ignore);
        let sret = PassMode::Indirect;
        // `struct {}`.
        assert_eq!(armv7m(strukt(&[], 0, 1, false)), ignore);
        assert_eq!(armv7m(bytes(0)), ignore);
        // 1, 2, 3 and 4 bytes.
        assert_eq!(armv7m(bytes(1)), (words_of(1), PassMode::Cast(Cast::Byte)));
        assert_eq!(armv7m(bytes(2)), (words_of(1), PassMode::Cast(Cast::Half)));
        assert_eq!(armv7m(bytes(3)), (words_of(1), PassMode::Cast(Cast::Word)));
        assert_eq!(armv7m(bytes(4)), (words_of(1), PassMode::Cast(Cast::Word)));
        assert_eq!(
            armv7m(words(IntTy::U32, 1)),
            (words_of(1), PassMode::Cast(Cast::Word))
        );
        // 5 bytes of alignment 1.
        assert_eq!(armv7m(bytes(5)), (words_of(2), sret));
        // `{ uint64_t }`.
        let long = strukt(&[Ty::Int(IntTy::U64)], 8, 8, false);
        assert_eq!(armv7m(long), (longs_of(1), sret));
        assert_eq!(armv7m(words(IntTy::U64, 1)), (longs_of(1), sret));
        // 64 bytes of alignment 4 are the largest in registers and stack.
        assert_eq!(armv7m(words(IntTy::U32, 16)), (words_of(16), sret));
        // 68 bytes of alignment 4, and 72 of alignment 8, by copy.
        assert_eq!(
            armv7m(words(IntTy::U32, 17)),
            (PassMode::ByVal { align: 4 }, sret)
        );
        assert_eq!(
            armv7m(words(IntTy::U64, 9)),
            (PassMode::ByVal { align: 8 }, sret)
        );
        // 64 bytes of alignment 8 still go in `[8 x i64]`.
        assert_eq!(armv7m(words(IntTy::U64, 8)), (longs_of(8), sret));
        // 16 bytes `aligned(16)` whose fields have alignment 4: the
        // aggregate's own alignment does not count.
        let aligned = strukt(&[words(IntTy::U32, 4)], 16, 16, false);
        assert_eq!(armv7m(aligned), (words_of(4), sret));
        // 16 bytes holding a field of an `aligned(16)` type: the field's
        // alignment does.
        let holder = strukt(&[aligned], 16, 16, false);
        assert_eq!(armv7m(holder), (longs_of(2), sret));
        // 80 bytes `aligned(16)` with fields of alignment 4: by copy,
        // aligned to 4.
        let big = strukt(&[words(IntTy::U32, 20)], 80, 16, false);
        assert_eq!(armv7m(big), (PassMode::ByVal { align: 4 }, sret));
        // ... and aligned to 8 (clamped from 16) with a field of alignment
        // 16.
        let big_holder = strukt(&[aligned, words(IntTy::U32, 16)], 80, 16, false);
        assert_eq!(armv7m(big_holder), (PassMode::ByVal { align: 8 }, sret));
        // 9 bytes, `packed`: alignment 1.
        let fields = [Ty::Int(IntTy::U32), Ty::Int(IntTy::U32), Ty::Int(IntTy::U8)];
        let packed = strukt(&fields, 9, 1, true);
        assert_eq!(armv7m(packed), (words_of(3), sret));
        // A slice, as `{ const T *ptr; size_t len; }`.
        assert_eq!(armv7m(slice()), (words_of(2), sret));
    }

    /// Scalars and `()` cross as on every target.
    #[test]
    fn armv7m_scalars_cross_as_themselves() {
        for position in [Position::Param, Position::Return] {
            let classify = |ty| super::classify(Arch::Armv7m, ty, position);
            assert_eq!(classify(Ty::Int(IntTy::U64)), PassMode::Direct);
            assert_eq!(classify(Ty::Bool), PassMode::Direct);
            assert_eq!(classify(Ty::Unit), PassMode::Ignore);
            assert_eq!(classify(Ty::Never), PassMode::Ignore);
        }
    }

    /// An array counts by its elements' alignment, a struct holding only
    /// `#[packed]` ones by 1.
    #[test]
    fn armv7m_natural_alignment_of_nested_aggregates() {
        let packed = strukt(&[Ty::Int(IntTy::U64)], 8, 1, true);
        assert_eq!(natural_align(packed), 1);
        // Not packed itself, but holding packed structs: alignment 1.
        let outer = strukt(&[packed, packed], 16, 1, false);
        assert_eq!(natural_align(outer), 1);
        assert_eq!(armv7m(outer), (words_of(4), PassMode::Indirect));
        // An array of 8-aligned words, in a struct and in an array.
        let longs = strukt(&[words(IntTy::U64, 2)], 16, 8, false);
        assert_eq!(natural_align(longs), 8);
        assert_eq!(natural_align(Ty::Array(Box::leak(Box::new(longs)), 2)), 8);
    }
}
