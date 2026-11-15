//! Where a reference may be (ADR 0094, 0105, 0118): only the type of a
//! parameter or of a `let` that is not `mut`, the top of a return type, and
//! the value of an expression that is an argument, an initializer, the operand
//! of `*` or `as`, the base of a field, an index or a range in an index, the
//! receiver of a method call (ADR 0114), the value a function returns, or
//! yields one of those. A reference held anywhere else is
//! `ty::reference-position`. Which references a function may return is the
//! signature's rule (`ty::reference-return`, `Checker::reference_return`).

use std::collections::HashSet;

use crate::diag::Diagnostic;
use crate::hir::{Block, ExprId, ExprKind, Fn, NeverPosition, ReferencePosition, Ty};
use crate::query::Queries;
use crate::span::Span;

use super::{Checker, memory_position};

/// What `ty::reference-position` suggests instead of a reference written or
/// made in a position of memory.
const MEMORY_HELP: &str = "a reference is held only by a parameter, a `let` without `mut` or the result of a call, and lent to calls (ADR 0094, 0105, 0118); to keep an address, use a raw pointer, `*const T` or `*mut T`";

/// How the diagnostic names `position`.
fn describe(position: ReferencePosition) -> String {
    match position {
        ReferencePosition::Memory(position) => memory_position(position),
        ReferencePosition::MutParameter => "the type of a `mut` parameter".to_owned(),
        ReferencePosition::MutVariable => "the type of a `mut` variable".to_owned(),
    }
}

/// What `ty::reference-position` suggests instead for `position`.
fn help(position: ReferencePosition) -> &'static str {
    match position {
        ReferencePosition::Memory(NeverPosition::Static | NeverPosition::Const) => {
            "a static or a constant holds a value computed at compile time, which is never a reference (ADR 0094, 0105); keep a raw pointer, `*const T` or `*mut T`, instead"
        }
        ReferencePosition::Memory(NeverPosition::LayoutQuery(_)) => {
            "a reference has no layout of its own (ADR 0105); it is laid out as a raw pointer is, so ask for `*const T`"
        }
        ReferencePosition::Memory(_) => MEMORY_HELP,
        ReferencePosition::MutParameter | ReferencePosition::MutVariable => {
            "a reference is bound once (ADR 0094, 0105); declare a new `let` or parameter without `mut` instead of changing this one"
        }
    }
}

/// `ty::reference-position`: "a reference cannot be `what`".
fn diagnostic(what: &str) -> Diagnostic {
    Diagnostic::error(
        "ty::reference-position",
        format!("a reference cannot be {what}"),
    )
}

/// Reports each reference type written where none may be, which HIR lowering
/// recorded at the outermost reference of the type (ADR 0105): the type is
/// reported once, whatever it holds.
pub fn report_reference_types(q: &mut Queries) {
    let hir = q.hir;
    for found in &hir.reference_types {
        let ty = found.ty;
        q.diags.push(
            diagnostic(&describe(found.position))
                .label(found.span, format!("`{ty}` is a reference"))
                .help(help(found.position)),
        );
    }
}

/// Whether `ty` is a reference type.
pub(super) fn is_reference(ty: Ty) -> bool {
    matches!(ty, Ty::Ref(_))
}

/// The `break`s in `block` that leave it: those whose loop is not inside it.
/// Erebos has no labels, so a `break` targets the innermost loop around it,
/// and when that loop is outside the block, the `break` crosses the block's
/// end. A `break` that leaves a `critical` block nested in `block` too leaves
/// the innermost one first, and is found for each block it leaves.
fn breaks_out_of(block: &Block) -> Vec<ExprId> {
    let mut loops: HashSet<ExprId> = HashSet::new();
    let mut breaks: Vec<(ExprId, ExprId)> = Vec::new();
    block.walk(&mut |expr| match &expr.kind {
        ExprKind::Loop(_) | ExprKind::While { .. } => {
            loops.insert(expr.id);
        }
        ExprKind::Break { target, .. } => breaks.push((expr.id, *target)),
        _ => {}
    });
    breaks
        .into_iter()
        .filter(|(_, target)| !loops.contains(target))
        .map(|(id, _)| id)
        .collect()
}

