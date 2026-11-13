//! The conflicts of a call with what its callee reaches (ADR 0111, 0112). A
//! borrow of a static or a peripheral that is in use during a call conflicts
//! with an access to an overlapping place in the callee, or in a function the
//! callee reaches, because any function can name a static: `bump(&mut COUNT)`
//! with `bump` writing `COUNT`.
//!
//! The borrow analysis (`borrows::facts`) tells, for each body, its accesses
//! rooted at a static or a peripheral and the borrows of that kind in use at
//! each of its calls. The walk of the call graph (`call_graph`) tells what a
//! callee reaches, along `Statement::Call` only: a check that may call the panic
//! function is not followed, since no borrow is used once it runs.

use std::collections::HashMap;

use super::bits::{reachable, shown};
use super::borrows::{self, Facts, Held, How, Touch};
use super::call_graph::{Call, Edges, Reach, Site};
use crate::diag::{Diagnostic, Diagnostics};
use crate::mir::{Body, BodyId, Mir, Statement};
use crate::span::Span;

const CONFLICT_IN_CALL: &str = "analysis::borrow-conflict-in-call";

/// Reports the calls during which a borrow of a static or a peripheral is in
/// use and whose callee reaches an access that conflicts with it.
pub fn check(mir: &Mir, diags: &mut Diagnostics) {
    over(&mir.bodies, diags);
}

/// `check` over the bodies of a program, in the order of their ids.
fn over(bodies: &[Body], diags: &mut Diagnostics) {
    let facts: Vec<Facts> = bodies
        .iter()
        .map(|body| {
            if runs(body) {
                borrows::facts(body)
            } else {
                Facts::default()
            }
        })
        .collect();
    let calls: Vec<Vec<Call>> = bodies.iter().map(calls_of).collect();
    // What each callee reaches, searched once however many calls lend it a
    // borrow.
    let mut reaches: HashMap<u32, Reach> = HashMap::new();
    for facts_of_body in &facts {
        for used in &facts_of_body.calls {
            let reach = reaches
                .entry(used.callee.0)
                .or_insert_with(|| Reach::search(&calls, &[used.callee], Edges::Calls, false));
            for held in &used.held {
                let Some((at, touch)) = first_conflict(held, reach, &facts) else {
                    continue;
                };
                let path = reach.path(&calls, at);
                diags.push(error(bodies, used.callee, used.span, held, &path, touch));
            }
        }
    }
}

/// Whether a body is part of the program: not external, not run only at
/// compile time (ADR 0029).
fn runs(body: &Body) -> bool {
    !body.external && !body.comptime && !body.blocks.is_empty()
}

/// The `Statement::Call`s of the blocks `bb0` reaches, in the order of the
/// blocks and the statements, as the check of shared statics lists them. No
/// `critical` depth is kept: this search follows every call.
fn calls_of(body: &Body) -> Vec<Call> {
    if !runs(body) {
        return Vec::new();
    }
    let mut calls = Vec::new();
    for block in reachable(body).0 {
        let data = &body.blocks[block];
        for (at, statement) in data.statements.iter().enumerate() {
            if let Statement::Call { callee, .. } = statement {
                calls.push(Call {
                    site: Site {
                        span: (data.spans.get(at).copied()).unwrap_or(data.terminator_span),
                        outside: true,
                    },
                    callee: *callee,
                    check: false,
                });
            }
        }
    }
    calls
}

/// The first access, in the order the search found the bodies and then the
/// order of the accesses of each, that conflicts with the borrow.
fn first_conflict<'a>(
    held: &Held,
    reach: &Reach,
    facts: &'a [Facts],
) -> Option<(BodyId, &'a Touch)> {
    reach.order().iter().find_map(|&body| {
        (facts[body.0 as usize].accesses.iter())
            .find(|touch| held.conflicts(touch.how, &touch.path))
            .map(|touch| (body, touch))
    })
}

/// What the callee does, for the message: "writes `COUNT`".
fn does(how: How, name: &str) -> String {
    match how {
        How::Use => format!("reads {name}"),
        How::RawConst | How::RawMut => format!("takes the address of {name}"),
        How::Borrow | How::Convert | How::ConvertSlice => format!("borrows {name}"),
        How::BorrowMut | How::Lend | How::ConvertSliceMut => {
            format!("borrows {name} as mutable")
        }
        How::Assign => format!("writes {name}"),
        How::Move => format!("moves {name}"),
        How::End => format!("ends {name}"),
        How::Sym => format!("passes {name} to `asm` as `sym`"),
    }
}

/// What happens to the place at the access, for its label: "is written
/// here".
fn happens(how: How) -> &'static str {
    match how {
        How::Use => "is read here",
        How::RawConst => "has its address taken (`&raw const`) here",
        How::RawMut => "has its address taken (`&raw mut`) here",
        How::Borrow | How::Convert | How::ConvertSlice => "is borrowed here",
        How::BorrowMut | How::Lend | How::ConvertSliceMut => "is borrowed `&mut` here",
        How::Assign => "is written here",
        How::Move => "is moved here",
        How::End => "is ended here",
        How::Sym => "is passed as `sym` here",
    }
}

