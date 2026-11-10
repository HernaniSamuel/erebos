//! The analysis of moves (ADR 0097, 0099): a use of a place after the value
//! in it, or a part of it, was moved, with no assignment in between.
//!
//! A **move path** is a local followed by `Field` and `Payload` projections,
//! the places a `move` operand can name (ADR 0098, M3), and each `move`
//! operand of the body is a **move site**. A forward analysis computes, at the
//! start of each reachable block, the set of sites that may reach it; the
//! errors are found in one pass from those sets. The transfer of a use or of
//! a write kills every site related to its place and a `move` generates its
//! own, so the sets, and the fixed point, do not depend on the order the
//! blocks are visited in.
//!
//! Only variables and parameters are followed (see `Flow::named`): the
//! temporaries of MIR building are written by parts and reused at each
//! iteration of a loop, and no source text names them.

use std::collections::{HashMap, HashSet};

use crate::diag::{Diagnostic, Diagnostics};
use crate::mir::{
    AsmOperandKind, Body, InlineAsm, Local, Operand, PanicCause, Place, PlaceElem, Rvalue,
    Statement, Terminator, Ty,
};
use crate::span::Span;

const USE_AFTER_MOVE: &str = "analysis::use-after-move";

/// Reports the uses of moved places in `body`; whether there are none.
pub fn check(body: &Body, diags: &mut Diagnostics) -> bool {
    if body.external || body.blocks.is_empty() {
        return true;
    }
    let flow = Flow::of(body);
    if flow.sites.is_empty() {
        return true;
    }
    let ins = flow.solve(&flow.blocks);
    let faults = flow.faults(&ins);
    let passed = faults.is_empty();
    for diag in flow.diagnostics(faults) {
        diags.push(diag);
    }
    passed
}

/// A block and a statement of it, the terminator last.
type Loc = (usize, usize);

/// A `move` operand: where it is, the path it moves and the type of that.
struct Site {
    local: Local,
    path: Vec<PlaceElem>,
    span: Span,
    at: Loc,
    ty: Ty,
}

enum Access {
    /// An operand, or the place of an `&raw`, `SliceAddress` or `SliceLen`;
    /// the site it generates if it is a `move`.
    Use(Option<usize>),
    /// The destination of an assignment, a call or an `asm` output.
    Write,
}

/// A use or a write of a place, in the order they happen.
struct Event {
    place: Place,
    access: Access,
    /// The `move` of a use that is one, else the statement or terminator.
    span: Span,
    at: Loc,
}

/// Where the statement or terminator being read is.
#[derive(Clone, Copy)]
struct At {
    block: usize,
    index: usize,
    span: Span,
}

/// The sets of sites that reach a point.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Bits(Vec<u64>);

impl Bits {
    fn new(len: usize) -> Bits {
        Bits(vec![0; len.div_ceil(64)])
    }

    fn contains(&self, i: usize) -> bool {
        self.0
            .get(i / 64)
            .is_some_and(|word| word >> (i % 64) & 1 == 1)
    }

    fn insert(&mut self, i: usize) {
        if let Some(word) = self.0.get_mut(i / 64) {
            *word |= 1 << (i % 64);
        }
    }

    fn remove(&mut self, i: usize) {
        if let Some(word) = self.0.get_mut(i / 64) {
            *word &= !(1 << (i % 64));
        }
    }

    /// Adds what `other` holds; whether that changed anything.
    fn union(&mut self, other: &Bits) -> bool {
        let mut changed = false;
        for (word, &more) in self.0.iter_mut().zip(&other.0) {
            let joined = *word | more;
            changed |= joined != *word;
            *word = joined;
        }
        changed
    }
}

/// A use or a write that a site reaches.
struct Fault {
    place: Place,
    write: bool,
    span: Span,
    at: Loc,
    /// The `move` that is the use, whose site is among `sites`: it reaches
    /// itself through a back edge.
    own: Option<usize>,
    sites: Vec<usize>,
}

