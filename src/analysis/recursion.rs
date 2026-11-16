//! The rule against recursion in code that runs on the target (ADR 0120): no
//! cycle in the run-time call graph, so that a program's stack has a bound.
//! The graph is the one of ADR 0093 with `Statement::Call` as its only edge
//! (a check that calls the panic function is none here), and the code it
//! covers is what the run-time roots reach, where a root is every body that
//! is not external and that compile time does not reach.
//!
//! Each body is scanned once for its calls, `comptime` bodies included; the
//! searches and the components then work on those scans alone.

use super::bits::{reachable, shown};
use super::call_graph::{Call, Edges, Reach, Site, components, cycle};
use crate::diag::{Diagnostic, Diagnostics};
use crate::mir::{Body, BodyId, Mir, Statement};

const RECURSION: &str = "analysis::recursion";

/// The calls of one body.
#[derive(Default)]
struct Scan {
    calls: Vec<Call>,
}

impl AsRef<[Call]> for Scan {
    fn as_ref(&self) -> &[Call] {
        &self.calls
    }
}

impl Scan {
    /// The `Statement::Call`s in the blocks reachable from the start of
    /// `body`, in the order of the blocks, then of the statements, each with
    /// the span MIR keeps for it. An external body holds none.
    fn of(body: &Body) -> Scan {
        let mut scan = Scan::default();
        if body.external {
            return scan;
        }
        let (blocks, _) = reachable(body);
        for index in blocks {
            let block = &body.blocks[index];
            for (at, statement) in block.statements.iter().enumerate() {
                let Statement::Call { callee, .. } = statement else {
                    continue;
                };
                let span = block
                    .spans
                    .get(at)
                    .copied()
                    .unwrap_or(block.terminator_span);
                scan.calls.push(Call {
                    site: Site {
                        span,
                        outside: true,
                    },
                    callee: *callee,
                    check: false,
                });
            }
        }
        scan
    }
}

/// Reports one error for each strongly connected component of the run-time
/// call graph that holds a cycle, in the order of the components' first
/// bodies.
pub fn check(mir: &Mir, diags: &mut Diagnostics) {
    let scans: Vec<Scan> = mir.bodies().map(|(_, body)| Scan::of(body)).collect();
    let at_compile_time: Vec<BodyId> = (mir.bodies())
        .filter(|(_, body)| body.comptime)
        .map(|(id, _)| id)
        .collect();
    let compile_time = Reach::search(&scans, &at_compile_time, Edges::Calls, false);
    let roots: Vec<BodyId> = (mir.bodies())
        .filter(|&(id, body)| !body.external && !body.comptime && !compile_time.reaches(id))
        .map(|(id, _)| id)
        .collect();
    let run_time = Reach::search(&scans, &roots, Edges::Calls, false);
    let nodes: Vec<bool> = (0..scans.len())
        .map(|body| run_time.reaches(BodyId(body as u32)))
        .collect();
    for component in components(&scans, &nodes, Edges::Calls) {
        if !component.cyclic {
            continue;
        }
        let path = cycle(&scans, &component, Edges::Calls);
        let name = |id: BodyId| format!("`{}`", shown(&mir.body(id).name));
        let names: Vec<String> = path.iter().map(|&(body, _)| name(body)).collect();
        let message = match names.as_slice() {
            [only] => format!("{only} calls itself"),
            [first, second] => format!("{first} and {second} call each other"),
            [init @ .., last] => {
                format!("{} and {last} call each other in a cycle", init.join(", "))
            }
            [] => continue,
        };
        let mut diag = Diagnostic::error(RECURSION, message);
        for &(body, call) in &path {
            let label = if call.callee.0 == body.0 {
                format!("{} calls itself here", name(body))
            } else {
                format!("{} calls {} here", name(body), name(call.callee))
            };
            diag = diag.label(call.site.span, label);
        }
        let mut order = names.clone();
        order.extend(names.first().cloned());
        diags.push(
            diag.note(format!("the cycle: {}", order.join(" \u{2192} ")))
                .help(
                    "a function that runs on the target may not reach itself, so that its \
                     stack has a bound; write the recursion as a loop, keeping what it needs \
                     in a fixed-size array if it needs a stack",
                ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::{BasicBlock, BasicBlockData, Terminator, Ty};
    use crate::span::{SourceMap, Span};

    fn call(callee: u32) -> Statement {
        Statement::Call {
            dest: None,
            callee: BodyId(callee),
            args: Vec::new(),
        }
    }

    fn block(calls: &[u32], terminator: Terminator) -> BasicBlockData {
        let file = SourceMap::default().add("main.ers", "");
        BasicBlockData {
            statements: calls.iter().map(|&callee| call(callee)).collect(),
            spans: (0..calls.len())
                .map(|at| Span::new(file, at, at + 1))
                .collect(),
            terminator,
            terminator_span: Span::new(file, 9, 10),
        }
    }

    fn body(blocks: Vec<BasicBlockData>) -> Body {
        let file = SourceMap::default().add("main.ers", "");
        Body {
            name: "f".to_owned(),
            span: Span::new(file, 0, 1),
            comptime: false,
            ret: Ty::Unit,
            param_count: 0,
            locals: Vec::new(),
            blocks,
            runtime: false,
            external: false,
            naked: false,
            align: None,
        }
    }

    fn callees(scan: &Scan) -> Vec<u32> {
        scan.calls.iter().map(|call| call.callee.0).collect()
    }

    /// MIR keeps no block that nothing reaches in any form of source that was
    /// tried (code after `return`, `loop {}`, a call to a `-> !` function, a
    /// `match` or an `if` whose branches all return), so the case is a body
    /// built by hand: its block 1, which calls body 8, is reached from no
    /// block.
    #[test]
    fn a_call_in_a_block_that_cannot_be_reached_is_no_edge() {
        let body = body(vec![
            block(&[7], Terminator::Goto(BasicBlock(2))),
            block(&[8], Terminator::Return),
            block(&[9], Terminator::Return),
        ]);
        assert_eq!(callees(&Scan::of(&body)), [7, 9]);
    }

    #[test]
    fn calls_come_in_the_order_of_the_blocks_then_the_statements() {
        let body = body(vec![
            block(&[5, 3], Terminator::Goto(BasicBlock(2))),
            block(&[1], Terminator::Return),
            block(&[4, 2], Terminator::Goto(BasicBlock(1))),
        ]);
        assert_eq!(callees(&Scan::of(&body)), [5, 3, 1, 4, 2]);
    }

    #[test]
    fn a_comptime_body_is_scanned_and_an_external_one_is_not() {
        let mut compile_time = body(vec![block(&[3], Terminator::Return)]);
        compile_time.comptime = true;
        assert_eq!(callees(&Scan::of(&compile_time)), [3]);
        let mut external = body(vec![block(&[3], Terminator::Return)]);
        external.external = true;
        assert!(Scan::of(&external).calls.is_empty());
    }
}
