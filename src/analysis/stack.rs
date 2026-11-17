//! The stack bound (ADR 0123, 0122): the most stack a program can use, from
//! the frames code generation measured and the numbers the program declares
//! with `#[stack(N)]`, compared with the stack the `memory` block declares.
//!
//! The graph has a node for every body that is not `comptime`. Its edges are
//! the calls MIR shows, the two calls of a `critical` block, the checks that
//! call the panic function, the bodies `sym` operands name, and the calls the
//! compiler inserts (ADR 0123, "The graph"). The depth of a body is computed
//! in two states, before a panic and while panicking (ADR 0121), and the
//! worst case is the sum of four parts, one per context that can preempt
//! another (ADR 0078, 0084, 0085).
//!
//! The frames are plain numbers: the analysis names nothing of code
//! generation (ADR 0123, "Where it runs").

use std::ptr;

use super::bits::{reachable, shown};
use super::call_graph::{Call, Edges, Reach, Site, components, cycle};
use crate::diag::{Diagnostic, Diagnostics};
use crate::mir::{AsmSym, Body, BodyId, InlineAsm, Mir, Statement, Terminator, VectorWord};
use crate::span::Span;
use crate::target::{Target, VectorKind};

const OVERFLOW: &str = "analysis::stack-overflow";
const MISSING: &str = "analysis::missing-stack";
const CYCLE: &str = "analysis::stack-cycle";

/// The state of the depth of a body before a panic, and while the panic
/// function runs (ADR 0123, "Depth, before and during a panic").
const BEFORE: usize = 0;
const WHILE: usize = 1;

/// What an edge of the graph is (ADR 0123, "The graph").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// A `Statement::Call`.
    Call,
    /// A `Statement::CriticalEnter` or `CriticalExit`.
    Critical,
    /// A `sym` operand that names a body.
    Sym,
    /// A check, which calls the panic function.
    Panic,
    /// A call the compiler may insert, to an inserted function.
    Inserted,
}

/// Why a body needs `#[stack(N)]` (ADR 0122, "Where it is required").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Need {
    Naked,
    Asm,
    Extern,
}

/// The edges of one body, in the order of ADR 0123, and why it needs a
/// declared stack, if it does.
#[derive(Default)]
struct Scan {
    calls: Vec<Call>,
    kinds: Vec<Kind>,
    need: Option<Need>,
}

impl AsRef<[Call]> for Scan {
    fn as_ref(&self) -> &[Call] {
        &self.calls
    }
}

impl Scan {
    /// The edges of the blocks reachable from the start of `body`, in the
    /// order of the blocks, then of the statements, then of the terminator.
    /// A body run only at compile time has none and is no node; an external
    /// one has none.
    fn of(mir: &Mir, body: &Body) -> Scan {
        let mut scan = Scan::default();
        if body.comptime {
            return scan;
        }
        if body.external {
            scan.need = Some(Need::Extern);
            return scan;
        }
        if body.naked {
            scan.need = Some(Need::Naked);
        }
        if body.blocks.is_empty() {
            return scan;
        }
        let (blocks, _) = reachable(body);
        for index in blocks {
            let block = &body.blocks[index];
            for (at, statement) in block.statements.iter().enumerate() {
                let span = (block.spans.get(at).copied()).unwrap_or(block.terminator_span);
                match statement {
                    Statement::Call { callee, .. } => scan.edge(mir, span, *callee, Kind::Call),
                    Statement::CriticalEnter(_) => {
                        if let Some(callee) = mir.critical_enter {
                            scan.edge(mir, span, callee, Kind::Critical);
                        }
                    }
                    Statement::CriticalExit(_) => {
                        if let Some(callee) = mir.critical_exit {
                            scan.edge(mir, span, callee, Kind::Critical);
                        }
                    }
                    Statement::InlineAsm(asm) => scan.asm(mir, asm, span),
                    Statement::Assign(..) | Statement::New(..) | Statement::End(..) => {}
                }
            }
            match &block.terminator {
                Terminator::Panic(..) => {
                    scan.edge(mir, block.terminator_span, mir.panic, Kind::Panic);
                }
                Terminator::InlineAsm(asm) => scan.asm(mir, asm, block.terminator_span),
                Terminator::Goto(_)
                | Terminator::If { .. }
                | Terminator::Switch { .. }
                | Terminator::Return
                | Terminator::Unreachable => {}
            }
        }
        scan
    }

    /// An edge to `callee`, unless it is a body run only at compile time.
    fn edge(&mut self, mir: &Mir, span: Span, callee: BodyId, kind: Kind) {
        if mir.body(callee).comptime {
            return;
        }
        self.calls.push(Call {
            site: Site {
                span,
                outside: true,
            },
            callee,
            check: kind == Kind::Panic,
        });
        self.kinds.push(kind);
    }

    /// The edges of an `asm`, one per `sym` operand that names a body; and
    /// the need of the stack it makes, if it lacks `nostack`.
    fn asm(&mut self, mir: &Mir, asm: &InlineAsm, span: Span) {
        if !asm.options.nostack && self.need.is_none() {
            self.need = Some(Need::Asm);
        }
        for sym in asm.syms() {
            if let AsmSym::Body(callee) = sym {
                self.edge(mir, span, callee, Kind::Sym);
            }
        }
    }

    /// The bodies the edges reach other than by an inserted call, in the
    /// order of the edges, each once.
    fn callees(&self) -> Vec<BodyId> {
        let mut callees: Vec<BodyId> = Vec::new();
        for (call, kind) in self.calls.iter().zip(&self.kinds) {
            if *kind != Kind::Inserted && !callees.contains(&call.callee) {
                callees.push(call.callee);
            }
        }
        callees
    }

    /// Whether the body has the inserted calls.
    fn inserts(&self) -> bool {
        self.kinds.contains(&Kind::Inserted)
    }

    /// The kind of an edge of this body, given by the reference to it.
    fn kind_of(&self, call: &Call) -> Kind {
        (self.calls.iter())
            .position(|other| ptr::eq(other, call))
            .map_or(Kind::Call, |at| self.kinds[at])
    }
}

/// What a step of a path is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepKind {
    /// A body of the program or of `core`.
    Body,
    /// A body reached through an inserted call (ADR 0123, "The graph").
    Inserted,
    /// The entry of an exception: `Target::exception_frame`.
    Exception,
    /// The default handlers, by their largest frame.
    DefaultHandler,
}

/// One step of a path of the report: a body and its cost, or a part of a
/// context that is no body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    /// The body's symbol as diagnostics show it (the dots written `::`); a
    /// name of its own for the steps that are no body.
    pub name: String,
    pub cost: u64,
    pub kind: StepKind,
}

/// The contexts whose depths add up (ADR 0123, "Contexts and the total").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Context {
    Thread,
    Maskable,
    Unmaskable,
    Faults,
}

impl Context {
    /// How the diagnostics and the report name the part.
    pub fn name(self) -> &'static str {
        match self {
            Context::Thread => "thread context",
            Context::Maskable => "maskable handler context",
            Context::Unmaskable => "unmaskable handler context",
            Context::Faults => "fault context",
        }
    }
}

/// One of the four parts of the total.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub context: Context,
    pub bytes: u64,
    /// The deepest path; an unmaskable part is the sum of several, one per
    /// handler, and the fault part of two.
    pub paths: Vec<Vec<Step>>,
}

impl Part {
    /// `the thread context needs 1460 bytes: `_start` (0) → `main` (48)`.
    fn describe(&self) -> String {
        let paths: Vec<String> = self.paths.iter().map(|path| show_path(path)).collect();
        format!(
            "the {} needs {} bytes: {}",
            self.context.name(),
            self.bytes,
            paths.join(" + ")
        )
    }
}

