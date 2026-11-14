//! What the analyses of one body share (ADR 0099, 0103): bit sets, the
//! order of spans, the blocks a body's first block reaches, and how a symbol is
//! shown.

use crate::mir::{Body, PeripheralRef};
use crate::span::{FileId, Span};

/// A set of small numbers, such as the sites or the facts that reach a point.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Bits(Vec<u64>);

impl Bits {
    pub(super) fn new(len: usize) -> Bits {
        Bits(vec![0; len.div_ceil(64)])
    }

    pub(super) fn contains(&self, i: usize) -> bool {
        self.0
            .get(i / 64)
            .is_some_and(|word| word >> (i % 64) & 1 == 1)
    }

    pub(super) fn insert(&mut self, i: usize) {
        if let Some(word) = self.0.get_mut(i / 64) {
            *word |= 1 << (i % 64);
        }
    }

    pub(super) fn remove(&mut self, i: usize) {
        if let Some(word) = self.0.get_mut(i / 64) {
            *word &= !(1 << (i % 64));
        }
    }

    /// Adds what `other` holds; whether that changed anything.
    pub(super) fn union(&mut self, other: &Bits) -> bool {
        let mut changed = false;
        for (word, &more) in self.0.iter_mut().zip(&other.0) {
            let joined = *word | more;
            changed |= joined != *word;
            *word = joined;
        }
        changed
    }
}

/// Spans are ordered by their start, then their end (ADR 0099).
pub(super) fn key(span: Span) -> (FileId, u32, u32) {
    (span.file, span.lo, span.hi)
}

/// The blocks reachable from the first, in ascending order, and for each block
/// of the body the blocks its terminator goes to (those that exist).
pub(super) fn reachable(body: &Body) -> (Vec<usize>, Vec<Vec<usize>>) {
    let successors: Vec<Vec<usize>> = body
        .blocks
        .iter()
        .map(|block| {
            (block.terminator.successors().into_iter())
                .map(|next| next.0 as usize)
                .filter(|&next| next < body.blocks.len())
                .collect()
        })
        .collect();
    let mut reached = vec![false; body.blocks.len()];
    let mut work = vec![0];
    while let Some(block) = work.pop() {
        if !std::mem::replace(&mut reached[block], true) {
            work.extend(&successors[block]);
        }
    }
    let blocks = (0..body.blocks.len()).filter(|&b| reached[b]).collect();
    (blocks, successors)
}

/// A symbol as diagnostics show it: the dots of a path written `::`
/// (ADR 0051).
pub(super) fn shown(symbol: &str) -> String {
    symbol.replace('.', "::")
}

/// The bytes a peripheral occupies, from its address to the end of its type's
/// last register; empty while its type has no layout.
fn bytes(p: PeripheralRef) -> (u64, u64) {
    let def = p.def();
    let extent = def.ty.def().layout().map_or(0, |layout| layout.extent);
    (def.address, def.address.saturating_add(extent))
}

/// Whether two peripherals share a byte (ADR 0111, 0115). A peripheral with no
/// byte meets none, even one whose address lies inside the other's range.
pub(super) fn meet(p: PeripheralRef, q: PeripheralRef) -> bool {
    let (p, q) = (bytes(p), bytes(q));
    p.0 < p.1 && q.0 < q.1 && p.0 < q.1 && q.0 < p.1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::{PeripheralLayout, PeripheralTypeRef};

    /// A peripheral of `extent` bytes at `at`.
    fn device(at: u64, extent: u64) -> PeripheralRef {
        let ty = PeripheralTypeRef::new("Device".to_owned(), Vec::new());
        ty.def().set_layout(PeripheralLayout {
            offsets: Vec::new(),
            arrays: Vec::new(),
            extent,
            align: 4,
        });
        PeripheralRef::new("D".to_owned(), at, ty)
    }

    /// `meet` is symmetric: both orders are checked.
    fn meets(p: PeripheralRef, q: PeripheralRef) -> bool {
        let forward = meet(p, q);
        assert_eq!(forward, meet(q, p), "meet is symmetric");
        forward
    }

    #[test]
    fn peripherals_sharing_a_byte_meet() {
        assert!(meets(device(0x1000, 8), device(0x1004, 8)));
        assert!(meets(device(0x1000, 8), device(0x1007, 8)));
        assert!(meets(device(0x1000, 8), device(0x1000, 8)));
    }

    #[test]
    fn adjacent_or_distant_peripherals_do_not_meet() {
        assert!(!meets(device(0x1000, 8), device(0x1008, 8)));
        assert!(!meets(device(0x1000, 8), device(0x2000, 8)));
    }

    #[test]
    fn a_peripheral_inside_another_meets_it() {
        assert!(meets(device(0x1000, 16), device(0x1004, 4)));
        assert!(meets(device(0x1000, 16), device(0x1000, 4)));
        assert!(meets(device(0x1000, 16), device(0x100c, 4)));
    }

    #[test]
    fn an_empty_peripheral_meets_no_other() {
        let outer = device(0x1000, 16);
        // Strictly inside, at the start, at the end, and just after.
        assert!(!meets(outer, device(0x1008, 0)));
        assert!(!meets(outer, device(0x1000, 0)));
        assert!(!meets(outer, device(0x100f, 0)));
        assert!(!meets(outer, device(0x1010, 0)));
        // Two at one address.
        assert!(!meets(device(0x1000, 0), device(0x1000, 0)));
    }
}
