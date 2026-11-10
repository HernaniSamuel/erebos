//! The analysis of linearity (ADR 0102, 0103): a value of a linear type must
//! be consumed before every point where the function holding it returns, and
//! is never overwritten.
//!
//! A **linear path** is a local other than `_0` followed by `Field` and
//! `Payload` projections, whose type is a struct marked `#[linear]`; an
//! **origin** is where a value comes into the body (a parameter, a `new`, a
//! call whose result has linear paths); a **fact** is a pair of a path and an
//! origin, and holds where the path holds the value that came in there. A
//! forward analysis computes, at the start of each reachable block, the set of
//! facts that may hold; every transfer kills the facts of some paths and adds
//! others computed from the state, so the sets, and the fixed point, do not
//! depend on the order the blocks are visited in. The errors are found in one
//! pass from those sets and grouped by origin, one error per value.
//!
//! Unlike the analysis of moves, every local is followed, temporaries
//! included: the result of a discarded call is held by one.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::bits::{Bits, key, reachable};
use crate::diag::{Diagnostic, Diagnostics};
use crate::mir::{
    BasicBlock, Body, Local, Operand, Place, PlaceElem, Rvalue, Statement, Terminator, Ty,
};
use crate::span::{FileId, Span};

const NOT_CONSUMED: &str = "analysis::linear-not-consumed";
const OVERWRITTEN: &str = "analysis::linear-overwritten";

/// Reports the linear values of `body` that are not consumed or are
/// overwritten; whether there are none.
pub fn check(body: &Body, diags: &mut Diagnostics) -> bool {
    if body.external || body.blocks.is_empty() {
        return true;
    }
    let flow = Flow::of(body);
    if flow.origins.is_empty() {
        return true;
    }
    let ins = flow.solve(&flow.blocks);
    let sink = flow.faults(&ins);
    let errors = flow.diagnostics(&sink);
    let passed = errors.is_empty();
    for error in errors {
        diags.push(error);
    }
    passed
}

/// A block and a statement of it, the terminator last.
type Loc = (usize, usize);

type Key = (FileId, u32, u32);

/// A linear path and the type of the value it holds.
struct Path {
    local: Local,
    proj: Vec<PlaceElem>,
    ty: Ty,
}

/// How a value comes into the body.
enum Source {
    Param(Local),
    New,
    Call,
}

struct Origin {
    source: Source,
    /// The body for a parameter, the literal for a `new`, the call.
    span: Span,
    /// The paths the value gives when it comes in: the parameter's, the
    /// `new`'s own, those the call's destination is a prefix of.
    paths: Vec<usize>,
}

enum Kind {
    /// Held at a `return`.
    Return,
    /// Held when the write that gave it comes again, round a loop.
    Round,
    /// Held when another value is written over it.
    Overwrite,
    /// A field still held when its struct is ended.
    End,
}

struct Fault {
    kind: Kind,
    origin: usize,
    path: usize,
    span: Span,
}

/// A move that carried the value of `origin` into `path`.
struct Hop {
    origin: usize,
    path: usize,
    span: Span,
}

#[derive(Default)]
struct Sink {
    faults: Vec<Fault>,
    hops: Vec<Hop>,
}

/// An error before it is a diagnostic: errors that have the same span, the
/// same message and the same labels are one.
#[derive(Clone, PartialEq, Eq, Hash)]
struct Error {
    code: &'static str,
    span: Span,
    message: String,
    labels: Vec<(Span, String)>,
    help: &'static str,
}

struct Flow<'b> {
    body: &'b Body,
    paths: Vec<Path>,
    /// The paths of each local, by local.
    by_local: Vec<Vec<usize>>,
    origins: Vec<Origin>,
    /// The origin of the `new` or the call at a location.
    origin_at: HashMap<Loc, usize>,
    /// The bit of each fact. Only a path whose type is one of the types the
    /// origin gives can hold its value.
    bit: HashMap<(usize, usize), usize>,
    /// The facts of each path: the origin and the bit.
    of_path: Vec<Vec<(usize, usize)>>,
    bits: usize,
    /// The blocks reachable from the first, in ascending order.
    blocks: Vec<usize>,
}