/// The error of one call and one borrow in use during it.
fn error(
    bodies: &[Body],
    callee: BodyId,
    call: Span,
    held: &Held,
    path: &[&Call],
    touch: &Touch,
) -> Diagnostic {
    let callee_name = shown(&bodies[callee.0 as usize].name);
    let same = touch.name == held.name;
    let it = if same { "it" } else { touch.name.as_str() };
    let mut diag = Diagnostic::error(
        CONFLICT_IN_CALL,
        format!(
            "{} is borrowed during this call, and the callee {}",
            held.name,
            does(touch.how, it)
        ),
    )
    .label(
        call,
        format!(
            "`{callee_name}` reaches {} while {} is borrowed",
            touch.name,
            if same { "it" } else { held.name.as_str() }
        ),
    )
    .label(
        held.span,
        if held.how.mutable() {
            format!("{} is borrowed `&mut` here", held.name)
        } else {
            format!("{} is borrowed here", held.name)
        },
    );
    if let Some((span, round)) = held.later {
        diag = diag.label(
            span,
            if round {
                "the borrow is used in the next iteration of the loop"
            } else {
                "the borrow is used later here"
            },
        );
    }
    for step in path {
        diag = diag.label(
            step.site.span,
            format!(
                "`{}` is called here",
                shown(&bodies[step.callee.0 as usize].name)
            ),
        );
    }
    diag.label(touch.span, format!("{} {}", touch.name, happens(touch.how)))
        .help(format!(
            "pass the reference on instead of naming {} in the callee, or end the borrow \
             before the call",
            held.name
        ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::borrows::{
        address, asm_sym, block, body, decl, deref, device_address, local, mut_ref, mutable,
        peripheral, raw, set, shared, span, static_, through,
    };
    use crate::mir::{Operand, PanicCause, PeripheralRef, StaticRef, Terminator, Ty};

    // The bodies of each program: 0 holds the borrows, the others are
    // callees. The locals of every body are `_1`, the address of a static or
    // a peripheral, and `_2`, a reference.

    /// `callee(move _2)`: the reference is lent.
    fn lend_to(callee: u32) -> Statement {
        Statement::Call {
            dest: None,
            callee: BodyId(callee),
            args: vec![Operand::Move(local(2), span(900))],
        }
    }

    /// `callee()`.
    fn call_to(callee: u32) -> Statement {
        Statement::Call {
            dest: None,
            callee: BodyId(callee),
            args: Vec::new(),
        }
    }

    fn function(name: &str, address_ty: Ty, reference: Ty, statements: Vec<Statement>) -> Body {
        let mut function = body(
            vec![decl(address_ty, None), decl(reference, Some("r"))],
            vec![block(statements, Terminator::Return)],
        );
        function.name = name.to_owned();
        function
    }

    /// A function over statics.
    fn plain(name: &str, statements: Vec<Statement>) -> Body {
        function(name, raw(), mut_ref(), statements)
    }

    /// `holder`: `r = &mut COUNT; callee(r)`.
    fn lender(count: StaticRef, callee: u32) -> Body {
        plain(
            "holder",
            vec![address(1, count), mutable(2, deref(1)), lend_to(callee)],
        )
    }

    /// A function that writes `s` once.
    fn writer(name: &str, s: StaticRef) -> Body {
        plain(name, vec![address(1, s), set(deref(1))])
    }

    /// A function that ends in a check.
    fn checked(name: &str) -> Body {
        let mut checked = plain(name, Vec::new());
        checked.blocks[0].terminator = Terminator::Panic(PanicCause::IndexOutOfBounds, None);
        checked
    }

    /// Each error as its message and the texts of its labels.
    fn errors(bodies: &[Body]) -> Vec<(String, Vec<String>)> {
        let mut diags = Diagnostics::default();
        over(bodies, &mut diags);
        diags
            .into_vec()
            .into_iter()
            .map(|diag| {
                assert_eq!(diag.code, CONFLICT_IN_CALL);
                let labels = diag.labels.iter().map(|l| l.message.clone()).collect();
                (diag.message, labels)
            })
            .collect()
    }

    fn only(bodies: &[Body]) -> (String, Vec<String>) {
        let mut found = errors(bodies);
        assert_eq!(found.len(), 1, "{found:?}");
        found.remove(0)
    }

    #[test]
    fn a_conflict_two_calls_down_labels_the_call_the_path_and_the_access() {
        let count = static_(0, "m.COUNT", true);
        let bodies = [
            lender(count, 1),
            plain("m.down", vec![call_to(2)]),
            writer("m.bottom", count),
        ];
        let (message, labels) = only(&bodies);
        assert_eq!(
            message,
            "`m::COUNT` is borrowed during this call, and the callee writes it"
        );
        assert_eq!(
            labels,
            [
                "`m::down` reaches `m::COUNT` while it is borrowed",
                "`m::COUNT` is borrowed `&mut` here",
                "`m::bottom` is called here",
                "`m::COUNT` is written here",
            ]
        );
        // A callee that writes another static, or only uses the reference
        // it is given, conflicts with nothing.
        let other = static_(1, "OTHER", true);
        assert_eq!(errors(&[lender(count, 1), writer("w", other)]), []);
        let through_it = plain("t", vec![set(deref(2))]);
        assert_eq!(errors(&[lender(count, 1), through_it]), []);
    }

    #[test]
    fn a_sym_in_the_callee_is_a_write_of_a_static_mut_and_nothing_for_a_static() {
        let count = static_(0, "COUNT", true);
        let bodies = [lender(count, 1), plain("f", vec![asm_sym(count)])];
        let (message, labels) = only(&bodies);
        assert_eq!(
            message,
            "`COUNT` is borrowed during this call, and the callee passes it to `asm` as `sym`"
        );
        assert_eq!(labels[2], "`COUNT` is passed as `sym` here");
        // `f(&TABLE)` with `f` passing `sym TABLE`, `TABLE` a `static`.
        let table = static_(1, "TABLE", false);
        let lends_table = plain(
            "holder",
            vec![address(1, table), shared(2, deref(1)), lend_to(1)],
        );
        let passes = plain("f", vec![asm_sym(table)]);
        assert_eq!(errors(&[lends_table, passes]), []);
    }

    #[test]
    fn a_call_to_an_external_body_reaches_nothing() {
        let count = static_(0, "COUNT", true);
        let mut outside = plain("outside", Vec::new());
        outside.external = true;
        outside.blocks.clear();
        assert_eq!(errors(&[lender(count, 1), outside]), []);
    }

    #[test]
    fn a_check_is_not_followed_and_a_call_of_the_panic_function_by_name_is() {
        let count = static_(0, "COUNT", true);
        // The callee ends in a check, and the panic function (body 2) writes
        // the static: `over` follows no `Terminator::Panic`.
        let bodies = [lender(count, 1), checked("f"), writer("on_panic", count)];
        assert_eq!(errors(&bodies), []);
        // The same one call down, through a function that returns `!`, called
        // as any function, and has a check.
        let mut diverges = checked("fail");
        diverges.ret = Ty::Never;
        let bodies = [
            lender(count, 1),
            plain("f", vec![call_to(2)]),
            diverges,
            writer("on_panic", count),
        ];
        assert_eq!(errors(&bodies), []);
        // A `Statement::Call` of the panic function, written by name, is a
        // call like any other.
        let bodies = [
            lender(count, 1),
            plain("f", vec![call_to(2)]),
            writer("on_panic", count),
        ];
        assert_eq!(errors(&bodies).len(), 1);
    }

    #[test]
    fn a_callee_given_a_borrow_by_two_calls_is_searched_once_and_both_are_reported() {
        let count = static_(0, "COUNT", true);
        let holder = plain(
            "holder",
            vec![
                address(1, count),
                mutable(2, deref(1)),
                lend_to(1),
                mutable(2, deref(1)),
                lend_to(1),
            ],
        );
        let found = errors(&[holder, writer("writes", count)]);
        assert_eq!(found.len(), 2, "{found:?}");
        // Each call is one error, and both reach the same access.
        assert_eq!(found[0], found[1]);
    }

    #[test]
    fn a_borrow_held_across_a_call_is_told_with_its_later_use() {
        let count = static_(0, "COUNT", true);
        let holder = plain(
            "holder",
            vec![
                address(1, count),
                mutable(2, deref(1)),
                call_to(1),
                set(deref(2)),
            ],
        );
        let (_, labels) = only(&[holder, writer("refill", count)]);
        assert_eq!(labels[2], "the borrow is used later here");
        // Once the reference is dead there is no borrow in use.
        let released = plain(
            "holder",
            vec![
                address(1, count),
                mutable(2, deref(1)),
                set(deref(2)),
                call_to(1),
            ],
        );
        assert_eq!(errors(&[released, writer("refill", count)]), []);
    }

    #[test]
    fn the_registers_of_a_peripheral_conflict_and_so_do_peripherals_whose_bytes_meet() {
        let uart = peripheral("main.UART0", 0x1000);
        let alias = peripheral("main.ALIAS", 0x1004);
        let apart = peripheral("main.OTHER", 0x2000);
        let device = |p: PeripheralRef, name: &str, statements: Vec<Statement>| {
            function(
                name,
                p.def().address_ty(),
                mut_ref(),
                [vec![device_address(1, p)], statements].concat(),
            )
        };
        let lends =
            |p: PeripheralRef| device(p, "holder", vec![mutable(2, through(1, 0)), lend_to(1)]);
        let writes = |p: PeripheralRef, register: u32, name: &str| {
            device(p, name, vec![set(through(1, register))])
        };
        // Another register of the same peripheral.
        let (message, _) = only(&[lends(uart), writes(uart, 1, "w")]);
        assert_eq!(
            message,
            "`main::UART0.ctrl` is borrowed during this call, and the callee writes \
             `main::UART0.data`"
        );
        // A peripheral declared over it, and one apart.
        assert_eq!(errors(&[lends(uart), writes(alias, 0, "w")]).len(), 1);
        assert_eq!(errors(&[lends(uart), writes(apart, 0, "w")]), []);
    }
}
