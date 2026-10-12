//! Operators. Shared vocabulary for every layer, like integer types: the
//! meaning of `+` does not change between AST, HIR and MIR.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    /// `-x` (signed integers only)
    Neg,
    /// `!x`: logical not on `bool`, bitwise not on integers
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
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// `&&` and `||`. Not a [`BinOp`]: the right side is evaluated only when
/// needed, so they are control flow rather than operations on two values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogicalOp {
    And,
    Or,
}

impl LogicalOp {
    pub fn symbol(self) -> &'static str {
        match self {
            LogicalOp::And => "&&",
            LogicalOp::Or => "||",
        }
    }
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
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
        }
    }

    /// Comparisons produce `bool` from two operands of one type.
    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
        )
    }

    /// `&`, `|` and `^`, which also apply to `bool`.
    pub fn is_bitwise(self) -> bool {
        matches!(self, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor)
    }

    pub fn is_shift(self) -> bool {
        matches!(self, BinOp::Shl | BinOp::Shr)
    }
}