impl<'b> Flow<'b> {
    fn of(body: &'b Body) -> Flow<'b> {
        let (blocks, _) = reachable(body);
        let mut paths = Vec::new();
        let mut by_local = vec![Vec::new(); body.locals.len()];
        for (index, decl) in body.locals.iter().enumerate().skip(1) {
            for proj in decl.ty.linear_parts() {
                let Some(ty) = type_at(decl.ty, &proj) else {
                    continue;
                };
                by_local[index].push(paths.len());
                paths.push(Path {
                    local: Local(index as u32),
                    proj,
                    ty,
                });
            }
        }
        let mut flow = Flow {
            body,
            paths,
            by_local,
            origins: Vec::new(),
            origin_at: HashMap::new(),
            bit: HashMap::new(),
            of_path: Vec::new(),
            bits: 0,
            blocks,
        };
        flow.find_origins();
        flow.number_facts();
        flow
    }

    fn find_origins(&mut self) {
        let body = self.body;
        for index in 1..=body.param_count {
            let paths = self.by_local.get(index).cloned().unwrap_or_default();
            if !paths.is_empty() {
                self.origins.push(Origin {
                    source: Source::Param(Local(index as u32)),
                    span: body.span,
                    paths,
                });
            }
        }
        for &block in &self.blocks.clone() {
            let data = &body.blocks[block];
            for (index, statement) in data.statements.iter().enumerate() {
                let span = (data.spans.get(index).copied()).unwrap_or(data.terminator_span);
                let (source, span, paths) = match statement {
                    Statement::New(place, span) => (
                        Source::New,
                        *span,
                        self.path_of(*place).into_iter().collect(),
                    ),
                    Statement::Call {
                        dest: Some(dest), ..
                    } => (Source::Call, span, self.under(*dest)),
                    _ => continue,
                };
                if !paths.is_empty() {
                    self.origin_at.insert((block, index), self.origins.len());
                    self.origins.push(Origin {
                        source,
                        span,
                        paths,
                    });
                }
            }
        }
    }

    /// Numbers the facts: a path holds the value of an origin only if its
    /// type is the type of one of the paths the origin gives.
    fn number_facts(&mut self) {
        self.of_path = vec![Vec::new(); self.paths.len()];
        for (origin, data) in self.origins.iter().enumerate() {
            let types: Vec<Ty> = data.paths.iter().map(|&p| self.paths[p].ty).collect();
            for (path, candidate) in self.paths.iter().enumerate() {
                if types.contains(&candidate.ty) {
                    self.bit.insert((path, origin), self.bits);
                    self.of_path[path].push((origin, self.bits));
                    self.bits += 1;
                }
            }
        }
    }

    /// The linear path that is exactly `place`.
    fn path_of(&self, place: Place) -> Option<usize> {
        self.find(place.local, place.projection)
    }

    fn find(&self, local: Local, proj: &[PlaceElem]) -> Option<usize> {
        (self.by_local.get(local.0 as usize)?.iter().copied())
            .find(|&path| self.paths[path].proj == proj)
    }

    /// The linear paths that `place` is a prefix of: the place, if it is one,
    /// and its parts.
    fn under(&self, place: Place) -> Vec<usize> {
        let Some(paths) = self.by_local.get(place.local.0 as usize) else {
            return Vec::new();
        };
        (paths.iter().copied())
            .filter(|&path| self.paths[path].proj.starts_with(place.projection))
            .collect()
    }

    /// The facts at the start of the body: those of every parameter.
    fn start(&self) -> Bits {
        let mut start = Bits::new(self.bits);
        for (origin, data) in self.origins.iter().enumerate() {
            if matches!(data.source, Source::Param(_)) {
                for &path in &data.paths {
                    self.generate(&mut start, path, origin);
                }
            }
        }
        start
    }

    /// The facts at the start of each reachable block, by visiting `order`
    /// until none changes; `None` for a block nothing reached.
    fn solve(&self, order: &[usize]) -> Vec<Option<Bits>> {
        let mut ins: Vec<Option<Bits>> = vec![None; self.body.blocks.len()];
        ins[0] = Some(self.start());
        loop {
            let mut changed = false;
            for &block in order {
                let Some(mut state) = ins[block].clone() else {
                    continue;
                };
                self.block(&mut state, block, None);
                for (next, edge) in self.edges(block, &state) {
                    match &mut ins[next] {
                        Some(bits) => changed |= bits.union(&edge),
                        none => {
                            *none = Some(edge);
                            changed = true;
                        }
                    }
                }
            }
            if !changed {
                return ins;
            }
        }
    }

    /// Every fault, found in one pass over the reachable blocks from their
    /// sets at the fixed point.
    fn faults(&self, ins: &[Option<Bits>]) -> Sink {
        let mut sink = Sink::default();
        for &block in &self.blocks {
            if let Some(mut state) = ins[block].clone() {
                self.block(&mut state, block, Some(&mut sink));
            }
        }
        sink
    }

    /// Applies the statements of a block and its terminator to `state`, and
    /// records the faults they make in `sink`.
    fn block(&self, state: &mut Bits, block: usize, mut sink: Option<&mut Sink>) {
        let data = &self.body.blocks[block];
        for (index, statement) in data.statements.iter().enumerate() {
            let span = (data.spans.get(index).copied()).unwrap_or(data.terminator_span);
            self.statement(state, (block, index), span, statement, sink.as_deref_mut());
        }
        data.terminator
            .visit_operands(&mut |operand| self.moved(state, operand));
        if let (Terminator::Return, Some(sink)) = (&data.terminator, sink) {
            for (path, facts) in self.of_path.iter().enumerate() {
                for &(origin, bit) in facts {
                    if state.contains(bit) {
                        sink.faults.push(Fault {
                            kind: Kind::Return,
                            origin,
                            path,
                            span: data.terminator_span,
                        });
                    }
                }
            }
        }
    }

    /// One statement: its operands, left to right, then its write (ADR 0103).
    fn statement(
        &self,
        state: &mut Bits,
        at: Loc,
        span: Span,
        statement: &Statement,
        mut sink: Option<&mut Sink>,
    ) {
        match statement {
            Statement::Assign(dest, Rvalue::Use(Operand::Move(from, moved))) => {
                // What the value held, read before the move kills it.
                let carried = self.carried(state, *from, *dest);
                self.kill(state, *from);
                let incoming: Vec<usize> = carried.iter().map(|&(_, origin)| origin).collect();
                self.write(
                    state,
                    &self.under(*dest),
                    &incoming,
                    (span, *moved),
                    sink.as_deref_mut(),
                );
                for (path, origin) in carried {
                    self.generate(state, path, origin);
                    if let Some(sink) = sink.as_deref_mut() {
                        sink.hops.push(Hop {
                            origin,
                            path,
                            span: *moved,
                        });
                    }
                }
            }
            Statement::Call { dest, args, .. } => {
                for arg in args {
                    self.moved(state, arg);
                }
                if let (Some(dest), Some(&origin)) = (dest, self.origin_at.get(&at)) {
                    self.write(state, &self.under(*dest), &[origin], (span, span), sink);
                    self.come_in(state, origin);
                }
            }
            Statement::New(..) => {
                if let Some(&origin) = self.origin_at.get(&at) {
                    let span = self.origins[origin].span;
                    let paths = &self.origins[origin].paths;
                    self.write(state, paths, &[origin], (span, span), sink);
                    self.come_in(state, origin);
                }
            }
            Statement::End(place, end) => {
                for path in self.under(*place) {
                    if self.paths[path].proj.len() > place.projection.len() {
                        self.held(state, path, |origin| {
                            if let Some(sink) = sink.as_deref_mut() {
                                sink.faults.push(Fault {
                                    kind: Kind::End,
                                    origin,
                                    path,
                                    span: *end,
                                });
                            }
                        });
                    }
                }
                self.kill(state, *place);
            }
            other => other.visit_operands(&mut |operand| self.moved(state, operand)),
        }
    }

    /// The facts the value moved from `from` into `dest` brings: the paths
    /// that `from` is a prefix of, followed in `dest`, with their origins.
    fn carried(&self, state: &Bits, from: Place, dest: Place) -> Vec<(usize, usize)> {
        let mut carried = Vec::new();
        for path in self.under(from) {
            let mut target = dest.projection.to_vec();
            target.extend_from_slice(&self.paths[path].proj[from.projection.len()..]);
            let Some(to) = self.find(dest.local, &target) else {
                continue;
            };
            self.held(state, path, |origin| carried.push((to, origin)));
        }
        carried
    }

    /// Calls `f` with the origin of each fact of `path` that holds.
    fn held(&self, state: &Bits, path: usize, mut f: impl FnMut(usize)) {
        for &(origin, bit) in &self.of_path[path] {
            if state.contains(bit) {
                f(origin);
            }
        }
    }

    fn generate(&self, state: &mut Bits, path: usize, origin: usize) {
        if let Some(&bit) = self.bit.get(&(path, origin)) {
            state.insert(bit);
        }
    }

    /// The facts an origin brings.
    fn come_in(&self, state: &mut Bits, origin: usize) {
        for &path in &self.origins[origin].paths {
            self.generate(state, path, origin);
        }
    }

    /// A `move` kills the facts of the place and of its parts.
    fn moved(&self, state: &mut Bits, operand: &Operand) {
        if let Operand::Move(place, _) = operand {
            self.kill(state, *place);
        }
    }

    fn kill(&self, state: &mut Bits, place: Place) {
        for path in self.under(place) {
            self.kill_path(state, path);
        }
    }

    fn kill_path(&self, state: &mut Bits, path: usize) {
        for &(_, bit) in &self.of_path[path] {
            state.remove(bit);
        }
    }

    /// A write to `paths`: what they hold is lost, which is a fault. The
    /// value that comes in has the origins `incoming`; a held value of one of
    /// them came round a loop without being consumed, which is reported at
    /// `round`, and another value is overwritten, which is reported at `span`.
    /// The two differ for a `let`: its statement has the span of the block
    /// that holds it, and the value moved is the span of its initializer.
    fn write(
        &self,
        state: &mut Bits,
        paths: &[usize],
        incoming: &[usize],
        (span, round): (Span, Span),
        mut sink: Option<&mut Sink>,
    ) {
        for &path in paths {
            self.held(state, path, |origin| {
                if let Some(sink) = sink.as_deref_mut() {
                    let (kind, span) = if incoming.contains(&origin) {
                        (Kind::Round, round)
                    } else {
                        (Kind::Overwrite, span)
                    };
                    sink.faults.push(Fault {
                        kind,
                        origin,
                        path,
                        span,
                    });
                }
            });
            self.kill_path(state, path);
        }
    }

    /// The state at the start of each block `block` goes to.
    fn edges(&self, block: usize, state: &Bits) -> Vec<(usize, Bits)> {
        let plain = |next: BasicBlock| (next.0 as usize, state.clone());
        let edges = match self.body.blocks[block].terminator {
            Terminator::Goto(next) => vec![plain(next)],
            Terminator::If { then, else_, .. } => vec![plain(then), plain(else_)],
            Terminator::Switch {
                discr,
                cases,
                otherwise,
            } => {
                let mut edges = Vec::new();
                for &(value, next) in cases {
                    let mut edge = plain(next);
                    self.refine(&mut edge.1, discr, Some(value), cases);
                    edges.push(edge);
                }
                let mut edge = plain(otherwise);
                self.refine(&mut edge.1, discr, None, cases);
                edges.push(edge);
                edges
            }
            Terminator::Return
            | Terminator::Unreachable
            | Terminator::Panic(..)
            | Terminator::InlineAsm(_) => Vec::new(),
        };
        let len = self.body.blocks.len();
        edges.into_iter().filter(|&(next, _)| next < len).collect()
    }

    /// The one refinement (ADR 0103): on the edge out of a `switch` on the
    /// tag of a place `p`, the payload values of the variants that cannot be
    /// the value hold nothing. `value` is the case of the edge, `None` for
    /// `otherwise`.
    fn refine(
        &self,
        state: &mut Bits,
        discr: Operand,
        value: Option<u64>,
        cases: &[(u64, BasicBlock)],
    ) {
        let Operand::Copy(place) = discr else {
            return;
        };
        let [head @ .., PlaceElem::Tag] = place.projection else {
            return;
        };
        let Some(Ty::Enum(e)) = type_at(self.body.local_ty(place.local), head) else {
            return;
        };
        let values: Vec<Option<u64>> = (e.def().variants().iter())
            .map(|variant| variant.value.get().copied())
            .collect();
        let count = values.len() as u32;
        // The variants have values of their own (ADR 0104): on the edge of a
        // case only the variant with that value is possible, and on
        // `otherwise` none that has a case. A value that is not stored is no
        // case's: it is not known only for a variant no `switch` names (a
        // case is a value that was asked for, and two variants never have one
        // value), so it is dead on the edge of a case and alive on
        // `otherwise`.
        let dead: Vec<u32> = match value {
            Some(case) => match values.iter().position(|&v| v == Some(case)) {
                Some(live) => (0..count).filter(|&k| k as usize != live).collect(),
                None => return,
            },
            None => (0..count)
                .filter(|&k| {
                    values[k as usize].is_some_and(|v| cases.iter().any(|&(case, _)| case == v))
                })
                .collect(),
        };
        let Some(paths) = self.by_local.get(place.local.0 as usize) else {
            return;
        };
        for &path in paths {
            let proj = &self.paths[path].proj;
            let Some(PlaceElem::Payload { variant, .. }) = proj.get(head.len()) else {
                continue;
            };
            if proj.starts_with(head) && dead.contains(variant) {
                self.kill_path(state, path);
            }
        }
    }

    // -- Errors ------------------------------------------------------------

    /// The errors, one per value: ordered by the span they are at, and one
    /// for two that have the same span, message and labels (ADR 0099, 0103).
    fn diagnostics(&self, sink: &Sink) -> Vec<Diagnostic> {
        let mut late: BTreeMap<usize, Vec<&Fault>> = BTreeMap::new();
        let mut ends: BTreeMap<(Key, usize), Vec<&Fault>> = BTreeMap::new();
        let mut over: BTreeMap<usize, Vec<&Fault>> = BTreeMap::new();
        for fault in &sink.faults {
            match fault.kind {
                Kind::Return | Kind::Round => late.entry(fault.origin).or_default().push(fault),
                Kind::End => ends
                    .entry((key(fault.span), fault.origin))
                    .or_default()
                    .push(fault),
                Kind::Overwrite => over.entry(fault.origin).or_default().push(fault),
            }
        }
        let mut errors = Vec::new();
        for (&origin, faults) in &late {
            errors.push(self.not_consumed(origin, faults, &sink.hops));
        }
        for faults in ends.values() {
            errors.push(self.not_consumed_at_end(faults, &sink.hops));
        }
        for (&origin, faults) in &over {
            // A value reported as not consumed is not reported again.
            let reported = late.contains_key(&origin) || ends.keys().any(|&(_, o)| o == origin);
            if !reported {
                errors.push(self.overwritten(origin, faults, &sink.hops));
            }
        }
        errors.sort_by_key(|error| key(error.span));
        let mut seen = HashSet::new();
        (errors.into_iter())
            .filter(|error| seen.insert(error.clone()))
            .map(|error| {
                let mut diag = Diagnostic::error(error.code, error.message);
                for (span, text) in error.labels {
                    diag = diag.label(span, text);
                }
                diag.help(error.help)
            })
            .collect()
    }

    /// A value held at a return, or again at the write that made it, with a
    /// label at each; at the point where it came in.
    fn not_consumed(&self, origin: usize, faults: &[&Fault], hops: &[Hop]) -> Error {
        let data = &self.origins[origin];
        let mut faults = faults.to_vec();
        faults.sort_by_key(|fault| (key(fault.span), fault.path));
        let paths = self.distinct_paths(&faults);
        let (subject, plural) = self.subject(&paths);
        let mut labels = vec![(data.span, self.origin_label(data))];
        let mut groups: BTreeMap<(Key, bool), (Span, Vec<usize>)> = BTreeMap::new();
        for fault in &faults {
            let round = matches!(fault.kind, Kind::Round);
            let group =
                (groups.entry((key(fault.span), round))).or_insert((fault.span, Vec::new()));
            if !group.1.contains(&fault.path) {
                group.1.push(fault.path);
            }
        }
        for ((_, round), (span, paths)) in groups {
            let (list, plural) = self.list(&paths);
            let is = if plural { "are" } else { "is" };
            labels.push((
                span,
                if round {
                    format!(
                        "{list} {is} still held when a value of the same origin is written \
                         here"
                    )
                } else {
                    format!("{list} {is} still held when the function returns here")
                },
            ));
        }
        self.hop_labels(origin, &paths, hops, &mut labels);
        Error {
            code: NOT_CONSUMED,
            span: data.span,
            message: format!(
                "{subject} {} not consumed",
                if plural { "are" } else { "is" }
            ),
            labels: merged(labels),
            help: "consume it: pass it to a function that does, return it, or end it with \
                   `end` where its fields are visible (ADR 0102)",
        }
    }

    /// A field still held when its struct is ended, at the `end`.
    fn not_consumed_at_end(&self, faults: &[&Fault], hops: &[Hop]) -> Error {
        let origin = faults[0].origin;
        let data = &self.origins[origin];
        let span = faults[0].span;
        let paths = self.distinct_paths(faults);
        let (subject, plural) = self.subject(&paths);
        let (list, _) = self.list(&paths);
        let mut labels = vec![(
            span,
            format!(
                "{list} {} still held when `end` runs",
                if plural { "are" } else { "is" }
            ),
        )];
        labels.push((data.span, self.origin_label(data)));
        self.hop_labels(origin, &paths, hops, &mut labels);
        Error {
            code: NOT_CONSUMED,
            span,
            message: format!(
                "{subject} {} not consumed before `end`",
                if plural { "are" } else { "is" }
            ),
            labels: merged(labels),
            help: "move the linear field out of the struct and consume it before `end`; `end` \
                   consumes only the struct (ADR 0102)",
        }
    }

    /// A held value written over, at the first write, labeling the others and
    /// where the value came in.
    fn overwritten(&self, origin: usize, faults: &[&Fault], hops: &[Hop]) -> Error {
        let data = &self.origins[origin];
        let mut faults = faults.to_vec();
        faults.sort_by_key(|fault| (key(fault.span), fault.path));
        let paths = self.distinct_paths(&faults);
        let (subject, plural) = self.subject(&paths);
        let mut labels = Vec::new();
        let mut writes: BTreeMap<Key, (Span, Vec<usize>)> = BTreeMap::new();
        for fault in &faults {
            let group = (writes.entry(key(fault.span))).or_insert((fault.span, Vec::new()));
            if !group.1.contains(&fault.path) {
                group.1.push(fault.path);
            }
        }
        for (span, paths) in writes.values() {
            let (list, _) = self.list(paths);
            labels.push((
                span.to_owned(),
                format!("overwrites the value held by {list}"),
            ));
        }
        labels.push((data.span, self.origin_label(data)));
        self.hop_labels(origin, &paths, hops, &mut labels);
        let span = faults[0].span;
        Error {
            code: OVERWRITTEN,
            span,
            message: format!(
                "{subject} {} overwritten",
                if plural { "are" } else { "is" }
            ),
            labels: merged(labels),
            help: "consume the value before writing the place: pass it to a function that \
                   does, return it, or end it with `end` where its fields are visible (ADR 0102)",
        }
    }

    fn origin_label(&self, origin: &Origin) -> String {
        match origin.source {
            Source::Param(local) => {
                let name = (self.body.locals.get(local.0 as usize)).and_then(|d| d.name.as_deref());
                match name {
                    Some(name) => format!("linear value comes in as the parameter `{name}`"),
                    None => "linear value comes in as a parameter".to_owned(),
                }
            }
            Source::New => "linear value is made here".to_owned(),
            Source::Call => "linear value comes from this call".to_owned(),
        }
    }

    /// Labels where the value of `origin` was moved into each of `paths`.
    fn hop_labels(
        &self,
        origin: usize,
        paths: &[usize],
        hops: &[Hop],
        labels: &mut Vec<(Span, String)>,
    ) {
        for &path in paths {
            let target = self.describe(path);
            for hop in hops.iter().filter(|h| h.origin == origin && h.path == path) {
                if hop.span != self.origins[origin].span {
                    labels.push((
                        hop.span,
                        if target == THIS {
                            "moved here".to_owned()
                        } else {
                            format!("moved into {target} here")
                        },
                    ));
                }
            }
        }
    }

    fn distinct_paths(&self, faults: &[&Fault]) -> Vec<usize> {
        let mut paths: Vec<usize> = faults.iter().map(|fault| fault.path).collect();
        paths.sort_unstable();
        paths.dedup();
        paths
    }

    /// The names of `paths`, each once, and whether they are several.
    fn list(&self, paths: &[usize]) -> (String, bool) {
        let names = self.names(paths);
        (join(&names), names.len() > 1)
    }

    fn names(&self, paths: &[usize]) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for &path in paths {
            let name = self.describe(path);
            if !names.contains(&name) {
                names.push(name);
            }
        }
        names
    }