fn show_path(path: &[Step]) -> String {
    let steps: Vec<String> = path
        .iter()
        .map(|step| match step.kind {
            StepKind::Body => format!("`{}` ({})", step.name, step.cost),
            StepKind::Inserted => format!("`{}` ({}, inserted)", step.name, step.cost),
            StepKind::Exception | StepKind::DefaultHandler => {
                format!("{} ({})", step.name, step.cost)
            }
        })
        .collect();
    steps.join(" \u{2192} ")
}

/// What the bound found, for the report of `--emit=stack`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// The worst case; `None` when an error stopped the bound from computing
    /// it.
    pub total: Option<u64>,
    /// The stack the `memory` block declares, if the program has one.
    pub declared: Option<u64>,
    /// The four parts, in the order of [`Context`], when the total was
    /// computed.
    pub parts: Vec<Part>,
    /// The depth of each body before a panic, by body id; `None` for a body
    /// run only at compile time, for one that reaches a function whose stack
    /// is not known, and for all of them when the graph has a cycle.
    pub depths: Vec<Option<u64>>,
    /// Whether some part reaches the body, by body id (ADR 0123, "The
    /// report").
    pub reached: Vec<bool>,
    /// The bodies each body's edges reach, by body id, in the order of its
    /// edges and each once; the inserted functions are not here, but in
    /// `inserted`.
    pub calls: Vec<Vec<BodyId>>,
    /// Whether the body has the inserted calls (ADR 0123, "The graph"), by
    /// body id.
    pub inserted: Vec<bool>,
}

impl Report {
    /// The text of `--emit=stack` (ADR 0123, "The report"): the worst case
    /// and the declared size, each part with its deepest path, and a line for
    /// every body that is not `comptime`, in MIR's order, with its symbol, its
    /// frame (from `frames`, by body id), its `N` if it has one, its depth
    /// before a panic, `not reached` when no part reaches it, and the bodies
    /// its edges reach (`inserted calls` for the inserted functions). The
    /// symbols of the lines are written as the linker sees them, with their
    /// dots, which the paths of the parts write `::`.
    pub fn render(&self, mir: &Mir, frames: &[u64]) -> String {
        let mut out = String::new();
        match (self.total, self.declared) {
            (Some(total), Some(declared)) => {
                out += &format!("worst case: {total} bytes, {declared} declared");
            }
            (Some(total), None) => out += &format!("worst case: {total} bytes"),
            (None, _) => out += "worst case: unknown",
        }
        out.push('\n');
        for part in &self.parts {
            out += &format!("{}: {} bytes", part.context.name(), part.bytes);
            if !part.paths.is_empty() {
                let paths: Vec<String> = part.paths.iter().map(|path| show_path(path)).collect();
                out += &format!(": {}", paths.join(" + "));
            }
            out.push('\n');
        }
        out += "bodies:\n";
        for (id, body) in mir.bodies() {
            if body.comptime {
                continue;
            }
            let at = id.0 as usize;
            let frame = frames.get(at).copied().unwrap_or(0);
            out += &format!("{}: frame {frame}", body.name);
            if let Some(stack) = body.stack {
                out += &format!(", stack {stack}");
            }
            match self.depths.get(at).copied().flatten() {
                Some(depth) => out += &format!(", depth {depth}"),
                None => out += ", depth unknown",
            }
            if !self.reached.get(at).copied().unwrap_or(false) {
                out += ", not reached";
            }
            let mut calls: Vec<&str> = (self.calls.get(at).into_iter().flatten())
                .map(|&callee| mir.body(callee).name.as_str())
                .collect();
            if self.inserted.get(at).copied().unwrap_or(false) {
                calls.push("inserted calls");
            }
            if !calls.is_empty() {
                out += &format!("; calls {}", calls.join(", "));
            }
            out.push('\n');
        }
        out
    }
}

/// Computes the stack bound of `mir` on `target` and reports its errors:
/// `analysis::stack-cycle`, `analysis::missing-stack`, and, when neither is
/// found, `analysis::stack-overflow`. `frames` is the frame of each body, by
/// body id (0 for a body missing from it), and `default_handlers` the largest
/// frame among the default handlers (ADR 0123, "Frames").
///
/// # Panics
///
/// When the inserted closure holds a body that is not `core`'s or is the
/// panic function: only a change to `core` can cause it (ADR 0123, "The
/// graph").
pub fn bound(
    mir: &Mir,
    target: &Target,
    frames: &[u64],
    default_handlers: u64,
    diags: &mut Diagnostics,
) -> Report {
    let scans = graph(mir);
    let mut report = Report {
        declared: mir.memory.as_ref().map(|map| map.stack_size),
        depths: vec![None; scans.len()],
        reached: vec![false; scans.len()],
        calls: scans.iter().map(Scan::callees).collect(),
        inserted: scans.iter().map(Scan::inserts).collect(),
        ..Report::default()
    };
    let nodes: Vec<bool> = mir.bodies().map(|(_, body)| !body.comptime).collect();
    let cycles = report_cycles(mir, &scans, &nodes, diags);

    let roots = roots(mir);
    let reach = Reach::search(&scans, &roots, Edges::CallsAndPanics, false);
    for (id, _) in mir.bodies() {
        report.reached[id.0 as usize] = reach.reaches(id);
    }
    let missing = report_missing(mir, &scans, &reach, diags);
    if cycles {
        return report;
    }

    let mut depths = Depths::new(mir, &scans, frames);
    for (id, body) in mir.bodies() {
        if !body.comptime {
            report.depths[id.0 as usize] = depths.of(id, BEFORE).value;
        }
    }
    if missing {
        return report;
    }
    let Some(parts) = parts(mir, target, default_handlers, &mut depths) else {
        return report;
    };
    let total = parts
        .iter()
        .fold(0u64, |sum, part| sum.saturating_add(part.bytes));
    report.total = Some(total);
    if let Some(map) = &mir.memory
        && total > map.stack_size
    {
        let notes: Vec<String> = (parts.iter())
            .filter(|part| part.bytes > 0)
            .map(Part::describe)
            .collect();
        diags.push(
            Diagnostic::error(
                OVERFLOW,
                format!(
                    "the stack may need {total} bytes, more than the {} declared",
                    map.stack_size
                ),
            )
            .label(
                map.stack_span,
                format!("{} bytes declared here", map.stack_size),
            )
            .note(notes.join("\n"))
            .help(format!(
                "declare a stack of at least {total} bytes, or make the deepest path need less"
            )),
        );
    }
    report.parts = parts;
    report
}

/// The graph of the bound (ADR 0123, "The graph"): the edges of every body,
/// then the inserted calls, from every body that is not naked, not external
/// and not in the inserted closure. The closure is checked on the way.
fn graph(mir: &Mir) -> Vec<Scan> {
    let mut scans: Vec<Scan> = mir.bodies().map(|(_, body)| Scan::of(mir, body)).collect();
    // The inserted functions and what they reach by calls, `critical` blocks
    // and `sym` operands, without checks.
    let closure = Reach::search(&scans, &mir.inserted, Edges::Calls, false);
    for (id, body) in mir.bodies() {
        if closure.reaches(id) && (!body.runtime || id == mir.panic) {
            panic!(
                "the inserted closure holds `{}`, which is not code of `core` that needs no \
                 inserted call (ADR 0123)",
                body.name
            );
        }
    }
    for (id, body) in mir.bodies() {
        if body.comptime || body.external || body.naked || closure.reaches(id) {
            continue;
        }
        let scan = &mut scans[id.0 as usize];
        for &callee in &mir.inserted {
            scan.calls.push(Call {
                site: Site {
                    span: body.span,
                    outside: true,
                },
                callee,
                check: false,
            });
            scan.kinds.push(Kind::Inserted);
        }
    }
    scans
}

