//! The walk of the call graph (ADR 0093, 0112): a breadth-first search from
//! some bodies over the calls MIR shows, in the order of the bodies' calls,
//! that keeps for each body it reaches the shortest path of calls found. The
//! check of shared statics and the check of the conflicts across calls both
//! use it, each with the edges it follows.

use std::collections::VecDeque;

use crate::mir::BodyId;
use crate::span::Span;

/// Where a call is in a body.
#[derive(Clone, Copy)]
pub(super) struct Site {
    pub(super) span: Span,
    /// Some path from the start of the body reaches it outside every
    /// `critical` block (ADR 0093, "Inside or outside `critical`").
    pub(super) outside: bool,
}

/// A call MIR shows: a `Statement::Call`, or a check that calls the panic
/// function (a `Terminator::Panic`).
pub(super) struct Call {
    pub(super) site: Site,
    pub(super) callee: BodyId,
    pub(super) check: bool,
}

/// The edges a search follows (ADR 0112).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Edges {
    /// `Statement::Call` only.
    Calls,
    /// `Statement::Call` and `Terminator::Panic`, the checks that call the
    /// panic function.
    CallsAndPanics,
}

/// How a search reached a body: from the root it started at, through the
/// call at `via` in the body it came from; none for a root.
#[derive(Clone, Copy)]
struct Found {
    root: BodyId,
    via: Option<(BodyId, usize)>,
}

/// The bodies a search reaches, each with the shortest path found.
pub(super) struct Reach {
    found: Vec<Option<Found>>,
}

impl Reach {
    /// A breadth-first search from `roots`, in order, over the calls of
    /// `calls`, one list per body in the order of the bodies, each in order,
    /// along the `edges`. With `outside_only` it follows the calls that some
    /// path reaches outside `critical`, and no others.
    pub(super) fn search<C: AsRef<[Call]>>(
        calls: &[C],
        roots: &[BodyId],
        edges: Edges,
        outside_only: bool,
    ) -> Reach {
        let mut found: Vec<Option<Found>> = vec![None; calls.len()];
        let mut queue = VecDeque::new();
        for &root in roots {
            if found[root.0 as usize].is_none() {
                found[root.0 as usize] = Some(Found { root, via: None });
                queue.push_back(root);
            }
        }
        while let Some(body) = queue.pop_front() {
            let Some(here) = found[body.0 as usize] else {
                continue;
            };
            for (k, call) in calls[body.0 as usize].as_ref().iter().enumerate() {
                if edges == Edges::Calls && call.check {
                    continue;
                }
                if outside_only && !call.site.outside {
                    continue;
                }
                let slot = &mut found[call.callee.0 as usize];
                if slot.is_none() {
                    *slot = Some(Found {
                        root: here.root,
                        via: Some((body, k)),
                    });
                    queue.push_back(call.callee);
                }
            }
        }
        Reach { found }
    }

    pub(super) fn reaches(&self, body: BodyId) -> bool {
        self.found[body.0 as usize].is_some()
    }

    /// The root the search found `body` from.
    pub(super) fn root(&self, body: BodyId) -> Option<BodyId> {
        self.found[body.0 as usize].map(|found| found.root)
    }

    /// The calls from a root to `body`, in order, as the search followed them.
    pub(super) fn path<'a, C: AsRef<[Call]>>(&self, calls: &'a [C], body: BodyId) -> Vec<&'a Call> {
        let mut path = Vec::new();
        let mut at = body;
        while let Some(Found {
            via: Some((from, k)),
            ..
        }) = self.found[at.0 as usize]
        {
            path.push(&calls[from.0 as usize].as_ref()[k]);
            at = from;
        }
        path.reverse();
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::SourceMap;

    fn call(callee: u32, check: bool) -> Call {
        let file = SourceMap::default().add("main.ers", "");
        Call {
            site: Site {
                span: Span::new(file, 0, 1),
                outside: true,
            },
            callee: BodyId(callee),
            check,
        }
    }

    #[test]
    fn a_check_is_an_edge_only_when_asked_for() {
        // 0 calls 1 and, by a check, the panic function 2; 1 calls 3.
        let calls = vec![
            vec![call(1, false), call(2, true)],
            vec![call(3, false)],
            Vec::new(),
            Vec::new(),
        ];
        let roots = [BodyId(0)];
        let with = Reach::search(&calls, &roots, Edges::CallsAndPanics, false);
        assert!((0..4).all(|b| with.reaches(BodyId(b))));
        assert_eq!(with.path(&calls, BodyId(2)).len(), 1);
        assert_eq!(with.path(&calls, BodyId(3)).len(), 2);
        let without = Reach::search(&calls, &roots, Edges::Calls, false);
        assert!(without.reaches(BodyId(3)) && !without.reaches(BodyId(2)));
        assert_eq!(without.root(BodyId(3)), Some(BodyId(0)));
        assert_eq!(without.root(BodyId(2)), None);
    }

    #[test]
    fn the_first_path_found_is_kept_and_a_body_is_reached_once() {
        // 0 calls 1 and 2; both call 3: 3 is reached through 1, the first.
        let calls = vec![
            vec![call(1, false), call(2, false)],
            vec![call(3, false)],
            vec![call(3, false)],
            Vec::new(),
        ];
        let reach = Reach::search(&calls, &[BodyId(0)], Edges::Calls, false);
        let path = reach.path(&calls, BodyId(3));
        assert_eq!(path.iter().map(|c| c.callee.0).collect::<Vec<_>>(), [1, 3]);
    }
}
