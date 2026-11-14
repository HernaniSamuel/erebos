//! The rule of shared statics (ADR 0078, 0093), which covers peripherals too
//! (ADR 0115). A `static mut` or a peripheral that the thread context and the
//! handler context both reach may be reached by the thread only inside
//! `critical` (rule 1); one that an unmaskable handler reaches may be reached
//! by no other context, `critical` or not (rule 2). A peripheral is reached
//! also when a context reaches one whose bytes meet its own. The contexts are
//! read off the call graph MIR shows: `Statement::Call` and the checks that
//! call the panic function, nothing else.
//!
//! Each body is scanned once for its places (the operands and `sym` operands
//! that name a `static mut`, the operands that name a peripheral) and its
//! calls, each with whether some path reaches it outside every `critical`
//! block. The searches over the call graph then work on those scans alone.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::bits::{meet, shown};
use super::call_graph::{Call, Edges, Reach, Site};
use crate::diag::{Diagnostic, Diagnostics};
use crate::mir::{
    AsmSym, Body, BodyId, Const, Mir, Operand, PeripheralRef, Statement, StaticId, StaticRef,
    Terminator,
};
use crate::span::Span;
use crate::target::VectorKind;

const UNPROTECTED: &str = "analysis::unprotected-static";
const UNMASKABLE: &str = "analysis::unmaskable-static";
const UNPROTECTED_PERIPHERAL: &str = "analysis::unprotected-peripheral";
const UNMASKABLE_PERIPHERAL: &str = "analysis::unmaskable-peripheral";

/// Reports the errors of the rule over the whole program: for each rule, the
/// statics, then the peripherals.
pub fn check(mir: &Mir, diags: &mut Diagnostics) {
    let scans: Vec<Scan> = mir
        .bodies()
        .map(|(_, body)| Scan::of(body, mir.panic, &mir.peripherals))
        .collect();
    let roots = |kind: VectorKind| -> Vec<BodyId> {
        let mut roots: Vec<BodyId> = Vec::new();
        for handler in mir.handlers.iter().filter(|handler| handler.kind == kind) {
            if !roots.contains(&handler.body) {
                roots.push(handler.body);
            }
        }
        roots
    };
    let maskable = roots(VectorKind::Maskable);
    let unmaskable: Vec<(BodyId, Context)> = roots(VectorKind::Unmaskable)
        .into_iter()
        .map(|root| (root, Context::of(mir, &scans, &[root])))
        .collect();
    let thread = Context::of(mir, &scans, &[mir.entry]);
    let handler = Context::of(mir, &scans, &maskable);
    unprotected(mir, &scans, &thread, &handler, &maskable, diags);
    unprotected_peripherals(mir, &scans, &handler, &maskable, diags);
    unmaskable_statics(mir, &scans, &thread, &handler, &unmaskable, diags);
    unmaskable_peripherals(mir, &scans, &thread, &handler, &unmaskable, diags);
}

/// What a place names: a `static mut`, or a peripheral with its index in
/// `Mir::peripherals`, which has no id of its own.
#[derive(Clone, Copy, PartialEq)]
enum Named {
    Static(StaticRef),
    Peripheral {
        index: usize,
        peripheral: PeripheralRef,
    },
}

impl Named {
    /// Statics by id, then peripherals by index, the order the places of one
    /// statement are kept in.
    fn key(self) -> (bool, u64) {
        match self {
            Named::Static(stat) => (false, u64::from(stat.def().id.0)),
            Named::Peripheral { index, .. } => (true, index as u64),
        }
    }
}

/// A statement or terminator that names a `static mut` or a peripheral.
struct Place {
    site: Site,
    named: Named,
}

impl Place {
    /// The peripheral the place names, if it names one.
    fn peripheral(&self) -> Option<PeripheralRef> {
        match self.named {
            Named::Peripheral { peripheral, .. } => Some(peripheral),
            Named::Static(_) => None,
        }
    }
}

/// What one body holds, in the order of its blocks, then its statements.
#[derive(Default)]
struct Scan {
    places: Vec<Place>,
    calls: Vec<Call>,
}

impl AsRef<[Call]> for Scan {
    fn as_ref(&self) -> &[Call] {
        &self.calls
    }
}

/// What a statement or terminator names or calls.
struct Item {
    span: Span,
    named: Vec<Named>,
    call: Option<(BodyId, bool)>,
    outside: bool,
}