/// Where each context starts: the reset entry (the entry function when
/// `core` is left out), the handlers in the order of their bodies, and the
/// panic function, which the default handlers call.
fn roots(mir: &Mir) -> Vec<BodyId> {
    let mut roots = vec![mir.reset.unwrap_or(mir.entry)];
    roots.extend(mir.handlers.iter().map(|handler| handler.body));
    roots.push(mir.panic);
    roots
}

/// `analysis::stack-cycle`, one error for each strongly connected component
/// of the graph without the edges of checks that holds a cycle (ADR 0123,
/// "Cycles"); whether it found any.
fn report_cycles(mir: &Mir, scans: &[Scan], nodes: &[bool], diags: &mut Diagnostics) -> bool {
    let mut found = false;
    for component in components(scans, nodes, Edges::Calls) {
        if !component.cyclic {
            continue;
        }
        let path = cycle(scans, &component, Edges::Calls);
        let name = |id: BodyId| format!("`{}`", shown(&mir.body(id).name));
        let names: Vec<String> = path.iter().map(|&(body, _)| name(body)).collect();
        let who = match names.as_slice() {
            [only] => format!("{only} reaches itself"),
            [first, second] => format!("{first} and {second} reach each other"),
            [init @ .., last] => format!("{} and {last} reach each other", init.join(", ")),
            [] => continue,
        };
        found = true;
        let mut diag = Diagnostic::error(
            CYCLE,
            format!("{who} through assembly, so the stack has no bound"),
        );
        for &(body, call) in &path {
            let (from, to) = (name(body), name(call.callee));
            let label = match scans[body.0 as usize].kind_of(call) {
                Kind::Sym => format!("{from} names {to} with `sym` here"),
                Kind::Inserted => format!("{from} may call {to}, which the compiler inserts"),
                Kind::Critical => format!("{from} calls {to} in a `critical` block here"),
                Kind::Call | Kind::Panic => format!("{from} calls {to} here"),
            };
            diag = diag.label(call.site.span, label);
        }
        let mut order = names.clone();
        order.extend(names.first().cloned());
        diags.push(diag.note(format!("the cycle: {}", order.join(" \u{2192} "))));
    }
    found
}

/// `analysis::missing-stack`, one error for each body some context reaches
/// that needs `#[stack(N)]` and has none, in the order of the bodies
/// (ADR 0122, "Where it is required"); whether it found any.
fn report_missing(mir: &Mir, scans: &[Scan], reach: &Reach, diags: &mut Diagnostics) -> bool {
    let mut found = false;
    for (id, body) in mir.bodies() {
        let Some(need) = scans[id.0 as usize].need else {
            continue;
        };
        if body.stack.is_some() || !reach.reaches(id) {
            continue;
        }
        found = true;
        let name = shown(&body.name);
        let why = match need {
            Need::Naked => "it is naked",
            Need::Asm => "it holds `asm` without `nostack`",
            Need::Extern => "it is an `extern fn`",
        };
        let mut path: Vec<String> = Vec::new();
        if let Some(root) = reach.root(id) {
            path.push(format!("`{}`", shown(&mir.body(root).name)));
        }
        for (from, k) in reach.steps(id) {
            let callee = scans[from.0 as usize].calls[k].callee;
            path.push(format!("`{}`", shown(&mir.body(callee).name)));
        }
        diags.push(
            Diagnostic::error(
                MISSING,
                format!("the stack of `{name}` is not known: {why}"),
            )
            .label(body.span, "its stack is not declared")
            .note(format!(
                "the stack bound reaches it through {}",
                path.join(" \u{2192} ")
            ))
            .help(
                "declare it with `#[stack(N)]`, the bytes it uses that the compiler does not \
                 measure (ADR 0122)",
            ),
        );
    }
    found
}

/// The memo of one body in one state: its depth, or `None` when it reaches a
/// function whose stack is not known; and the edge that gives the depth, the
/// first of those with the largest target.
#[derive(Clone, Copy)]
struct Memo {
    value: Option<u64>,
    next: Option<usize>,
}

/// The depths of the bodies, in both states, computed on demand and kept.
struct Depths<'a> {
    mir: &'a Mir,
    scans: &'a [Scan],
    frames: &'a [u64],
    memo: Vec<[Option<Memo>; 2]>,
    /// The pairs being computed, to refuse a cycle instead of following it
    /// (there is none once `analysis::stack-cycle` is clear).
    open: Vec<[bool; 2]>,
}

impl<'a> Depths<'a> {
    fn new(mir: &'a Mir, scans: &'a [Scan], frames: &'a [u64]) -> Self {
        Depths {
            mir,
            scans,
            frames,
            memo: vec![[None; 2]; scans.len()],
            open: vec![[false; 2]; scans.len()],
        }
    }

    fn frame(&self, body: BodyId) -> u64 {
        self.frames.get(body.0 as usize).copied().unwrap_or(0)
    }

    /// The cost of a body in a state (ADR 0123, "Frames"): its frame plus its
    /// `N`; the panic function's frame alone while panicking. `None` for a
    /// body that needs `#[stack(N)]` and has none.
    fn cost(&self, body: BodyId, state: usize) -> Option<u64> {
        if body == self.mir.panic && state == WHILE {
            return Some(self.frame(body));
        }
        let data = self.mir.body(body);
        if self.scans[body.0 as usize].need.is_some() && data.stack.is_none() {
            return None;
        }
        Some(self.frame(body).saturating_add(data.stack.unwrap_or(0)))
    }

    /// The state a body's edges lead to: the panic function, before a panic,
    /// leads into the state while panicking.
    fn next_state(&self, body: BodyId, state: usize) -> usize {
        if body == self.mir.panic && state == BEFORE {
            WHILE
        } else {
            state
        }
    }

    /// The edges that count: none for the panic function while panicking.
    fn edges(&self, body: BodyId, state: usize) -> &'a [Call] {
        let scans: &'a [Scan] = self.scans;
        if body == self.mir.panic && state == WHILE {
            &[]
        } else {
            &scans[body.0 as usize].calls
        }
    }

    /// The depth of `body` in `state`, with the edge that gives it. No
    /// recursion: a work list, so that a long chain of calls cannot overflow
    /// the compiler's stack.
    fn of(&mut self, body: BodyId, state: usize) -> Memo {
        let mut work = vec![(body, state)];
        while let Some(&(b, s)) = work.last() {
            if self.memo[b.0 as usize][s].is_some() {
                work.pop();
                continue;
            }
            self.open[b.0 as usize][s] = true;
            let child = self.next_state(b, s);
            let mut waiting = false;
            for call in self.edges(b, s) {
                let t = call.callee.0 as usize;
                if self.memo[t][child].is_none() && !self.open[t][child] {
                    work.push((call.callee, child));
                    waiting = true;
                }
            }
            if waiting {
                continue;
            }
            let cost = self.cost(b, s);
            let mut best: Option<(u64, usize)> = None;
            let mut known = cost.is_some();
            for (k, call) in self.edges(b, s).iter().enumerate() {
                match self.memo[call.callee.0 as usize][child].and_then(|memo| memo.value) {
                    // An edge to a depth of 0 adds nothing, and a path
                    // would end in a body that costs nothing: it is not
                    // followed.
                    Some(value) => {
                        if value > 0 && best.is_none_or(|(most, _)| value > most) {
                            best = Some((value, k));
                        }
                    }
                    None => known = false,
                }
            }
            let memo = match cost {
                Some(cost) if known => Memo {
                    value: Some(cost.saturating_add(best.map_or(0, |(most, _)| most))),
                    next: best.map(|(_, k)| k),
                },
                _ => Memo {
                    value: None,
                    next: None,
                },
            };
            self.memo[b.0 as usize][s] = Some(memo);
            self.open[b.0 as usize][s] = false;
            work.pop();
        }
        self.memo[body.0 as usize][state].unwrap_or(Memo {
            value: None,
            next: None,
        })
    }

    /// The deepest path from `body` in `state`, each step with its cost.
    fn path(&mut self, body: BodyId, state: usize) -> Vec<Step> {
        self.of(body, state);
        let mut steps = Vec::new();
        let (mut at, mut state, mut inserted) = (body, state, false);
        loop {
            steps.push(Step {
                name: shown(&self.mir.body(at).name),
                cost: self.cost(at, state).unwrap_or(0),
                kind: if inserted {
                    StepKind::Inserted
                } else {
                    StepKind::Body
                },
            });
            let Some(next) = self.memo[at.0 as usize][state].and_then(|memo| memo.next) else {
                return steps;
            };
            let scan = &self.scans[at.0 as usize];
            inserted = scan.kinds[next] == Kind::Inserted;
            let callee = scan.calls[next].callee;
            state = self.next_state(at, state);
            at = callee;
        }
    }
}

