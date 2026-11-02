//! The extent of a peripheral type, and the peripherals that share bytes
//! without `alternatePeripheral` (ADR 0072): which of them are `over`
//! another.

use crate::span::Span;

use super::{Item, Repeat};

/// An extent past 2³² is only compared with 2³², and saturates at the first
/// value that does not fit, as the compiler's does (ADR 0064).
const PAST_ADDRESS_SPACE: u128 = (1 << 32) + 1;

/// The width in bytes of a register of `size` bits, if the language has one
/// (ADR 0058).
pub(super) fn width(size: u32) -> Option<u64> {
    match size {
        8 => Some(1),
        16 => Some(2),
        32 => Some(4),
        _ => None,
    }
}

/// The extent of a peripheral type with these entries: the end of the last
/// byte any of them occupies, as the compiler computes it (ADR 0058, 0064).
/// A register of a size the language has no register of is left out, so it
/// occupies nothing.
pub(super) fn extent(items: &[Item]) -> u128 {
    items
        .iter()
        .map(|item| match item {
            Item::Register(r) => width(r.size).map_or(0, |bytes| {
                end(r.offset, r.repeat.as_ref(), u128::from(bytes))
            }),
            Item::Cluster(c) => end(c.offset, c.repeat.as_ref(), extent(&c.items)),
        })
        .max()
        .unwrap_or(0)
}

/// The end of the last byte of an entry's elements, each `size` bytes.
fn end(offset: u64, repeat: Option<&Repeat>, size: u128) -> u128 {
    if size == 0 {
        return 0;
    }
    let (count, step) = match repeat {
        None => (1, 0),
        Some(Repeat::Array { count, step }) => (u128::from(*count), u128::from(*step)),
        Some(Repeat::List { indices, step }) => (indices.len() as u128, u128::from(*step)),
    };
    let last = count.saturating_sub(1) * step;
    (u128::from(offset) + last + size).min(PAST_ADDRESS_SPACE)
}

/// A peripheral as the generator writes it, one per element of a list.
pub(super) struct Placed {
    pub span: Span,
    /// The first byte it covers and one past the last: equal for a
    /// peripheral of an empty type, which covers none.
    pub start: u128,
    pub end: u128,
    /// The `over` its SVD states (`alternatePeripheral`), as an index of the
    /// list.
    pub explicit: Option<usize>,
}

/// What the generator writes after a peripheral's address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Over {
    /// No `over`: it shares no byte with an earlier peripheral.
    No,
    /// The SVD states it.
    Stated(usize),
    /// Found from the bytes it shares with these earlier peripherals.
    Found { target: usize, shares: Vec<usize> },
    /// It shares bytes with these earlier peripherals, which no one `over`
    /// names: written without one.
    Unresolved(Vec<usize>),
}

impl Over {
    /// The peripheral it is written `over`.
    fn target(&self) -> Option<usize> {
        match self {
            Over::Stated(target) | Over::Found { target, .. } => Some(*target),
            Over::No | Over::Unresolved(_) => None,
        }
    }
}

fn share_a_byte(a: &Placed, b: &Placed) -> bool {
    a.start < a.end && b.start < b.end && a.start < b.end && b.start < a.end
}

/// For each peripheral, in the order of the file, its `over` (ADR 0072). Let
/// `S` be the earlier peripherals a peripheral `P` shares a byte with. When
/// `S` is empty, `P` has none. Otherwise the candidates are the peripherals,
/// other than `P`, such that every peripheral of `S` is the candidate or is
/// `over` it; `P` is `over` the first of them in the order of the file. A
/// candidate has to be the first peripheral of `S` or the one it is `over`,
/// since it is or is `over` the candidate, so only those two are looked at.
pub(super) fn find(placed: &[Placed]) -> Vec<Over> {
    let mut overs: Vec<Over> = Vec::with_capacity(placed.len());
    for (p, this) in placed.iter().enumerate() {
        let over = match this.explicit {
            Some(target) => Over::Stated(target),
            None => {
                let shares: Vec<usize> = (0..p)
                    .filter(|&earlier| share_a_byte(&placed[earlier], this))
                    .collect();
                match shares.first() {
                    None => Over::No,
                    Some(&first) => {
                        let fits = |candidate: usize| {
                            candidate != p
                                && shares.iter().all(|&s| {
                                    s == candidate || overs[s].target() == Some(candidate)
                                })
                        };
                        let candidates = [Some(first), overs[first].target()];
                        match candidates.into_iter().flatten().filter(|&c| fits(c)).min() {
                            Some(target) => Over::Found { target, shares },
                            None => Over::Unresolved(shares),
                        }
                    }
                }
            }
        };
        overs.push(over);
    }
    overs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::SourceMap;

    fn placed(ranges: &[(u128, u128, Option<usize>)]) -> Vec<Placed> {
        let mut sources = SourceMap::default();
        let file = sources.add("x.svd", "");
        ranges
            .iter()
            .map(|&(start, end, explicit)| Placed {
                span: Span::new(file, 0, 0),
                start,
                end,
                explicit,
            })
            .collect()
    }

    #[test]
    fn disjoint_peripherals_have_no_over() {
        let overs = find(&placed(&[(0, 4, None), (4, 8, None), (8, 8, None)]));
        assert_eq!(overs, [Over::No, Over::No, Over::No]);
    }

    #[test]
    fn the_later_of_two_is_over_the_earlier() {
        let overs = find(&placed(&[(0, 16, None), (8, 12, None)]));
        assert_eq!(
            overs[1],
            Over::Found {
                target: 0,
                shares: vec![0]
            }
        );
    }

    #[test]
    fn a_third_peripheral_is_over_the_first_both_are_over() {
        // A; B and C both overlap A, and each other.
        let overs = find(&placed(&[(0, 16, None), (0, 8, None), (4, 12, None)]));
        assert_eq!(overs[1].target(), Some(0));
        assert_eq!(overs[2].target(), Some(0));
    }

    #[test]
    fn one_covering_two_that_are_over_a_third_it_does_not_overlap() {
        // A at 100..110 stated over by B (0..8) and C (8..16), D covers both.
        let overs = find(&placed(&[
            (100, 110, None),
            (0, 8, Some(0)),
            (8, 16, Some(0)),
            (0, 16, None),
        ]));
        assert_eq!(overs[3].target(), Some(0));
    }

    #[test]
    fn one_covering_only_b_is_over_the_first_candidate() {
        // B is over A; D shares with B alone: B and A are both candidates.
        let overs = find(&placed(&[(100, 110, None), (0, 8, Some(0)), (4, 6, None)]));
        assert_eq!(
            overs[2],
            Over::Found {
                target: 0,
                shares: vec![1]
            }
        );
    }

    #[test]
    fn one_covering_two_that_do_not_overlap_is_unresolved() {
        let overs = find(&placed(&[(0, 4, None), (8, 12, None), (0, 12, None)]));
        assert_eq!(overs[2], Over::Unresolved(vec![0, 1]));
    }

    #[test]
    fn an_empty_peripheral_shares_nothing() {
        let overs = find(&placed(&[(0, 8, None), (4, 4, None)]));
        assert_eq!(overs[1], Over::No);
    }
}