impl Scan {
    /// The places and calls in the blocks reachable from the start of `body`,
    /// by a forward walk over pairs of block and depth of `critical` blocks.
    /// A body that is external, or run only at compile time, holds none.
    fn of(body: &Body, panic: BodyId, peripherals: &[PeripheralRef]) -> Scan {
        if body.external || body.comptime || body.blocks.is_empty() {
            return Scan::default();
        }
        let mut items: BTreeMap<(usize, usize), Item> = BTreeMap::new();
        let mut seen: HashSet<(usize, usize)> = HashSet::new();
        let mut work = vec![(0, 0)];
        while let Some((index, depth)) = work.pop() {
            if !seen.insert((index, depth)) {
                continue;
            }
            let block = &body.blocks[index];
            let mut depth = depth;
            for (at, statement) in block.statements.iter().enumerate() {
                let span = block
                    .spans
                    .get(at)
                    .copied()
                    .unwrap_or(block.terminator_span);
                let call = match statement {
                    Statement::Call { callee, .. } => Some((*callee, false)),
                    _ => None,
                };
                let mut named = Vec::new();
                statement.visit_operands(&mut |operand| names(operand, peripherals, &mut named));
                if let Some(asm) = statement.asm() {
                    syms(asm.syms(), &mut named);
                }
                note(&mut items, (index, at), span, depth, named, call);
                match statement {
                    Statement::CriticalEnter(_) => depth += 1,
                    Statement::CriticalExit(_) => depth = depth.saturating_sub(1),
                    Statement::Assign(..)
                    | Statement::Call { .. }
                    | Statement::InlineAsm(_)
                    | Statement::New(..)
                    | Statement::End(..) => {}
                }
            }
            let terminator = &block.terminator;
            let call = match terminator {
                Terminator::Panic(..) => Some((panic, true)),
                _ => None,
            };
            let mut named = Vec::new();
            terminator.visit_operands(&mut |operand| names(operand, peripherals, &mut named));
            if let Some(asm) = terminator.asm() {
                syms(asm.syms(), &mut named);
            }
            let at = block.statements.len();
            note(
                &mut items,
                (index, at),
                block.terminator_span,
                depth,
                named,
                call,
            );
            for next in terminator.successors() {
                work.push((next.0 as usize, depth));
            }
        }
        let mut scan = Scan::default();
        for item in items.into_values() {
            let site = Site {
                span: item.span,
                outside: item.outside,
            };
            for named in item.named {
                scan.places.push(Place { site, named });
            }
            if let Some((callee, check)) = item.call {
                scan.calls.push(Call {
                    site,
                    callee,
                    check,
                });
            }
        }
        scan
    }
}

/// Records what the item at `key` holds, seen at `depth`: it is outside if
/// any visit is at depth 0.
fn note(
    items: &mut BTreeMap<(usize, usize), Item>,
    key: (usize, usize),
    span: Span,
    depth: usize,
    mut named: Vec<Named>,
    call: Option<(BodyId, bool)>,
) {
    if named.is_empty() && call.is_none() {
        return;
    }
    named.sort_by_key(|named| named.key());
    named.dedup();
    let item = items.entry(key).or_insert(Item {
        span,
        named,
        call,
        outside: false,
    });
    item.outside |= depth == 0;
}

/// Adds the `static mut` or the peripheral an operand names, if it names one.
/// A peripheral that `Mir::peripherals` lacks, which the validator rejects
/// (P2), is not a place.
fn names(operand: &Operand, peripherals: &[PeripheralRef], named: &mut Vec<Named>) {
    match operand {
        Operand::Const(Const::Static(stat)) if stat.def().mutable => {
            named.push(Named::Static(*stat));
        }
        Operand::Const(Const::Peripheral(peripheral)) => {
            if let Some(index) = peripherals.iter().position(|p| p == peripheral) {
                named.push(Named::Peripheral {
                    index,
                    peripheral: *peripheral,
                });
            }
        }
        _ => {}
    }
}

/// Adds the `static mut`s that `sym` operands name.
fn syms(syms: impl Iterator<Item = AsmSym>, named: &mut Vec<Named>) {
    for sym in syms {
        match sym {
            AsmSym::Static(stat) if stat.def().mutable => named.push(Named::Static(stat)),
            AsmSym::Static(_) | AsmSym::Body(_) => {}
        }
    }
}

