//! Bit-field structs (ADR 0059). MIR sees a value as its integer type: a
//! field read is a shift and a mask of the whole value, a field write
//! clears the field's bits and or's in the value, checked to fit, and a
//! literal or's its fields together. In a volatile place, a field write
//! also gives every other field with a write effect the value whose write
//! has no effect (ADR 0060).

use crate::diag::Diagnostic;
use crate::hir;
use crate::layout;
use crate::mir::{BinOp, Const, Operand, PanicCause, Place, Rvalue, Ty};
use crate::query::{bit_count, max_value};
use crate::span::Span;

use super::known::Known;
use super::{Builder, is_simple};

/// A field of a bit-field struct, as MIR building sees it.
#[derive(Clone, Copy)]
struct Bits {
    /// The struct's integer type, which the field's bits are part of.
    int: hir::IntTy,
    low: u32,
    high: u32,
    /// The field's MIR type: `bool`, or an integer, an enum's included.
    ty: Ty,
    /// Whether a value of the field's type may not fit in its bits: not
    /// for a `bool`, nor for a type exactly as wide as the field.
    checked: bool,
}

impl Bits {
    fn width(self) -> u32 {
        self.high - self.low + 1
    }

    /// The largest value the field holds.
    fn max(self) -> u64 {
        max_value(self.width())
    }

    /// The field's bits set, in the struct's integer type.
    fn mask(self) -> u64 {
        self.max() << self.low
    }
}

impl Builder<'_, '_> {
    /// The field `index` of the bit-field struct `id`. A body is built only
    /// when none of its bit-field structs reaches a failed value (ADR 0048),
    /// so every field has its bits.
    fn bits(&mut self, id: hir::BitStructId, index: u32) -> Bits {
        let layout = self
            .q
            .bit_layout(id)
            .expect("a body is built only when its bit-field structs are sound");
        let (low, high) =
            layout.fields[index as usize].expect("a sound bit-field struct has every field's bits");
        let hir = self.q.hir;
        let def = hir.bit_struct(id);
        let ty = self.q.normalize(def.fields[index as usize].ty);
        Bits {
            int: def.ty.int,
            low,
            high,
            ty: self.q.mir_ty(ty),
            checked: layout::bit_field_holds(ty) != Some(high - low + 1),
        }
    }

    /// `base.f`, the field `index` of the bit-field struct `id`: one copy
    /// of the whole value, then the field taken from it (ADR 0059).
    pub(super) fn read_bit_field(
        &mut self,
        base: &hir::Expr,
        id: hir::BitStructId,
        index: u32,
    ) -> Operand {
        let bits = self.bits(id, index);
        let place = self.place(base);
        let whole = self.temp(Ty::Int(bits.int), Rvalue::Use(Operand::Copy(place)));
        self.extract(whole, bits)
    }

    /// `base.f = value`, to the field `index` of the bit-field struct `id`
    /// named at `name`: the value and its check, then the place, one copy
    /// of the whole value, the field changed in it, in a volatile place the
    /// other fields with write effects made neutral, and one assignment back
    /// (ADR 0059, "Evaluation order"; ADR 0060).
    pub(super) fn assign_bit_field(
        &mut self,
        base: &hir::Expr,
        id: hir::BitStructId,
        index: u32,
        name: Span,
        value_expr: &hir::Expr,
    ) {
        let bits = self.bits(id, index);
        let mut value = self.expr(value_expr);
        // Read now if evaluating the place could assign it (ADR 0018).
        if matches!(value, Operand::Copy(_)) && !is_simple(base) {
            value = self.temp(bits.ty, Rvalue::Use(value));
        }
        self.check_field_value(value_expr, value, bits, id, index, name);
        let place = self.place(base);
        let whole = self.temp(Ty::Int(bits.int), Rvalue::Use(Operand::Copy(place)));
        let whole = self.insert(whole, bits, value);
        let whole = self.neutral_effects(place, whole, id, index);
        self.assign(place, Rvalue::Use(whole));
    }