/// The four parts of the worst case (ADR 0123, "Contexts and the total");
/// `None` when a depth they need is not known.
fn parts(
    mir: &Mir,
    target: &Target,
    default_handlers: u64,
    depths: &mut Depths,
) -> Option<Vec<Part>> {
    let entry = Step {
        name: "exception entry".to_owned(),
        cost: target.exception_frame,
        kind: StepKind::Exception,
    };
    let default = Step {
        name: "default handler".to_owned(),
        cost: default_handlers,
        kind: StepKind::DefaultHandler,
    };
    // The depth of a default handler, before a panic: its frame, then the
    // panic function.
    let panic_depth = depths.of(mir.panic, BEFORE).value?;
    let panic_path = depths.path(mir.panic, BEFORE);
    let default_depth = default_handlers.saturating_add(panic_depth);
    let default_path: Vec<Step> = [entry.clone(), default.clone()]
        .into_iter()
        .chain(panic_path)
        .collect();
    let through_default = target.exception_frame.saturating_add(default_depth);
    let handled = |word: u32| mir.handlers.iter().any(|handler| handler.word == word);

    // The thread starts where the reset entry does.
    let thread_root = mir.reset.unwrap_or(mir.entry);
    let thread = Part {
        context: Context::Thread,
        bytes: depths.of(thread_root, BEFORE).value?,
        paths: vec![depths.path(thread_root, BEFORE)],
    };

    // A handler's depth, on top of the entry of its exception.
    let mut in_handler = |kind: VectorKind| -> Option<Vec<(u64, Vec<Step>)>> {
        let mut found = Vec::new();
        for handler in mir.handlers.iter().filter(|handler| handler.kind == kind) {
            let depth = depths.of(handler.body, BEFORE).value?;
            let path: Vec<Step> = std::iter::once(entry.clone())
                .chain(depths.path(handler.body, BEFORE))
                .collect();
            found.push((target.exception_frame.saturating_add(depth), path));
        }
        Some(found)
    };

    // One level of maskable handlers: the deepest, or a default handler for a
    // maskable word that has no handler.
    let mut maskable = in_handler(VectorKind::Maskable)?;
    let default_maskable = (mir.vector_table.as_ref()).is_some_and(|table| {
        (table.words.iter().enumerate()).any(|(word, kind)| {
            matches!(kind, VectorWord::Default(_)) && target.maskable_word(word as u32)
        })
    });
    if default_maskable {
        maskable.push((through_default, default_path.clone()));
    }
    let mut deepest: Option<(u64, Vec<Step>)> = None;
    for (bytes, path) in maskable {
        if deepest.as_ref().is_none_or(|(most, _)| bytes > *most) {
            deepest = Some((bytes, path));
        }
    }
    let maskable = Part {
        context: Context::Maskable,
        bytes: deepest.as_ref().map_or(0, |(bytes, _)| *bytes),
        paths: deepest.into_iter().map(|(_, path)| path).collect(),
    };

    // Each unmaskable handler once, and the NMI that has none.
    let mut unmaskable = in_handler(VectorKind::Unmaskable)?;
    if target.nmi_word.is_some_and(|word| !handled(word)) {
        unmaskable.push((through_default, default_path.clone()));
    }
    let unmaskable = Part {
        context: Context::Unmaskable,
        bytes: unmaskable
            .iter()
            .fold(0u64, |sum, (bytes, _)| sum.saturating_add(*bytes)),
        paths: unmaskable.into_iter().map(|(_, path)| path).collect(),
    };

    // A fault that reaches a default handler, and a second inside the panic
    // function, where the guard stops (ADR 0121).
    let mut again = vec![entry.clone(), default];
    again.push(Step {
        name: shown(&mir.body(mir.panic).name),
        cost: depths.cost(mir.panic, WHILE)?,
        kind: StepKind::Body,
    });
    let second = target
        .exception_frame
        .saturating_add(default_handlers)
        .saturating_add(depths.cost(mir.panic, WHILE)?);
    let faults = Part {
        context: Context::Faults,
        bytes: through_default.saturating_add(second),
        paths: vec![default_path, again],
    };

    Some(vec![thread, maskable, unmaskable, faults])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::{RISCV32IMAC, THUMBV7M};

    fn mir_of(target: &'static Target, text: &'static str) -> Mir {
        crate::driver::whole_mir_of(target, text).unwrap_or_else(|e| panic!("{e}"))
    }

    /// The frames of the bodies, by symbol; 0 for those not named.
    fn frames(mir: &Mir, named: &[(&str, u64)]) -> Vec<u64> {
        (mir.bodies())
            .map(|(_, body)| {
                (named.iter())
                    .find(|(name, _)| *name == body.name)
                    .map_or(0, |&(_, frame)| frame)
            })
            .collect()
    }

    fn run(
        mir: &Mir,
        target: &Target,
        named: &[(&str, u64)],
        default_handlers: u64,
    ) -> (Report, Vec<Diagnostic>) {
        let mut diags = Diagnostics::default();
        let report = bound(
            mir,
            target,
            &frames(mir, named),
            default_handlers,
            &mut diags,
        );
        (report, diags.into_vec())
    }

    fn id(mir: &Mir, name: &str) -> usize {
        (mir.bodies())
            .find(|(_, body)| body.name == name)
            .unwrap_or_else(|| panic!("no body `{name}`"))
            .0
            .0 as usize
    }

    fn codes(diags: &[Diagnostic]) -> Vec<&str> {
        diags.iter().map(|diag| diag.code).collect()
    }

    fn part(report: &Report, context: Context) -> &Part {
        (report.parts.iter())
            .find(|part| part.context == context)
            .unwrap_or_else(|| panic!("no part {context:?}"))
    }

    fn names(path: &[Step]) -> Vec<&str> {
        path.iter().map(|step| step.name.as_str()).collect()
    }

    /// A program with the memory block the tests give it, and `stack` bytes of
    /// stack.
    fn with_stack(stack: u32, program: &str) -> &'static str {
        Box::leak(
            format!(
                "memory {{
    region ROM {{ origin: 0x0800_0000, size: 0x1_0000 }}
    region RAM {{ origin: 0x2000_0000, size: 0x1_0000 }}
    code in ROM;
    data in RAM;
    stack in RAM {{ size: {stack} }}
}}
{program}"
            )
            .into_boxed_str(),
        )
    }

    /// A check in the entry function's callee and a call from the panic
    /// function.
    const STATES: &str = "
