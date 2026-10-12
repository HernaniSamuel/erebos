//! Mid-level IR: each function body is a control-flow graph of basic blocks.
//! This is the core of the compiler (ADR 0003): analyses run here and codegen
//! consumes only this. It is self-contained and shares no types with the HIR.
//!
//! M0 only needs terminators; statements and places arrive with M1.

mod build;

pub use build::build;

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodyId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BasicBlock(pub u32);

impl BasicBlock {
    pub const START: BasicBlock = BasicBlock(0);
}

#[derive(Debug)]
pub struct Mir {
    pub bodies: Vec<Body>,
    pub entry: BodyId,
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
    /// `blocks[0]` is the start block.
    pub blocks: Vec<BasicBlockData>,
}

impl Body {
    pub fn blocks(&self) -> impl Iterator<Item = (BasicBlock, &BasicBlockData)> {
        self.blocks
            .iter()
            .enumerate()
            .map(|(i, b)| (BasicBlock(i as u32), b))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    Unit,
    Never,
}

#[derive(Debug)]
pub struct BasicBlockData {
    pub terminator: Terminator,
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
        f.write_str(match self {
            Ty::Unit => "()",
            Ty::Never => "!",
        })
    }
}

impl fmt::Display for BasicBlock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bb{}", self.0)
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
            let entry = if id == self.entry { "entry " } else { "" };
            writeln!(f, "{entry}fn {}() -> {} {{", body.name, body.ret)?;
            for (bb, data) in body.blocks() {
                writeln!(f, "    {bb}: {{")?;
                writeln!(f, "        {};", data.terminator)?;
                writeln!(f, "    }}")?;
            }
            writeln!(f, "}}")?;
        }
        Ok(())
    }
}
