//! The rule of shared statics (ADR 0078, 0093). A `static mut` that the thread
//! context and the handler context both reach may be reached by the thread
//! only inside `critical` (rule 1); one that an unmaskable handler reaches may
//! be reached by no other context, `critical` or not (rule 2). The contexts
//! are read off the call graph MIR shows: `Statement::Call` and the checks
//! that call the panic function, nothing else.
//!
//! Each body is scanned once for its places (the operands and `sym` operands
//! that name a `static mut`) and its calls, each with whether some path
//! reaches it outside every `critical` block. The searches over the call
//! graph then work on those scans alone.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use crate::diag::{Diagnostic, Diagnostics};
use crate::mir::{
    AsmSym, Body, BodyId, Const, Mir, Operand, Statement, StaticId, StaticRef, Terminator,
};
use crate::span::Span;
use crate::target::VectorKind;

const UNPROTECTED: &str = "analysis::unprotected-static";
const UNMASKABLE: &str = "analysis::unmaskable-static";

/// Reports the errors of the rule over the whole program.
pub fn check(mir: &Mir, diags: &mut Diagnostics) {
    let scans: Vec<Scan> = mir
        .bodies()
        .map(|(_, body)| Scan::of(body, mir.panic))
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
    let unmaskable = roots(VectorKind::Unmaskable);
    let thread = Context::of(mir, &scans, &[mir.entry]);
    let handler = Context::of(mir, &scans, &maskable);
    unprotected(mir, &scans, &thread, &handler, &maskable, diags);
    unmaskable_statics(mir, &scans, &thread, &handler, &unmaskable, diags);
}

/// Where something is in a body.
#[derive(Clone, Copy)]
struct Site {
    span: Span,
    /// Some path from the start of the body reaches it outside every
    /// `critical` block (ADR 0093, "Inside or outside `critical`").
    outside: bool,
}

/// A statement or terminator that names a `static mut`.
struct Place {
    site: Site,
    stat: StaticRef,
}

/// A call MIR shows: a `Statement::Call`, or a check that calls the panic
/// function.
struct Call {
    site: Site,
    callee: BodyId,
    check: bool,
}

/// What one body holds, in the order of its blocks, then its statements.
#[derive(Default)]
struct Scan {
    places: Vec<Place>,
    calls: Vec<Call>,
}

/// What a statement or terminator names or calls.
struct Item {
    span: Span,
    statics: Vec<StaticRef>,
    call: Option<(BodyId, bool)>,
    outside: bool,
}

