//! Mid-level IR: each function body is a control-flow graph of basic blocks
//! holding simple statements over numbered locals. This is the core of the
//! compiler (ADR 0003): analyses run here and codegen consumes only this.
//! Apart from the shared vocabulary (integer types, operators) it shares no
//! types with the HIR.
//!
//! Every value lives in a local, and every read of a local is explicit
//! (`copy _1`). That is the shape later analyses, such as borrow checking
//! (ADR 0009), need.

mod build;

pub use build::build;

use std::fmt;

pub use crate::int::IntTy;
pub use crate::ops::{BinOp, UnOp};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodyId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BasicBlock(pub u32);

impl BasicBlock {
    pub const START: BasicBlock = BasicBlock(0);
}

/// A local slot of a body. `_0` holds the return value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Local(pub u32);

impl Local {
    pub const RETURN: Local = Local(0);
}

#[derive(Debug)]
pub struct Mir {
    pub bodies: Vec<Body>,
    pub entry: BodyId,
    pub panic: BodyId,
}

impl Mir {
    pub fn bodies(&self) -> impl Iterator<Item = (BodyId, &Body)> {
        self.bodies
            .iter()
            .enumerate()
            .map(|(i, b)| (BodyId(i as u32), b))
    }

    pub fn body(&self, id: BodyId) -> &Body {
        &self.bodies[id.0 as usize]
    }
}

#[derive(Debug)]
pub struct Body {
    /// Symbol name; Erebos uses the C ABI and does not mangle (ADR 0006).
    pub name: String,
    pub ret: Ty,
    /// `locals[0]` is the return place.
    pub locals: Vec<LocalDecl>,
    /// `blocks[0]` is the start block; nothing jumps back to it.
    pub blocks: Vec<BasicBlockData>,
}

impl Body {
    pub fn blocks(&self) -> impl Iterator<Item = (BasicBlock, &BasicBlockData)> {
        self.blocks
            .iter()
            .enumerate()
            .map(|(i, b)| (BasicBlock(i as u32), b))
    }

    pub fn locals(&self) -> impl Iterator<Item = (Local, &LocalDecl)> {
        self.locals
            .iter()
            .enumerate()
            .map(|(i, l)| (Local(i as u32), l))
    }

    pub fn local_ty(&self, local: Local) -> Ty {
        self.locals[local.0 as usize].ty
    }

    pub fn operand_ty(&self, operand: &Operand) -> Ty {
        match operand {
            Operand::Copy(place) => self.local_ty(place.local),
            Operand::Const(Const::Int(_, int)) => Ty::Int(*int),
            Operand::Const(Const::Unit) => Ty::Unit,
        }
    }
}

#[derive(Debug)]
pub struct LocalDecl {
    pub ty: Ty,
    /// The source name, for user variables; temporaries have none.
    pub name: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    Unit,
    Never,
    Int(IntTy),
}

#[derive(Debug)]
pub struct BasicBlockData {
    pub statements: Vec<Statement>,
    pub terminator: Terminator,
}

#[derive(Debug)]
pub enum Statement {
    Assign(Place, Rvalue),
}

/// A memory location. Projections (such as dereferences) arrive with
/// pointers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Place {
    pub local: Local,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operand {
    Copy(Place),
    Const(Const),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Const {
    /// The bits of an integer of the given type.
    Int(u64, IntTy),
    Unit,
}

#[derive(Debug)]
pub enum Rvalue {
    Use(Operand),
    Unary(UnOp, Operand),
    Binary(BinOp, Operand, Operand),
    Cast(Operand, IntTy),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Terminator {
    Goto(BasicBlock),
    Return,
    /// Control never reaches the end of this block.
    Unreachable,
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Unit => f.write_str("()"),
            Ty::Never => f.write_str("!"),
            Ty::Int(int) => write!(f, "{int}"),
        }
    }
}

impl fmt::Display for BasicBlock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bb{}", self.0)
    }
}

impl fmt::Display for Local {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "_{}", self.0)
    }
}

impl fmt::Display for Place {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.local)
    }
}

impl fmt::Display for Operand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Operand::Copy(place) => write!(f, "copy {place}"),
            Operand::Const(Const::Int(value, int)) => write!(f, "const {value}_{int}"),
            Operand::Const(Const::Unit) => f.write_str("const ()"),
        }
    }
}

impl fmt::Display for Rvalue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Rvalue::Use(operand) => write!(f, "{operand}"),
            Rvalue::Unary(op, operand) => write!(f, "{op:?}({operand})"),
            Rvalue::Binary(op, lhs, rhs) => write!(f, "{op:?}({lhs}, {rhs})"),
            Rvalue::Cast(operand, int) => write!(f, "{operand} as {int}"),
        }
    }
}

impl fmt::Display for Statement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Statement::Assign(place, rvalue) => write!(f, "{place} = {rvalue}"),
        }
    }
}

impl fmt::Display for Terminator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Terminator::Goto(target) => write!(f, "goto -> {target}"),
            Terminator::Return => f.write_str("return"),
            Terminator::Unreachable => f.write_str("unreachable"),
        }
    }
}

impl fmt::Display for Mir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, (id, body)) in self.bodies().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            let kind = if id == self.entry {
                "entry "
            } else if id == self.panic {
                "panic "
            } else {
                ""
            };
            writeln!(f, "{kind}fn {}() -> {} {{", body.name, body.ret)?;
            for (local, decl) in body.locals() {
                write!(f, "    let {local}: {};", decl.ty)?;
                match &decl.name {
                    Some(name) => writeln!(f, " // {name}")?,
                    None => writeln!(f)?,
                }
            }
            for (bb, data) in body.blocks() {
                writeln!(f)?;
                writeln!(f, "    {bb}: {{")?;
                for statement in &data.statements {
                    writeln!(f, "        {statement};")?;
                }
                writeln!(f, "        {};", data.terminator)?;
                writeln!(f, "    }}")?;
            }
            writeln!(f, "}}")?;
        }
        Ok(())
    }
}