/// What a context reaches, and its first place of each static: the place in
/// the lowest body, then block, then statement (ADR 0093). A place is the body
/// and the index in its scan.
struct Context {
    reach: Reach,
    first: HashMap<StaticId, (BodyId, usize)>,
    /// By index in `Mir::peripherals`: the first place naming the peripheral
    /// or one whose bytes meet it, if the context has one (ADR 0115).
    near: Vec<Option<(BodyId, usize)>>,
    /// By index in `Mir::peripherals`: whether some place names the
    /// peripheral itself.
    named: Vec<bool>,
}

impl Context {
    fn of(mir: &Mir, scans: &[Scan], roots: &[BodyId]) -> Context {
        let reach = Reach::search(scans, roots, Edges::CallsAndPanics, false);
        let mut first: HashMap<StaticId, (BodyId, usize)> = HashMap::new();
        let mut near: Vec<Option<(BodyId, usize)>> = vec![None; mir.peripherals.len()];
        let mut named = vec![false; mir.peripherals.len()];
        for (id, _) in mir.bodies() {
            if !reach.reaches(id) {
                continue;
            }
            for (p, place) in scans[id.0 as usize].places.iter().enumerate() {
                match place.named {
                    Named::Static(stat) => {
                        first.entry(stat.def().id).or_insert((id, p));
                    }
                    Named::Peripheral { index, peripheral } => {
                        if let Some(itself) = named.get_mut(index) {
                            *itself = true;
                        }
                        let all = near.iter_mut().zip(&mir.peripherals).enumerate();
                        for (i, (slot, &other)) in all {
                            if slot.is_none() && (i == index || meet(other, peripheral)) {
                                *slot = Some((id, p));
                            }
                        }
                    }
                }
            }
        }
        Context {
            reach,
            first,
            near,
            named,
        }
    }

    /// The handler the search found `body` from.
    fn root(&self, body: BodyId) -> Option<BodyId> {
        self.reach.root(body)
    }
}

/// The calls from a root to a place, as labels: each one to protect (rule 1,
/// `suffix` says it is outside `critical`) or to see (rule 2).
fn path_labels(
    mut diag: Diagnostic,
    reach: &Reach,
    scans: &[Scan],
    body: BodyId,
    suffix: &str,
) -> Diagnostic {
    for call in reach.path(scans, body) {
        diag = diag.label(
            call.site.span,
            if call.check {
                format!("this check may call the panic function{suffix}")
            } else {
                format!("called here{suffix}")
            },
        );
    }
    diag
}

/// Rule 1 of ADR 0093: a place outside `critical` in the thread context, that
/// names a static the handler context also reaches.
fn unprotected(
    mir: &Mir,
    scans: &[Scan],
    thread: &Context,
    handler: &Context,
    handlers: &[BodyId],
    diags: &mut Diagnostics,
) {
    if handlers.is_empty() {
        return;
    }
    let shared: HashSet<StaticId> = thread
        .first
        .keys()
        .filter(|stat| handler.first.contains_key(stat))
        .copied()
        .collect();
    if shared.is_empty() {
        return;
    }
    let first = &handler.first;
    let open = Reach::search(scans, &[mir.entry], Edges::CallsAndPanics, true);
    for (id, _) in mir.bodies() {
        if !open.reaches(id) {
            continue;
        }
        for (p, place) in scans[id.0 as usize].places.iter().enumerate() {
            let Named::Static(stat) = place.named else {
                continue;
            };
            let stat = stat.def();
            if !place.site.outside || !shared.contains(&stat.id) {
                continue;
            }
            // A shared static has a place in the handler context.
            let Some(&(other, q)) = first.get(&stat.id) else {
                continue;
            };
            let root = handler.root(other).unwrap_or(other);
            let by = format!("the handler `{}`", shown(&mir.body(root).name));
            let name = shown(&stat.name);
            let mut diag = Diagnostic::error(
                UNPROTECTED,
                format!(
                    "`{name}` is reached outside `critical` in the thread context, \
                     and a handler reaches it too"
                ),
            );
            let same = (other, q) == (id, p);
            diag = if same {
                diag.label(
                    place.site.span,
                    format!("reached here, outside `critical`, by the thread and by {by}"),
                )
            } else {
                diag.label(place.site.span, "reached here, outside `critical`")
            };
            diag = path_labels(diag, &open, scans, id, ", outside `critical`");
            if !same {
                let there = &scans[other.0 as usize].places[q];
                diag = diag.label(there.site.span, format!("{by} reaches it here"));
            }
            diags.push(
                diag.help("put the access, or one call on the path to it, in a `critical` block"),
            );
        }
    }
}