impl Scan {
    /// The places and calls in the blocks reachable from the start of `body`,
    /// by a forward walk over pairs of block and depth of `critical` blocks.
    /// A body that is external, or run only at compile time, holds none.
    fn of(body: &Body, panic: BodyId) -> Scan {
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
                let mut statics = Vec::new();
                statement.visit_operands(&mut |operand| names(operand, &mut statics));
                if let Some(asm) = statement.asm() {
                    syms(asm.syms(), &mut statics);
                }
                note(&mut items, (index, at), span, depth, statics, call);
                match statement {
                    Statement::CriticalEnter(_) => depth += 1,
                    Statement::CriticalExit(_) => depth = depth.saturating_sub(1),
                    Statement::Assign(..)
                    | Statement::Call { .. }
                    | Statement::InlineAsm(_)
                    | Statement::New(..) => {}
                }
            }
            let terminator = &block.terminator;
            let call = match terminator {
                Terminator::Panic(..) => Some((panic, true)),
                _ => None,
            };
            let mut statics = Vec::new();
            terminator.visit_operands(&mut |operand| names(operand, &mut statics));
            if let Some(asm) = terminator.asm() {
                syms(asm.syms(), &mut statics);
            }
            let at = block.statements.len();
            note(
                &mut items,
                (index, at),
                block.terminator_span,
                depth,
                statics,
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
            for stat in item.statics {
                scan.places.push(Place { site, stat });
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
    mut statics: Vec<StaticRef>,
    call: Option<(BodyId, bool)>,
) {
    if statics.is_empty() && call.is_none() {
        return;
    }
    statics.sort_by_key(|stat| stat.def().id.0);
    statics.dedup();
    let item = items.entry(key).or_insert(Item {
        span,
        statics,
        call,
        outside: false,
    });
    item.outside |= depth == 0;
}

/// Adds the `static mut` an operand names, if it names one.
fn names(operand: &Operand, statics: &mut Vec<StaticRef>) {
    if let Operand::Const(Const::Static(stat)) = operand
        && stat.def().mutable
    {
        statics.push(*stat);
    }
}

/// Adds the `static mut`s that `sym` operands name.
fn syms(syms: impl Iterator<Item = AsmSym>, statics: &mut Vec<StaticRef>) {
    for sym in syms {
        match sym {
            AsmSym::Static(stat) if stat.def().mutable => statics.push(stat),
            AsmSym::Static(_) | AsmSym::Body(_) => {}
        }
    }
}

/// How a search reached a body: from the root it started at, through the
/// call at `via` in the body it came from; none for a root.
#[derive(Clone, Copy)]
struct Found {
    root: BodyId,
    via: Option<(BodyId, usize)>,
}

/// The bodies a context reaches, each with the shortest path found.
struct Reach {
    found: Vec<Option<Found>>,
}

impl Reach {
    /// A breadth-first search from `roots`, in order, over the calls of the
    /// scans in order. With `outside_only` it follows the calls that some
    /// path reaches outside `critical`, and no others.
    fn search(scans: &[Scan], roots: &[BodyId], outside_only: bool) -> Reach {
        let mut found: Vec<Option<Found>> = vec![None; scans.len()];
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
            for (k, call) in scans[body.0 as usize].calls.iter().enumerate() {
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

    fn reaches(&self, body: BodyId) -> bool {
        self.found[body.0 as usize].is_some()
    }

    /// The calls from a root to `body`, in order, as the search followed them.
    fn path<'a>(&self, scans: &'a [Scan], body: BodyId) -> Vec<&'a Call> {
        let mut path = Vec::new();
        let mut at = body;
        while let Some(Found {
            via: Some((from, k)),
            ..
        }) = self.found[at.0 as usize]
        {
            path.push(&scans[from.0 as usize].calls[k]);
            at = from;
        }
        path.reverse();
        path
    }
}

/// What a context reaches, and its first place of each static: the place in
/// the lowest body, then block, then statement (ADR 0093).
struct Context {
    reach: Reach,
    first: HashMap<StaticId, (BodyId, usize)>,
}

impl Context {
    fn of(mir: &Mir, scans: &[Scan], roots: &[BodyId]) -> Context {
        let reach = Reach::search(scans, roots, false);
        let mut first: HashMap<StaticId, (BodyId, usize)> = HashMap::new();
        for (id, _) in mir.bodies() {
            if !reach.reaches(id) {
                continue;
            }
            for (p, place) in scans[id.0 as usize].places.iter().enumerate() {
                first.entry(place.stat.def().id).or_insert((id, p));
            }
        }
        Context { reach, first }
    }

    /// The handler the search found `body` from.
    fn root(&self, body: BodyId) -> Option<BodyId> {
        self.reach.found[body.0 as usize].map(|found| found.root)
    }
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
    let open = Reach::search(scans, &[mir.entry], true);
    for (id, _) in mir.bodies() {
        if !open.reaches(id) {
            continue;
        }
        for (p, place) in scans[id.0 as usize].places.iter().enumerate() {
            let stat = place.stat.def();
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
            for call in open.path(scans, id) {
                diag = diag.label(
                    call.site.span,
                    if call.check {
                        "this check may call the panic function, outside `critical`"
                    } else {
                        "called here, outside `critical`"
                    },
                );
            }
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

/// Another context that reaches a static, as rule 2 names it.
struct Other {
    /// How the context is named: "the thread context", "the handler `h`".
    name: String,
    /// Its first place of the static: the body and the index in its scan.
    place: (BodyId, usize),
}

/// The other context of rule 2 that reaches `stat`, other than the unmaskable
/// context `own`: the thread's first place, else the handler context's, else
/// that of the other unmaskable contexts in the order of `Mir::handlers`
/// (ADR 0093).
fn other_context(
    mir: &Mir,
    thread: &Context,
    handler: &Context,
    unmaskable: &[(BodyId, Context)],
    own: usize,
    stat: StaticId,
) -> Option<Other> {
    if let Some(&place) = thread.first.get(&stat) {
        return Some(Other {
            name: "the thread context".to_string(),
            place,
        });
    }
    if let Some(&place) = handler.first.get(&stat) {
        let root = handler.root(place.0).unwrap_or(place.0);
        return Some(Other {
            name: format!("the handler `{}`", shown(&mir.body(root).name)),
            place,
        });
    }
    unmaskable
        .iter()
        .enumerate()
        .filter(|&(j, _)| j != own)
        .find_map(|(_, (root, context))| {
            context.first.get(&stat).map(|&place| Other {
                name: format!("the unmaskable handler `{}`", shown(&mir.body(*root).name)),
                place,
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
    roots: &[BodyId],
    diags: &mut Diagnostics,
) {
    let contexts: Vec<(BodyId, Context)> = roots
        .iter()
        .map(|&root| (root, Context::of(mir, scans, &[root])))
        .collect();
    for (id, _) in mir.bodies() {
        // The first unmaskable handler that reaches the body owns its places.
        let Some(i) = contexts
            .iter()
            .position(|(_, context)| context.reach.reaches(id))
        else {
            continue;
        };
        let (root, context) = &contexts[i];
        let by = format!("the unmaskable handler `{}`", shown(&mir.body(*root).name));
        for (p, place) in scans[id.0 as usize].places.iter().enumerate() {
            let stat = place.stat.def();
            // A shared static has a place in another context.
            let Some(other) = other_context(mir, thread, handler, &contexts, i, stat.id) else {
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
            for call in context.reach.path(scans, id) {
                diag = diag.label(
                    call.site.span,
                    if call.check {
                        "this check may call the panic function"
                    } else {
                        "called here"
                    },
                );
            }
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

/// A symbol as diagnostics show it: the dots of a path written `::`
/// (ADR 0051).
fn shown(symbol: &str) -> String {
    symbol.replace('.', "::")
}
