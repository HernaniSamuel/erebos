//! Values known at compile time, and the checks on them (ADR 0032): shift
//! amounts, divisors and array indices that are known are checked while
//! MIR is built, except in code that cannot run, such as the arms a
//! `match` on a known value never takes (ADR 0037).
//!
//! Computing a known value never reads memory, and reports nothing: a
//! failing operation is reported where it is built, once.

use crate::diag::Diagnostic;
use crate::eval;
use crate::hir::{self, LogicalOp};
use crate::mir::{BinOp, Ty, UnOp};
use crate::target;

use super::Builder;

/// The bits of an address on the target.
const ADDRESS_MASK: u64 = (1 << target::POINTER_BITS) - 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Known {
    /// The value, masked to its type; `bool` is 0 or 1, and a pointer
    /// made from an integer is its address.
    Value(u64),
    /// Not known at compile time.
    Unknown,
    /// Known, but computing it fails, which is reported where it fails:
    /// at an operation in this body, or at a constant it uses.
    Failed,
}

impl Builder<'_, '_> {
    /// The value of `expr` if it is known at compile time: built only from
    /// literals, constants, variants (ADR 0035, 0044), `comptime` expressions,
    /// layout queries, unary `-` and `!`, binary operators and `as`.
    pub(super) fn known(&mut self, expr: &hir::Expr) -> Known {
        if let Some(&known) = self.known.get(&expr.id) {
            return known;
        }
        let known = self.compute_known(expr);
        self.known.insert(expr.id, known);
        known
    }

    fn compute_known(&mut self, expr: &hir::Expr) -> Known {
        // No value of a bit-field struct is known, though MIR sees an
        // integer (ADR 0059 amending 0032): not a literal, a constant, a
        // `comptime` expression or a cast to one, and so neither a field
        // read from one nor a cast from one.
        if let hir::Ty::BitStruct(_) = self.q.tables.expr_ty(expr.id) {
            return Known::Unknown;
        }
        let ty = self.ty_of(expr);
        let mask = match ty {
            Ty::Int(int) => int.mask(),
            Ty::Bool => 1,
            // A slice is an aggregate (ADR 0031).
            Ty::Ptr(_) if ty.slice_elem().is_some() => return Known::Unknown,
            Ty::Ptr(_) => ADDRESS_MASK,
            // Aggregates, enums with payloads among them (ADR 0039), `()`
            // and `!` are never checked.
            _ => return Known::Unknown,
        };
        match &expr.kind {
            hir::ExprKind::Int(value) => Known::Value(value & mask),
            hir::ExprKind::Byte(value) => Known::Value(u64::from(*value)),
            hir::ExprKind::Bool(value) => Known::Value(u64::from(*value)),
            hir::ExprKind::Const(id) => {
                let value = self.const_value(*id);
                scalar(ty, value)
            }
            hir::ExprKind::Comptime(inner) => {
                let value = self.comptime_value(inner);
                scalar(ty, value)
            }
            // Only a variant of an enum without payloads, an integer; no
            // value of an enum with payloads is known (ADR 0039), and its
            // type is not one checked here.
            hir::ExprKind::Variant(id, index, _) => match self.discriminant(*id, *index) {
                Some(value) => Known::Value(value & mask),
                None => Known::Failed,
            },
            // As `E::V`, once type checking resolved it (ADR 0044).
            hir::ExprKind::ContextVariant(..) => {
                let (id, index) = self.q.tables.variant(expr.id);
                match self.discriminant(id, index) {
                    Some(value) => Known::Value(value & mask),
                    None => Known::Failed,
                }
            }
            hir::ExprKind::SizeOf(_) | hir::ExprKind::AlignOf(_) | hir::ExprKind::OffsetOf(..) => {
                Known::Value(self.layout_query(expr))
            }
            hir::ExprKind::Unary(op, operand) => {
                let operand_ty = self.ty_of(operand);
                match self.known(operand) {
                    Known::Value(value) => Known::Value(eval::unary(*op, operand_ty, value)),
                    other => other,
                }
            }
            hir::ExprKind::Binary(op, lhs, rhs, _) => self.known_binary(*op, lhs, rhs),
            hir::ExprKind::Logical(op, lhs, rhs) => {
                let decided_by = u64::from(*op == LogicalOp::Or);
                match self.known(lhs) {
                    // The right side never runs, so its failure does not
                    // matter, but it must be known for the whole to be.
                    Known::Value(l) if l == decided_by => match self.known(rhs) {
                        Known::Unknown => Known::Unknown,
                        _ => Known::Value(decided_by),
                    },
                    Known::Value(_) => self.known(rhs),
                    Known::Failed => Known::Failed,
                    Known::Unknown => match self.known(rhs) {
                        Known::Failed => Known::Failed,
                        _ => Known::Unknown,
                    },
                }
            }
            hir::ExprKind::Cast(operand, _, _) => {
                let (from, to) = (self.ty_of(operand), ty);
                match (self.known(operand), from, to) {
                    (Known::Value(value), Ty::Int(from), Ty::Int(to)) => {
                        Known::Value(eval::cast_int(value, from, to))
                    }
                    (Known::Value(value), Ty::Bool, Ty::Int(_)) => Known::Value(value),
                    // As the evaluator does (ADR 0019): an address is a
                    // `usize`, truncated or zero-extended.
                    (Known::Value(value), Ty::Int(_), Ty::Ptr(_)) => {
                        Known::Value(value & ADDRESS_MASK)
                    }
                    (Known::Value(value), Ty::Ptr(_), Ty::Int(to)) => {
                        Known::Value(value & to.mask())
                    }
                    (Known::Value(value), Ty::Ptr(_), Ty::Ptr(_)) => Known::Value(value),
                    (Known::Failed, _, _) => Known::Failed,
                    _ => Known::Unknown,
                }
            }
            _ => Known::Unknown,
        }
    }

