# 0062. Bit-field defaults are no part of the bit layout

- Status: Accepted
- Date: 2026-10-27
- Amends: 0059 (`ty::cycle` applies to the struct's bits, meaning its
  bit positions, its field types and the values of an enum field type's
  variants, needing those bits; a default may use its own struct's size,
  layout and bits, and is a cycle only when it needs its own value; a
  variant value that fails leaves its field a failed value)

## Context

ADR 0059, "Declaration", says that "a bit position or a default whose
computation needs the struct's own bits, as `at L as u32` with
`const L: Lcr`, is `ty::cycle`", and its table "What is checked" lists
`ty::cycle` for "a bit position or default that needs the struct's own
bits".

M3.3 computes a bit-field struct in two queries: one computes the bits
of its fields, which tells whether the struct reaches a failed value
(ADR 0048) and so whether `size_of` of it, or of a type holding it, can
be computed; the other computes the default of one field. What the
first needs is more than the bit positions: it normalizes each field's
type, whose array lengths are computed, and it checks an enum field's
width against the values of the enum's variants (ADR 0059, "Declaration"),
so a field type or a variant value computed from `size_of` of the struct
needs the struct's bits as a bit position would.

A struct's bits never ask for its own defaults. A default is only read by
a literal of the struct, and a body is built into MIR only after its
types are asked whether they reach a failed value (ADR 0048): a literal
of `Lcr`, wherever it is, asks for `Lcr`'s bits before it can take one of
`Lcr`'s defaults. So a default that needs its own struct's bits, as
`= size_of(Defaults) == 1` in `Defaults`, needs a value that is already
computed or that fails first, and nothing circular happens through the
default. The compiler accepts it, and `tests/ui/typeck_bit_struct.ers`
pins its value, which contradicts ADR 0059's text. A default can still
need itself: one that reads a constant of its struct's type, whose
literal leaves the defaulted field out with `..`, needs its own value.

Across structs, bits may need another struct's default, through a
constant: with `struct B: u8 { n: u8 at 0..=3 = size_of(A) as u8 }`,
`const CB: B = B { .. };` and `struct A: u8 { f: bool at CB.n as u32 }`,
`A`'s bits need `CB`, which needs `B::n`'s default, which needs `A`'s
bits. That cycle is one of `A`'s bits needing themselves.

An enum field also showed that a cycle must leave the same result
whichever item it is found from (ADR 0028). With
`enum Level: u8 { Low = 0, High = size_of(Variant) as u8 + 3 }` and
`struct Variant: u8 { level: Level at 0..=1 }`, the cycle is found from
`Level::High` when enums are checked first, and from `Variant`'s bits
when a constant asks for `size_of(Variant)` first. Had the bits skipped
a variant whose value is missing, as a value already failed and
reported, the first order would leave `Variant` sound and `High` with a
value (4, which does not fit its two bits, unreported), and the second
would leave `High` failed.

## Decision

`ty::cycle` applies to bit-field structs as follows (amends ADR 0059,
"Declaration" and "What is checked"):

- **The struct's bits**, meaning its bit positions, its field types and
  the values of an enum field type's variants, are `ty::cycle` when their
  computation needs those bits: `at L as u32` with `const L: Lcr`;
  `size_of(Lcr)` inside `Lcr`'s bit positions, which asks whether `Lcr`
  reaches a failed value (ADR 0048), also through a struct holding an
  `Lcr`; a field type `[u8; size_of(Lcr)]`; an enum field type one of
  whose variants is computed from `size_of(Lcr)`; and a bit position
  needing another struct's default that needs `Lcr`'s bits.
- **A default** may use its own struct's size, layout and bits:
  `= size_of(Lcr) == 1` is accepted, since the struct's bits never ask
  for its own defaults. A default is `ty::cycle` only when its
  computation needs its own value, as through a literal that takes it:
  with `const C: Lcr = Lcr { .. };`, a default `= C.dlab` of the field
  `parity` needs `C`, whose literal takes `parity`'s default, so it
  needs itself; with `const C: Lcr = Lcr { parity: false, .. };`, the
  same default reads a value whose literal does not take it, and is
  accepted unless a default that literal takes needs `parity`'s in turn.
- **A variant value that fails** (amends ADR 0059, "Failed values"),
  whether reported for its own error or as part of a cycle, is not
  checked against an enum field of its type, and that field is a failed
  value: the struct reaches a failed value and reports nothing more. The
  other variants are still checked against the field. So a cycle through
  a variant leaves the variant and the struct failed, whichever is found
  first.

The table "What is checked" of ADR 0059 reads, for `ty::cycle`: "the
struct's bits (its bit positions, its field types and the values of an
enum field type's variants) needing those bits, or a default that needs
its own value".

## Consequences

- A default may depend on its struct's bits, never the reverse: it may
  test the struct's size, or read a constant of the struct's type, as
  long as the literal making that constant does not take that default.
- The rule matches the implementation of M3.3, step 3, whose two queries
  have their own cycle items ("the bit layout of `Lcr`", "the default of
  the field `Lcr::f`").
- A cycle through a default, and one through another struct's default,
  need a literal that takes it, which is built from M3.3, step 5 on;
  their tests belong there.
- An enum whose variant fails, for any reason, makes every bit-field
  struct with a field of that enum fail silently; the variant's error is
  the one reported.

## Alternatives considered

- **Keep ADR 0059's text, computing defaults with the bits** — a default
  would be a cycle whenever it names its struct's size or a constant of
  its type, though nothing it computes changes the bits; the restriction
  buys nothing.
- **Keep ADR 0059's text and report a default that names the struct's
  bits without a cycle** — a rule of its own that no other compile-time
  value has: everywhere else, `ty::cycle` means a value needing itself
  (ADR 0028).
- **Skip a variant whose value is missing, checking the others** — the
  outcome of a cycle through the variant would depend on which item is
  computed first, and the first order lets a value that was part of a
  reported cycle be used.
- **Make the item at the head of a reported cycle a failed value** —
  would hold for every query, but changes how every cycle of the
  compiler ends, for a case that the field's own check settles.
