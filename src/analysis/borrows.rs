//! The borrow check (ADR 0106, 0107): an access to a place while a borrow of
//! an overlapping place is in use.
//!
//! A **site** is a `Ref` statement of a reachable block whose place has a
//! **path**: a root local followed by `Field`, `Elem` (an index, whatever it
//! is) and `Deref` of a reference. A place that goes through a raw pointer has
//! no path, and neither a borrow of it nor an access to it is followed.
//!
//! Two forward and backward analyses over the blocks `bb0` reaches, in bit
//! sets, give what the check needs: which sites each local of a reference type
//! **may hold** (forward), and which of those locals are **live** (backward).
//! The sites **in use** at an access are what the live locals hold, with what
//! the later operands of the same statement read, and, for a call, what its
//! arguments hold. The errors are found in one pass from the fixed points. As
//! in the analysis of moves, the transfers kill and generate, so the fixed
//! points do not depend on the order the blocks are visited in.

use std::collections::HashSet;

use super::bits::{Bits, key, reachable};
use crate::diag::{Diagnostic, Diagnostics};
use crate::mir::{
    AsmOperandKind, Body, InlineAsm, Local, Operand, PanicCause, Place, PlaceElem, Rvalue,
    Statement, Terminator, Ty,
};
use crate::span::Span;

const BORROW_CONFLICT: &str = "analysis::borrow-conflict";

/// Reports the accesses of `body` that conflict with a borrow in use; whether
/// there are none.
pub fn check(body: &Body, diags: &mut Diagnostics) -> bool {
    if body.external || body.blocks.is_empty() {
        return true;
    }
    let flow = Flow::of(body);
    if flow.sites.is_empty() {
        return true;
    }
    let held = flow.solve_held(&flow.blocks);
    let backwards: Vec<usize> = flow.blocks.iter().rev().copied().collect();
    let live = flow.solve_live(&backwards);
    let faults = flow.faults(&held, &live);
    let passed = faults.is_empty();
    for diag in flow.diagnostics(faults, &held) {
        diags.push(diag);
    }
    passed
}

/// A block, a statement of it (the terminator last) and an access of that.
type Loc = (usize, usize, usize);

/// What each local of a reference type may hold, by its number among them: a
/// set of sites.
type State = Vec<Bits>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Field(u32),
    /// An element of an array, whatever its index (ADR 0106).
    Elem,
    /// The referent of a reference.
    Deref,
    /// The tag or a payload value of an enum: related to nothing but itself.
    Other,
}

/// A root local and the steps from it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Path {
    root: Local,
    steps: Vec<Step>,
}

/// Whether one path contains the other, or both are the same: compared step by
/// step from the root, they meet two different fields nowhere before one of
/// them ends (ADR 0106).
fn overlap(a: &Path, b: &Path) -> bool {
    a.root == b.root
        && !(a.steps.iter().zip(&b.steps))
            .any(|pair| matches!(pair, (Step::Field(i), Step::Field(j)) if i != j))
}

/// What an access does to its place, as the table of ADR 0106 and the
/// messages tell them apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum How {
    /// A copy, a test of a tag, the place of `SliceAddress` or `SliceLen`.
    Use,
    RawConst,
    /// A `&` borrow.
    Borrow,
    /// The `&*q` of a `&mut` converted to a `&` (ADR 0105).
    Convert,
    /// A write: an assignment, a call's result, an `asm` output.
    Assign,
    RawMut,
    Move,
    End,
    /// A `&mut` borrow.
    BorrowMut,
    /// The `&mut *r` that lends a `&mut` argument to a call (ADR 0106).
    Lend,
}

impl How {
    /// Whether it conflicts with a shared borrow in use too: every access but
    /// those that read (ADR 0106).
    fn writes(self) -> bool {
        !matches!(self, How::Use | How::RawConst | How::Borrow | How::Convert)
    }

    /// Whether it makes a mutable borrow.
    fn mutable(self) -> bool {
        matches!(self, How::BorrowMut | How::Lend)
    }
}

/// A `Ref` statement and the path it borrows.
struct Site {
    how: How,
    place: Place,
    path: Path,
    span: Span,
}

/// An access of one statement, in the order the program evaluates them.
struct Access {
    how: How,
    place: Place,
    /// None for a place through a raw pointer, which is not followed.
    path: Option<Path>,
    /// The number, among the locals of a reference type, of the one this
    /// access reads: the base of its place, unless it writes it whole.
    reads: Option<usize>,
    span: Span,
    /// The site this access makes, which it is never in conflict with.
    site: Option<usize>,
    /// The write of a call's result, which happens after the callee returns.
    after_call: bool,
}

/// What a statement makes a whole local of a reference type hold.
enum Holds {
    Nothing,
    /// What another such local holds.
    Copy(usize),
    /// A site, and what the local it borrows through holds (ADR 0106: a
    /// reborrow keeps in use what its base holds).
    Site {
        site: usize,
        through: Option<usize>,
    },
}

struct Def {
    local: usize,
    holds: Holds,
}

/// A statement or a terminator.
struct Stmt {
    span: Span,
    accesses: Vec<Access>,
    def: Option<Def>,
    call: bool,
    /// The locals of a reference type that a call receives.
    args: Vec<usize>,
}

/// An access in conflict, and the sites in use it conflicts with.
struct Fault {
    at: Loc,
    after_call: bool,
    how: How,
    place: Place,
    span: Span,
    sites: Vec<usize>,
}

struct Flow<'b> {
    body: &'b Body,
    /// The number of locals of a reference type.
    refs: usize,
    /// For each local, its number among those of a reference type.
    ref_index: Vec<Option<usize>>,
    /// The locals moved into a call as an argument, as the temporary of a lent
    /// borrow is.
    passed: HashSet<u32>,
    sites: Vec<Site>,
    /// The blocks reachable from the first, in ascending order.
    blocks: Vec<usize>,
    successors: Vec<Vec<usize>>,
    /// By block; empty for a block nothing reaches.
    code: Vec<Vec<Stmt>>,
}