    fn known_binary(&mut self, op: BinOp, lhs: &hir::Expr, rhs: &hir::Expr) -> Known {
        let (lhs_ty, rhs_ty) = (self.ty_of(lhs), self.ty_of(rhs));
        let (l, r) = (self.known(lhs), self.known(rhs));
        // An operation that fails fails whatever its other operand.
        match (op, r, lhs_ty) {
            (BinOp::Div | BinOp::Rem, Known::Value(0), _) => return Known::Failed,
            (BinOp::Shl | BinOp::Shr, Known::Value(amount), Ty::Int(int))
                if shift_out_of_range(int, rhs_ty, amount).is_some() =>
            {
                return Known::Failed;
            }
            _ => {}
        }
        match (l, r) {
            (Known::Failed, _) | (_, Known::Failed) => Known::Failed,
            (Known::Value(l), Known::Value(r)) if op.is_comparison() => {
                Known::Value(u64::from(eval::compare_ints(op, lhs_ty, l, r)))
            }
            (Known::Value(l), Known::Value(r)) => {
                Known::Value(eval::arith(op, lhs_ty, rhs_ty, l, r))
            }
            _ => Known::Unknown,
        }
    }

    /// The value of a condition, if it is known and checks are on: code it
    /// never runs is then built without checks.
    pub(super) fn known_condition(&mut self, cond: &hir::Expr) -> Option<bool> {
        if !self.checks {
            return None;
        }
        match self.known(cond) {
            Known::Value(value) => Some(value != 0),
            _ => None,
        }
    }

    /// The arm a `match` takes when its scrutinee is known and checks are
    /// on (ADR 0032, 0037): `Some(Some(i))`, or `Some(None)` if no arm
    /// matches, so that none can run; `None` if any arm may run. `values`
    /// gives, for each arm, what its alternatives match: the bits of a
    /// value, or `None` for every value.
    pub(super) fn known_arm(
        &mut self,
        scrutinee: &hir::Expr,
        values: &[Vec<Option<u64>>],
    ) -> Option<Option<usize>> {
        if !self.checks {
            return None;
        }
        let Known::Value(value) = self.known(scrutinee) else {
            return None;
        };
        Some(values.iter().position(|alternatives| {
            alternatives
                .iter()
                .any(|matched| matched.is_none_or(|matched| matched == value))
        }))
    }

    /// Runs `build` with the checks off unless `can_run`.
    pub(super) fn checked_if<T>(&mut self, can_run: bool, build: impl FnOnce(&mut Self) -> T) -> T {
        let outer = self.checks;
        self.checks = outer && can_run;
        let value = build(self);
        self.checks = outer;
        value
    }