fn helper() {}
fn take(x: u32) {}
fn divide(a: u32, b: u32) -> u32 { a / b }
entry fn main() -> ! { helper(); take(divide(1, 0)); loop {} }
panic fn on_panic() -> ! { helper(); loop {} }
";

    const STATES_FRAMES: &[(&str, u64)] = &[
        ("main", 100),
        ("divide", 20),
        ("helper", 5),
        ("take", 3),
        ("on_panic", 30),
    ];

    #[test]
    fn a_body_is_deeper_before_a_panic_than_while_panicking() {
        let mir = mir_of(&RISCV32IMAC, STATES);
        let (report, diags) = run(&mir, &RISCV32IMAC, STATES_FRAMES, 0);
        assert!(diags.is_empty(), "{diags:?}");
        // Before a panic the panic function costs its frame and the deepest
        // of its callees: 30 + 5. `divide` reaches it through its check.
        assert_eq!(report.depths[id(&mir, "on_panic")], Some(35));
        assert_eq!(report.depths[id(&mir, "divide")], Some(20 + 35));
        assert_eq!(report.depths[id(&mir, "main")], Some(100 + 20 + 35));
        // The thread: `_start` (0), the entry function, then the check that
        // reaches the panic function, which calls `helper`.
        let thread = part(&report, Context::Thread);
        assert_eq!(thread.bytes, 155);
        assert_eq!(
            names(&thread.paths[0]),
            ["_start", "main", "divide", "on_panic", "helper"]
        );
        assert_eq!(
            (thread.paths[0].iter().map(|step| step.cost)).collect::<Vec<_>>(),
            [0, 100, 20, 30, 5]
        );
        // A fault that reaches a default handler (the panic function costs
        // 35), and a second one inside the panic function, where the guard
        // stops: its frame alone (30).
        let faults = part(&report, Context::Faults);
        assert_eq!(faults.bytes, (64 + 35) + (64 + 30));
        assert_eq!(
            names(&faults.paths[0]),
            ["exception entry", "default handler", "on_panic", "helper"]
        );
        assert_eq!(
            names(&faults.paths[1]),
            ["exception entry", "default handler", "on_panic"]
        );
        assert_eq!(faults.paths[1][2].cost, 30);
    }

    #[test]
    fn the_total_is_the_sum_of_the_four_parts() {
        let mir = mir_of(&RISCV32IMAC, STATES);
        let (report, _) = run(&mir, &RISCV32IMAC, STATES_FRAMES, 0);
        let parts: Vec<(Context, u64)> = (report.parts.iter())
            .map(|part| (part.context, part.bytes))
            .collect();
        // The maskable part is a default handler: no handler is declared, and
        // the interrupts of riscv32 have words that none serves.
        assert_eq!(
            parts,
            [
                (Context::Thread, 155),
                (Context::Maskable, 64 + 35),
                (Context::Unmaskable, 0),
                (Context::Faults, 193),
            ]
        );
        assert_eq!(report.total, Some(155 + 99 + 193));
        assert_eq!(report.declared, None);
    }

    #[test]
    fn a_check_in_the_panic_function_costs_one_more_frame_and_is_no_cycle() {
        let mir = mir_of(
            &RISCV32IMAC,
            "
fn take(x: u32) {}
fn divide(a: u32, b: u32) -> u32 { a / b }
entry fn main() -> ! { loop {} }
panic fn on_panic() -> ! { take(divide(1, 0)); loop {} }
",
        );
        let named = [("divide", 20), ("take", 3), ("on_panic", 30)];
        let (report, diags) = run(&mir, &RISCV32IMAC, &named, 0);
        assert!(diags.is_empty(), "no cycle through the check: {diags:?}");
        // `divide` while panicking: 20 and the panic function's frame, 30.
        assert_eq!(report.depths[id(&mir, "on_panic")], Some(30 + 50));
        assert_eq!(part(&report, Context::Faults).bytes, (64 + 80) + (64 + 30));
    }

    /// A handler of each kind and a program that fills the frames.
    const HANDLERS_RISCV32: &str = "