    /// `base.f op= value`, to the field `index` of the bit-field struct `id`
    /// named at `name`, the operator at `op_span`: the value, then the
    /// place and one copy of the whole value, the new field value computed
    /// from it and checked at run time only, since it depends on the
    /// field's value, then, in a volatile place, the other fields with write
    /// effects made neutral, and one assignment back (ADR 0059, 0060).
    #[expect(clippy::too_many_arguments, reason = "the parts of the assignment")]
    pub(super) fn compound_assign_bit_field(
        &mut self,
        op: BinOp,
        base: &hir::Expr,
        id: hir::BitStructId,
        index: u32,
        name: Span,
        value_expr: &hir::Expr,
        op_span: Span,
    ) {
        let bits = self.bits(id, index);
        let mut value = self.expr(value_expr);
        // Read now if evaluating the place could assign it (ADR 0018).
        if matches!(value, Operand::Copy(_)) && !is_simple(base) {
            value = self.temp(self.operand_ty(value), Rvalue::Use(value));
        }
        let place = self.place(base);
        let whole = self.temp(Ty::Int(bits.int), Rvalue::Use(Operand::Copy(place)));
        let old = self.extract(whole, bits);
        if op.is_shift() {
            self.check_shift(bits.ty, value_expr);
        }
        let reported = self.check_divisor(op, value_expr);
        let new = if matches!(op, BinOp::Div | BinOp::Rem) && !reported {
            self.divide(op, old, value, bits.ty, op_span)
        } else {
            self.temp(bits.ty, Rvalue::Binary(op, old, value))
        };
        if bits.checked {
            self.check_fits(new, bits, name);
        }
        let whole = self.insert(whole, bits, new);
        let whole = self.neutral_effects(place, whole, id, index);
        self.assign(place, Rvalue::Use(whole));
    }

    /// A literal of the bit-field struct `s` giving `fields`, each with the
    /// span of its name, and ending with `rest`, its `..`, if written
    /// (ADR 0059): the values written, in order, each checked and or-ed in
    /// where it is computed; then, as one constant, the values written as
    /// constants and the fields `..` leaves out. Reserved bits are 0.
    pub(super) fn bit_struct_lit(
        &mut self,
        s: hir::BitStructTy,
        fields: &[(u32, Span, hir::Expr)],
        rest: Option<Span>,
    ) -> Operand {
        let int = Ty::Int(s.int);
        let mut value: Option<Operand> = None;
        let mut constant = 0;
        for (i, (index, name, init)) in fields.iter().enumerate() {
            let bits = self.bits(s.id, *index);
            let mut field = self.expr(init);
            // Read now if a later value could assign it (ADR 0018).
            if matches!(field, Operand::Copy(_))
                && !fields[i + 1..].iter().all(|(_, _, init)| is_simple(init))
            {
                field = self.temp(bits.ty, Rvalue::Use(field));
            }
            self.check_field_value(init, field, bits, s.id, *index, *name);
            match self.placed(field, bits) {
                Operand::Const(Const::Int(part, _)) => constant |= part,
                part => {
                    value = Some(match value {
                        None => part,
                        Some(value) => self.temp(int, Rvalue::Binary(BinOp::BitOr, value, part)),
                    });
                }
            }
        }
        if rest.is_some() {
            constant |= self.filled_bits(s.id, fields);
        }
        let constant_operand = Operand::Const(Const::Int(constant, s.int));
        match value {
            None => constant_operand,
            Some(value) if constant == 0 => value,
            Some(value) => self.temp(int, Rvalue::Binary(BinOp::BitOr, value, constant_operand)),
        }
    }

