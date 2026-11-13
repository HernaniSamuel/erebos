//! What the analyses of one body share (ADR 0099, 0103): bit sets, the
//! order of spans, the blocks a body's first block reaches, and how a symbol is
//! shown.

use crate::mir::Body;
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