use core::arch::Vector;
interrupt fn tick() at Vector::MachineTimer {}
interrupt fn fault() -> ! at Vector::IllegalInstruction { loop {} }
entry fn main() -> ! { loop {} }
panic fn on_panic() -> ! { loop {} }
";

    const HANDLER_FRAMES: &[(&str, u64)] =
        &[("main", 100), ("tick", 50), ("fault", 30), ("on_panic", 20)];

    #[test]
    fn each_part_adds_its_context_on_riscv32() {
        let mir = mir_of(&RISCV32IMAC, HANDLERS_RISCV32);
        let (report, diags) = run(&mir, &RISCV32IMAC, HANDLER_FRAMES, 7);
        assert!(diags.is_empty(), "{diags:?}");
        // The default handler: its frame 7 and the panic function, 20.
        let through_default = 64 + 7 + 20;
        let bytes = |context| part(&report, context).bytes;
        assert_eq!(bytes(Context::Thread), 100);
        // One level of maskable handlers: the deepest of the handler (64 +
        // 50) and the default handlers (the words of the interrupts without a
        // handler).
        assert_eq!(bytes(Context::Maskable), 64 + 50);
        assert_eq!(
            names(&part(&report, Context::Maskable).paths[0]),
            ["exception entry", "tick"]
        );
        // The exception handler, once; riscv32 has no NMI.
        assert_eq!(bytes(Context::Unmaskable), 64 + 30);
        // The fault to a default handler, then the second in the panic
        // function: its frame alone.
        assert_eq!(bytes(Context::Faults), through_default + (64 + 7 + 20));
        assert_eq!(report.total, Some(100 + 114 + 94 + 182));
    }

    #[test]
    fn a_default_handler_deeper_than_the_handlers_is_the_maskable_part() {
        let mir = mir_of(&RISCV32IMAC, HANDLERS_RISCV32);
        let (report, _) = run(&mir, &RISCV32IMAC, HANDLER_FRAMES, 200);
        let maskable = part(&report, Context::Maskable);
        assert_eq!(maskable.bytes, 64 + 200 + 20);
        assert_eq!(
            names(&maskable.paths[0]),
            ["exception entry", "default handler", "on_panic"]
        );
    }

    #[test]
    fn each_part_adds_its_context_on_thumbv7m() {
        let mir = mir_of(
            &THUMBV7M,
            "
use core::arch::Vector;
interrupt fn tick() at Vector::SysTick {}
interrupt fn fault() -> ! at Vector::HardFault { loop {} }
entry fn main() -> ! { loop {} }
panic fn on_panic() -> ! { loop {} }
",
        );
        let (report, diags) = run(&mir, &THUMBV7M, HANDLER_FRAMES, 7);
        assert!(diags.is_empty(), "{diags:?}");
        // The two functions of `core` whose `#[stack(24)]` every body may call
        // are 24 more under each body that is not in the inserted closure.
        // The panic function's depth: 20 and the deepest callee, 24.
        let through_default = 36 + 7 + 44;
        let bytes = |context| part(&report, context).bytes;
        assert_eq!(bytes(Context::Thread), 100 + 24);
        assert_eq!(bytes(Context::Maskable), 36 + 50 + 24);
        // The HardFault handler, and the NMI that has none: a default handler.
        assert_eq!(bytes(Context::Unmaskable), (36 + 30 + 24) + through_default);
        assert_eq!(
            names(&part(&report, Context::Unmaskable).paths[1])[..3],
            ["exception entry", "default handler", "on_panic"]
        );
        assert_eq!(bytes(Context::Faults), through_default + (36 + 7 + 20));
    }

    #[test]
    fn a_handler_for_the_nmi_takes_the_place_of_its_default() {
        let mir = mir_of(
            &THUMBV7M,
            "
use core::arch::Vector;
interrupt fn nonmaskable() at Vector::NMI {}
entry fn main() -> ! { loop {} }
panic fn on_panic() -> ! { loop {} }
",
        );
        let (report, _) = run(&mir, &THUMBV7M, &[("nonmaskable", 10)], 7);
        let unmaskable = part(&report, Context::Unmaskable);
        assert_eq!(unmaskable.bytes, 36 + 10 + 24);
        assert_eq!(unmaskable.paths.len(), 1);
    }

    #[test]
    fn inserted_calls_come_from_every_body_but_the_inserted_closure_and_naked_ones() {
        let mir = mir_of(&RISCV32IMAC, STATES);
        let scans = graph(&mir);
        let inserted = |name: &str| {
            (scans[id(&mir, name)].kinds.iter())
                .filter(|kind| **kind == Kind::Inserted)
                .count()
        };
        assert!(mir.inserted.len() >= 3);
        for name in ["main", "helper", "divide", "on_panic", "take"] {
            assert_eq!(inserted(name), mir.inserted.len(), "{name}");
        }
        // `_start` is naked, and the inserted functions are the closure.
        assert_eq!(inserted("_start"), 0);
        for &root in &mir.inserted {
            assert_eq!(inserted(&mir.body(root).name), 0, "{}", mir.body(root).name);
        }
        // The order of the edges: the calls, then the inserted ones, in the
        // order of `Mir::inserted`.
        let main = &scans[id(&mir, "main")];
        let first_inserted = main
            .kinds
            .iter()
            .position(|k| *k == Kind::Inserted)
            .unwrap();
        assert!(
            main.kinds[..first_inserted]
                .iter()
                .all(|k| *k != Kind::Inserted)
        );
        let callees: Vec<BodyId> = (main.calls[first_inserted..].iter())
            .map(|call| call.callee)
            .collect();
        assert_eq!(callees, mir.inserted);
    }

    #[test]
    fn an_inserted_function_is_a_callee_of_every_body_and_costs_what_it_does() {
        let mir = mir_of(
            &RISCV32IMAC,
            "
fn helper() {}
entry fn main() -> ! { helper(); loop {} }
panic fn on_panic() -> ! { loop {} }
",
        );
        let first = mir.inserted[0];
        let a = mir.body(first).name.clone();
        let (report, _) = run(&mir, &RISCV32IMAC, &[("main", 10), (a.as_str(), 80)], 0);
        assert_eq!(report.depths[first.0 as usize], Some(80));
        // `main` may call it, through `helper` first (the first of the equal
        // depths in the order of the edges), and so may `helper`.
        assert_eq!(report.depths[id(&mir, "helper")], Some(80));
        assert_eq!(report.depths[id(&mir, "main")], Some(90));
        let thread = &part(&report, Context::Thread).paths[0];
        assert_eq!(names(thread), ["_start", "main", "helper", a.as_str()]);
        assert_eq!(thread[3].kind, StepKind::Inserted);
        assert_eq!(thread[2].kind, StepKind::Body);
    }

    #[test]
    fn the_two_calls_of_a_critical_block_are_edges() {
        let mir = mir_of(
            &RISCV32IMAC,
            "
entry fn main() -> ! { critical { } loop {} }
panic fn on_panic() -> ! { loop {} }
",
        );
        let (enter, exit) = (mir.critical_enter.unwrap(), mir.critical_exit.unwrap());
        let scans = graph(&mir);
        let main = &scans[id(&mir, "main")];
        let critical: Vec<BodyId> = (main.calls.iter().zip(&main.kinds))
            .filter(|(_, kind)| **kind == Kind::Critical)
            .map(|(call, _)| call.callee)
            .collect();
        assert_eq!(critical, [enter, exit]);
        let (enter_name, exit_name) = (mir.body(enter).name.clone(), mir.body(exit).name.clone());
        let named = [(enter_name.as_str(), 30), (exit_name.as_str(), 90)];
        let (report, _) = run(&mir, &RISCV32IMAC, &named, 0);
        assert_eq!(report.depths[id(&mir, "main")], Some(90));
    }

    #[test]
    #[should_panic(expected = "the inserted closure holds")]
    fn the_panic_function_in_the_inserted_closure_is_an_internal_error() {
        let mut mir = mir_of(&RISCV32IMAC, STATES);
        mir.inserted.push(mir.panic);
        run(&mir, &RISCV32IMAC, &[], 0);
    }

    #[test]
    #[should_panic(expected = "the inserted closure holds")]
    fn code_of_the_program_in_the_inserted_closure_is_an_internal_error() {
        let mut mir = mir_of(&RISCV32IMAC, STATES);
        let helper = id(&mir, "helper");
        mir.inserted.push(BodyId(helper as u32));
        run(&mir, &RISCV32IMAC, &[], 0);
    }

    #[test]
    fn the_equal_depths_choose_the_first_edge() {
        let mir = mir_of(
            &RISCV32IMAC,
            "
fn left() {}
fn right() {}
entry fn main() -> ! { right(); left(); loop {} }
panic fn on_panic() -> ! { loop {} }
",
        );
        let named = [("left", 100), ("right", 100)];
        let (report, _) = run(&mir, &RISCV32IMAC, &named, 0);
        let thread = &part(&report, Context::Thread).paths[0];
        assert_eq!(names(thread), ["_start", "main", "right"]);
        // The same bound on every run.
        let (again, _) = run(&mir, &RISCV32IMAC, &named, 0);
        assert_eq!(report, again);
    }

    const NAKED_PAIR: &str = "
