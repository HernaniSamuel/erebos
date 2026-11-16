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
    /// The bodies reached, in the order the search found them: the roots,
    /// then by the length of their path.
    order: Vec<BodyId>,
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
        let mut order = Vec::new();
        let mut queue = VecDeque::new();
        for &root in roots {
            if found[root.0 as usize].is_none() {
                found[root.0 as usize] = Some(Found { root, via: None });
                queue.push_back(root);
                order.push(root);
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
                    order.push(call.callee);
                }
            }
        }
        Reach { found, order }
    }

    /// The bodies the search reached, in the order it found them.
    pub(super) fn order(&self) -> &[BodyId] {
        &self.order
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

/// A strongly connected component of the call graph: bodies that each reach
/// all the others.
pub(super) struct Component {
    /// Its bodies, in the order of the bodies; the first is where a cycle
    /// shown for it starts.
    pub(super) bodies: Vec<BodyId>,
    /// It holds a cycle: two or more bodies, or one that calls itself.
    pub(super) cyclic: bool,
}

/// The strongly connected components of the graph of the bodies marked in
/// `nodes`, along the `edges` of `calls` (a call to a body not marked is no
/// edge), in the order of their first bodies (ADR 0120, "The diagnostic").
/// Tarjan's algorithm with a work list of its own: it does not recurse, so
/// that a long chain of calls cannot overflow the compiler's stack.
pub(super) fn components<C: AsRef<[Call]>>(
    calls: &[C],
    nodes: &[bool],
    edges: Edges,
) -> Vec<Component> {
    let adjacent: Vec<Vec<usize>> = calls
        .iter()
        .enumerate()
        .map(|(body, list)| {
            if !nodes[body] {
                return Vec::new();
            }
            (list.as_ref().iter())
                .filter(|call| edges == Edges::CallsAndPanics || !call.check)
                .map(|call| call.callee.0 as usize)
                .filter(|&callee| nodes[callee])
                .collect()
        })
        .collect();
    const UNSEEN: usize = usize::MAX;
    let mut index = vec![UNSEEN; calls.len()];
    let mut low = vec![0; calls.len()];
    let mut open = vec![false; calls.len()];
    let mut stack = Vec::new();
    let mut counter = 0;
    let mut found = Vec::new();
    for first in 0..calls.len() {
        if !nodes[first] || index[first] != UNSEEN {
            continue;
        }
        // The bodies being visited, each with the next of its calls to follow.
        let mut work = vec![(first, 0)];
        index[first] = counter;
        low[first] = counter;
        counter += 1;
        stack.push(first);
        open[first] = true;
        while let Some(top) = work.last_mut() {
            let body = top.0;
            if let Some(&next) = adjacent[body].get(top.1) {
                top.1 += 1;
                if index[next] == UNSEEN {
                    index[next] = counter;
                    low[next] = counter;
                    counter += 1;
                    stack.push(next);
                    open[next] = true;
                    work.push((next, 0));
                } else if open[next] {
                    low[body] = low[body].min(index[next]);
                }
                continue;
            }
            work.pop();
            if let Some(&(parent, _)) = work.last() {
                low[parent] = low[parent].min(low[body]);
            }
            if low[body] != index[body] {
                continue;
            }
            let mut bodies = Vec::new();
            while let Some(member) = stack.pop() {
                open[member] = false;
                bodies.push(BodyId(member as u32));
                if member == body {
                    break;
                }
            }
            bodies.sort_by_key(|member| member.0);
            let cyclic = bodies.len() > 1 || adjacent[body].contains(&body);
            found.push(Component { bodies, cyclic });
        }
    }
    found.sort_by_key(|component| component.bodies[0].0);
    found
}

/// One cycle of `component`: the shortest path of calls from its first body
/// back to itself, through bodies of the component only, found breadth first
/// with each body's calls in order. Each call comes with the body that holds
/// it, in the order of the cycle; the callee of the last is the first body.
pub(super) fn cycle<'a, C: AsRef<[Call]>>(
    calls: &'a [C],
    component: &Component,
    edges: Edges,
) -> Vec<(BodyId, &'a Call)> {
    let Some(&start) = component.bodies.first() else {
        return Vec::new();
    };
    let mut inside = vec![false; calls.len()];
    for body in &component.bodies {
        inside[body.0 as usize] = true;
    }
    let mut via: Vec<Option<(BodyId, usize)>> = vec![None; calls.len()];
    let mut seen = vec![false; calls.len()];
    seen[start.0 as usize] = true;
    let mut queue = VecDeque::from([start]);
    while let Some(body) = queue.pop_front() {
        for (k, call) in calls[body.0 as usize].as_ref().iter().enumerate() {
            if (edges == Edges::Calls && call.check) || !inside[call.callee.0 as usize] {
                continue;
            }
            if call.callee.0 == start.0 {
                let mut path = vec![(body, &calls[body.0 as usize].as_ref()[k])];
                let mut at = body;
                while let Some((from, j)) = via[at.0 as usize] {
                    path.push((from, &calls[from.0 as usize].as_ref()[j]));
                    at = from;
                }
                path.reverse();
                return path;
            }
            if !std::mem::replace(&mut seen[call.callee.0 as usize], true) {
                via[call.callee.0 as usize] = Some((body, k));
                queue.push_back(call.callee);
            }
        }
    }
    Vec::new()
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
        assert_eq!(without.order(), [BodyId(0), BodyId(1), BodyId(3)]);
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

    #[test]
    fn a_cycle_ends_the_search_and_each_body_is_reached_once() {
        // 0 calls 1; 1 calls 2 and itself; 2 calls 1 again and 3; 3 calls 0,
        // the root. Every call back to a body already found adds nothing.
        let calls = vec![
            vec![call(1, false)],
            vec![call(2, false), call(1, false)],
            vec![call(1, false), call(3, false)],
            vec![call(0, false)],
        ];
        for edges in [Edges::Calls, Edges::CallsAndPanics] {
            let reach = Reach::search(&calls, &[BodyId(0)], edges, false);
            assert_eq!(reach.order(), [BodyId(0), BodyId(1), BodyId(2), BodyId(3)]);
            assert!((0..4).all(|b| reach.root(BodyId(b)) == Some(BodyId(0))));
            let path = reach.path(&calls, BodyId(3));
            assert_eq!(
                path.iter().map(|c| c.callee.0).collect::<Vec<_>>(),
                [1, 2, 3]
            );
            assert!(reach.path(&calls, BodyId(0)).is_empty());
        }
    }

    /// The components with each one's bodies and whether it holds a cycle.
    fn shape(calls: &[Vec<Call>]) -> Vec<(Vec<u32>, bool)> {
        let nodes = vec![true; calls.len()];
        (components(calls, &nodes, Edges::Calls).iter())
            .map(|c| (c.bodies.iter().map(|b| b.0).collect(), c.cyclic))
            .collect()
    }

    /// The cycle of the component that holds `body`, as pairs of the body
    /// that calls and the callee.
    fn cycle_of(calls: &[Vec<Call>], body: u32) -> Vec<(u32, u32)> {
        let nodes = vec![true; calls.len()];
        let all = components(calls, &nodes, Edges::Calls);
        let component = all
            .iter()
            .find(|c| c.bodies.iter().any(|b| b.0 == body))
            .unwrap();
        cycle(calls, component, Edges::Calls)
            .iter()
            .map(|(from, call)| (from.0, call.callee.0))
            .collect()
    }

    #[test]
    fn a_body_that_calls_itself_is_a_cycle_of_one_call() {
        let calls = vec![vec![call(0, false)]];
        assert_eq!(shape(&calls), [(vec![0], true)]);
        assert_eq!(cycle_of(&calls, 0), [(0, 0)]);
    }

    #[test]
    fn a_body_alone_without_a_call_to_itself_holds_no_cycle() {
        // 0 calls 1, which calls nothing.
        let calls = vec![vec![call(1, false)], Vec::new()];
        assert_eq!(shape(&calls), [(vec![0], false), (vec![1], false)]);
        let nodes = vec![true; 2];
        let all = components(&calls, &nodes, Edges::Calls);
        assert!(cycle(&calls, &all[0], Edges::Calls).is_empty());
    }

    #[test]
    fn two_cycles_through_one_body_give_the_shorter() {
        // 0 calls 1 first, and 1 calls 2, which calls 0: three calls; 0 also
        // calls 3, which calls 0: two calls.
        let calls = vec![
            vec![call(1, false), call(3, false)],
            vec![call(2, false)],
            vec![call(0, false)],
            vec![call(0, false)],
        ];
        assert_eq!(shape(&calls), [(vec![0, 1, 2, 3], true)]);
        assert_eq!(cycle_of(&calls, 0), [(0, 3), (3, 0)]);
    }

    #[test]
    fn the_cycle_follows_the_order_of_a_bodys_calls() {
        // 0 calls 2 and then 1; both call 0 back: the cycle goes through 2.
        let calls = vec![
            vec![call(2, false), call(1, false)],
            vec![call(0, false)],
            vec![call(0, false)],
        ];
        assert_eq!(cycle_of(&calls, 0), [(0, 2), (2, 0)]);
        // The first of two calls to the same body is the one kept.
        let calls = vec![vec![call(1, false), call(1, false)], vec![call(0, false)]];
        let nodes = vec![true; 2];
        let all = components(&calls, &nodes, Edges::Calls);
        let found = cycle(&calls, &all[0], Edges::Calls);
        assert!(std::ptr::eq(found[0].1, &calls[0][0]));
    }

    #[test]
    fn components_come_in_the_order_of_their_first_body() {
        // 0 calls 3; 1 and 4 call each other; 2 calls itself; 3 calls 1.
        let calls = vec![
            vec![call(3, false)],
            vec![call(4, false)],
            vec![call(2, false)],
            vec![call(1, false)],
            vec![call(1, false)],
        ];
        assert_eq!(
            shape(&calls),
            [
                (vec![0], false),
                (vec![1, 4], true),
                (vec![2], true),
                (vec![3], false)
            ]
        );
    }

    #[test]
    fn components_ignore_checks_and_bodies_not_marked() {
        // 0 calls 1; 1 calls 0 only by a check, and calls 2 (not marked),
        // which calls 1.
        let calls = vec![
            vec![call(1, false)],
            vec![call(0, true), call(2, false)],
            vec![call(1, false)],
        ];
        let nodes = [true, true, false];
        let plain = components(&calls, &nodes, Edges::Calls);
        assert!(plain.iter().all(|c| !c.cyclic) && plain.len() == 2);
        let with = components(&calls, &nodes, Edges::CallsAndPanics);
        assert!(with.len() == 1 && with[0].cyclic);
    }

    #[test]
    fn a_long_chain_of_calls_does_not_overflow_the_stack() {
        let n = 200_000;
        let calls: Vec<Vec<Call>> = (0..n)
            .map(|b| vec![call(if b + 1 < n { b + 1 } else { 0 }, false)])
            .collect();
        let nodes = vec![true; n as usize];
        let all = components(&calls, &nodes, Edges::Calls);
        assert_eq!(all.len(), 1);
        assert_eq!(cycle(&calls, &all[0], Edges::Calls).len(), n as usize);
    }
}