    /// What an error is about: the places that hold the value, and its type
    /// when one place does; and whether the places are several.
    fn subject(&self, paths: &[usize]) -> (String, bool) {
        let names = self.names(paths);
        let named: Vec<String> = names.iter().filter(|name| *name != THIS).cloned().collect();
        let mut types: Vec<Ty> = paths.iter().map(|&path| self.paths[path].ty).collect();
        types.dedup();
        let of_type = match types.as_slice() {
            [ty] if names.len() == 1 => format!(" of type `{ty}`"),
            _ => String::new(),
        };
        let subject = match named.len() {
            0 => format!("linear value{of_type}"),
            1 => format!("linear value {}{of_type}", named[0]),
            _ => format!("linear values {}", join(&named)),
        };
        (subject, names.len() > 1)
    }

    /// The place as the source names it: the name of the variable, followed
    /// by its fields, and for a payload value the variant that holds it, as
    /// `` `o` (in `.Some`) ``; "this value" for a temporary.
    fn describe(&self, path: usize) -> String {
        let path = &self.paths[path];
        let decl = &self.body.locals[path.local.0 as usize];
        let Some(name) = &decl.name else {
            return THIS.to_owned();
        };
        let mut text = name.clone();
        let mut notes = Vec::new();
        let mut ty = decl.ty;
        for elem in &path.proj {
            match (elem, ty) {
                (PlaceElem::Field(index), Ty::Struct(s)) => {
                    let Some(field) = s.def().fields().get(*index as usize) else {
                        break;
                    };
                    text.push('.');
                    text.push_str(&field.name);
                    ty = field.ty;
                }
                (PlaceElem::Payload { variant, index }, Ty::Enum(e)) => {
                    let Some(data) = e.def().variants().get(*variant as usize) else {
                        break;
                    };
                    let Some(value) = data.payload.get(*index as usize) else {
                        break;
                    };
                    notes.push(if data.payload.len() > 1 {
                        format!("in `.{}`, value {}", data.name, index + 1)
                    } else {
                        format!("in `.{}`", data.name)
                    });
                    ty = value.ty;
                }
                _ => break,
            }
        }
        let mut described = format!("`{text}`");
        for note in notes {
            described.push_str(&format!(" ({note})"));
        }
        described
    }
}