    /// The bits that `..` gives the fields of the bit-field struct `id` that
    /// a literal giving `fields` leaves out, in its integer type: each
    /// field's default, or for a field with a write effect the value whose
    /// write has no effect, all zeros for `write 1 ...` and all ones for
    /// `write 0 ...` (ADR 0059, 0060). A default in error, reported where
    /// it is computed, leaves the body unused, as a constant that failed
    /// does; the defaults after it are not asked for, since one of them
    /// may be part of the same cycle, which was reported once.
    fn filled_bits(&mut self, id: hir::BitStructId, fields: &[(u32, Span, hir::Expr)]) -> u64 {
        let hir = self.q.hir;
        let def = hir.bit_struct(id);
        let mut filled = 0;
        for (index, field) in def.fields.iter().enumerate() {
            let index = index as u32;
            if fields.iter().any(|(given, _, _)| *given == index) {
                continue;
            }
            let bits = self.bits(id, index);
            let value = match field.write_effect {
                Some((effect, _)) if effect.bit == 1 => 0,
                Some(_) => bits.max(),
                None => match self.q.bit_default(id, index) {
                    Some(value) => value,
                    None => {
                        self.failed = true;
                        return 0;
                    }
                },
            };
            filled |= value << bits.low;
        }
        filled
    }