struct Flow<'b> {
    body: &'b Body,
    sites: Vec<Site>,
    /// The sites that move a part of each local.
    by_local: HashMap<u32, Vec<usize>>,
    /// The blocks reachable from the first, in ascending order.
    blocks: Vec<usize>,
    successors: Vec<Vec<usize>>,
    /// By block; empty for a block nothing reaches.
    events: Vec<Vec<Event>>,
}

impl<'b> Flow<'b> {
    fn of(body: &'b Body) -> Flow<'b> {
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
        let blocks: Vec<usize> = (0..body.blocks.len()).filter(|&b| reached[b]).collect();
        let mut flow = Flow {
            body,
            sites: Vec::new(),
            by_local: HashMap::new(),
            blocks,
            successors,
            events: (0..body.blocks.len()).map(|_| Vec::new()).collect(),
        };
        for block in flow.blocks.clone() {
            flow.collect(block);
        }
        flow
    }

    /// The events of a block: for each statement and then the terminator, its
    /// uses left to right, then its writes (ADR 0099).
    fn collect(&mut self, block: usize) {
        let data = &self.body.blocks[block];
        for (index, statement) in data.statements.iter().enumerate() {
            let span = (data.spans.get(index).copied()).unwrap_or(data.terminator_span);
            let at = At { block, index, span };
            match statement {
                Statement::Assign(dest, rvalue) => {
                    match rvalue {
                        Rvalue::Use(operand)
                        | Rvalue::Unary(_, operand)
                        | Rvalue::Cast(operand, _) => self.operand(at, operand),
                        Rvalue::Binary(_, lhs, rhs)
                        | Rvalue::Slice {
                            ptr: lhs, len: rhs, ..
                        } => {
                            self.operand(at, lhs);
                            self.operand(at, rhs);
                        }
                        Rvalue::AddressOf { place, .. }
                        | Rvalue::SliceAddress(place)
                        | Rvalue::SliceLen(place) => self.event(at, *place, Access::Use(None)),
                    }
                    self.event(at, *dest, Access::Write);
                }
                Statement::Call { dest, args, .. } => {
                    for arg in args {
                        self.operand(at, arg);
                    }
                    if let Some(dest) = dest {
                        self.event(at, *dest, Access::Write);
                    }
                }
                Statement::InlineAsm(asm) => self.asm(at, asm),
                // Names a temporary the analysis of moves does not follow
                // (ADR 0103).
                Statement::CriticalEnter(_) | Statement::CriticalExit(_) | Statement::New(..) => {}
            }
        }
        let at = At {
            block,
            index: data.statements.len(),
            span: data.terminator_span,
        };
        match &data.terminator {
            Terminator::If { cond: operand, .. }
            | Terminator::Switch { discr: operand, .. }
            | Terminator::Panic(PanicCause::AssertionFailed(Some((operand, _))), _) => {
                self.operand(at, operand)
            }
            Terminator::InlineAsm(asm) => self.asm(at, asm),
            Terminator::Goto(_)
            | Terminator::Return
            | Terminator::Unreachable
            | Terminator::Panic(..) => {}
        }
    }

    /// The inputs of an `asm`, then its outputs.
    fn asm(&mut self, at: At, asm: &InlineAsm) {
        for operand in asm.inputs() {
            self.operand(at, operand);
        }
        for operand in &asm.operands {
            match &operand.kind {
                AsmOperandKind::Out {
                    place: Some(place), ..
                }
                | AsmOperandKind::InOut {
                    place: Some(place), ..
                } => self.event(at, *place, Access::Write),
                AsmOperandKind::In(_)
                | AsmOperandKind::Out { place: None, .. }
                | AsmOperandKind::InOut { place: None, .. }
                | AsmOperandKind::Sym(_)
                | AsmOperandKind::Const(..) => {}
            }
        }
    }

    fn operand(&mut self, at: At, operand: &Operand) {
        match operand {
            Operand::Copy(place) => self.event(at, *place, Access::Use(None)),
            Operand::Move(place, _) if !self.named(place.local) => {}
            Operand::Move(place, span) => {
                let site = self.sites.len();
                self.sites.push(Site {
                    local: place.local,
                    path: place.projection.to_vec(),
                    span: *span,
                    at: (at.block, at.index),
                    ty: self.body.place_ty(*place),
                });
                self.by_local.entry(place.local.0).or_default().push(site);
                let at = At { span: *span, ..at };
                self.event(at, *place, Access::Use(Some(site)));
            }
            Operand::Const(_) => {}
        }
    }

    /// Whether the local is a variable or a parameter. The analysis follows
    /// only those: a temporary is made by MIR building, which writes it by
    /// parts, as a literal or a variant, and reuses it at each iteration of a
    /// loop, so a flow that does not know which parts a loop wrote cannot tell
    /// a fault from the next iteration's construction. No source text names a
    /// temporary, so no error of the program is lost.
    fn named(&self, local: Local) -> bool {
        (self.body.locals.get(local.0 as usize)).is_some_and(|decl| decl.name.is_some())
    }

    fn event(&mut self, at: At, place: Place, access: Access) {
        if !self.named(place.local) {
            return;
        }
        self.events[at.block].push(Event {
            place,
            access,
            span: at.span,
            at: (at.block, at.index),
        });
    }

    /// The sites at the start of each block, by visiting `order` until none
    /// changes; `None` for a block nothing reached.
    fn solve(&self, order: &[usize]) -> Vec<Option<Bits>> {
        let mut ins: Vec<Option<Bits>> = vec![None; self.body.blocks.len()];
        ins[0] = Some(Bits::new(self.sites.len()));
        loop {
            let mut changed = false;
            for &block in order {
                let Some(mut state) = ins[block].clone() else {
                    continue;
                };
                for event in &self.events[block] {
                    self.apply(&mut state, event);
                }
                for &next in &self.successors[block] {
                    match &mut ins[next] {
                        Some(bits) => changed |= bits.union(&state),
                        none => {
                            *none = Some(state.clone());
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

    /// What an event does to the sites that reach it: it kills every site
    /// related to its place, and a `move` generates its own.
    fn apply(&self, state: &mut Bits, event: &Event) {
        for &site in self.of_local(event.place) {
            if related(&self.sites[site].path, event.place.projection) {
                state.remove(site);
            }
        }
        if let Access::Use(Some(site)) = event.access {
            state.insert(site);
        }
    }

    /// The sites that move a part of the local of `place`.
    fn of_local(&self, place: Place) -> &[usize] {
        self.by_local.get(&place.local.0).map_or(&[], Vec::as_slice)
    }

    /// The sites that reach `event` and make it a fault: for a use, those
    /// related to its place; for a write, those whose path is a proper prefix
    /// of the destination (a part of a place moved whole).
    fn reaching(&self, state: &Bits, event: &Event) -> Vec<usize> {
        let projection = event.place.projection;
        let fault = |site: &Site| match event.access {
            Access::Use(_) => related(&site.path, projection),
            Access::Write => site.path.len() < projection.len() && related(&site.path, projection),
        };
        (self.of_local(event.place).iter().copied())
            .filter(|&site| state.contains(site) && fault(&self.sites[site]))
            .collect()
    }

    /// Every fault, found in one pass over the reachable blocks from their
    /// sets at the fixed point. A use that is a fault kills what it
    /// reached, so a fault is found once on the paths through it.
    fn faults(&self, ins: &[Option<Bits>]) -> Vec<Fault> {
        let mut faults = Vec::new();
        for &block in &self.blocks {
            let Some(mut state) = ins[block].clone() else {
                continue;
            };
            for event in &self.events[block] {
                let sites = self.reaching(&state, event);
                if !sites.is_empty() {
                    faults.push(Fault {
                        place: event.place,
                        write: matches!(event.access, Access::Write),
                        span: event.span,
                        at: event.at,
                        own: match event.access {
                            Access::Use(own) => own,
                            Access::Write => None,
                        },
                        sites,
                    });
                }
                self.apply(&mut state, event);
            }
        }
        faults
    }

    /// The errors, in the order of their uses' spans, then of their
    /// locations, one for two faults that have the same use and the same
    /// labels.
    fn diagnostics(&self, mut faults: Vec<Fault>) -> Vec<Diagnostic> {
        faults.sort_by_key(|fault| (key(fault.span), fault.at));
        let mut seen = HashSet::new();
        let mut diags = Vec::new();
        for fault in faults {
            let mut sites: Vec<&Site> = fault.sites.iter().map(|&site| &self.sites[site]).collect();
            sites.sort_by_key(|site| (key(site.span), site.at));
            let mut labels: Vec<(Span, bool)> = fault
                .sites
                .iter()
                .map(|&index| {
                    let site = &self.sites[index];
                    let later = fault.own == Some(index)
                        || (site.span.file == fault.span.file && site.span.lo > fault.span.lo);
                    (site.span, later)
                })
                .collect();
            labels.sort_by_key(|&(span, later)| (key(span), later));
            labels.dedup();
            if !seen.insert((fault.span, fault.write, labels.clone())) {
                continue;
            }
            diags.push(self.diagnostic(&fault, &sites, &labels));
        }
        diags
    }

    fn diagnostic(&self, fault: &Fault, sites: &[&Site], labels: &[(Span, bool)]) -> Diagnostic {
        let place = fault.place;
        let movable = (place.projection.iter())
            .take_while(|elem| matches!(elem, PlaceElem::Field(_) | PlaceElem::Payload { .. }))
            .count();
        // The place moved, which the used one is or is a part of; else, when
        // only parts of the used place were moved, the used place.
        let partly = !fault.write && sites.iter().all(|site| site.path.len() > movable);
        let moved = match sites.first() {
            Some(site) if !partly => self.describe(site.local, &site.path),
            _ => self.describe(place.local, &place.projection[..movable]),
        };
        let (message, primary) = if fault.write {
            (
                format!("assignment to a part of the moved value `{moved}`"),
                "assigned here",
            )
        } else if partly {
            (
                format!("use of partly moved value `{moved}`"),
                "used here, after a part of it was moved",
            )
        } else {
            (
                format!("use of moved value `{moved}`"),
                "used here, after it was moved",
            )
        };
        let mut diag = Diagnostic::error(USE_AFTER_MOVE, message).label(fault.span, primary);
        for &(span, later) in labels {
            diag = diag.label(
                span,
                if later {
                    "moved here, in an earlier iteration of the loop"
                } else {
                    "moved here"
                },
            );
        }
        let help = sites
            .iter()
            .find_map(|site| suggest_copy(site.ty))
            .unwrap_or_else(|| {
                "a place that was moved is valid again once it is assigned a new value".to_owned()
            });
        diag.help(format!("{help} (ADR 0097)"))
    }

    /// The place as the source names it, as far as its fields go: the name of
    /// the local, followed by its fields.
    fn describe(&self, local: Local, path: &[PlaceElem]) -> String {
        let Some(decl) = self.body.locals.get(local.0 as usize) else {
            return local.to_string();
        };
        let mut text = decl.name.clone().unwrap_or_else(|| local.to_string());
        let mut ty = decl.ty;
        for elem in path {
            let field = match (elem, ty) {
                (PlaceElem::Field(index), Ty::Struct(s)) => s.def().fields().get(*index as usize),
                (PlaceElem::Payload { variant, index }, Ty::Enum(e)) => e
                    .def()
                    .variants()
                    .get(*variant as usize)
                    .and_then(|variant| {
                        text.push('.');
                        text.push_str(&variant.name);
                        variant.payload.get(*index as usize)
                    }),
                _ => None,
            };
            let Some(field) = field else {
                break;
            };
            text.push('.');
            text.push_str(&field.name);
            ty = field.ty;
        }
        text
    }
}

/// Whether one of the two paths is a prefix of the other: the place of the
/// site was moved, or contains the moved place, or is a part of it. The tag of
/// an enum is no part of its payload values, so the two differ at `Tag`.
fn related(site: &[PlaceElem], place: &[PlaceElem]) -> bool {
    site.iter().zip(place).all(|(a, b)| a == b)
}

/// Spans are ordered by their start, then their end (ADR 0099).
fn key(span: Span) -> (crate::span::FileId, u32, u32) {
    (span.file, span.lo, span.hi)
}

/// The help of ADR 0097: `#[copy]` on the type of the moved place when it is
/// a struct or an enum with payloads whose fields and payload values are all
/// copy, so that the attribute would be accepted. A linear type is never
/// copy: `#[copy]` on a `#[linear]` struct is an error (ADR 0101, 0103).
fn suggest_copy(ty: Ty) -> Option<String> {
    if ty.is_linear() {
        return None;
    }
    let (name, all_copy) = match ty {
        Ty::Struct(s) => (
            &s.def().name,
            s.def().fields().iter().all(|field| field.ty.is_copy()),
        ),
        Ty::Enum(e) => (
            &e.def().name,
            (e.def().variants().iter())
                .all(|variant| variant.payload.iter().all(|value| value.ty.is_copy())),
        ),
        _ => return None,
    };
    all_copy.then(|| format!("mark `{name}` with `#[copy]` if its values may be duplicated"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::{
        BasicBlock, BasicBlockData, Const, EnumId, EnumRef, Field, IntTy, Layout, LocalDecl,
        StructRef, Variant,
    };
    use crate::span::SourceMap;

    fn span(lo: usize) -> Span {
        let file = SourceMap::default().add("main.ers", "");
        Span::new(file, lo, lo + 1)
    }

    /// `struct Handle { n: u32 }`, not copy.
    fn handle() -> Ty {
        let s = StructRef::new(
            "Handle".to_owned(),
            Layout { size: 4, align: 4 },
            false,
            false,
            false,
        );
        s.def().set_fields(vec![Field {
            name: "n".to_owned(),
            ty: Ty::Int(IntTy::U32),
            offset: 0,
        }]);
        Ty::Struct(s)
    }

    /// `enum Slot { Free, Full(Handle) }`, not copy.
    fn slot() -> Ty {
        let e = EnumRef::new(
            EnumId(1),
            "Slot".to_owned(),
            IntTy::U8,
            Layout { size: 8, align: 4 },
            false,
        );
        e.def().set_variants(vec![
            Variant {
                name: "Free".to_owned(),
                payload: Vec::new(),
            },
            Variant {
                name: "Full".to_owned(),
                payload: vec![Field {
                    name: "0".to_owned(),
                    ty: handle(),
                    offset: 4,
                }],
            },
        ]);
        Ty::Enum(e)
    }

    fn decl(ty: Ty, name: Option<&str>) -> LocalDecl {
        LocalDecl {
            ty,
            name: name.map(str::to_owned),
            constant: None,
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

    const FULL: PlaceElem = PlaceElem::Payload {
        variant: 1,
        index: 0,
    };

    /// A move of `place`, written at byte `at`.
    fn mv(place: Place, at: usize) -> Operand {
        Operand::Move(place, span(at))
    }

    /// `_dest = <operand>`.
    fn assign(dest: u32, operand: Operand) -> Statement {
        Statement::Assign(local(dest), Rvalue::Use(operand))
    }

    fn one() -> Operand {
        Operand::Const(Const::Int(1, IntTy::U32))
    }

    fn block(statements: Vec<Statement>, terminator: Terminator) -> BasicBlockData {
        BasicBlockData {
            spans: (0..statements.len()).map(|i| span(100 + i)).collect(),
            statements,
            terminator,
            terminator_span: span(200),
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

    /// A body whose `_1` and `_4` are `Handle`s named `a` and `b`, and `_2`
    /// and `_3` are temporaries of that type, with these blocks.
    fn body(blocks: Vec<BasicBlockData>) -> Body {
        body_of(handle(), blocks)
    }

    /// The same, with locals of type `ty`.
    fn body_of(ty: Ty, blocks: Vec<BasicBlockData>) -> Body {
        Body {
            name: "f".to_owned(),
            span: span(0),
            comptime: false,
            ret: Ty::Unit,
            param_count: 1,
            locals: vec![
                decl(Ty::Unit, None),
                decl(ty, Some("a")),
                decl(ty, None),
                decl(ty, None),
                decl(ty, Some("b")),
            ],
            blocks,
            runtime: false,
            external: false,
            naked: false,
            align: None,
        }
    }

    /// The errors of `body`, each as its message and its labels' texts.
    fn errors_of(body: &Body) -> Vec<(String, Vec<String>)> {
        let mut diags = Diagnostics::default();
        let passed = check(body, &mut diags);
        let list = diags.into_vec();
        assert_eq!(passed, list.is_empty());
        list.into_iter()
            .map(|diag| {
                assert_eq!(diag.code, USE_AFTER_MOVE);
                let labels = diag.labels.iter().map(|l| l.message.clone()).collect();
                (diag.message, labels)
            })
            .collect()
    }

    #[test]
    fn a_use_after_a_move_is_an_error_with_the_move_as_a_label() {
        let body = body(vec![block(
            vec![assign(2, mv(local(1), 10)), assign(3, mv(local(1), 20))],
            Terminator::Return,
        )]);
        let errors = errors_of(&body);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].0, "use of moved value `a`");
        assert_eq!(errors[0].1, ["used here, after it was moved", "moved here"]);
    }

    #[test]
    fn a_move_in_a_block_nothing_reaches_reaches_nothing() {
        // bb1 moves `a` and jumps into bb2, which bb0 reaches too.
        let body = body(vec![
            block(vec![], goto(2)),
            block(vec![assign(2, mv(local(1), 10))], goto(2)),
            block(vec![assign(3, mv(local(1), 20))], Terminator::Return),
        ]);
        assert!(errors_of(&body).is_empty());
    }

    #[test]
    fn a_move_in_a_loop_reaches_itself_through_the_back_edge() {
        // bb0 -> bb1 -> bb2 -> bb1, with the move in bb2.
        let body = body(vec![
            block(vec![], goto(1)),
            block(vec![], branch(2, 3)),
            block(vec![assign(2, mv(local(1), 10))], goto(1)),
            block(vec![], Terminator::Return),
        ]);
        let errors = errors_of(&body);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(
            errors[0].1,
            [
                "used here, after it was moved",
                "moved here, in an earlier iteration of the loop"
            ]
        );
    }

    #[test]
    fn an_assignment_in_the_loop_makes_the_place_valid_again() {
        let body = body(vec![
            block(vec![], goto(1)),
            block(vec![], branch(2, 3)),
            block(
                vec![assign(2, mv(local(1), 10)), assign(1, mv(local(3), 20))],
                goto(1),
            ),
            block(vec![], Terminator::Return),
        ]);
        assert!(errors_of(&body).is_empty());
    }

    #[test]
    fn the_fixed_point_is_the_same_whatever_order_the_blocks_are_visited_in() {
        // A loop with an exit and a join: the move in bb2 reaches bb1 only
        // through the back edge, and the blocks after bb1 through it.
        let body = body(vec![
            block(vec![], goto(1)),
            block(vec![], branch(2, 3)),
            block(vec![assign(2, mv(local(1), 10))], goto(1)),
            block(vec![], branch(4, 5)),
            block(vec![assign(3, mv(local(4), 20))], goto(5)),
            block(vec![], Terminator::Return),
        ]);
        let flow = Flow::of(&body);
        let forward = flow.solve(&[0, 1, 2, 3, 4, 5]);
        for order in [
            [5, 4, 3, 2, 1, 0],
            [2, 4, 0, 5, 3, 1],
            [3, 1, 5, 0, 2, 4],
            [1, 1, 2, 2, 0, 0],
        ] {
            // Whatever the order, every block is visited at least once.
            let mut order = order.to_vec();
            order.extend(0..6);
            assert_eq!(flow.solve(&order), forward, "order {order:?}");
        }
        for block in [1, 2, 3, 4, 5] {
            assert!(forward[block].as_ref().unwrap().contains(0), "bb{block}");
        }
        assert!(!forward[0].as_ref().unwrap().contains(0));
    }

    #[test]
    fn the_tag_is_no_part_of_a_payload_value() {
        // The payload of `a` is moved, then its tag is read: valid.
        let read_tag = Statement::Assign(
            local(0),
            Rvalue::Cast(
                Operand::Copy(projected(1, &[PlaceElem::Tag])),
                Ty::Int(IntTy::U32),
            ),
        );
        let moved = assign(2, mv(projected(1, &[FULL]), 10));
        let tag_only = body_of(
            slot(),
            vec![block(
                vec![moved.clone(), read_tag.clone()],
                Terminator::Return,
            )],
        );
        assert!(errors_of(&tag_only).is_empty());
        // Used whole after it: partly moved.
        let whole = body_of(
            slot(),
            vec![block(
                vec![moved, read_tag, assign(3, mv(local(1), 20))],
                Terminator::Return,
            )],
        );
        let errors = errors_of(&whole);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].0, "use of partly moved value `a`");
    }

    #[test]
    fn assigning_a_part_of_a_place_moved_whole_is_a_fault_and_the_moved_part_is_not() {
        let field = projected(1, &[PlaceElem::Field(0)]);
        let write = Statement::Assign(field, Rvalue::Use(one()));
        let after_whole = body(vec![block(
            vec![assign(2, mv(local(1), 10)), write],
            Terminator::Return,
        )]);
        let errors = errors_of(&after_whole);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].0, "assignment to a part of the moved value `a`");
        // The payload value moved, then assigned whole: the enum is whole again.
        let payload = projected(1, &[FULL]);
        let after_part = body_of(
            slot(),
            vec![block(
                vec![
                    assign(2, mv(payload, 10)),
                    Statement::Assign(payload, Rvalue::Use(mv(local(4), 20))),
                    assign(3, mv(local(1), 30)),
                ],
                Terminator::Return,
            )],
        );
        assert!(errors_of(&after_part).is_empty());
    }

    #[test]
    fn a_temporary_is_never_followed() {
        // `_2` is moved and used again, but no source text names it.
        let body = body(vec![block(
            vec![assign(3, mv(local(2), 10)), assign(3, mv(local(2), 20))],
            Terminator::Return,
        )]);
        assert!(errors_of(&body).is_empty());
    }

    #[test]
    fn a_body_without_a_move_passes_with_no_diagnostics() {
        let body = body(vec![block(
            vec![assign(2, Operand::Copy(local(1)))],
            Terminator::Return,
        )]);
        let mut diags = Diagnostics::default();
        assert!(check(&body, &mut diags));
        assert!(diags.is_empty());
    }

    /// A struct with a `u32` field, copy or linear as asked.
    fn kinded(linear: bool) -> Ty {
        let s = StructRef::new(
            "Ticket".to_owned(),
            Layout { size: 4, align: 4 },
            false,
            false,
            linear,
        );
        s.def().set_fields(vec![Field {
            name: "n".to_owned(),
            ty: Ty::Int(IntTy::U32),
            offset: 0,
        }]);
        Ty::Struct(s)
    }

    #[test]
    fn copy_is_not_suggested_for_a_linear_type() {
        // The same fields: `#[copy]` would be accepted on the one that is not
        // linear, and on a linear one it is `resolve::conflicting-attributes`.
        assert!(suggest_copy(kinded(false)).is_some_and(|help| help.contains("`#[copy]`")));
        assert_eq!(suggest_copy(kinded(true)), None);
    }
}