/// How an error names a temporary.
const THIS: &str = "this value";

/// The type at the end of a path of `Field` and `Payload` projections.
fn type_at(ty: Ty, path: &[PlaceElem]) -> Option<Ty> {
    path.iter().try_fold(ty, |ty, elem| match (elem, ty) {
        (PlaceElem::Field(index), Ty::Struct(s)) => {
            s.def().fields().get(*index as usize).map(|f| f.ty)
        }
        (PlaceElem::Payload { variant, index }, Ty::Enum(e)) => e
            .def()
            .variants()
            .get(*variant as usize)
            .and_then(|variant| variant.payload.get(*index as usize))
            .map(|value| value.ty),
        _ => None,
    })
}

/// `a`, `a and b`, `a, b and c`.
fn join(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [only] => only.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// The labels of an error: the first stays first, the others are in the order
/// of their spans, and the texts of one span are one label.
fn merged(labels: Vec<(Span, String)>) -> Vec<(Span, String)> {
    let mut labels = labels.into_iter();
    let Some(mut first) = labels.next() else {
        return Vec::new();
    };
    let mut rest: BTreeMap<Key, (Span, Vec<String>)> = BTreeMap::new();
    for (span, text) in labels {
        if span == first.0 {
            if !first.1.split("; ").any(|part| part == text) {
                first.1 = format!("{}; {text}", first.1);
            }
            continue;
        }
        let texts = &mut rest.entry(key(span)).or_insert((span, Vec::new())).1;
        if !texts.contains(&text) {
            texts.push(text);
        }
    }
    let mut merged = vec![first];
    merged.extend(
        rest.into_values()
            .map(|(span, texts)| (span, texts.join("; "))),
    );
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    use crate::mir::{
        BasicBlockData, BodyId, Const, EnumId, EnumRef, Field, IntTy, Layout, LocalDecl,
        PanicCause, StructRef, Variant,
    };
    use crate::span::SourceMap;

    fn span(lo: usize) -> Span {
        let file = SourceMap::default().add("main.ers", "");
        Span::new(file, lo, lo + 1)
    }

    fn field(name: &str, ty: Ty, offset: u64) -> Field {
        Field {
            name: name.to_owned(),
            ty,
            offset,
        }
    }

    const U32: Ty = Ty::Int(IntTy::U32);

    struct Types {
        /// `#[linear] struct Transfer { channel: u32 }`.
        transfer: Ty,
        /// `struct Pair { a: Transfer, b: Transfer }`, not linear.
        pair: Ty,
        /// `#[linear] struct Holder { inner: Transfer, n: u32 }`.
        holder: Ty,
        /// `Option(Transfer)`: `None` is variant 0, `Some` variant 1.
        option: Ty,
    }

    fn types() -> Types {
        valued(Some(0), Some(1))
    }

    /// The same, where `None` has the value `none` and `Some` the value `some`.
    fn valued(none: Option<u64>, some: Option<u64>) -> Types {
        let make = |name: &str, linear: bool, fields: Vec<Field>| {
            let s = StructRef::new(
                name.to_owned(),
                Layout { size: 8, align: 4 },
                false,
                false,
                linear,
            );
            s.def().set_fields(fields);
            Ty::Struct(s)
        };
        let transfer = make("Transfer", true, vec![field("channel", U32, 0)]);
        let pair = make(
            "Pair",
            false,
            vec![field("a", transfer, 0), field("b", transfer, 4)],
        );
        let holder = make(
            "Holder",
            true,
            vec![field("inner", transfer, 0), field("n", U32, 4)],
        );
        let option = EnumRef::new(
            EnumId(1),
            "Option".to_owned(),
            IntTy::U8,
            Layout { size: 8, align: 4 },
            false,
        );
        option.def().set_variants(vec![
            Variant {
                name: "None".to_owned(),
                value: none.map_or_else(Default::default, OnceLock::from),
                payload: Vec::new(),
            },
            Variant {
                name: "Some".to_owned(),
                value: some.map_or_else(Default::default, OnceLock::from),
                payload: vec![field("0", transfer, 4)],
            },
        ]);
        Types {
            transfer,
            pair,
            holder,
            option: Ty::Enum(option),
        }
    }

    fn local(n: u32) -> Place {
        Place::local(Local(n))
    }

    fn projected(n: u32, projection: &[PlaceElem]) -> Place {
        Place {
            local: Local(n),
            projection: Box::leak(projection.to_vec().into_boxed_slice()),
        }
    }

    const SOME: PlaceElem = PlaceElem::Payload {
        variant: 1,
        index: 0,
    };

    /// A move of `place`, written at byte `at`.
    fn mv(place: Place, at: usize) -> Operand {
        Operand::Move(place, span(at))
    }

    /// `dest = move from`.
    fn moved(dest: Place, from: Place, at: usize) -> Statement {
        Statement::Assign(dest, Rvalue::Use(mv(from, at)))
    }

    /// `dest = call`, with no arguments.
    fn call(dest: u32) -> Statement {
        Statement::Call {
            dest: Some(local(dest)),
            callee: BodyId(0),
            args: Vec::new(),
        }
    }

    /// A call that consumes `place`.
    fn consume(place: Place, at: usize) -> Statement {
        Statement::Call {
            dest: None,
            callee: BodyId(0),
            args: vec![mv(place, at)],
        }
    }

    fn new(place: Place) -> Statement {
        Statement::New(place, span(300))
    }

    fn end(place: Place, at: usize) -> Statement {
        Statement::End(place, span(at))
    }

    /// A block whose statements have the spans `base`, `base + 1`, ..., and
    /// whose terminator has the span `base + 90`.
    fn block(base: usize, statements: Vec<Statement>, terminator: Terminator) -> BasicBlockData {
        BasicBlockData {
            spans: (0..statements.len()).map(|i| span(base + i)).collect(),
            statements,
            terminator,
            terminator_span: span(base + 90),
        }
    }

    fn goto(block: u32) -> Terminator {
        Terminator::Goto(BasicBlock(block))
    }

    fn branch(then: u32, else_: u32) -> Terminator {
        Terminator::If {
            cond: Operand::Const(Const::Bool(true)),
            then: BasicBlock(then),
            else_: BasicBlock(else_),
        }
    }

    fn switch(discr: Place, cases: &[(u64, u32)], otherwise: u32) -> Terminator {
        Terminator::Switch {
            discr: Operand::Copy(discr),
            cases: Box::leak(
                (cases.iter())
                    .map(|&(value, block)| (value, BasicBlock(block)))
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
            ),
            otherwise: BasicBlock(otherwise),
        }
    }

    /// A body whose locals `_1..` have these types and names (`None` for a
    /// temporary), the first `params` of them parameters.
    fn body(locals: &[(Ty, Option<&str>)], params: usize, blocks: Vec<BasicBlockData>) -> Body {
        let mut decls = vec![LocalDecl {
            ty: Ty::Unit,
            name: None,
            constant: None,
        }];
        decls.extend(locals.iter().map(|&(ty, name)| LocalDecl {
            ty,
            name: name.map(str::to_owned),
            constant: None,
        }));
        Body {
            name: "f".to_owned(),
            span: span(0),
            comptime: false,
            ret: Ty::Unit,
            param_count: params,
            locals: decls,
            blocks,
            runtime: false,
            external: false,
            naked: false,
            align: None,
        }
    }

    /// An error as its code, message and the texts of its labels.
    type Shown = (String, String, Vec<String>);

    /// The errors of `body`.
    fn errors_of(body: &Body) -> Vec<Shown> {
        let mut diags = Diagnostics::default();
        let passed = check(body, &mut diags);
        let list = diags.into_vec();
        assert_eq!(passed, list.is_empty());
        list.into_iter()
            .map(|diag| {
                let labels = diag.labels.iter().map(|l| l.message.clone()).collect();
                (diag.code.to_owned(), diag.message, labels)
            })
            .collect()
    }

    fn only(errors: Vec<Shown>) -> Shown {
        assert_eq!(errors.len(), 1, "{errors:#?}");
        errors.into_iter().next().unwrap_or_default()
    }

    #[test]
    fn a_parameter_never_moved_is_not_consumed() {
        let t = types();
        let body = body(
            &[(t.transfer, Some("t"))],
            1,
            vec![block(100, vec![], Terminator::Return)],
        );
        let (code, message, labels) = only(errors_of(&body));
        assert_eq!(code, NOT_CONSUMED);
        assert_eq!(
            message,
            "linear value `t` of type `Transfer` is not consumed"
        );
        assert_eq!(
            labels,
            [
                "linear value comes in as the parameter `t`",
                "`t` is still held when the function returns here"
            ]
        );
    }

    #[test]
    fn a_temporary_never_moved_is_not_consumed() {
        let t = types();
        let body = body(
            &[(t.transfer, None)],
            0,
            vec![block(100, vec![call(1)], Terminator::Return)],
        );
        let (code, message, labels) = only(errors_of(&body));
        assert_eq!(code, NOT_CONSUMED);
        assert_eq!(message, "linear value of type `Transfer` is not consumed");
        assert_eq!(
            labels,
            [
                "linear value comes from this call",
                "this value is still held when the function returns here"
            ]
        );
    }

    #[test]
    fn a_new_never_moved_is_not_consumed_and_one_moved_is_not() {
        let t = types();
        let forgotten = body(
            &[(t.transfer, None)],
            0,
            vec![block(100, vec![new(local(1))], Terminator::Return)],
        );
        let (_, _, labels) = only(errors_of(&forgotten));
        assert_eq!(labels[0], "linear value is made here");
        let given = body(
            &[(t.transfer, None), (t.transfer, Some("t"))],
            0,
            vec![block(
                100,
                vec![
                    new(local(1)),
                    moved(local(2), local(1), 10),
                    consume(local(2), 20),
                ],
                Terminator::Return,
            )],
        );
        assert!(errors_of(&given).is_empty());
    }

    #[test]
    fn a_move_on_one_branch_only_leaves_the_value_held_at_the_join() {
        let t = types();
        let locals = [(t.transfer, Some("t"))];
        let one = body(
            &locals,
            1,
            vec![
                block(100, vec![], branch(1, 2)),
                block(200, vec![consume(local(1), 10)], goto(3)),
                block(300, vec![], goto(3)),
                block(400, vec![], Terminator::Return),
            ],
        );
        let (code, _, labels) = only(errors_of(&one));
        assert_eq!(code, NOT_CONSUMED);
        assert_eq!(labels.len(), 2, "{labels:?}");
        let both = body(
            &locals,
            1,
            vec![
                block(100, vec![], branch(1, 2)),
                block(200, vec![consume(local(1), 10)], goto(3)),
                block(300, vec![consume(local(1), 20)], goto(3)),
                block(400, vec![], Terminator::Return),
            ],
        );
        assert!(errors_of(&both).is_empty());
    }

    /// `o: Option(Transfer)` held, then a `switch` on `tag` with these cases
    /// to `bb1` and `otherwise` to `bb2`, each of which returns after the
    /// statements given. `tag` is a copy of the tag of `o`, or the tag.
    fn tested(
        cases: &[(u64, u32)],
        first: Vec<Statement>,
        second: Vec<Statement>,
        tag: Place,
    ) -> Body {
        tested_with(types(), cases, first, second, tag)
    }

    fn tested_with(
        t: Types,
        cases: &[(u64, u32)],
        first: Vec<Statement>,
        second: Vec<Statement>,
        tag: Place,
    ) -> Body {
        body(
            &[
                (t.option, Some("o")),
                (t.transfer, Some("t")),
                (Ty::Int(IntTy::U8), Some("tag")),
            ],
            1,
            vec![
                block(
                    100,
                    vec![Statement::Assign(
                        local(3),
                        Rvalue::Use(Operand::Copy(projected(1, &[PlaceElem::Tag]))),
                    )],
                    switch(tag, cases, 2),
                ),
                block(200, first, Terminator::Return),
                block(300, second, Terminator::Return),
            ],
        )
    }

    fn take_payload() -> Vec<Statement> {
        vec![
            moved(local(2), projected(1, &[SOME]), 10),
            consume(local(2), 20),
        ]
    }

    #[test]
    fn a_test_of_the_tag_shows_which_variant_the_edge_is_for() {
        let on_o = projected(1, &[PlaceElem::Tag]);
        // The edge of `Some` consumes the payload, and on `otherwise` the
        // value is not `Some`, so nothing is owed there. The cases are 1 in
        // the blocks `bb1`; `otherwise` is `bb2`.
        let some = tested(&[(1, 1)], take_payload(), vec![], on_o);
        assert!(errors_of(&some).is_empty());
        // The case `None`: nothing is owed there; `otherwise` consumes it.
        let none = tested(&[(0, 1)], vec![], take_payload(), on_o);
        assert!(errors_of(&none).is_empty());
        // The case `Some` that forgets the payload: an error at that return
        // only.
        let forgotten = tested(&[(1, 1)], vec![], vec![], on_o);
        let (_, message, labels) = only(errors_of(&forgotten));
        assert_eq!(
            message,
            "linear value `o` (in `.Some`) of type `Transfer` is not consumed"
        );
        assert_eq!(labels.len(), 2, "{labels:?}");
        // Both cases listed: `otherwise` is no variant and owes nothing.
        let both = tested(&[(0, 1), (1, 1)], take_payload(), vec![], on_o);
        assert!(errors_of(&both).is_empty());
    }

    #[test]
    fn a_tag_copied_to_another_local_refines_nothing() {
        // `tag = copy o.tag`, then a `switch` on `tag`: both edges still hold
        // the payload.
        let copied = tested(&[(0, 1)], vec![], take_payload(), local(3));
        let (code, _, labels) = only(errors_of(&copied));
        assert_eq!(code, NOT_CONSUMED);
        assert_eq!(labels.len(), 2, "{labels:?}");
    }

    #[test]
    fn the_variant_of_an_edge_is_found_by_its_value_not_its_position() {
        let on_o = projected(1, &[PlaceElem::Tag]);
        // `None` is 5 and `Some` is 3.
        let some = tested_with(
            valued(Some(5), Some(3)),
            &[(3, 1)],
            take_payload(),
            vec![],
            on_o,
        );
        assert!(errors_of(&some).is_empty());
        let none = tested_with(
            valued(Some(5), Some(3)),
            &[(5, 1)],
            vec![],
            take_payload(),
            on_o,
        );
        assert!(errors_of(&none).is_empty());
        let forgotten = tested_with(valued(Some(5), Some(3)), &[(3, 1)], vec![], vec![], on_o);
        assert_eq!(errors_of(&forgotten).len(), 1);
        // `Some` has no value stored (it is being computed, or failed): it is
        // dead on the edge of the case `None`, and alive on `otherwise`.
        let unknown = tested_with(
            valued(Some(0), None),
            &[(0, 1)],
            vec![],
            take_payload(),
            on_o,
        );
        assert!(errors_of(&unknown).is_empty());
        // A case that is no variant's value refines nothing.
        let stray = tested_with(
            valued(Some(5), Some(3)),
            &[(9, 1)],
            vec![],
            take_payload(),
            on_o,
        );
        assert_eq!(errors_of(&stray).len(), 1);
    }

    #[test]
    fn a_move_carries_what_is_held_with_its_origin() {
        let t = types();
        // `_1 = call; a = move _1; b = move a;` then a return: the error is
        // at the call, naming `b`, and says where the value moved to `b`.
        let chain = body(
            &[
                (t.transfer, None),
                (t.transfer, Some("a")),
                (t.transfer, Some("b")),
            ],
            0,
            vec![block(
                100,
                vec![
                    call(1),
                    moved(local(2), local(1), 10),
                    moved(local(3), local(2), 20),
                ],
                Terminator::Return,
            )],
        );
        let (_, message, labels) = only(errors_of(&chain));
        assert_eq!(
            message,
            "linear value `b` of type `Transfer` is not consumed"
        );
        assert_eq!(labels[0], "linear value comes from this call");
        assert!(
            labels.contains(&"moved into `b` here".to_owned()),
            "{labels:?}"
        );
        assert!(labels.iter().all(|l| !l.contains("`a`")), "{labels:?}");
    }

    #[test]
    fn a_literal_holds_what_its_fields_held_and_a_part_consumed_is_left_out() {
        let t = types();
        // `_1 = call; _2 = call; _3.a = move _1; _3.b = move _2; p = move _3;`
        // then `p.a` is consumed: `p.b` is the one forgotten.
        let locals = [
            (t.transfer, None),
            (t.transfer, None),
            (t.pair, None),
            (t.pair, Some("p")),
        ];
        let build = vec![
            call(1),
            call(2),
            moved(projected(3, &[PlaceElem::Field(0)]), local(1), 10),
            moved(projected(3, &[PlaceElem::Field(1)]), local(2), 20),
            moved(local(4), local(3), 30),
        ];
        let mut partly = build.clone();
        partly.push(consume(projected(4, &[PlaceElem::Field(0)]), 40));
        let partly = body(&locals, 0, vec![block(100, partly, Terminator::Return)]);
        let (_, message, _) = only(errors_of(&partly));
        assert_eq!(
            message,
            "linear value `p.b` of type `Transfer` is not consumed"
        );
        // Both forgotten: two values, so two errors, one for each call.
        let both = body(&locals, 0, vec![block(100, build, Terminator::Return)]);
        let errors = errors_of(&both);
        assert_eq!(errors.len(), 2, "{errors:#?}");
        assert!(errors[0].1.contains("`p.a`") && !errors[0].1.contains("`p.b`"));
        assert!(errors[1].1.contains("`p.b`"));
    }

    #[test]
    fn one_call_giving_several_paths_is_one_error_naming_them_all() {
        let t = types();
        let body = body(
            &[(t.pair, Some("p"))],
            0,
            vec![block(100, vec![call(1)], Terminator::Return)],
        );
        let (_, message, labels) = only(errors_of(&body));
        assert_eq!(message, "linear values `p.a` and `p.b` are not consumed");
        assert_eq!(
            labels[1],
            "`p.a` and `p.b` are still held when the function returns here"
        );
    }

    #[test]
    fn new_faults_on_its_own_path_only() {
        let t = types();
        let locals = [(t.transfer, Some("a")), (t.holder, Some("h"))];
        let inner = projected(2, &[PlaceElem::Field(0)]);
        // The field is held when `new` runs: no fault there. It is held when
        // the struct is ended, which is the one error, at the `end`.
        let ended = body(
            &locals,
            1,
            vec![block(
                100,
                vec![moved(inner, local(1), 10), new(local(2)), end(local(2), 20)],
                Terminator::Return,
            )],
        );
        let (code, message, labels) = only(errors_of(&ended));
        assert_eq!(code, NOT_CONSUMED);
        assert_eq!(
            message,
            "linear value `h.inner` of type `Transfer` is not consumed before `end`"
        );
        assert_eq!(
            labels,
            [
                "`h.inner` is still held when `end` runs",
                "linear value comes in as the parameter `a`",
                "moved into `h.inner` here"
            ]
        );
        // A second `new` over the struct overwrites the first: the first
        // value is lost, the second held at the return.
        let twice = body(
            &locals[1..],
            0,
            vec![block(
                100,
                vec![new(local(1)), new(local(1))],
                Terminator::Return,
            )],
        );
        let errors = errors_of(&twice);
        assert_eq!(errors.len(), 2, "{errors:#?}");
        assert!(errors.iter().any(|e| e.0 == OVERWRITTEN));
        assert!(errors.iter().any(|e| e.0 == NOT_CONSUMED));
    }

    #[test]
    fn end_kills_the_struct_and_its_parts_and_a_field_moved_out_first_is_no_fault() {
        let t = types();
        let locals = [(t.holder, Some("h")), (t.transfer, Some("f"))];
        let inner = projected(1, &[PlaceElem::Field(0)]);
        let taken_apart = body(
            &locals,
            1,
            vec![block(
                100,
                vec![
                    moved(local(2), inner, 10),
                    consume(local(2), 20),
                    end(local(1), 30),
                ],
                Terminator::Return,
            )],
        );
        assert!(errors_of(&taken_apart).is_empty());
        // The field moved out and forgotten: reported where it was held.
        let forgotten = body(
            &locals,
            1,
            vec![block(
                100,
                vec![moved(local(2), inner, 10), end(local(1), 30)],
                Terminator::Return,
            )],
        );
        let (_, message, _) = only(errors_of(&forgotten));
        assert_eq!(
            message,
            "linear value `f` of type `Transfer` is not consumed"
        );
    }

    #[test]
    fn a_return_in_a_loop_is_a_return() {
        let t = types();
        // bb1 branches to a `return` and round the loop.
        let body = body(
            &[(t.transfer, Some("t"))],
            1,
            vec![
                block(100, vec![], goto(1)),
                block(200, vec![], branch(2, 3)),
                block(300, vec![], Terminator::Return),
                block(400, vec![], goto(1)),
            ],
        );
        let (code, _, labels) = only(errors_of(&body));
        assert_eq!(code, NOT_CONSUMED);
        assert_eq!(labels.len(), 2, "{labels:?}");
    }

    #[test]
    fn a_path_that_does_not_return_owes_nothing() {
        let t = types();
        let locals = [(t.transfer, Some("t"))];
        let unreachable = body(
            &locals,
            1,
            vec![block(100, vec![], Terminator::Unreachable)],
        );
        assert!(errors_of(&unreachable).is_empty());
        let panics = body(
            &locals,
            1,
            vec![block(
                100,
                vec![],
                Terminator::Panic(PanicCause::DivisionByZero, None),
            )],
        );
        assert!(errors_of(&panics).is_empty());
        // A loop with no exit.
        let spins = body(&locals, 1, vec![block(100, vec![], goto(0))]);
        assert!(errors_of(&spins).is_empty());
    }

    #[test]
    fn a_call_written_again_round_a_loop_is_not_consumed() {
        let t = types();
        // bb1: `_1 = call` (a discarded call), round the loop or out.
        let body = body(
            &[(t.transfer, None)],
            0,
            vec![
                block(100, vec![], goto(1)),
                block(200, vec![call(1)], branch(1, 2)),
                block(300, vec![], Terminator::Return),
            ],
        );
        let (code, message, labels) = only(errors_of(&body));
        assert_eq!(code, NOT_CONSUMED);
        assert_eq!(message, "linear value of type `Transfer` is not consumed");
        // The call is the origin and the write that comes round: one label.
        assert_eq!(
            labels,
            [
                "linear value comes from this call; this value is still held when a value of \
                 the same origin is written here",
                "this value is still held when the function returns here"
            ]
        );
    }

    #[test]
    fn a_value_moved_into_a_variable_written_again_round_a_loop_is_not_consumed() {
        let t = types();
        // `let t = call();` in a loop: `_1 = call; t = move _1`.
        let body = body(
            &[(t.transfer, None), (t.transfer, Some("t"))],
            0,
            vec![
                block(100, vec![], goto(1)),
                block(
                    200,
                    vec![call(1), moved(local(2), local(1), 50)],
                    branch(1, 2),
                ),
                block(300, vec![], Terminator::Return),
            ],
        );
        let (code, _, labels) = only(errors_of(&body));
        assert_eq!(code, NOT_CONSUMED);
        assert!(
            labels
                .iter()
                .any(|l| l.contains("when a value of the same origin is written here")),
            "{labels:?}"
        );
    }

    #[test]
    fn a_write_over_the_value_of_another_origin_is_an_overwrite() {
        let t = types();
        // `t = call; t = call;` then consumed.
        let body = body(
            &[
                (t.transfer, None),
                (t.transfer, None),
                (t.transfer, Some("t")),
            ],
            0,
            vec![block(
                100,
                vec![
                    call(1),
                    moved(local(3), local(1), 10),
                    call(2),
                    moved(local(3), local(2), 20),
                    consume(local(3), 30),
                ],
                Terminator::Return,
            )],
        );
        let (code, message, labels) = only(errors_of(&body));
        assert_eq!(code, OVERWRITTEN);
        assert_eq!(
            message,
            "linear value `t` of type `Transfer` is overwritten"
        );
        assert_eq!(labels[0], "overwrites the value held by `t`");
        assert!(labels.contains(&"linear value comes from this call".to_owned()));
    }

    #[test]
    fn a_value_reported_as_not_consumed_is_not_reported_as_overwritten() {
        let t = types();
        // The first value is overwritten on one branch and reaches the return
        // on the other: not consumed, and not also overwritten.
        let body = body(
            &[
                (t.transfer, None),
                (t.transfer, None),
                (t.transfer, Some("t")),
            ],
            0,
            vec![
                block(
                    100,
                    vec![call(1), moved(local(3), local(1), 10)],
                    branch(1, 2),
                ),
                block(200, vec![call(2), moved(local(3), local(2), 20)], goto(2)),
                block(300, vec![], Terminator::Return),
            ],
        );
        let errors = errors_of(&body);
        assert_eq!(errors.len(), 2, "{errors:#?}");
        assert!(errors.iter().all(|e| e.0 == NOT_CONSUMED), "{errors:#?}");
    }

    #[test]
    fn a_value_held_by_different_places_on_two_paths_is_one_error() {
        let t = types();
        let body = body(
            &[
                (t.transfer, Some("t")),
                (t.transfer, Some("a")),
                (t.transfer, Some("b")),
            ],
            1,
            vec![
                block(100, vec![], branch(1, 2)),
                block(200, vec![moved(local(2), local(1), 10)], Terminator::Return),
                block(300, vec![moved(local(3), local(1), 20)], Terminator::Return),
            ],
        );
        let (_, message, labels) = only(errors_of(&body));
        assert_eq!(message, "linear values `a` and `b` are not consumed");
        assert!(labels.contains(&"`a` is still held when the function returns here".to_owned()));
        assert!(labels.contains(&"`b` is still held when the function returns here".to_owned()));
    }

    #[test]
    fn a_copy_field_written_is_no_overwrite() {
        let t = types();
        let write = Statement::Assign(
            projected(1, &[PlaceElem::Field(1)]),
            Rvalue::Use(Operand::Const(Const::Int(5, IntTy::U32))),
        );
        let body = body(
            &[(t.holder, Some("h"))],
            1,
            vec![block(100, vec![write], Terminator::Return)],
        );
        // The write is fine; `h` and its field are still held at the return.
        let (code, message, _) = only(errors_of(&body));
        assert_eq!(code, NOT_CONSUMED);
        assert_eq!(message, "linear values `h` and `h.inner` are not consumed");
    }

    #[test]
    fn the_fixed_point_is_the_same_whatever_order_the_blocks_are_visited_in() {
        let t = types();
        // A loop with an exit and a join; the value is consumed in the loop on
        // one branch.
        let body = body(
            &[(t.transfer, Some("t"))],
            1,
            vec![
                block(100, vec![], goto(1)),
                block(200, vec![], branch(2, 3)),
                block(300, vec![consume(local(1), 10)], goto(1)),
                block(400, vec![], branch(4, 5)),
                block(500, vec![], goto(5)),
                block(600, vec![], Terminator::Return),
            ],
        );
        let flow = Flow::of(&body);
        let forward = flow.solve(&[0, 1, 2, 3, 4, 5]);
        for order in [
            [5, 4, 3, 2, 1, 0],
            [2, 4, 0, 5, 3, 1],
            [3, 1, 5, 0, 2, 4],
            [1, 1, 2, 2, 0, 0],
        ] {
            let mut order = order.to_vec();
            order.extend(0..6);
            assert_eq!(flow.solve(&order), forward, "order {order:?}");
        }
        assert!(forward[5].is_some());
    }

    #[test]
    fn a_body_without_a_linear_value_passes_with_no_diagnostics() {
        let body = body(
            &[(U32, Some("n"))],
            1,
            vec![block(100, vec![], Terminator::Return)],
        );
        let mut diags = Diagnostics::default();
        assert!(check(&body, &mut diags));
        assert!(diags.is_empty());
    }
}