    /// The value of the field at `bits` in `whole`, of the field's type: a
    /// `bool` field is its bit compared with 0; any other is shifted down,
    /// masked and converted to its type, each where needed.
    fn extract(&mut self, whole: Operand, bits: Bits) -> Operand {
        let int = Ty::Int(bits.int);
        if bits.ty == Ty::Bool {
            let mask = Operand::Const(Const::Int(bits.mask(), bits.int));
            let bit = self.temp(int, Rvalue::Binary(BinOp::BitAnd, whole, mask));
            let zero = Operand::Const(Const::Int(0, bits.int));
            return self.temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, bit, zero));
        }
        let mut value = whole;
        if bits.low > 0 {
            value = self.temp(int, Rvalue::Binary(BinOp::Shr, value, u32_const(bits.low)));
        }
        if bits.high < bits.int.bits() - 1 {
            let max = Operand::Const(Const::Int(bits.max(), bits.int));
            value = self.temp(int, Rvalue::Binary(BinOp::BitAnd, value, max));
        }
        if bits.ty != int {
            value = self.temp(bits.ty, Rvalue::Cast(value, bits.ty));
        }
        value
    }

    /// `value`, of the field's type, converted to the integer type and
    /// shifted up to the field's bits, where needed; a constant as a
    /// constant.
    fn placed(&mut self, value: Operand, bits: Bits) -> Operand {
        let int = Ty::Int(bits.int);
        let constant = match value {
            Operand::Const(Const::Int(value, _)) => Some(value),
            Operand::Const(Const::Bool(value)) => Some(u64::from(value)),
            _ => None,
        };
        if let Some(value) = constant {
            // One that does not fit was checked before: it is never
            // written, and is masked only to stay a value of the type.
            let placed = (value << bits.low) & bits.int.mask();
            return Operand::Const(Const::Int(placed, bits.int));
        }
        let mut part = value;
        if bits.ty != int {
            part = self.temp(int, Rvalue::Cast(part, int));
        }
        if bits.low > 0 {
            part = self.temp(int, Rvalue::Binary(BinOp::Shl, part, u32_const(bits.low)));
        }
        part
    }

    /// `whole` with the field at `bits` set to `value`, of the field's
    /// type, which fits in its bits: the field's bits cleared, and the
    /// value, placed, or-ed in. A field of every bit keeps nothing of
    /// `whole`, which was read all the same (ADR 0059).
    fn insert(&mut self, whole: Operand, bits: Bits, value: Operand) -> Operand {
        let int = Ty::Int(bits.int);
        let keep = !bits.mask() & bits.int.mask();
        if keep == 0 {
            return self.placed(value, bits);
        }
        let keep = Operand::Const(Const::Int(keep, bits.int));
        let kept = self.temp(int, Rvalue::Binary(BinOp::BitAnd, whole, keep));
        let part = self.placed(value, bits);
        self.temp(int, Rvalue::Binary(BinOp::BitOr, kept, part))
    }

    /// `whole`, in which the field `index` of the bit-field struct `id` was
    /// changed, as it is written back to `place`: in a volatile place, a
    /// register or one reached through a volatile pointer, every other field
    /// with a write effect gets the value whose write has no effect, its
    /// bits cleared for `write 1 ...` and set for `write 0 ...`, each with
    /// one constant mask, left out when no field needs it. In ordinary
    /// memory the effects mean nothing, and `whole` is written as it is
    /// (ADR 0060, "Write effects").
    fn neutral_effects(
        &mut self,
        place: Place,
        whole: Operand,
        id: hir::BitStructId,
        index: u32,
    ) -> Operand {
        if !self.is_volatile(place) {
            return whole;
        }
        let hir = self.q.hir;
        let def = hir.bit_struct(id);
        let (mut zeros, mut ones) = (0, 0);
        for (other, field) in def.fields.iter().enumerate() {
            let other = other as u32;
            let Some((effect, _)) = field.write_effect else {
                continue;
            };
            if other == index {
                continue;
            }
            let mask = self.bits(id, other).mask();
            if effect.bit == 1 {
                zeros |= mask;
            } else {
                ones |= mask;
            }
        }
        let int = def.ty.int;
        let mut whole = whole;
        if zeros != 0 {
            let keep = Operand::Const(Const::Int(!zeros & int.mask(), int));
            whole = self.temp(Ty::Int(int), Rvalue::Binary(BinOp::BitAnd, whole, keep));
        }
        if ones != 0 {
            let set = Operand::Const(Const::Int(ones, int));
            whole = self.temp(Ty::Int(int), Rvalue::Binary(BinOp::BitOr, whole, set));
        }
        whole
    }

    /// Checks that `value`, computed by `value_expr` and written to the
    /// field `index` of the bit-field struct `id` at `bits`, fits in its
    /// bits (ADR 0059): a value known at compile time that does not is
    /// `ty::field-value-out-of-range` where the checks of ADR 0032 apply,
    /// and one that does needs no check; any other is checked at run time,
    /// located at `name`, the field's name.
    fn check_field_value(
        &mut self,
        value_expr: &hir::Expr,
        value: Operand,
        bits: Bits,
        id: hir::BitStructId,
        index: u32,
        name: Span,
    ) {
        if !bits.checked {
            return;
        }
        if let Known::Value(known) = self.known(value_expr) {
            let needed = layout::bits_needed(known);
            if needed <= bits.width() {
                return;
            }
            if self.checks {
                let hir = self.q.hir;
                let field = &hir.bit_struct(id).fields[index as usize];
                self.report(
                    Diagnostic::error(
                        "ty::field-value-out-of-range",
                        format!(
                            "the value written to the field `{}` does not fit in its bits",
                            field.name
                        ),
                    )
                    .label(value_expr.span, format!("`{known}` needs {}", bit_count(needed)))
                    .label(
                        field.bits_span(),
                        format!(
                            "{}, holding 0 to {}",
                            bit_count(bits.width()),
                            bits.max()
                        ),
                    )
                    .help("a value written to a field fits in its bits, and at run time this would call the panic function; a program that wants the low bits of a wider value masks them itself (ADR 0059)"),
                );
            }
        }
        self.check_fits(value, bits, name);
    }

    /// Calls the panic function unless `value`, of the field's type, is at
    /// most the largest value the field at `bits` holds; the check is
    /// located at `name`, the field's name, which its statements and its
    /// failure at compile time are shown at too (ADR 0053, 0059).
    fn check_fits(&mut self, value: Operand, bits: Bits, name: Span) {
        let Ty::Int(int) = bits.ty else {
            unreachable!("a `bool` always fits in its bit")
        };
        let outer = std::mem::replace(&mut self.span, name);
        let max = Operand::Const(Const::Int(bits.max(), int));
        let fits = self.temp(Ty::Bool, Rvalue::Binary(BinOp::Le, value, max));
        self.panic_unless(fits, PanicCause::FieldOutOfRange, name);
        self.span = outer;
    }
}

/// A shift amount, a `u32` as written in the source (ADR 0007).
fn u32_const(value: u32) -> Operand {
    Operand::Const(Const::Int(u64::from(value), hir::IntTy::U32))
}