impl Checker<'_, '_> {
    /// The signature rule of a function that returns a reference (ADR 0118,
    /// "The rule, read off the signature"): `ty::reference-return`, at the
    /// return type, with a label at each parameter of a reference type, for
    /// two or more of them; for a `&mut` result of one `&` parameter; for a
    /// `&mut` or volatile result with none. The parameters that are not
    /// references count for nothing. A volatile result of one reference
    /// parameter is not judged by its volatility: a `Ref` that is not that of
    /// its tied parameter fails to type, as any reference does (ADR 0110).
    ///
    /// Judged for a function written with a return type, `extern fn`,
    /// `#[export]` and `#[naked]` included; the `Fn` that holds a constant's
    /// value or a `comptime` expression is not (its type is not a return
    /// type), and a signature with an `{error}` type in it was reported.
    pub(super) fn reference_return(&mut self, f: &Fn) {
        let (Some(at), None) = (f.ret_span, f.comptime) else {
            return;
        };
        let Ty::Ref(result) = self.ret else {
            return;
        };
        let mut tied: Vec<(Span, Ty)> = Vec::new();
        for param in f.params() {
            match self.norm(param.ty) {
                Ty::Error => return,
                ty @ Ty::Ref(_) => tied.push((param.span, ty)),
                _ => {}
            }
        }
        let ret = self.ret;
        let diag = match tied.as_slice() {
            [] if result.mutable || result.volatile => {
                let what = if result.mutable {
                    "a `&mut`"
                } else {
                    "a volatile reference"
                };
                Diagnostic::error(
                    "ty::reference-return",
                    format!("cannot return {what} when no parameter is a reference"),
                )
                .label(at, format!("returns `{ret}`, with nothing to be tied to"))
                .help("with no reference parameter, a returned reference borrows read-only static data, which is never `&mut` or volatile: return a raw pointer")
            }
            [(param, Ty::Ref(lender))] if result.mutable && !lender.mutable => Diagnostic::error(
                "ty::reference-return",
                "cannot return a `&mut` tied to a `&` parameter",
            )
            .label(at, format!("returns `{ret}`"))
            .label(
                *param,
                format!("`{}` lends only a shared reference", Ty::Ref(*lender)),
            )
            .help("make the parameter `&mut`, or return a `&`"),
            [_, _, ..] => {
                let mut diag = Diagnostic::error(
                    "ty::reference-return",
                    format!(
                        "cannot return a reference when {} parameters are references",
                        tied.len()
                    ),
                )
                .label(at, format!("returns `{ret}`, tied to which one?"));
                for (param, ty) in &tied {
                    diag = diag.label(*param, format!("`{ty}` is a reference parameter"));
                }
                diag.help("a returned reference is tied to the one reference parameter: pass one reference, or return an index or a copy")
            }
            _ => return,
        };
        self.q.diags.push(diag);
    }

