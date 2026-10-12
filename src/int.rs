//! Integer types. Shared vocabulary for every layer, like spans: an integer
//! type means the same thing in HIR, MIR and codegen.

use std::fmt;

use crate::target;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IntTy {
    U8,
    U16,
    U32,
    U64,
    Usize,
    I8,
    I16,
    I32,
    I64,
    Isize,
}

impl IntTy {
    pub const ALL: [IntTy; 10] = [
        IntTy::U8,
        IntTy::U16,
        IntTy::U32,
        IntTy::U64,
        IntTy::Usize,
        IntTy::I8,
        IntTy::I16,
        IntTy::I32,
        IntTy::I64,
        IntTy::Isize,
    ];

    pub fn name(self) -> &'static str {
        match self {
            IntTy::U8 => "u8",
            IntTy::U16 => "u16",
            IntTy::U32 => "u32",
            IntTy::U64 => "u64",
            IntTy::Usize => "usize",
            IntTy::I8 => "i8",
            IntTy::I16 => "i16",
            IntTy::I32 => "i32",
            IntTy::I64 => "i64",
            IntTy::Isize => "isize",
        }
    }

    pub fn from_name(name: &str) -> Option<IntTy> {
        IntTy::ALL.into_iter().find(|ty| ty.name() == name)
    }

    pub fn bits(self) -> u32 {
        match self {
            IntTy::U8 | IntTy::I8 => 8,
            IntTy::U16 | IntTy::I16 => 16,
            IntTy::U32 | IntTy::I32 => 32,
            IntTy::U64 | IntTy::I64 => 64,
            IntTy::Usize | IntTy::Isize => target::POINTER_BITS,
        }
    }

    pub fn is_signed(self) -> bool {
        matches!(
            self,
            IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64 | IntTy::Isize
        )
    }

    /// The largest value a literal of this type can have. Signed types also
    /// accept `max + 1` when negated (e.g. `-128` for `i8`).
    pub fn max(self) -> u64 {
        let value_bits = if self.is_signed() {
            self.bits() - 1
        } else {
            self.bits()
        };
        if value_bits == 64 {
            u64::MAX
        } else {
            (1 << value_bits) - 1
        }
    }
}

impl fmt::Display for IntTy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}