    /// A known shift amount must be below the width of `shifted`, taken
    /// with its own type's sign (ADR 0007, 0032).
    pub(super) fn check_shift(&mut self, shifted: Ty, amount: &hir::Expr) {
        let Ty::Int(int) = shifted else {
            return;
        };
        if !self.checks {
            return;
        }
        let Known::Value(value) = self.known(amount) else {
            return;
        };
        let amount_ty = self.ty_of(amount);
        let Some(signed) = shift_out_of_range(int, amount_ty, value) else {
            return;
        };
        let range = format!("must be between 0 and {}", int.bits() - 1);
        let label = if is_literal(amount) {
            range
        } else {
            format!("is {signed}, but {range}")
        };
        self.report(
            Diagnostic::error(
                "ty::shift-out-of-range",
                format!("shift amount out of range for `{int}`"),
            )
            .label(amount.span, label)
            .help(format!(
                "shifts mask their amount, so this would shift by {} (ADR 0007)",
                signed.rem_euclid(i128::from(int.bits()))
            )),
        );
    }

    /// A known divisor of `/` or `%` must not be zero (ADR 0007, 0032).
    /// Returns whether it was, and reported.
    pub(super) fn check_divisor(&mut self, op: BinOp, divisor: &hir::Expr) -> bool {
        if !matches!(op, BinOp::Div | BinOp::Rem)
            || !self.checks
            || self.known(divisor) != Known::Value(0)
        {
            return false;
        }
        self.report(
            Diagnostic::error("ty::division-by-zero", "division by zero")
                .label(divisor.span, "this divisor is zero")
                .help("at run time this would call the panic function (ADR 0007)"),
        );
        true
    }

    /// A known index must be below the array's length (ADR 0021, 0032).
    pub(super) fn check_index(&mut self, index: &hir::Expr, len: u64) {
        if !self.checks {
            return;
        }
        let Known::Value(value) = self.known(index) else {
            return;
        };
        if value < len {
            return;
        }
        self.report(
            Diagnostic::error("ty::index-out-of-bounds", "index out of bounds")
                .label(
                    index.span,
                    format!("the length is {len} but the index is {value}"),
                )
                .help("at run time this would call the panic function (ADR 0021)"),
        );
    }

    /// Reports a failed check; the body is not used (ADR 0032).
    pub(super) fn report(&mut self, diag: Diagnostic) {
        self.q.diags.push(diag);
        self.failed = true;
    }
}

/// The amount, with its type's sign, if it is out of range for `shifted`.
fn shift_out_of_range(shifted: hir::IntTy, amount_ty: Ty, amount: u64) -> Option<i128> {
    let signed = match amount_ty {
        Ty::Int(int) if int.is_signed() => i128::from(eval::sign_extend(amount, int)),
        _ => i128::from(amount),
    };
    (!(0..i128::from(shifted.bits())).contains(&signed)).then_some(signed)
}

/// An integer, `bool` or pointer computed at compile time; `None` failed,
/// reported.
fn scalar(ty: Ty, value: Option<eval::ConstValue>) -> Known {
    let Some(value) = value else {
        return Known::Failed;
    };
    match ty {
        Ty::Int(int) => Known::Value(value.int(int.bytes() as usize)),
        Ty::Bool => Known::Value(u64::from(value.bytes[0] != 0)),
        // A pointer to a static is not known: its address only exists after
        // linking (ADR 0030, 0032).
        Ty::Ptr(_) if !value.relocations.is_empty() => Known::Unknown,
        // Made from an integer, its bytes are its address.
        Ty::Ptr(_) => Known::Value(value.int(target::POINTER_BYTES as usize)),
        _ => Known::Unknown,
    }
}

/// A literal operand, whose value the source already shows.
fn is_literal(expr: &hir::Expr) -> bool {
    match &expr.kind {
        hir::ExprKind::Int(_) | hir::ExprKind::Byte(_) => true,
        hir::ExprKind::Unary(UnOp::Neg, operand) => matches!(operand.kind, hir::ExprKind::Int(_)),
        _ => false,
    }
}
