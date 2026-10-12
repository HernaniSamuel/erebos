//! Operators. Shared vocabulary for every layer, like integer types: the
//! meaning of `+` does not change between AST, HIR and MIR.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    /// `-x` (signed integers only)
    Neg,
    /// `!x`, bitwise not on integers
    Not,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

impl BinOp {
    pub fn symbol(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::BitAnd => "&",
            BinOp::BitOr => "|",
            BinOp::BitXor => "^",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
        }
    }

    pub fn is_shift(self) -> bool {
        matches!(self, BinOp::Shl | BinOp::Shr)
    }
}