/// Rule 1 of ADR 0115: a place outside `critical` in the thread context, that
/// names a peripheral the handler context also reaches, by its own name or by
/// one whose bytes meet it.
fn unprotected_peripherals(
    mir: &Mir,
    scans: &[Scan],
    handler: &Context,
    handlers: &[BodyId],
    diags: &mut Diagnostics,
) {
    if handlers.is_empty() || handler.near.iter().all(Option::is_none) {
        return;
    }
    let open = Reach::search(scans, &[mir.entry], Edges::CallsAndPanics, true);
    for (id, _) in mir.bodies() {
        if !open.reaches(id) {
            continue;
        }
        for (p, place) in scans[id.0 as usize].places.iter().enumerate() {
            let Named::Peripheral { index, peripheral } = place.named else {
                continue;
            };
            if !place.site.outside {
                continue;
            }
            // A shared peripheral has a place in the handler context.
            let Some((other, q)) = handler.near.get(index).copied().flatten() else {
                continue;
            };
            let root = handler.root(other).unwrap_or(other);
            let by = format!("the handler `{}`", shown(&mir.body(root).name));
            let name = shown(&peripheral.def().name);
            let there = &scans[other.0 as usize].places[q];
            let near = meeting_of(peripheral, there);
            let itself = handler.named.get(index).copied().unwrap_or(false);
            let reaches = match near {
                Some(x) if !itself => format!("`{}`, whose bytes meet it", shown(&x.def().name)),
                _ => "it too".to_owned(),
            };
            let mut diag = Diagnostic::error(
                UNPROTECTED_PERIPHERAL,
                format!(
                    "`{name}` is reached outside `critical` in the thread context, \
                     and a handler reaches {reaches}"
                ),
            );
            let same = (other, q) == (id, p);
            diag = if same {
                diag.label(
                    place.site.span,
                    format!("reached here, outside `critical`, by the thread and by {by}"),
                )
            } else {
                diag.label(place.site.span, "reached here, outside `critical`")
            };
            diag = path_labels(diag, &open, scans, id, ", outside `critical`");
            if !same {
                diag = diag.label(there.site.span, reaches_here(&by, &name, near));
            }
            diags.push(diag.help(
                "put the access, or one call on the path to it, in a `critical` block, or \
                 leave the device to one context",
            ));
        }
    }
}

/// The peripheral `there` names when it is not `peripheral` itself, so one
/// whose bytes meet it.
fn meeting_of(peripheral: PeripheralRef, there: &Place) -> Option<PeripheralRef> {
    there.peripheral().filter(|other| *other != peripheral)
}

/// The label on the other context's place of the peripheral called `name`:
/// `near` is the peripheral that place names when that is another.
fn reaches_here(context: &str, name: &str, near: Option<PeripheralRef>) -> String {
    match near {
        Some(other) => format!(
            "{context} reaches `{}` here, whose bytes meet `{name}`'s",
            shown(&other.def().name)
        ),
        None => format!("{context} reaches it here"),
    }
}

/// Another context that reaches a static or a peripheral, as rule 2 names it.
struct Other<'a> {
    /// How the context is named: "the thread context", "the handler `h`".
    name: String,
    /// Its first place: the body and the index in its scan.
    place: (BodyId, usize),
    context: &'a Context,
}

/// The other context of rule 2 that reaches what `first` finds a place of,
/// other than the unmaskable context `own`: the thread's, else the handler
/// context's, else that of the other unmaskable contexts in the order of
/// `Mir::handlers` (ADR 0093).
fn other_context<'a>(
    mir: &Mir,
    thread: &'a Context,
    handler: &'a Context,
    unmaskable: &'a [(BodyId, Context)],
    own: usize,
    first: impl Fn(&Context) -> Option<(BodyId, usize)>,
) -> Option<Other<'a>> {
    if let Some(place) = first(thread) {
        return Some(Other {
            name: "the thread context".to_string(),
            place,
            context: thread,
        });
    }
    if let Some(place) = first(handler) {
        let root = handler.root(place.0).unwrap_or(place.0);
        return Some(Other {
            name: format!("the handler `{}`", shown(&mir.body(root).name)),
            place,
            context: handler,
        });
    }
    unmaskable
        .iter()
        .enumerate()
        .filter(|&(j, _)| j != own)
        .find_map(|(_, (root, context))| {
            first(context).map(|place| Other {
                name: format!("the unmaskable handler `{}`", shown(&mir.body(*root).name)),
                place,
                context,
            })
        })
}

