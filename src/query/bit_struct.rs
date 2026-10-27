//! Bit-field structs (ADR 0059): the bits of their fields, the defaults of
//! their fields, and their checks.

use std::rc::Rc;

use crate::diag::Diagnostic;
use crate::eval::{self, Origin};
use crate::hir::{self, BitFieldDef, BitStructDef, BitStructId, IntTy};
use crate::layout::{self, BitLayout};
use crate::span::Span;

use super::{Item, Queries, State, describe};

impl Queries<'_> {
    /// Checks every bit-field struct of the program, used or not (ADR 0059):
    /// computes the bits of its fields, which checks them, and the default
    /// of each field that has one.
    pub(super) fn check_bit_structs(&mut self) {
        let hir = self.hir;
        for (id, def) in hir.bit_structs.iter().enumerate() {
            let id = BitStructId(id as u32);
            self.bit_layout(id);
            for index in 0..def.fields.len() {
                self.bit_default(id, index as u32);
            }
        }
    }

    /// Whether the bit-field struct `id` reaches a failed value (ADR 0048,
    /// 0059), reported: a field whose bits or type failed, or whose bits
    /// do not suit its type, or bits whose computation needs themselves.
    pub(super) fn bit_struct_failed(&mut self, id: BitStructId) -> bool {
        self.bit_layout(id).is_none_or(|layout| layout.failed())
    }

    /// The bits of the fields of the bit-field struct `id` (ADR 0059),
    /// computed and checked once: each field's ends, in its integer type,
    /// lowest first (`ty::reversed-bit-range`, `ty::bit-field-outside`); its
    /// type, a bit field type (`ty::bit-field-type`) that suits its width
    /// (`ty::bit-field-width`); and no two fields sharing a bit
    /// (`ty::bit-field-overlap`, once per pair, at the later field), which
    /// leaves the fields sound. A check that needs a value that failed,
    /// which was reported, is skipped, and a type in error is not also
    /// `ty::bit-field-type`; a variant of an enum field type whose value
    /// failed is not checked against the field, which is then a failed
    /// value, while the other variants still are (ADR 0062). `None` when
    /// asked for again while it is computed: an end, a field type's array
    /// length or a variant's value whose computation needs the struct's
    /// own bits, as through `size_of` of it, which `start` reports as
    /// `ty::cycle` (ADR 0062).
    pub fn bit_layout(&mut self, id: BitStructId) -> Option<Rc<BitLayout>> {
        if let State::Done(layout) = &self.bit_layouts[id.0 as usize] {
            return Some(layout.clone());
        }
        let hir = self.hir;
        let def = hir.bit_struct(id);
        let what = format!("the bit layout of `{}`", def.path);
        if !self.start(Item::BitLayout(id), def.name_span, what) {
            return None;
        }
        self.bit_layouts[id.0 as usize] = State::InProgress;
        let placed: Vec<Option<(u32, u32)>> = (def.fields.iter())
            .map(|field| self.field_bits(def, field))
            .collect();
        let fields = (def.fields.iter().zip(&placed))
            .map(|(field, &bits)| {
                let suits = self.field_type_suits(field, bits);
                bits.filter(|_| suits)
            })
            .collect();
        self.check_overlaps(def, &placed);
        self.finish(Item::BitLayout(id));
        let layout = Rc::new(BitLayout { fields });
        self.bit_layouts[id.0 as usize] = State::Done(layout.clone());
        Some(layout)
    }

    /// The lowest and highest bit of `field`, a field of `def`: `None` when
    /// an end failed, or the bits are reversed or reach past the integer
    /// type, all reported.
    fn field_bits(&mut self, def: &BitStructDef, field: &BitFieldDef) -> Option<(u32, u32)> {
        // Both ends are computed, so that both report their errors.
        let low = self.comptime_int(field.low.0, IntTy::U32.bytes());
        let high = match field.high {
            Some((high, _)) => self.comptime_int(high, IntTy::U32.bytes()),
            None => low,
        };
        let (low, high) = (low? as u32, high? as u32);
        let span = bits_span(field);
        if low > high {
            self.diags.push(
                Diagnostic::error(
                    "ty::reversed-bit-range",
                    format!("the bits of the field `{}` are reversed", field.name),
                )
                .label(span, format!("from bit {low} down to bit {high}"))
                .help(format!(
                    "bits are written lowest first, as `{high}..={low}`, both included (ADR 0059)"
                )),
            );
            return None;
        }
        let int = def.ty.int;
        if high >= int.bits() {
            self.diags.push(
                Diagnostic::error(
                    "ty::bit-field-outside",
                    format!(
                        "the field `{}` reaches past the last bit of `{int}`",
                        field.name
                    ),
                )
                .label(span, bits_text(low, high))
                .label(def.int_span, format!("has bits 0 to {}", int.bits() - 1))
                .help("a field's bits are bits of its struct's integer type, numbered from 0, the least significant (ADR 0059)"),
            );
            return None;
        }
        Some((low, high))
    }

    /// Whether the type of `field` suits it: a bit field type, reported
    /// otherwise unless it is in error, and when its bits were computed,
    /// `bits`, one that suits their width, reported otherwise. A variant
    /// whose value is missing, because it failed or is being computed in a
    /// cycle, both reported, cannot be checked: the field does not suit,
    /// so that whichever item a cycle is found from, the variant and the
    /// struct both end up failed (ADR 0028, 0048).
    fn field_type_suits(&mut self, field: &BitFieldDef, bits: Option<(u32, u32)>) -> bool {
        let ty = self.normalize(field.ty);
        let Some(holds) = layout::bit_field_holds(ty) else {
            // `!`, `[T]`, a peripheral type and an array whose length
            // failed were reported where they are written (ADR 0048).
            if ty != hir::Ty::Error {
                let what = match ty {
                    hir::Ty::Enum(e) if e.payloads => ", an enum with payloads".to_owned(),
                    hir::Ty::Enum(e) => format!(", an enum on `{}`", e.int),
                    _ => String::new(),
                };
                self.diags.push(
                    Diagnostic::error("ty::bit-field-type", format!("`{ty}` is not a bit field type"))
                        .label(field.ty_span, format!("the type of the field `{}`{what}", field.name))
                        .help("a bit field is a `bool`, a `u8`, `u16`, `u32` or `u64`, or an enum without payloads on one of these four, whose value its bits hold as they are (ADR 0059)"),
                );
            }
            return false;
        };
        let Some((low, high)) = bits else {
            return true;
        };
        let width = high - low + 1;
        let span = bits_span(field);
        if width > holds {
            let diag = match ty {
                hir::Ty::Bool => Diagnostic::error(
                    "ty::bit-field-width",
                    format!("the `bool` field `{}` has {}", field.name, bit_count(width)),
                )
                .label(span, bits_text(low, high))
                .label(field.ty_span, "a `bool` takes exactly one bit")
                .help("a `bool` field is one bit, `true` when it is 1: give it a single bit, as `at 7` (ADR 0059)"),
                _ => {
                    let holds_label = match ty {
                        hir::Ty::Enum(e) => {
                            format!("`{ty}` is stored as `{}`, which holds {holds} bits", e.int)
                        }
                        _ => format!("`{ty}` holds {holds} bits"),
                    };
                    Diagnostic::error(
                        "ty::bit-field-width",
                        format!(
                            "the field `{}` has {}, more than `{ty}` holds",
                            field.name,
                            bit_count(width)
                        ),
                    )
                    .label(span, bits_text(low, high))
                    .label(field.ty_span, holds_label)
                    .help("a field's type holds at least as many bits as the field has, so that a read of the field is a value of its type: use a wider type or fewer bits (ADR 0059)")
                }
            };
            self.diags.push(diag);
            return false;
        }
        let hir::Ty::Enum(e) = ty else {
            return true;
        };
        let enum_def = self.enum_def(e.id);
        let mut fits = true;
        for (index, variant) in enum_def.variants.iter().enumerate() {
            let Some(value) = self.discriminant(e.id, index as u32) else {
                fits = false;
                continue;
            };
            let needed = layout::bits_needed(value);
            if needed <= width {
                continue;
            }
            fits = false;
            let mut diag = Diagnostic::error(
                "ty::bit-field-width",
                format!(
                    "the variant `{}` does not fit in the field `{}`",
                    enum_def.variant_path(index),
                    field.name
                ),
            )
            .label(
                span,
                format!("{}, holding 0 to {}", bit_count(width), max_value(width)),
            );
            if let Some(variant_span) = variant.span {
                diag = diag.label(
                    variant_span,
                    format!(
                        "is {}, which needs {}",
                        e.int.display_bits(value),
                        bit_count(needed)
                    ),
                );
            }
            self.diags.push(diag.help(
                "every variant of an enum field fits in the field's bits, so that each can be written to it (ADR 0059)",
            ));
        }
        fits
    }

    /// Reports each pair of fields of `def` that share a bit, once, at the
    /// later field (ADR 0059); `placed` holds the bits of each field, `None`
    /// where they failed, which was reported.
    fn check_overlaps(&mut self, def: &BitStructDef, placed: &[Option<(u32, u32)>]) {
        // In source order, which fields are kept in.
        for (later, &later_bits) in placed.iter().enumerate() {
            let Some((later_lo, later_hi)) = later_bits else {
                continue;
            };
            for (earlier, &earlier_bits) in placed[..later].iter().enumerate() {
                let Some((earlier_lo, earlier_hi)) = earlier_bits else {
                    continue;
                };
                let lo = later_lo.max(earlier_lo);
                let hi = later_hi.min(earlier_hi);
                if lo > hi {
                    continue;
                }
                let (later, earlier) = (&def.fields[later], &def.fields[earlier]);
                self.diags.push(
                    Diagnostic::error(
                        "ty::bit-field-overlap",
                        format!("the fields `{}` and `{}` overlap", earlier.name, later.name),
                    )
                    .label(bits_span(later), bits_text(later_lo, later_hi))
                    .label(bits_span(earlier), bits_text(earlier_lo, earlier_hi))
                    .help(format!(
                        "they share {}; the fields of a bit-field struct share no bit (ADR 0059)",
                        bits_text(lo, hi)
                    )),
                );
            }
        }
    }

    /// The default of the field `index` of the bit-field struct `id`
    /// (ADR 0059), computed once, as the bits of the field's type. `None`
    /// without one, or when it is in error, reported: its body has errors,
    /// it fails to evaluate, or it does not fit in the field's bits
    /// (`ty::field-value-out-of-range`), which is checked when the field's
    /// bits and type are sound. The default of a field whose type is no
    /// bit field type, reported, is not computed. A default needs the
    /// struct's bits only as any value does: it is no part of them.
    pub fn bit_default(&mut self, id: BitStructId, index: u32) -> Option<u64> {
        let hir = self.hir;
        let def = hir.bit_struct(id);
        let field = &def.fields[index as usize];
        let (body, span) = field.default?;
        let slot = (id.0 as usize, index as usize);
        if let State::Done(value) = self.bit_defaults[slot.0][slot.1] {
            return value;
        }
        let ty = self.normalize(field.ty);
        if layout::bit_field_holds(ty).is_none() {
            self.bit_defaults[slot.0][slot.1] = State::Done(None);
            return None;
        }
        let what = describe(self, hir.fn_(body));
        if !self.start(Item::BitDefault(id, index), span, what.clone()) {
            return None;
        }
        self.bit_defaults[slot.0][slot.1] = State::InProgress;
        let value = self.mir_body(body).and_then(|body| {
            let origin = Origin { what, span };
            eval::evaluate(self, body, origin)
        });
        self.finish(Item::BitDefault(id, index));
        let bytes = self
            .layout_of(ty)
            .expect("a bit field type has a layout")
            .size;
        let value = value
            .map(|value| value.int(bytes as usize))
            .filter(|&value| self.default_fits(id, index, ty, value));
        self.bit_defaults[slot.0][slot.1] = State::Done(value);
        value
    }

    /// Whether `value`, the default of the field `index` of the bit-field
    /// struct `id`, of type `ty`, fits in the field's bits, reported if it
    /// does not; a field that is a failed value is not checked.
    fn default_fits(&mut self, id: BitStructId, index: u32, ty: hir::Ty, value: u64) -> bool {
        let Some((low, high)) = self
            .bit_layout(id)
            .and_then(|layout| layout.fields[index as usize])
        else {
            return true;
        };
        let width = high - low + 1;
        let needed = layout::bits_needed(value);
        if needed <= width {
            return true;
        }
        let field = &self.hir.bit_struct(id).fields[index as usize];
        let (_, span) = field.default.expect("a default was computed");
        // A `bool` and a variant always fit: the field's width suits them.
        let shown = match ty {
            hir::Ty::Int(int) => int.display_bits(value),
            _ => value.to_string(),
        };
        self.diags.push(
            Diagnostic::error(
                "ty::field-value-out-of-range",
                format!(
                    "the default of the field `{}` does not fit in its bits",
                    field.name
                ),
            )
            .label(span, format!("`{shown}` needs {}", bit_count(needed)))
            .label(
                bits_span(field),
                format!("{}, holding 0 to {}", bit_count(width), max_value(width)),
            )
            .help("a value written to a field fits in its bits; a program that wants the low bits of a wider value masks them itself (ADR 0059)"),
        );
        false
    }
}

/// The span of the bits of `field`, from its low end to its high one.
fn bits_span(field: &BitFieldDef) -> Span {
    match field.high {
        Some((_, high)) => field.low.1.to(high),
        None => field.low.1,
    }
}

/// How diagnostics name the bits from `low` to `high`: "bit 7", "bits 0 to
/// 1".
fn bits_text(low: u32, high: u32) -> String {
    if low == high {
        format!("bit {low}")
    } else {
        format!("bits {low} to {high}")
    }
}

/// "1 bit", "4 bits".
fn bit_count(n: u32) -> String {
    match n {
        1 => "1 bit".to_owned(),
        n => format!("{n} bits"),
    }
}

/// The largest value `width` bits hold, 2ⁿ − 1.
fn max_value(width: u32) -> u64 {
    u64::MAX >> (u64::BITS - width)
}