    /// Reports, in the body of a function, the expressions that make a type
    /// holding a reference below its top, at the innermost one, and the
    /// values of a `critical` block and of a `comptime` expression that are
    /// references (ADR 0105), and the values of a `break` that are references
    /// and leave a `critical` block (ADR 0110), and the values of a `return`
    /// that are references inside one (ADR 0118). The expressions above an
    /// expression reported are not: their types come from it. A type that was
    /// written is reported at its text (`report_reference_types`).
    pub(super) fn report_made_references(&mut self, body: &Block) {
        let mut found: Vec<Diagnostic> = Vec::new();
        // The `break`s, found at the `critical` block they leave, which the
        // walk reaches before them (ADR 0110).
        let mut leaving: HashSet<ExprId> = HashSet::new();
        // The `return`s inside a `critical` block, found the same way. A
        // `return` leaves every block around it, and is one error however
        // many (ADR 0118).
        let mut returning: HashSet<ExprId> = HashSet::new();
        let tables = &self.q.tables;
        body.walk(&mut |expr| {
            if let ExprKind::Critical(block) = &expr.kind {
                leaving.extend(breaks_out_of(block));
                block.walk(&mut |inner| {
                    if matches!(inner.kind, ExprKind::Return(Some(_))) {
                        returning.insert(inner.id);
                    }
                });
            }
            let ty = tables.expr_ty(expr.id);
            // Its own error was reported, as `&mut` of a variable that is
            // not `mut`.
            if ty == Ty::Error {
                return;
            }
            let diag = match &expr.kind {
                // `[&a, &b]`, `[&a; 2]`
                ExprKind::Array(_) | ExprKind::Repeat(..) => match ty {
                    Ty::Array(elem, _) if is_reference(*elem) => {
                        diagnostic("an array element").label(
                            expr.span,
                            format!("makes `{ty}`, an array of references"),
                        )
                    }
                    _ => return,
                },
                // `&r`, `&mut r`, `&&x`
                ExprKind::Ref { place, .. } if is_reference(tables.expr_ty(place.id)) => {
                    diagnostic("the referent of a reference").label(
                        expr.span,
                        format!("makes `{ty}`, a reference to a reference"),
                    )
                }
                // `&raw const r`
                ExprKind::AddrOf { place, .. } if is_reference(tables.expr_ty(place.id)) => {
                    diagnostic("the pointee of a pointer").label(
                        expr.span,
                        format!("makes `{ty}`, a pointer to a reference"),
                    )
                }
                ExprKind::Critical(block) if is_reference(ty) => {
                    let at = block.tail.as_ref().map_or(expr.span, |tail| tail.span);
                    found.push(
                        diagnostic("the value of a `critical` block")
                            .label(at, format!("`{ty}` would outlive the critical section"))
                            .help("a reference must not outlive the protection it was taken in (ADR 0094, 0105); use the value inside the block, or end the block with a copy of what it refers to"),
                    );
                    return;
                }
                // `loop { critical { break &x; } }`, with the `break` inside
                // the block and its loop outside it
                ExprKind::Break {
                    value: Some(value), ..
                } if leaving.contains(&expr.id) && is_reference(tables.expr_ty(value.id)) => {
                    let ty = tables.expr_ty(value.id);
                    found.push(
                        diagnostic("the value of a `break` out of a `critical` block")
                            .label(value.span, format!("`{ty}` would outlive the critical section"))
                            .help("a reference must not outlive the protection it was taken in (ADR 0094, 0105, 0110); use the value inside the block, or break with a copy of what it refers to"),
                    );
                    return;
                }
                // `critical { return &x; }`
                ExprKind::Return(Some(value))
                    if returning.contains(&expr.id) && is_reference(tables.expr_ty(value.id)) =>
                {
                    let ty = tables.expr_ty(value.id);
                    found.push(
                        diagnostic("the value of a `return` out of a `critical` block")
                            .label(value.span, format!("`{ty}` would outlive the critical section"))
                            .help("a reference must not outlive the protection it was taken in (ADR 0094, 0105, 0110, 0118); use the value inside the block, or return a copy of what it refers to"),
                    );
                    return;
                }
                ExprKind::Comptime(_) if is_reference(ty) => {
                    found.push(
                        diagnostic("the value of a `comptime` expression")
                            .label(expr.span, format!("`{ty}` is a reference"))
                            .help("a value computed at compile time is data, never a reference (ADR 0094, 0105); compute the value, and borrow it where it is used"),
                    );
                    return;
                }
                _ => return,
            };
            found.push(diag.help(MEMORY_HELP));
        });
        for diag in found {
            self.q.diags.push(diag);
        }
    }
}