/// Rule 2 of ADR 0093: a place in the context of an unmaskable handler, that
/// names a static another context also reaches. `critical` changes nothing.
/// A place that two unmaskable handlers reach is reported for the first.
fn unmaskable_statics(
    mir: &Mir,
    scans: &[Scan],
    thread: &Context,
    handler: &Context,
    unmaskable: &[(BodyId, Context)],
    diags: &mut Diagnostics,
) {
    for (id, _) in mir.bodies() {
        // The first unmaskable handler that reaches the body owns its places.
        let Some(i) = unmaskable
            .iter()
            .position(|(_, context)| context.reach.reaches(id))
        else {
            continue;
        };
        let (root, context) = &unmaskable[i];
        let by = format!("the unmaskable handler `{}`", shown(&mir.body(*root).name));
        for (p, place) in scans[id.0 as usize].places.iter().enumerate() {
            let Named::Static(stat) = place.named else {
                continue;
            };
            let stat = stat.def();
            // A shared static has a place in another context.
            let Some(other) = other_context(mir, thread, handler, unmaskable, i, |context| {
                context.first.get(&stat.id).copied()
            }) else {
                continue;
            };
            let mut diag = Diagnostic::error(
                UNMASKABLE,
                format!(
                    "`{}` is reached from {by}, and {} reaches it too",
                    shown(&stat.name),
                    other.name
                ),
            );
            let same = other.place == (id, p);
            diag = if same {
                diag.label(
                    place.site.span,
                    format!(
                        "reached here by this handler and by {}; `critical` does not stop \
                         this handler",
                        other.name
                    ),
                )
            } else {
                diag.label(
                    place.site.span,
                    "reached here; `critical` does not stop this handler",
                )
            };
            diag = path_labels(diag, &context.reach, scans, id, "");
            if !same {
                let (body, q) = other.place;
                let there = &scans[body.0 as usize].places[q];
                diag = diag.label(there.site.span, format!("{} reaches it here", other.name));
            }
            diags.push(diag.help(
                "give the handler a static of its own, or reach this memory through a pointer \
                 from a `const`",
            ));
        }
    }
}

/// Rule 2 of ADR 0115: a place in the context of an unmaskable handler, that
/// names a peripheral another context also reaches, by its own name or by one
/// whose bytes meet it. `critical` changes nothing.
fn unmaskable_peripherals(
    mir: &Mir,
    scans: &[Scan],
    thread: &Context,
    handler: &Context,
    unmaskable: &[(BodyId, Context)],
    diags: &mut Diagnostics,
) {
    for (id, _) in mir.bodies() {
        let Some(i) = unmaskable
            .iter()
            .position(|(_, context)| context.reach.reaches(id))
        else {
            continue;
        };
        let (root, context) = &unmaskable[i];
        let by = format!("the unmaskable handler `{}`", shown(&mir.body(*root).name));
        for (p, place) in scans[id.0 as usize].places.iter().enumerate() {
            let Named::Peripheral { index, peripheral } = place.named else {
                continue;
            };
            // A shared peripheral has a place in another context.
            let Some(other) = other_context(mir, thread, handler, unmaskable, i, |context| {
                context.near.get(index).copied().flatten()
            }) else {
                continue;
            };
            let name = shown(&peripheral.def().name);
            let (body, q) = other.place;
            let there = &scans[body.0 as usize].places[q];
            let near = meeting_of(peripheral, there);
            let itself = other.context.named.get(index).copied().unwrap_or(false);
            let reaches = match near {
                Some(x) if !itself => format!("`{}`, whose bytes meet it", shown(&x.def().name)),
                _ => "it too".to_owned(),
            };
            let mut diag = Diagnostic::error(
                UNMASKABLE_PERIPHERAL,
                format!(
                    "`{name}` is reached from {by}, and {} reaches {reaches}",
                    other.name
                ),
            );
            let same = other.place == (id, p);
            diag = if same {
                diag.label(
                    place.site.span,
                    format!(
                        "reached here by this handler and by {}; `critical` does not stop \
                         this handler",
                        other.name
                    ),
                )
            } else {
                diag.label(
                    place.site.span,
                    "reached here; `critical` does not stop this handler",
                )
            };
            diag = path_labels(diag, &context.reach, scans, id, "");
            if !same {
                diag = diag.label(there.site.span, reaches_here(&other.name, &name, near));
            }
            diags.push(diag.help(
                "leave the device to this handler, or reach it through a pointer from a `const`",
            ));
        }
    }
}