impl<'b> Flow<'b> {
    fn of(body: &'b Body) -> Flow<'b> {
        let (blocks, successors) = reachable(body);
        let mut refs = 0;
        let ref_index = (body.locals.iter())
            .map(|decl| {
                matches!(decl.ty, Ty::Ref(_)).then(|| {
                    refs += 1;
                    refs - 1
                })
            })
            .collect();
        let mut passed = HashSet::new();
        for &block in &blocks {
            for statement in &body.blocks[block].statements {
                if let Statement::Call { args, .. } = statement {
                    for arg in args {
                        if let Operand::Move(place, _) = arg
                            && place.projection.is_empty()
                        {
                            passed.insert(place.local.0);
                        }
                    }
                }
            }
        }
        let mut flow = Flow {
            body,
            refs,
            ref_index,
            passed,
            sites: Vec::new(),
            blocks,
            successors,
            code: (0..body.blocks.len()).map(|_| Vec::new()).collect(),
        };
        for block in flow.blocks.clone() {
            flow.collect(block);
        }
        flow
    }

    /// The statements of a block, then its terminator, each with its accesses:
    /// the operands left to right, then the place of the rvalue, then the
    /// writes (ADR 0099, 0107).
    fn collect(&mut self, block: usize) {
        let body = self.body;
        let data = &body.blocks[block];
        let mut code = Vec::new();
        for (index, statement) in data.statements.iter().enumerate() {
            let span = (data.spans.get(index).copied()).unwrap_or(data.terminator_span);
            let mut stmt = Stmt::new(span);
            match statement {
                Statement::Assign(dest, rvalue) => {
                    let mut site = None;
                    match rvalue {
                        Rvalue::Use(operand)
                        | Rvalue::Unary(_, operand)
                        | Rvalue::Cast(operand, _) => self.operand(&mut stmt, operand),
                        Rvalue::Binary(_, lhs, rhs)
                        | Rvalue::Slice {
                            ptr: lhs, len: rhs, ..
                        } => {
                            self.operand(&mut stmt, lhs);
                            self.operand(&mut stmt, rhs);
                        }
                        Rvalue::AddressOf { mutable, place } => {
                            let how = if *mutable { How::RawMut } else { How::RawConst };
                            self.access(&mut stmt, span, how, *place, None, false);
                        }
                        Rvalue::Ref { mutable, place } => {
                            let how = self.borrow_how(*mutable, *dest, *place);
                            site = self.borrow(&mut stmt, span, how, *place);
                        }
                        Rvalue::SliceAddress(place) | Rvalue::SliceLen(place) => {
                            self.access(&mut stmt, span, How::Use, *place, None, false)
                        }
                    }
                    self.access(&mut stmt, span, How::Assign, *dest, None, false);
                    stmt.def = self.def_of(*dest, Some(rvalue), site);
                }
                Statement::Call { dest, args, .. } => {
                    stmt.call = true;
                    for arg in args {
                        self.operand(&mut stmt, arg);
                        if let Some(place) = arg.place()
                            && place.projection.is_empty()
                            && let Some(local) = self.ref_index[place.local.0 as usize]
                        {
                            stmt.args.push(local);
                        }
                    }
                    if let Some(dest) = dest {
                        self.access(&mut stmt, span, How::Assign, *dest, None, true);
                        stmt.def = self.def_of(*dest, None, None);
                    }
                }
                Statement::InlineAsm(asm) => self.asm(&mut stmt, asm),
                Statement::End(place, end) => {
                    self.access(&mut stmt, *end, How::End, *place, None, false)
                }
                Statement::CriticalEnter(_) | Statement::CriticalExit(_) | Statement::New(..) => {}
            }
            code.push(stmt);
        }
        let mut stmt = Stmt::new(data.terminator_span);
        match &data.terminator {
            Terminator::If { cond: operand, .. }
            | Terminator::Switch { discr: operand, .. }
            | Terminator::Panic(PanicCause::AssertionFailed(Some((operand, _))), _) => {
                self.operand(&mut stmt, operand)
            }
            Terminator::InlineAsm(asm) => self.asm(&mut stmt, asm),
            Terminator::Goto(_)
            | Terminator::Return
            | Terminator::Unreachable
            | Terminator::Panic(..) => {}
        }
        code.push(stmt);
        self.code[block] = code;
    }

    /// The inputs of an `asm`, then its outputs.
    fn asm(&mut self, stmt: &mut Stmt, asm: &InlineAsm) {
        let span = stmt.span;
        for operand in asm.inputs() {
            self.operand(stmt, operand);
        }
        for operand in &asm.operands {
            match &operand.kind {
                AsmOperandKind::Out {
                    place: Some(place), ..
                }
                | AsmOperandKind::InOut {
                    place: Some(place), ..
                } => self.access(stmt, span, How::Assign, *place, None, false),
                AsmOperandKind::In(_)
                | AsmOperandKind::Out { place: None, .. }
                | AsmOperandKind::InOut { place: None, .. }
                | AsmOperandKind::Sym(_)
                | AsmOperandKind::Const(..) => {}
            }
        }
    }

    fn operand(&self, stmt: &mut Stmt, operand: &Operand) {
        let span = stmt.span;
        match operand {
            Operand::Copy(place) => self.access(stmt, span, How::Use, *place, None, false),
            Operand::Move(place, span) => self.access(stmt, *span, How::Move, *place, None, false),
            Operand::Const(_) => {}
        }
    }

    /// A `Ref` of `place`: an access, and a site if the place has a path.
    fn borrow(&mut self, stmt: &mut Stmt, span: Span, how: How, place: Place) -> Option<usize> {
        let site = self.path(place).map(|path| {
            self.sites.push(Site {
                how,
                place,
                path,
                span,
            });
            self.sites.len() - 1
        });
        self.access(stmt, span, how, place, site, false);
        site
    }

    /// Adds an access to the statement, unless it is of a place that is not
    /// followed and reads no local of a reference type.
    fn access(
        &self,
        stmt: &mut Stmt,
        span: Span,
        how: How,
        place: Place,
        site: Option<usize>,
        after_call: bool,
    ) {
        let path = self.path(place);
        let whole_write = how == How::Assign && place.projection.is_empty();
        let reads = if whole_write {
            None
        } else {
            self.ref_index[place.local.0 as usize]
        };
        if path.is_none() && reads.is_none() {
            return;
        }
        stmt.accesses.push(Access {
            how,
            place,
            path,
            reads,
            span,
            site,
            after_call,
        });
    }

    /// The path of a place, if it has one: it has none after the `Deref` of a
    /// raw pointer or an `Offset`.
    fn path(&self, place: Place) -> Option<Path> {
        let mut steps = Vec::new();
        for (k, elem) in place.projection.iter().enumerate() {
            steps.push(match elem {
                PlaceElem::Deref => {
                    let base = Place {
                        local: place.local,
                        projection: &place.projection[..k],
                    };
                    if !matches!(self.body.place_ty(base), Ty::Ref(_)) {
                        return None;
                    }
                    Step::Deref
                }
                PlaceElem::Offset(_) => return None,
                PlaceElem::Field(index) => Step::Field(*index),
                PlaceElem::Index(_)
                | PlaceElem::ConstIndex(_)
                | PlaceElem::Element(..)
                | PlaceElem::ConstElement(..) => Step::Elem,
                PlaceElem::Tag | PlaceElem::Payload { .. } => Step::Other,
            });
        }
        Some(Path {
            root: place.local,
            steps,
        })
    }

    fn named(&self, local: Local) -> bool {
        (self.body.locals.get(local.0 as usize)).is_some_and(|decl| decl.name.is_some())
    }

    /// How a `Ref` of `place` into `dest` is told in messages: `_t = &mut *r`
    /// with a variable or parameter `r`, moved into a call by its temporary,
    /// is the lending of ADR 0106, and `_t = &*q` with `q` a `&mut` is the
    /// conversion of ADR 0105. An explicit `&mut *r` passed to a call, or
    /// `&*q` written out, has the same MIR and is told the same.
    fn borrow_how(&self, mutable: bool, dest: Place, place: Place) -> How {
        let through = place.projection == [PlaceElem::Deref];
        let temporary = dest.projection.is_empty() && !self.named(dest.local);
        if through && temporary {
            match self.body.local_ty(place.local) {
                Ty::Ref(_)
                    if mutable
                        && self.named(place.local)
                        && self.passed.contains(&dest.local.0) =>
                {
                    return How::Lend;
                }
                Ty::Ref(r) if !mutable && r.mutable => return How::Convert,
                _ => {}
            }
        }
        if mutable { How::BorrowMut } else { How::Borrow }
    }

    /// What `dest` holds after the assignment of `rvalue` (a call's result
    /// when none: no call returns a reference), if it is a whole local of a
    /// reference type.
    fn def_of(&self, dest: Place, rvalue: Option<&Rvalue>, site: Option<usize>) -> Option<Def> {
        if !dest.projection.is_empty() {
            return None;
        }
        let local = self.ref_index[dest.local.0 as usize]?;
        let holds = match rvalue {
            Some(Rvalue::Ref { place, .. }) => match site {
                Some(site) => Holds::Site {
                    site,
                    through: match place.projection.first() {
                        Some(PlaceElem::Deref) => self.ref_index[place.local.0 as usize],
                        _ => None,
                    },
                },
                None => Holds::Nothing,
            },
            Some(Rvalue::Use(operand)) => match operand.place() {
                Some(place) if place.projection.is_empty() => {
                    self.ref_index[place.local.0 as usize].map_or(Holds::Nothing, Holds::Copy)
                }
                _ => Holds::Nothing,
            },
            Some(_) | None => Holds::Nothing,
        };
        Some(Def { local, holds })
    }

    /// What a statement does to what the locals hold.
    fn apply(&self, state: &mut State, stmt: &Stmt) {
        let Some(def) = &stmt.def else {
            return;
        };
        let mut held = Bits::new(self.sites.len());
        match def.holds {
            Holds::Nothing => {}
            Holds::Copy(from) => held = state[from].clone(),
            Holds::Site { site, through } => {
                if let Some(from) = through {
                    held = state[from].clone();
                }
                held.insert(site);
            }
        }
        state[def.local] = held;
    }

    /// What the locals hold at the start of each block, by visiting `order`
    /// until none changes; `None` for a block nothing reached.
    fn solve_held(&self, order: &[usize]) -> Vec<Option<State>> {
        let empty = || vec![Bits::new(self.sites.len()); self.refs];
        let mut ins: Vec<Option<State>> = vec![None; self.body.blocks.len()];
        ins[0] = Some(empty());
        loop {
            let mut changed = false;
            for &block in order {
                let Some(mut state) = ins[block].clone() else {
                    continue;
                };
                for stmt in &self.code[block] {
                    self.apply(&mut state, stmt);
                }
                for &next in &self.successors[block] {
                    match &mut ins[next] {
                        Some(held) => {
                            for (into, from) in held.iter_mut().zip(&state) {
                                changed |= into.union(from);
                            }
                        }
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

    /// The locals live after a block: those live at the start of a successor.
    fn live_out(&self, block: usize, live: &[Bits]) -> Bits {
        let mut out = Bits::new(self.refs);
        for &next in &self.successors[block] {
            out.union(&live[next]);
        }
        out
    }

    /// Before a statement: the locals live after it, but those it writes
    /// whole, and those it reads.
    fn live_before(stmt: &Stmt, live: &mut Bits) {
        if let Some(def) = &stmt.def {
            live.remove(def.local);
        }
        for access in &stmt.accesses {
            if let Some(local) = access.reads {
                live.insert(local);
            }
        }
    }

    /// The locals of a reference type live at the start of each block, by
    /// visiting `order` until none changes.
    fn solve_live(&self, order: &[usize]) -> Vec<Bits> {
        let mut ins = vec![Bits::new(self.refs); self.body.blocks.len()];
        loop {
            let mut changed = false;
            for &block in order {
                let mut live = self.live_out(block, &ins);
                for stmt in self.code[block].iter().rev() {
                    Self::live_before(stmt, &mut live);
                }
                if live != ins[block] {
                    ins[block] = live;
                    changed = true;
                }
            }
            if !changed {
                return ins;
            }
        }
    }

    /// The locals live after each statement of a block.
    fn live_after(&self, block: usize, live: &[Bits]) -> Vec<Bits> {
        let code = &self.code[block];
        let mut now = self.live_out(block, live);
        let mut after = vec![Bits::new(self.refs); code.len()];
        for (index, stmt) in code.iter().enumerate().rev() {
            after[index] = now.clone();
            Self::live_before(stmt, &mut now);
        }
        after
    }

    /// The sites in use at access `k` of a statement (ADR 0107): what the
    /// locals live after the statement hold, what the locals read by its later
    /// accesses hold, and, for all of a call's accesses but the write of its
    /// result, what its arguments hold.
    fn in_use(&self, stmt: &Stmt, k: usize, before: &State, after: &State, live: &Bits) -> Bits {
        let mut in_use = Bits::new(self.sites.len());
        for local in (0..self.refs).filter(|&local| live.contains(local)) {
            in_use.union(&after[local]);
        }
        for later in &stmt.accesses[k + 1..] {
            if let Some(local) = later.reads {
                in_use.union(&before[local]);
            }
        }
        if !stmt.accesses[k].after_call {
            for &local in &stmt.args {
                in_use.union(&before[local]);
            }
        }
        in_use
    }

    /// Whether an access conflicts with a site in use (ADR 0106).
    fn conflicts(&self, how: How, path: &Path, site: &Site) -> bool {
        (how.writes() || site.how.mutable()) && overlap(path, &site.path)
    }

    /// Every access in conflict, found in one pass over the reachable blocks
    /// from their sets at the fixed points.
    fn faults(&self, held: &[Option<State>], live: &[Bits]) -> Vec<Fault> {
        let mut faults = Vec::new();
        for &block in &self.blocks {
            let Some(mut state) = held[block].clone() else {
                continue;
            };
            let after = self.live_after(block, live);
            for (index, stmt) in self.code[block].iter().enumerate() {
                let before = state.clone();
                self.apply(&mut state, stmt);
                for (k, access) in stmt.accesses.iter().enumerate() {
                    let Some(path) = &access.path else {
                        continue;
                    };
                    let in_use = self.in_use(stmt, k, &before, &state, &after[index]);
                    let sites: Vec<usize> = (0..self.sites.len())
                        .filter(|&site| {
                            in_use.contains(site)
                                && access.site != Some(site)
                                && self.conflicts(access.how, path, &self.sites[site])
                        })
                        .collect();
                    if !sites.is_empty() {
                        faults.push(Fault {
                            at: (block, index, k),
                            after_call: access.after_call,
                            how: access.how,
                            place: access.place,
                            span: access.span,
                            sites,
                        });
                    }
                }
            }
        }
        faults
    }

    /// What the locals hold before each statement of each reachable block.
    fn held_before(&self, held: &[Option<State>]) -> Vec<Vec<State>> {
        let mut all = vec![Vec::new(); self.body.blocks.len()];
        for &block in &self.blocks {
            let Some(mut state) = held[block].clone() else {
                continue;
            };
            for stmt in &self.code[block] {
                all[block].push(state.clone());
                self.apply(&mut state, stmt);
            }
        }
        all
    }

    /// The blocks control may reach from the successors of `block`: `block`
    /// itself if it is in a loop.
    fn reach_from(&self, block: usize) -> Vec<bool> {
        let mut reached = vec![false; self.body.blocks.len()];
        let mut work = self.successors[block].clone();
        while let Some(next) = work.pop() {
            if !std::mem::replace(&mut reached[next], true) {
                work.extend(&self.successors[next]);
            }
        }
        reached
    }

    /// A read, after the access, of a local holding the site: among those the
    /// access reaches, the first in source order at or after the access, or,
    /// when every one starts before it, the first of those, marked as reached
    /// in a later iteration of a loop (ADR 0107). A read in the access's own
    /// block after it, or in the same statement after it, is ahead whatever
    /// its span.
    fn later_use(
        &self,
        fault: &Fault,
        site: usize,
        before: &[Vec<State>],
        reached: &[bool],
    ) -> Option<(Span, bool)> {
        let (fault_block, fault_index, fault_access) = fault.at;
        let mut ahead = Vec::new();
        let mut round = Vec::new();
        for &block in &self.blocks {
            for (index, stmt) in self.code[block].iter().enumerate() {
                for (k, access) in stmt.accesses.iter().enumerate() {
                    let Some(local) = access.reads else {
                        continue;
                    };
                    if !before[block][index][local].contains(site) {
                        continue;
                    }
                    // A call reads its arguments when it runs, after all of
                    // them are evaluated.
                    let after = block == fault_block && (index, k) > (fault_index, fault_access)
                        || (block, index) == (fault_block, fault_index)
                            && stmt.call
                            && !fault.after_call;
                    if !after && !reached[block] {
                        continue;
                    }
                    let span = if stmt.call { stmt.span } else { access.span };
                    let found = (key(span), (block, index, k), span);
                    let starts_after = span.file == fault.span.file && span.lo >= fault.span.lo;
                    // Outside a loop, whatever the access reaches comes after
                    // it, wherever its span starts: a call starts before its
                    // arguments, and a check of an index splits the blocks.
                    if after || starts_after || !reached[fault_block] {
                        ahead.push(found);
                    } else {
                        round.push(found);
                    }
                }
            }
        }
        match ahead
            .into_iter()
            .min_by_key(|&(order, loc, _)| (order, loc))
        {
            Some((_, _, span)) => Some((span, false)),
            None => (round
                .into_iter()
                .min_by_key(|&(order, loc, _)| (order, loc)))
            .map(|(_, _, span)| (span, true)),
        }
    }

    /// The errors, in the order of the spans of the accesses, then of their
    /// locations, one for two accesses that have the same span, message and
    /// labels.
    fn diagnostics(&self, mut faults: Vec<Fault>, held: &[Option<State>]) -> Vec<Diagnostic> {
        faults.sort_by_key(|fault| (key(fault.span), fault.at));
        let before = self.held_before(held);
        let mut seen = HashSet::new();
        let mut diags = Vec::new();
        for fault in faults {
            let reached = self.reach_from(fault.at.0);
            let (message, primary) = self.message(&fault);
            let mut labels = vec![(fault.span, primary.to_owned())];
            let mut sites = fault.sites.clone();
            sites.sort_by_key(|&site| (key(self.sites[site].span), site));
            for site in sites {
                labels.push((self.sites[site].span, self.site_label(&self.sites[site])));
                if let Some((span, round)) = self.later_use(&fault, site, &before, &reached) {
                    labels.push((
                        span,
                        if round {
                            "the borrow is used in the next iteration of the loop"
                        } else {
                            "the borrow is used later here"
                        }
                        .to_owned(),
                    ));
                }
            }
            if !seen.insert((fault.span, message.clone(), labels.clone())) {
                continue;
            }
            let mut diag = Diagnostic::error(BORROW_CONFLICT, message);
            for (span, text) in labels {
                diag = diag.label(span, text);
            }
            diags.push(diag.help(
                "end the borrow first: use the reference before this point, or take the value \
                 out into a `let` before borrowing it (ADR 0106)",
            ));
        }
        diags
    }

    /// The message of an access in conflict, and the label at the access.
    fn message(&self, fault: &Fault) -> (String, &'static str) {
        let name = self.name(fault.place);
        let holder = self.name(Place::local(fault.place.local));
        match fault.how {
            How::Use => (
                format!("cannot use {name} while it is borrowed as mutable"),
                "used here",
            ),
            How::RawConst => (
                format!("cannot take `&raw const` of {name} while it is borrowed as mutable"),
                "address taken here",
            ),
            How::Borrow => (
                format!("cannot borrow {name} while it is borrowed as mutable"),
                "borrowed here",
            ),
            How::Convert => (
                format!("cannot convert {holder} to `&` while it is borrowed as mutable"),
                "converted to `&` here",
            ),
            How::Assign => (
                format!("cannot assign to {name} while it is borrowed"),
                "assigned here",
            ),
            How::RawMut => (
                format!("cannot take `&raw mut` of {name} while it is borrowed"),
                "address taken here",
            ),
            How::Move => (
                format!("cannot move out of {name} while it is borrowed"),
                "moved here",
            ),
            How::End => (
                format!("cannot end {name} while it is borrowed"),
                "ended here",
            ),
            How::BorrowMut => (
                format!("cannot borrow {name} as mutable while it is borrowed"),
                "borrowed as mutable here",
            ),
            How::Lend => (
                format!("cannot lend {holder} to this call while it is borrowed"),
                "lent to this call here",
            ),
        }
    }

    fn site_label(&self, site: &Site) -> String {
        let name = self.name(site.place);
        let holder = self.name(Place::local(site.place.local));
        match site.how {
            How::Lend => format!("{holder} is lent to this call"),
            How::Convert => format!("{holder} is converted to `&` here"),
            _ if site.how.mutable() => format!("{name} is borrowed as mutable here"),
            _ => format!("{name} is borrowed here"),
        }
    }

    /// The place as the source names it, in backquotes (`x`, `s.f`, `a[_]`,
    /// `*r`, `(*r).f`), or "this value" for one the compiler keeps.
    fn name(&self, place: Place) -> String {
        let Some(base) =
            (self.body.locals.get(place.local.0 as usize)).and_then(|d| d.name.clone())
        else {
            return "this value".to_owned();
        };
        let mut text = base;
        let paren = |text: String| {
            if text.starts_with('*') {
                format!("({text})")
            } else {
                text
            }
        };
        for (k, elem) in place.projection.iter().enumerate() {
            let before = Place {
                local: place.local,
                projection: &place.projection[..k],
            };
            let ty = self.body.place_ty(before);
            text = match (elem, ty) {
                (PlaceElem::Deref, _) => format!("*{text}"),
                (PlaceElem::Field(index), Ty::Struct(s)) => {
                    match s.def().fields().get(*index as usize) {
                        Some(field) => format!("{}.{}", paren(text), field.name),
                        None => format!("{}.{index}", paren(text)),
                    }
                }
                (PlaceElem::Field(index), _) => format!("{}.{index}", paren(text)),
                (PlaceElem::Payload { variant, index }, Ty::Enum(e)) => {
                    match e.def().variants().get(*variant as usize) {
                        Some(v) => match v.payload.get(*index as usize) {
                            Some(field) => format!("{}.{}.{}", paren(text), v.name, field.name),
                            None => format!("{}.{}.{index}", paren(text), v.name),
                        },
                        None => text,
                    }
                }
                (
                    PlaceElem::Index(_)
                    | PlaceElem::ConstIndex(_)
                    | PlaceElem::Element(..)
                    | PlaceElem::ConstElement(..)
                    | PlaceElem::Offset(_),
                    _,
                ) => format!("{}[_]", paren(text)),
                (PlaceElem::Tag | PlaceElem::Payload { .. }, _) => text,
            };
        }
        format!("`{text}`")
    }
}

impl Stmt {
    fn new(span: Span) -> Stmt {
        Stmt {
            span,
            accesses: Vec::new(),
            def: None,
            call: false,
            args: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::{
        BasicBlock, BasicBlockData, BodyId, Const, Field, IntTy, Layout, LocalDecl, Ref, StructRef,
    };
    use crate::span::SourceMap;

    fn span(lo: usize) -> Span {
        let file = SourceMap::default().add("main.ers", "");
        Span::new(file, lo, lo + 1)
    }

    fn u32_ty() -> Ty {
        Ty::Int(IntTy::U32)
    }

    fn reference(mutable: bool, referent: Ty) -> Ty {
        Ty::Ref(Ref {
            mutable,
            referent: Box::leak(Box::new(referent)),
        })
    }

    fn mut_ref() -> Ty {
        reference(true, u32_ty())
    }

    fn shared_ref() -> Ty {
        reference(false, u32_ty())
    }

    fn raw() -> Ty {
        Ty::Ptr(crate::mir::Ptr {
            mutable: true,
            volatile: false,
            pointee: Box::leak(Box::new(u32_ty())),
        })
    }

    /// `struct Pair { a: u32, b: u32 }`.
    fn pair() -> Ty {
        let s = StructRef::new(
            "Pair".to_owned(),
            Layout { size: 8, align: 4 },
            false,
            false,
            false,
        );
        s.def().set_fields(vec![
            Field {
                name: "a".to_owned(),
                ty: u32_ty(),
                offset: 0,
            },
            Field {
                name: "b".to_owned(),
                ty: u32_ty(),
                offset: 4,
            },
        ]);
        Ty::Struct(s)
    }

    fn array() -> Ty {
        Ty::Array(Box::leak(Box::new(u32_ty())), 4)
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

    /// `*_n`.
    fn deref(n: u32) -> Place {
        projected(n, &[PlaceElem::Deref])
    }

    /// `_n.field`.
    fn field(n: u32, index: u32) -> Place {
        projected(n, &[PlaceElem::Field(index)])
    }

    /// `(*_n).index`.
    fn through(n: u32, index: u32) -> Place {
        projected(n, &[PlaceElem::Deref, PlaceElem::Field(index)])
    }

    /// `_n[index]`.
    fn element(n: u32, index: u64) -> Place {
        projected(n, &[PlaceElem::ConstIndex(index)])
    }

    fn ref_(dest: u32, mutable: bool, place: Place) -> Statement {
        Statement::Assign(local(dest), Rvalue::Ref { mutable, place })
    }

    /// `_dest = &place`.
    fn shared(dest: u32, place: Place) -> Statement {
        ref_(dest, false, place)
    }

    /// `_dest = &mut place`.
    fn mutable(dest: u32, place: Place) -> Statement {
        ref_(dest, true, place)
    }

    fn one() -> Operand {
        Operand::Const(Const::Int(1, IntTy::U32))
    }

    /// `place = const 1`.
    fn set(place: Place) -> Statement {
        Statement::Assign(place, Rvalue::Use(one()))
    }

    /// `_dest = copy place`.
    fn read(dest: u32, place: Place) -> Statement {
        Statement::Assign(local(dest), Rvalue::Use(Operand::Copy(place)))
    }

    /// `_dest = copy _from` or `move _from`.
    fn copy(dest: u32, from: u32) -> Statement {
        read(dest, local(from))
    }

    fn take(dest: u32, from: u32, at: usize) -> Statement {
        Statement::Assign(
            local(dest),
            Rvalue::Use(Operand::Move(local(from), span(at))),
        )
    }

    /// `call f(args)` with the destination, if any.
    fn call(dest: Option<Place>, args: Vec<Operand>) -> Statement {
        Statement::Call {
            dest,
            callee: BodyId(0),
            args,
        }
    }

    fn mv(n: u32) -> Operand {
        Operand::Move(local(n), span(900))
    }

    fn cp(place: Place) -> Operand {
        Operand::Copy(place)
    }

    /// Statements at `start`, `start + 1`, ...; the terminator at `start + 50`.
    fn block_at(
        start: usize,
        statements: Vec<Statement>,
        terminator: Terminator,
    ) -> BasicBlockData {
        BasicBlockData {
            spans: (0..statements.len()).map(|i| span(start + i)).collect(),
            statements,
            terminator,
            terminator_span: span(start + 50),
        }
    }

    fn block(statements: Vec<Statement>, terminator: Terminator) -> BasicBlockData {
        block_at(100, statements, terminator)
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

    fn body(locals: Vec<LocalDecl>, blocks: Vec<BasicBlockData>) -> Body {
        let mut all = vec![decl(Ty::Unit, None)];
        all.extend(locals);
        Body {
            name: "f".to_owned(),
            span: span(0),
            comptime: false,
            ret: Ty::Unit,
            param_count: 0,
            locals: all,
            blocks,
            runtime: false,
            external: false,
            naked: false,
            align: None,
        }
    }

    /// `_1: u32` named `x`, `_2: &mut u32` named `r`, `_3: &mut u32` named `s`,
    /// `_4: u32` and `_5: u32` unnamed.
    fn plain(blocks: Vec<BasicBlockData>) -> Body {
        body(
            vec![
                decl(u32_ty(), Some("x")),
                decl(mut_ref(), Some("r")),
                decl(mut_ref(), Some("s")),
                decl(u32_ty(), None),
                decl(u32_ty(), None),
            ],
            blocks,
        )
    }

    /// The errors of `body`, each as its message and its labels' texts.
    fn errors_of(body: &Body) -> Vec<(String, Vec<String>)> {
        let mut diags = Diagnostics::default();
        let passed = check(body, &mut diags);
        let list = diags.into_vec();
        assert_eq!(passed, list.is_empty());
        list.into_iter()
            .map(|diag| {
                assert_eq!(diag.code, BORROW_CONFLICT);
                let labels = diag.labels.iter().map(|l| l.message.clone()).collect();
                (diag.message, labels)
            })
            .collect()
    }

    fn accepted(body: &Body) {
        let errors = errors_of(body);
        assert!(errors.is_empty(), "{errors:?}");
    }

    fn only(body: &Body) -> (String, Vec<String>) {
        let mut errors = errors_of(body);
        assert_eq!(errors.len(), 1, "{errors:?}");
        errors.remove(0)
    }

    #[test]
    fn a_write_while_a_shared_borrow_is_used_later_is_an_error_and_after_its_last_use_is_not() {
        // _3 = &x; x = 1; _4 = copy *_3; with `_3: &u32`.
        let held = body(
            vec![
                decl(u32_ty(), Some("x")),
                decl(shared_ref(), Some("q")),
                decl(u32_ty(), None),
            ],
            vec![block(
                vec![shared(2, local(1)), set(local(1)), read(3, deref(2))],
                Terminator::Return,
            )],
        );
        let (message, labels) = only(&held);
        assert_eq!(message, "cannot assign to `x` while it is borrowed");
        assert_eq!(
            labels,
            [
                "assigned here",
                "`x` is borrowed here",
                "the borrow is used later here"
            ]
        );
        let released = body(
            vec![
                decl(u32_ty(), Some("x")),
                decl(shared_ref(), Some("q")),
                decl(u32_ty(), None),
            ],
            vec![block(
                vec![shared(2, local(1)), read(3, deref(2)), set(local(1))],
                Terminator::Return,
            )],
        );
        accepted(&released);
    }

    #[test]
    fn the_borrow_ends_at_the_last_use_of_what_holds_it() {
        // `let r = &mut x; *r = 1; x = 2;` and its inverse.
        let ends = plain(vec![block(
            vec![mutable(2, local(1)), set(deref(2)), set(local(1))],
            Terminator::Return,
        )]);
        accepted(&ends);
        let lasts = plain(vec![block(
            vec![mutable(2, local(1)), set(local(1)), set(deref(2))],
            Terminator::Return,
        )]);
        let (message, labels) = only(&lasts);
        assert_eq!(message, "cannot assign to `x` while it is borrowed");
        assert_eq!(labels[1], "`x` is borrowed as mutable here");
    }

    #[test]
    fn two_mutable_borrows_held_together_are_an_error() {
        let both = plain(vec![block(
            vec![
                mutable(2, local(1)),
                mutable(3, local(1)),
                set(deref(2)),
                set(deref(3)),
            ],
            Terminator::Return,
        )]);
        let (message, labels) = only(&both);
        assert_eq!(message, "cannot borrow `x` as mutable while it is borrowed");
        assert_eq!(
            labels,
            [
                "borrowed as mutable here",
                "`x` is borrowed as mutable here",
                "the borrow is used later here"
            ]
        );
        // The first is not used after the second is made: accepted.
        let one_at_a_time = plain(vec![block(
            vec![
                mutable(2, local(1)),
                set(deref(2)),
                mutable(3, local(1)),
                set(deref(3)),
            ],
            Terminator::Return,
        )]);
        accepted(&one_at_a_time);
    }

    #[test]
    fn a_read_during_a_mutable_borrow_is_an_error_and_through_it_is_not() {
        let direct = plain(vec![block(
            vec![mutable(2, local(1)), copy(4, 1), set(deref(2))],
            Terminator::Return,
        )]);
        let (message, labels) = only(&direct);
        assert_eq!(message, "cannot use `x` while it is borrowed as mutable");
        assert_eq!(labels[0], "used here");
        let through = plain(vec![block(
            vec![mutable(2, local(1)), read(4, deref(2)), set(deref(2))],
            Terminator::Return,
        )]);
        accepted(&through);
        // A shared borrow of the place conflicts as a read does.
        let borrowed = body(
            vec![
                decl(u32_ty(), Some("x")),
                decl(mut_ref(), Some("r")),
                decl(shared_ref(), Some("q")),
            ],
            vec![block(
                vec![mutable(2, local(1)), shared(3, local(1)), set(deref(2))],
                Terminator::Return,
            )],
        );
        let (message, _) = only(&borrowed);
        assert_eq!(message, "cannot borrow `x` while it is borrowed as mutable");
    }

    #[test]
    fn a_shared_borrow_is_compatible_with_reads_and_other_shared_borrows() {
        let body = body(
            vec![
                decl(u32_ty(), Some("x")),
                decl(shared_ref(), Some("a")),
                decl(shared_ref(), Some("b")),
                decl(u32_ty(), None),
                decl(u32_ty(), None),
            ],
            vec![block(
                vec![
                    shared(2, local(1)),
                    shared(3, local(1)),
                    copy(4, 1),
                    read(5, deref(2)),
                    read(5, deref(3)),
                ],
                Terminator::Return,
            )],
        );
        accepted(&body);
    }

    #[test]
    fn different_fields_do_not_overlap_but_elements_and_a_field_and_its_struct_do() {
        let fields = body(
            vec![
                decl(pair(), Some("s")),
                decl(mut_ref(), Some("a")),
                decl(shared_ref(), Some("b")),
                decl(u32_ty(), None),
            ],
            vec![block(
                vec![
                    mutable(2, field(1, 0)),
                    shared(3, field(1, 1)),
                    set(deref(2)),
                    read(4, deref(3)),
                ],
                Terminator::Return,
            )],
        );
        accepted(&fields);
        // The same field twice overlaps.
        let same = body(
            vec![
                decl(pair(), Some("s")),
                decl(mut_ref(), Some("a")),
                decl(shared_ref(), Some("b")),
                decl(u32_ty(), None),
            ],
            vec![block(
                vec![
                    mutable(2, field(1, 0)),
                    shared(3, field(1, 0)),
                    set(deref(2)),
                    read(4, deref(3)),
                ],
                Terminator::Return,
            )],
        );
        let (message, _) = only(&same);
        assert_eq!(
            message,
            "cannot borrow `s.a` while it is borrowed as mutable"
        );
        // Two elements of an array overlap whatever their indexes.
        let elements = body(
            vec![
                decl(array(), Some("a")),
                decl(mut_ref(), Some("p")),
                decl(shared_ref(), Some("q")),
                decl(u32_ty(), None),
            ],
            vec![block(
                vec![
                    mutable(2, element(1, 0)),
                    shared(3, element(1, 1)),
                    set(deref(2)),
                    read(4, deref(3)),
                ],
                Terminator::Return,
            )],
        );
        let (message, _) = only(&elements);
        assert_eq!(
            message,
            "cannot borrow `a[_]` while it is borrowed as mutable"
        );
        // A field and the struct it is in.
        let whole = body(
            vec![
                decl(pair(), Some("s")),
                decl(mut_ref(), Some("a")),
                decl(reference(false, pair()), Some("b")),
                decl(u32_ty(), None),
            ],
            vec![block(
                vec![
                    mutable(2, field(1, 0)),
                    shared(3, local(1)),
                    set(deref(2)),
                    read(4, through(3, 1)),
                ],
                Terminator::Return,
            )],
        );
        let (message, _) = only(&whole);
        assert_eq!(message, "cannot borrow `s` while it is borrowed as mutable");
    }

    #[test]
    fn a_reborrow_keeps_what_its_base_holds_in_use() {
        // r = &mut x; q = &mut *r; x = 1; *q = 2: the write of x conflicts with
        // the borrow of x that q reaches through r, though r is dead.
        let reborrow = plain(vec![block(
            vec![
                mutable(2, local(1)),
                mutable(3, deref(2)),
                set(local(1)),
                set(deref(3)),
            ],
            Terminator::Return,
        )]);
        let (message, labels) = only(&reborrow);
        assert_eq!(message, "cannot assign to `x` while it is borrowed");
        assert_eq!(labels[1], "`x` is borrowed as mutable here");
        // The reborrow itself is in conflict with a use of its base.
        let base = plain(vec![block(
            vec![
                mutable(2, local(1)),
                mutable(3, deref(2)),
                set(deref(2)),
                set(deref(3)),
            ],
            Terminator::Return,
        )]);
        let (message, labels) = only(&base);
        assert_eq!(message, "cannot assign to `*r` while it is borrowed");
        assert_eq!(labels[1], "`*r` is borrowed as mutable here");
        // Once the reborrow is dead, the base is usable again.
        let after = plain(vec![block(
            vec![
                mutable(2, local(1)),
                mutable(3, deref(2)),
                set(deref(3)),
                set(deref(2)),
            ],
            Terminator::Return,
        )]);
        accepted(&after);
    }

    #[test]
    fn a_borrow_held_by_one_of_two_locals_at_a_join_is_in_use() {
        // bb0: a = &x; b = &y; if -> bb1, bb2; bb1: c = a; bb2: c = b;
        // bb3: x = 1; y = 2; _ = *c.
        let locals = vec![
            decl(u32_ty(), Some("x")),
            decl(u32_ty(), Some("y")),
            decl(shared_ref(), Some("a")),
            decl(shared_ref(), Some("b")),
            decl(shared_ref(), Some("c")),
            decl(u32_ty(), None),
        ];
        let join = body(
            locals,
            vec![
                block(vec![shared(3, local(1)), shared(4, local(2))], branch(1, 2)),
                block_at(200, vec![copy(5, 3)], goto(3)),
                block_at(300, vec![copy(5, 4)], goto(3)),
                block_at(
                    400,
                    vec![set(local(1)), set(local(2)), read(6, deref(5))],
                    Terminator::Return,
                ),
            ],
        );
        let errors = errors_of(&join);
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert_eq!(errors[0].0, "cannot assign to `x` while it is borrowed");
        assert_eq!(errors[1].0, "cannot assign to `y` while it is borrowed");
    }

    #[test]
    fn a_use_only_round_a_loop_keeps_the_borrow_in_use_and_is_told_so() {
        // bb0: r = &mut x; bb1: *r = 1; bb2: x = 2; if -> bb1, bb3.
        let looped = plain(vec![
            block(vec![mutable(2, local(1))], goto(1)),
            block_at(200, vec![set(deref(2))], goto(2)),
            block_at(300, vec![set(local(1))], branch(1, 3)),
            block_at(400, vec![], Terminator::Return),
        ]);
        let (message, labels) = only(&looped);
        assert_eq!(message, "cannot assign to `x` while it is borrowed");
        assert_eq!(
            labels,
            [
                "assigned here",
                "`x` is borrowed as mutable here",
                "the borrow is used in the next iteration of the loop"
            ]
        );
        // Without the back edge nothing is used after the write.
        let straight = plain(vec![
            block(vec![mutable(2, local(1))], goto(1)),
            block_at(200, vec![set(deref(2))], goto(2)),
            block_at(300, vec![set(local(1))], Terminator::Return),
        ]);
        accepted(&straight);
    }

    #[test]
    fn a_borrow_through_a_raw_pointer_gives_no_site_and_an_access_to_one_conflicts_with_none() {
        // p: *mut u32; r = &mut x; q = &mut *p; *p = 1; *r = 2.
        let raw_borrow = body(
            vec![
                decl(u32_ty(), Some("x")),
                decl(mut_ref(), Some("r")),
                decl(raw(), Some("p")),
                decl(mut_ref(), Some("q")),
            ],
            vec![block(
                vec![
                    mutable(2, local(1)),
                    mutable(4, deref(3)),
                    set(deref(3)),
                    set(deref(2)),
                    set(deref(4)),
                ],
                Terminator::Return,
            )],
        );
        accepted(&raw_borrow);
        let flow = Flow::of(&raw_borrow);
        assert_eq!(flow.sites.len(), 1);
        // The reference made of a raw place holds nothing: x is free while q
        // lives.
        let free = body(
            vec![
                decl(u32_ty(), Some("x")),
                decl(raw(), Some("p")),
                decl(mut_ref(), Some("q")),
            ],
            vec![block(
                vec![mutable(3, deref(2)), set(local(1)), set(deref(3))],
                Terminator::Return,
            )],
        );
        assert!(Flow::of(&free).sites.is_empty());
        accepted(&free);
    }

    #[test]
    fn a_calls_arguments_are_in_use_during_the_call_and_not_at_its_result_write() {
        // f(&mut x, x): the second argument is read while the first is held.
        let during = plain(vec![block(
            vec![mutable(2, local(1)), call(None, vec![mv(2), cp(local(1))])],
            Terminator::Return,
        )]);
        let (message, labels) = only(&during);
        assert_eq!(message, "cannot use `x` while it is borrowed as mutable");
        assert_eq!(
            labels,
            [
                "used here",
                "`x` is borrowed as mutable here",
                "the borrow is used later here"
            ]
        );
        // f(x, &mut x): the read comes first.
        let first = plain(vec![block(
            vec![
                copy(4, 1),
                mutable(2, local(1)),
                call(None, vec![cp(local(4)), mv(2)]),
            ],
            Terminator::Return,
        )]);
        accepted(&first);
        // x = f(&x): the result is written after the callee returns.
        let result = body(
            vec![decl(u32_ty(), Some("x")), decl(shared_ref(), None)],
            vec![block(
                vec![shared(2, local(1)), call(Some(local(1)), vec![mv(2)])],
                Terminator::Return,
            )],
        );
        accepted(&result);
        // f(&x, &mut x) is rejected where the second borrow is made.
        let twice = body(
            vec![
                decl(u32_ty(), Some("x")),
                decl(shared_ref(), None),
                decl(mut_ref(), None),
            ],
            vec![block(
                vec![
                    shared(2, local(1)),
                    mutable(3, local(1)),
                    call(None, vec![mv(2), mv(3)]),
                ],
                Terminator::Return,
            )],
        );
        let (message, labels) = only(&twice);
        assert_eq!(message, "cannot borrow `x` as mutable while it is borrowed");
        assert_eq!(labels[1], "`x` is borrowed here");
    }

    #[test]
    fn an_operand_read_after_the_last_use_of_a_borrow_within_one_statement_is_accepted() {
        // _4 = Add(copy *r, copy x) with r = &mut x: the read of x comes after
        // the last use of r.
        let after = plain(vec![block(
            vec![
                mutable(2, local(1)),
                Statement::Assign(
                    local(4),
                    Rvalue::Binary(crate::mir::BinOp::Add, cp(deref(2)), cp(local(1))),
                ),
            ],
            Terminator::Return,
        )]);
        accepted(&after);
        let before = plain(vec![block(
            vec![
                mutable(2, local(1)),
                Statement::Assign(
                    local(4),
                    Rvalue::Binary(crate::mir::BinOp::Add, cp(local(1)), cp(deref(2))),
                ),
            ],
            Terminator::Return,
        )]);
        let (message, labels) = only(&before);
        assert_eq!(message, "cannot use `x` while it is borrowed as mutable");
        assert_eq!(labels[2], "the borrow is used later here");
    }

    #[test]
    fn a_move_and_an_end_conflict_with_a_shared_borrow() {
        let moved = body(
            vec![
                decl(pair(), Some("s")),
                decl(reference(false, pair()), Some("q")),
                decl(pair(), None),
                decl(u32_ty(), None),
            ],
            vec![block(
                vec![shared(2, local(1)), take(3, 1, 10), read(4, through(2, 0))],
                Terminator::Return,
            )],
        );
        let (message, labels) = only(&moved);
        assert_eq!(message, "cannot move out of `s` while it is borrowed");
        assert_eq!(labels[0], "moved here");
        let ended = body(
            vec![
                decl(pair(), Some("s")),
                decl(reference(false, pair()), Some("q")),
                decl(u32_ty(), None),
            ],
            vec![block(
                vec![
                    shared(2, local(1)),
                    Statement::End(local(1), span(20)),
                    read(3, through(2, 0)),
                ],
                Terminator::Return,
            )],
        );
        let (message, _) = only(&ended);
        assert_eq!(message, "cannot end `s` while it is borrowed");
    }

    #[test]
    fn a_raw_address_and_an_asm_output_are_accesses() {
        // `&raw mut x` is a write, `&raw const x` a read.
        let raw_mut = body(
            vec![
                decl(u32_ty(), Some("x")),
                decl(shared_ref(), Some("q")),
                decl(raw(), None),
                decl(u32_ty(), None),
            ],
            vec![block(
                vec![
                    shared(2, local(1)),
                    Statement::Assign(
                        local(3),
                        Rvalue::AddressOf {
                            mutable: true,
                            place: local(1),
                        },
                    ),
                    read(4, deref(2)),
                ],
                Terminator::Return,
            )],
        );
        let (message, _) = only(&raw_mut);
        assert_eq!(
            message,
            "cannot take `&raw mut` of `x` while it is borrowed"
        );
        let raw_const = body(
            vec![
                decl(u32_ty(), Some("x")),
                decl(shared_ref(), Some("q")),
                decl(raw(), None),
                decl(u32_ty(), None),
            ],
            vec![block(
                vec![
                    shared(2, local(1)),
                    Statement::Assign(
                        local(3),
                        Rvalue::AddressOf {
                            mutable: false,
                            place: local(1),
                        },
                    ),
                    read(4, deref(2)),
                ],
                Terminator::Return,
            )],
        );
        accepted(&raw_const);
    }

    #[test]
    fn a_lent_argument_and_a_conversion_are_told_as_such() {
        // twice(r, r): `_3 = &mut *r; _4 = &mut *r; call f(move _3, move _4)`.
        let lent = body(
            vec![
                decl(mut_ref(), Some("r")),
                decl(mut_ref(), None),
                decl(mut_ref(), None),
            ],
            vec![block(
                vec![
                    mutable(2, deref(1)),
                    mutable(3, deref(1)),
                    call(None, vec![mv(2), mv(3)]),
                ],
                Terminator::Return,
            )],
        );
        let (message, labels) = only(&lent);
        assert_eq!(message, "cannot lend `r` to this call while it is borrowed");
        assert_eq!(
            labels,
            [
                "lent to this call here",
                "`r` is lent to this call",
                "the borrow is used later here"
            ]
        );
        // A converted `&` in use while the `&mut` is lent.
        let converted = body(
            vec![
                decl(mut_ref(), Some("r")),
                decl(shared_ref(), None),
                decl(mut_ref(), None),
            ],
            vec![block(
                vec![
                    mutable(3, deref(1)),
                    shared(2, deref(1)),
                    call(None, vec![cp(local(2)), mv(3)]),
                ],
                Terminator::Return,
            )],
        );
        let (message, labels) = only(&converted);
        assert_eq!(
            message,
            "cannot convert `r` to `&` while it is borrowed as mutable"
        );
        assert_eq!(labels[0], "converted to `&` here");
        assert_eq!(labels[1], "`r` is lent to this call");
        // An explicit borrow whose reference is not a call's argument is told
        // by its place.
        let explicit = body(
            vec![
                decl(mut_ref(), Some("r")),
                decl(mut_ref(), Some("a")),
                decl(mut_ref(), Some("b")),
            ],
            vec![block(
                vec![
                    mutable(2, deref(1)),
                    mutable(3, deref(1)),
                    set(deref(2)),
                    set(deref(3)),
                ],
                Terminator::Return,
            )],
        );
        let (message, labels) = only(&explicit);
        assert_eq!(
            message,
            "cannot borrow `*r` as mutable while it is borrowed"
        );
        assert_eq!(labels[1], "`*r` is borrowed as mutable here");
    }

    #[test]
    fn a_place_the_compiler_keeps_is_this_value() {
        let temporary = body(
            vec![
                decl(u32_ty(), None),
                decl(shared_ref(), None),
                decl(u32_ty(), None),
            ],
            vec![block(
                vec![shared(2, local(1)), set(local(1)), read(3, deref(2))],
                Terminator::Return,
            )],
        );
        let (message, labels) = only(&temporary);
        assert_eq!(message, "cannot assign to this value while it is borrowed");
        assert_eq!(labels[1], "this value is borrowed here");
    }

    #[test]
    fn a_field_of_a_place_through_a_reference_is_named_with_parentheses() {
        let through = body(
            vec![
                decl(reference(true, pair()), Some("p")),
                decl(mut_ref(), Some("a")),
                decl(shared_ref(), Some("b")),
                decl(u32_ty(), None),
            ],
            vec![block(
                vec![
                    mutable(2, projected(1, &[PlaceElem::Deref, PlaceElem::Field(0)])),
                    shared(3, projected(1, &[PlaceElem::Deref, PlaceElem::Field(0)])),
                    set(deref(2)),
                    read(4, deref(3)),
                ],
                Terminator::Return,
            )],
        );
        let (message, _) = only(&through);
        assert_eq!(
            message,
            "cannot borrow `(*p).a` while it is borrowed as mutable"
        );
    }

    #[test]
    fn two_accesses_with_the_same_span_message_and_labels_are_one_error() {
        // Two blocks that have the same spans, as the copies of a statement
        // do, report once.
        let join = plain(vec![
            block(vec![mutable(2, local(1))], branch(1, 2)),
            block_at(200, vec![set(local(1))], goto(3)),
            block_at(200, vec![set(local(1))], goto(3)),
            block_at(400, vec![set(deref(2))], Terminator::Return),
        ]);
        let errors = errors_of(&join);
        assert_eq!(errors.len(), 1, "{errors:?}");
    }

    #[test]
    fn a_body_without_a_borrow_does_no_work() {
        let none = plain(vec![block(
            vec![set(local(1)), copy(4, 1)],
            Terminator::Return,
        )]);
        assert!(Flow::of(&none).sites.is_empty());
        let mut diags = Diagnostics::default();
        assert!(check(&none, &mut diags));
        assert!(diags.is_empty());
    }

    #[test]
    fn a_block_nothing_reaches_makes_no_site() {
        let unreachable_block = plain(vec![
            block(vec![set(local(1))], Terminator::Return),
            block(
                vec![mutable(2, local(1)), set(local(1)), set(deref(2))],
                Terminator::Return,
            ),
        ]);
        assert!(Flow::of(&unreachable_block).sites.is_empty());
        accepted(&unreachable_block);
    }

    #[test]
    fn paths_that_do_not_return_use_nothing_after_them() {
        // r = &mut x; if -> bb1, bb2; bb1: x = 1; panic; bb2: *r = 2.
        let panics = plain(vec![
            block(vec![mutable(2, local(1))], branch(1, 2)),
            block_at(
                200,
                vec![set(local(1))],
                Terminator::Panic(PanicCause::DivisionByZero, None),
            ),
            block_at(300, vec![set(deref(2))], Terminator::Return),
        ]);
        accepted(&panics);
    }

    /// The labels of the only error of `body`, as their texts and where each
    /// starts.
    fn label_starts(body: &Body) -> Vec<(String, u32)> {
        let mut diags = Diagnostics::default();
        check(body, &mut diags);
        let list = diags.into_vec();
        assert_eq!(list.len(), 1, "{list:?}");
        (list[0].labels.iter())
            .map(|label| (label.message.clone(), label.span.lo))
            .collect()
    }

    #[test]
    fn a_later_use_ahead_of_the_access_beats_one_reached_round_a_loop() {
        // bb1 is a loop: `*r = 1` at 200 is before the write of x at 201 and
        // reached only through the back edge, `*r = 3` at 202 is after it.
        let looped = plain(vec![
            block(vec![mutable(2, local(1))], goto(1)),
            block_at(
                200,
                vec![set(deref(2)), set(local(1)), set(deref(2))],
                branch(1, 2),
            ),
            block_at(300, vec![], Terminator::Return),
        ]);
        let labels = label_starts(&looped);
        assert_eq!(labels[0], ("assigned here".to_owned(), 201));
        assert_eq!(labels[2], ("the borrow is used later here".to_owned(), 202));
        assert_eq!(labels.len(), 3);
    }

    #[test]
    fn the_later_use_is_the_first_of_several_ahead() {
        // x = 2; *r = 1; *r = 3: the use that is named is `*r = 1`.
        let several = plain(vec![block_at(
            100,
            vec![
                mutable(2, local(1)),
                set(local(1)),
                set(deref(2)),
                set(deref(2)),
            ],
            Terminator::Return,
        )]);
        let labels = label_starts(&several);
        assert_eq!(labels[2], ("the borrow is used later here".to_owned(), 102));
        assert_eq!(labels.len(), 3);
    }

    #[test]
    fn the_fixed_points_are_the_same_whatever_order_the_blocks_are_visited_in() {
        // A loop with an exit and a join: the borrow of bb0 reaches bb1 and
        // the blocks after it, and bb2's use keeps it live round the loop.
        let looped = plain(vec![
            block(vec![mutable(2, local(1))], goto(1)),
            block_at(200, vec![], branch(2, 3)),
            block_at(300, vec![set(deref(2)), take(3, 2, 10)], goto(1)),
            block_at(400, vec![], branch(4, 5)),
            block_at(500, vec![copy(4, 1)], goto(5)),
            block_at(600, vec![], Terminator::Return),
        ]);
        let flow = Flow::of(&looped);
        let forward = flow.solve_held(&[0, 1, 2, 3, 4, 5]);
        let backward = flow.solve_live(&[5, 4, 3, 2, 1, 0]);
        for order in [
            [5, 4, 3, 2, 1, 0],
            [2, 4, 0, 5, 3, 1],
            [3, 1, 5, 0, 2, 4],
            [1, 1, 2, 2, 0, 0],
        ] {
            // Whatever the order, every block is visited at least once.
            let mut order = order.to_vec();
            order.extend(0..6);
            assert_eq!(flow.solve_held(&order), forward, "order {order:?}");
            assert_eq!(flow.solve_live(&order), backward, "order {order:?}");
        }
        for block in [1, 2, 3, 4, 5] {
            assert!(forward[block].as_ref().unwrap()[0].contains(0), "bb{block}");
        }
        assert!(!forward[0].as_ref().unwrap()[0].contains(0));
        // `r` is live at the start of bb1 and bb2 only.
        assert!(backward[1].contains(0) && backward[2].contains(0));
        assert!(!backward[3].contains(0) && !backward[0].contains(0));
    }
}
