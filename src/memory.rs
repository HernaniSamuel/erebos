//! A program's memory map with its values computed and checked
//! (ADR 0057). Like layouts, it is evaluated data rather than a stage's
//! output: `query` computes it, the `typeck` dump lists it, and MIR
//! carries it to the linker script.

use std::fmt;

use crate::span::Span;

/// A `memory` block with its values computed and checked (ADR 0057).
#[derive(Clone, Debug)]
pub struct MemoryMap {
    /// In source order, pairwise disjoint, each ending below 2³².
    pub regions: Vec<Region>,
    /// The index in `regions` of the region holding the reset entry, the
    /// code, the read-only data and the initial values of `.data`.
    pub code: usize,
    /// That of the region holding `.data` and `.bss`.
    pub data: usize,
    /// That of the region whose start holds the stack.
    pub stack: usize,
    /// The stack's size in bytes, a multiple of `target::STACK_ALIGN`.
    pub stack_size: u64,
}

/// A region of a memory map: `[origin, origin + size)`.
#[derive(Clone, Debug)]
pub struct Region {
    pub name: String,
    pub origin: u64,
    pub size: u64,
    /// The region's name in the source, where diagnostics about it point.
    pub span: Span,
}

/// As written in a `memory` block, but with the values computed and each
/// placement once, in the order `code`, `data`, `stack`.
impl fmt::Display for MemoryMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "memory {{")?;
        for region in &self.regions {
            writeln!(
                f,
                "    region {} {{ origin: {:#x}, size: {} }}",
                region.name, region.origin, region.size
            )?;
        }
        writeln!(f, "    code in {};", self.regions[self.code].name)?;
        writeln!(f, "    data in {};", self.regions[self.data].name)?;
        writeln!(
            f,
            "    stack in {} {{ size: {} }}",
            self.regions[self.stack].name, self.stack_size
        )?;
        writeln!(f, "}}")
    }
}