#[naked]
#[stack(8)]
fn stub() { asm(\"j {}\", sym helper, options(noreturn)) }
fn helper() {}
entry fn main() -> ! { unsafe { stub(); } loop {} }
panic fn on_panic() -> ! { loop {} }
";

    #[test]
    fn a_sym_operand_is_an_edge_and_the_function_it_names_is_a_callee() {
        let mir = mir_of(&RISCV32IMAC, NAKED_PAIR);
        let scans = graph(&mir);
        let stub = &scans[id(&mir, "stub")];
        assert_eq!(stub.kinds, [Kind::Sym]);
        assert_eq!(stub.need, Some(Need::Naked));
        let (report, diags) = run(&mir, &RISCV32IMAC, &[("helper", 50), ("main", 10)], 0);
        assert!(diags.is_empty(), "{diags:?}");
        // Its `N` (8) and the callee it names, with the frame of its own (50).
        assert_eq!(report.depths[id(&mir, "stub")], Some(8 + 50));
        assert_eq!(
            names(&part(&report, Context::Thread).paths[0]),
            ["_start", "main", "stub", "helper"]
        );
    }

    #[test]
    fn a_cycle_through_sym_is_a_stack_cycle_and_no_total_is_computed() {
        let mir = mir_of(
            &RISCV32IMAC,
            with_stack(
                16,
                "
#[naked]
#[stack(0)]
fn ping() { asm(\"j {}\", sym pong, options(noreturn)) }
#[naked]
#[stack(0)]
fn pong() { asm(\"j {}\", sym ping, options(noreturn)) }
entry fn main() -> ! { unsafe { ping(); } loop {} }
panic fn on_panic() -> ! { loop {} }
",
            ),
        );
        let (report, diags) = run(&mir, &RISCV32IMAC, &[("main", 1000)], 0);
        assert_eq!(codes(&diags), [CYCLE]);
        assert_eq!(
            diags[0].message,
            "`ping` and `pong` reach each other through assembly, so the stack has no bound"
        );
        assert_eq!(diags[0].labels.len(), 2);
        assert_eq!(
            diags[0].labels[0].message,
            "`ping` names `pong` with `sym` here"
        );
        assert_eq!(
            diags[0].note.as_deref(),
            Some("the cycle: `ping` \u{2192} `pong` \u{2192} `ping`")
        );
        assert!(diags[0].help.is_none());
        // No overflow with a stack of 16 bytes and a frame of 1000: the worst
        // case is not computed.
        assert_eq!(report.total, None);
        assert!(report.parts.is_empty());
    }

    #[test]
    fn a_body_that_names_itself_with_sym_is_a_cycle_of_one() {
        let mir = mir_of(
            &RISCV32IMAC,
            "
#[naked]
#[stack(0)]
fn spin() { asm(\"j {}\", sym spin, options(noreturn)) }
entry fn main() -> ! { unsafe { spin(); } loop {} }
panic fn on_panic() -> ! { loop {} }
",
        );
        let (_, diags) = run(&mir, &RISCV32IMAC, &[], 0);
        assert_eq!(codes(&diags), [CYCLE]);
        assert_eq!(
            diags[0].message,
            "`spin` reaches itself through assembly, so the stack has no bound"
        );
    }

    #[test]
    fn the_panic_function_that_jumps_back_to_the_entry_function_is_no_cycle() {
        let mir = mir_of(
            &RISCV32IMAC,
            "
fn divide(a: u32, b: u32) -> u32 { a / b }
entry fn main() -> ! { let x: u32 = divide(1, 0); loop {} }
panic fn on_panic() -> ! { unsafe { asm(\"j {}\", sym entry, options(noreturn, nostack)) } }
",
        );
        let named = [("main", 40), ("divide", 10), ("on_panic", 16)];
        let (report, diags) = run(&mir, &RISCV32IMAC, &named, 0);
        assert!(diags.is_empty(), "{diags:?}");
        // The restart: the panic function (16) and the entry function again,
        // while panicking: its calls, the check among them, cost the panic
        // function's frame only.
        assert_eq!(report.depths[id(&mir, "on_panic")], Some(16 + 40 + 10 + 16));
    }

    const NEEDS: &str = "
#[naked]
fn stub() { asm(\"ret\", options(noreturn)) }
fn pusher() { unsafe { asm(\"nop\"); } }
fn quiet() { unsafe { asm(\"nop\", options(nostack)); } }
#[stack(8)]
extern fn declared(x: u32) -> u32;
extern fn external(x: u32) -> u32;
#[naked]
fn nobody_calls_this() { asm(\"ret\", options(noreturn)) }
entry fn main() -> ! {
    unsafe { stub(); }
    pusher();
    quiet();
    let y: u32 = unsafe { declared(1) + external(2) };
    loop {}
}
panic fn on_panic() -> ! { loop {} }
";

    #[test]
    fn what_needs_a_stack_and_has_none_is_missing_stack_where_the_bound_reaches() {
        let mir = mir_of(&RISCV32IMAC, with_stack(16, NEEDS));
        let (report, diags) = run(&mir, &RISCV32IMAC, &[("main", 1000)], 0);
        assert_eq!(codes(&diags), [MISSING; 3]);
        let messages: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "the stack of `stub` is not known: it is naked",
                "the stack of `pusher` is not known: it holds `asm` without `nostack`",
                "the stack of `external` is not known: it is an `extern fn`",
            ]
        );
        assert_eq!(
            diags[2].note.as_deref(),
            Some("the stack bound reaches it through `_start` \u{2192} `main` \u{2192} `external`")
        );
        assert!(diags[2].help.as_deref().unwrap().contains("#[stack(N)]"));
        let stub = mir.body(BodyId(id(&mir, "stub") as u32));
        assert_eq!(diags[0].labels[0].span, stub.span);
        // No total, and no overflow for the stack of 16 bytes.
        assert_eq!(report.total, None);
        // A body that reaches one of them has no depth, and `main` is one.
        assert_eq!(report.depths[id(&mir, "main")], None);
        assert_eq!(report.depths[id(&mir, "quiet")], Some(0));
        assert_eq!(report.depths[id(&mir, "declared")], Some(8));
    }

    #[test]
    fn a_declared_stack_is_enough_and_a_body_nothing_reaches_needs_none() {
        let mir = mir_of(
            &RISCV32IMAC,
            "
#[naked]
#[stack(8)]
fn stub() { asm(\"ret\", options(noreturn)) }
fn pusher() { unsafe { asm(\"nop\"); } }
#[stack(0)]
extern fn declared(x: u32) -> u32;
extern fn unused(x: u32) -> u32;
entry fn main() -> ! { unsafe { stub(); } loop {} }
panic fn on_panic() -> ! { loop {} }
",
        );
        let (report, diags) = run(&mir, &RISCV32IMAC, &[], 0);
        // `pusher` and `unused` are not reached; `declared` is not called.
        assert!(diags.is_empty(), "{diags:?}");
        assert!(report.total.is_some());
        let reached = |name| report.reached[id(&mir, name)];
        assert!(reached("main") && reached("stub") && reached("_start"));
        assert!(!reached("pusher") && !reached("unused") && !reached("declared"));
        // Its depth is computed all the same, or unknown when it needs a number.
        assert_eq!(report.depths[id(&mir, "declared")], Some(0));
        assert_eq!(report.depths[id(&mir, "unused")], None);
        assert_eq!(report.depths[id(&mir, "pusher")], None);
    }

    #[test]
    fn a_body_no_part_reaches_is_in_the_report_and_not_in_the_total() {
        let mir = mir_of(
            &RISCV32IMAC,
            "
fn lonely() {}
entry fn main() -> ! { loop {} }
panic fn on_panic() -> ! { loop {} }
",
        );
        let (report, _) = run(&mir, &RISCV32IMAC, &[("lonely", 500), ("main", 80)], 0);
        assert!(!report.reached[id(&mir, "lonely")]);
        assert_eq!(report.depths[id(&mir, "lonely")], Some(500));
        let thread = part(&report, Context::Thread);
        assert_eq!(thread.bytes, 80);
        assert!(report.total.is_some_and(|total| total < 500));
    }

    #[test]
    fn a_body_run_only_at_compile_time_is_no_node() {
        let mir = mir_of(
            &RISCV32IMAC,
            "
const K: u32 = 1 + 1;
fn take(x: u32) {}
entry fn main() -> ! { take(K); loop {} }
panic fn on_panic() -> ! { loop {} }
",
        );
        let comptime: Vec<usize> = (mir.bodies())
            .filter(|(_, body)| body.comptime)
            .map(|(id, _)| id.0 as usize)
            .collect();
        assert!(!comptime.is_empty(), "the constant has a body");
        let (report, diags) = run(&mir, &RISCV32IMAC, &[], 0);
        assert!(diags.is_empty(), "{diags:?}");
        for body in comptime {
            assert_eq!(report.depths[body], None);
            assert!(!report.reached[body]);
        }
    }

    #[test]
    fn a_stack_too_small_is_an_overflow_with_the_deepest_path_of_each_part() {
        let mir = mir_of(&RISCV32IMAC, with_stack(432, STATES));
        let (report, diags) = run(&mir, &RISCV32IMAC, STATES_FRAMES, 0);
        assert_eq!(report.total, Some(447));
        assert_eq!(report.declared, Some(432));
        assert_eq!(codes(&diags), [OVERFLOW]);
        let diag = &diags[0];
        assert_eq!(
            diag.message,
            "the stack may need 447 bytes, more than the 432 declared"
        );
        assert_eq!(diag.labels.len(), 1);
        assert_eq!(diag.labels[0].message, "432 bytes declared here");
        assert_eq!(diag.labels[0].span, mir.memory.as_ref().unwrap().stack_span);
        assert_eq!(
            diag.help.as_deref(),
            Some("declare a stack of at least 447 bytes, or make the deepest path need less")
        );
        // One line per part that is not 0.
        let note = diag.note.as_deref().unwrap();
        let lines: Vec<&str> = note.lines().collect();
        assert_eq!(lines.len(), 3, "{note}");
        assert_eq!(
            lines[0],
            "the thread context needs 155 bytes: `_start` (0) \u{2192} `main` (100) \u{2192} \
             `divide` (20) \u{2192} `on_panic` (30) \u{2192} `helper` (5)"
        );
        assert!(
            lines[1]
                .starts_with("the maskable handler context needs 99 bytes: exception entry (64)")
        );
        assert!(lines[2].starts_with("the fault context needs 193 bytes: "));
        assert!(lines[2].contains(
            " + exception entry (64) \u{2192} default handler (0) \u{2192} `on_panic` (30)"
        ));
    }

    #[test]
    fn a_stack_that_holds_the_worst_case_exactly_is_enough() {
        for (size, errors) in [(448, 0), (432, 1)] {
            let mir = mir_of(&RISCV32IMAC, with_stack(size, STATES));
            let (report, diags) = run(&mir, &RISCV32IMAC, STATES_FRAMES, 0);
            assert_eq!(diags.len(), errors, "{size}");
            assert_eq!(report.total, Some(447));
        }
    }

    #[test]
    fn an_inserted_call_in_the_path_is_marked_inserted_in_the_note() {
        let mir = mir_of(
            &RISCV32IMAC,
            with_stack(
                16,
                "entry fn main() -> ! { loop {} } panic fn on_panic() -> ! { loop {} }",
            ),
        );
        let deep = mir.body(mir.inserted[0]).name.clone();
        let (_, diags) = run(&mir, &RISCV32IMAC, &[(&deep, 600)], 0);
        let note = diags[0].note.as_deref().unwrap();
        assert!(
            note.contains(&format!("`{}` (600, inserted)", shown(&deep))),
            "{note}"
        );
    }

    #[test]
    fn a_long_chain_of_calls_has_a_depth() {
        let n = 1500;
        let mut text = String::new();
        for i in 0..n {
            if i + 1 < n {
                text += &format!("fn f{i}() {{ f{}(); }}\n", i + 1);
            } else {
                text += &format!("fn f{i}() {{}}\n");
            }
        }
        text += "entry fn main() -> ! { f0(); loop {} }\npanic fn on_panic() -> ! { loop {} }\n";
        let mir = mir_of(&RISCV32IMAC, Box::leak(text.into_boxed_str()));
        let named: Vec<(String, u64)> = (0..n).map(|i| (format!("f{i}"), 2)).collect();
        let named: Vec<(&str, u64)> = named.iter().map(|(name, f)| (name.as_str(), *f)).collect();
        let (report, diags) = run(&mir, &RISCV32IMAC, &named, 0);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(report.depths[id(&mir, "f0")], Some(2 * n as u64));
    }

    /// The line of `name` in the text of `--emit=stack`.
    fn line<'a>(text: &'a str, name: &str) -> &'a str {
        text.lines()
            .find(|line| line.starts_with(&format!("{name}: ")))
            .unwrap_or_else(|| panic!("no line for `{name}` in\n{text}"))
    }

    #[test]
    fn the_report_lists_the_parts_and_a_line_for_each_body() {
        let mir = mir_of(
            &RISCV32IMAC,
            with_stack(
                4096,
                "
const LIMIT: u32 = 3;
fn helper() {}
fn lonely() {}
#[stack(12)]
extern fn outside(x: u32) -> u32;
fn divide(a: u32, b: u32) -> u32 { a / b }
entry fn main() -> ! { helper(); helper(); divide(1, 0); loop {} }
panic fn on_panic() -> ! { loop {} }
",
            ),
        );
        let named = [
            ("main", 100),
            ("divide", 20),
            ("helper", 5),
            ("on_panic", 30),
        ];
        let sizes = frames(&mir, &named);
        let mut diags = Diagnostics::default();
        let report = bound(&mir, &RISCV32IMAC, &sizes, 0, &mut diags);
        assert!(!diags.has_errors());
        let text = report.render(&mir, &sizes);
        let mut lines = text.lines();
        assert_eq!(
            lines.next(),
            Some(format!("worst case: {} bytes, 4096 declared", report.total.unwrap()).as_str())
        );
        // The thread's part with its path, and the unmaskable one, which is 0
        // on riscv32 without a handler, with none.
        assert_eq!(
            lines.next(),
            Some(
                "thread context: 150 bytes: `_start` (0) \u{2192} `main` (100) \u{2192} \
                 `divide` (20) \u{2192} `on_panic` (30)"
            )
        );
        assert!(text.contains("\nunmaskable handler context: 0 bytes\n"));
        assert!(text.contains("\nbodies:\n"));
        // The callees of a body once each, in the order of its edges, then the
        // inserted calls; its frame; `not reached` for the body nothing calls,
        // with its depth all the same.
        assert_eq!(
            line(&text, "main"),
            "main: frame 100, depth 150; calls helper, divide, inserted calls"
        );
        assert_eq!(
            line(&text, "lonely"),
            "lonely: frame 0, depth 0, not reached; calls inserted calls"
        );
        // An `extern fn` has no edges, so no `calls`; its `N` is shown.
        assert_eq!(
            line(&text, "outside"),
            "outside: frame 0, stack 12, depth 12, not reached"
        );
        // The panic function is a root of the search.
        assert!(!line(&text, "on_panic").contains("not reached"));
        // The symbols are the linker's, with their dots; a body run only at
        // compile time has no line.
        assert!(text.contains("\ncore.arch.__critical_enter: "));
        assert!(!text.contains("LIMIT"));
    }

    #[test]
    fn the_report_shows_an_unknown_depth_and_no_comparison_without_memory() {
        let mir = mir_of(
            &RISCV32IMAC,
            "
#[naked]
fn bare() {
    asm(\"ret\", options(noreturn))
}
entry fn main() -> ! { unsafe { bare(); } loop {} }
panic fn on_panic() -> ! { loop {} }
",
        );
        let sizes = frames(&mir, &[]);
        let mut diags = Diagnostics::default();
        let report = bound(&mir, &RISCV32IMAC, &sizes, 0, &mut diags);
        assert_eq!(codes(&diags.into_vec()), ["analysis::missing-stack"]);
        let text = report.render(&mir, &sizes);
        assert!(text.starts_with("worst case: unknown\nbodies:\n"), "{text}");
        assert_eq!(line(&text, "bare"), "bare: frame 0, depth unknown");
        assert!(line(&text, "main").contains("depth unknown"));

        // Without a `memory` block the worst case has no comparison.
        let mir = mir_of(&RISCV32IMAC, STATES);
        let sizes = frames(&mir, STATES_FRAMES);
        let mut diags = Diagnostics::default();
        let report = bound(&mir, &RISCV32IMAC, &sizes, 0, &mut diags);
        let total = report.total.unwrap();
        assert!(
            report
                .render(&mir, &sizes)
                .starts_with(&format!("worst case: {total} bytes\n"))
        );
    }

    /// The exception entry of riscv32 is what the trap entry saves, which
    /// `core` declares with `#[stack(N)]` (ADR 0123).
    #[test]
    fn the_exception_frame_of_riscv32_is_the_trap_entrys_stack() {
        let mir = mir_of(&RISCV32IMAC, STATES);
        let trap: Vec<&Body> = (mir.bodies())
            .map(|(_, body)| body)
            .filter(|body| body.name.ends_with("__erebos_trap"))
            .collect();
        assert_eq!(trap.len(), 1);
        assert_eq!(trap[0].stack, Some(RISCV32IMAC.exception_frame));
        assert_eq!(THUMBV7M.exception_frame, 32 + 4);
    }
}
