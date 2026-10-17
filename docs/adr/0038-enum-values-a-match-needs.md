# 0038. The enum values a `match` needs

- Status: Accepted; amended by 0040 (the enums a `match` needs, per position, and when MIR building does not use a body)
- Date: 2026-10-17
- Amends: 0035 and 0037 (which `match`es on an enum with a value that
  failed or repeats skip the exhaustiveness and dead-arm checks)

## Context

ADR 0035 says that when a value of an enum fails to evaluate or repeats
another, "a `match` on that enum is not checked for exhaustiveness or dead
arms", and ADR 0037 ("Dead arms") says the same. To know whether an enum
is in that state, the compiler computes the value of every variant
(ADR 0028).

Taken literally, every `match` on an enum then asks for all of the enum's
values, whatever its arms. A function that runs while one of those values
is computed cannot contain such a `match`, even one that looks at no
value at all:

```
enum G: u8 { A = h(1), B = 1 }

fn h(x: u8) -> u8 {
    if x == 0 { match G::B { _ => 0 } } else { 5 }
}
```

Computing `G::A` runs `h`; type checking `h` asks whether `G` has a value
that failed or repeats, which needs `G::A`: a `ty::cycle`. Yet the arms of
this `match` match every value, and neither its exhaustiveness nor its
dead arms depend on what `G::A` or `G::B` are. No ADR states that such a
`match` is a cycle; it follows only from the wording of the skip.

## Decision

### Which `match`es need the enum's values

A `match` on an enum **needs the values of the enum** when:

- some alternative of some arm is a **variant** (`G::B`) or a
  **constant** (`NAME`) pattern, whatever its type: what such an arm
  covers and whether it is dead depend on the values it names, and,
  without a catch-all arm, which variants are still missing depends on
  all of them; or
- the `match` has **no arms**: it is never exhaustive, and the missing
  variants it lists are found by their values.

Whether a pattern is a variant or a constant is read from its kind,
before it is checked against the scrutinee's type: in `h`,
`match G::B { H::X => 0, _ => 1 }`, with `H` another enum, is a
`ty::cycle` as well as `ty::mismatched-types`.

**Every other `match` needs no value.** In a function that type checks,
that is a `match` whose arms, at least one, are only `_` and `let`
(ADR 0037): its first arm covers every value of the integer type, so it
is exhaustive and every later arm is dead, whatever the variants are.
(A literal pattern on an enum, as in `0 => ...`, is a type error, and
needs no value either.)

A `match` with a variant or constant pattern and a catch-all arm needs
**all** the values, although its checks depend only on those its
patterns name. The skip of ADR 0035 is a property of the whole enum:
"uses of the enum report nothing more because of it". Deciding it from
the named values alone would be a second rule, per variant, for one kind
of `match`, to accept a narrow case: a function that names a variant of
an enum one of whose values it helps compute (see Alternatives).

### The skip applies only to those that need them

Only a `match` that needs the values of an enum one of whose values
failed or repeats another skips the exhaustiveness and dead-arm checks
(ADR 0035, 0037). The enum's errors are still reported once, by the enum,
and the build stops at them.

A `match` that needs no value asks for no value of the enum. So:

- it is **checked normally**, even on an enum with a value that failed or
  repeats: with `enum T: u8 { A = 0, B = 0 }`,
  `match t { _ => 1, _ => 2 }` reports `ty::duplicate-discriminant` for
  the enum and `ty::unreachable-pattern` for the second `_`;
- the `match` itself **adds no dependence** on the enum's values, so it is
  no `ty::cycle` in a function that runs while one of them is computed:
  in the Context, `G::A` is 5. Its scrutinee and arm expressions are uses
  like any other: in `h`, `match G::A { _ => 0 }` reads `G::A`, and is a
  `ty::cycle` through the scrutinee.

A `match` that needs the values is still a `ty::cycle` there, reported
once, even with a catch-all arm, as `match G::B { G::B => 0, _ => 1 }`
in `h` would be.

MIR building is unchanged: it asks whether the enum is in error only for
a `match` without a catch-all, which in a function that type checks
always needs the values, so type checking asked first.

### Conformance tests

- **ui (eval):** the program in the Context, and the same with a `let`
  arm in a function computing a value of its own enum, accepted and
  evaluated (the values in the dump, checked by `comptime` asserts).
- **ui (MIR):** in a function computing a value of its own enum, one
  `ty::cycle` each for a `match` with a variant pattern and a `_` arm,
  with a constant pattern and a `_` arm, without arms, and with a variant
  of another enum and a `_` arm (also `ty::mismatched-types`); and for a
  `match` with only a `_` arm whose scrutinee reads the value being
  computed. On an enum with a repeated value, a `match` without arms
  reports nothing more, and evaluating it fails without a diagnostic of
  its own.
- **ui (typeck):** on `enum T: u8 { A = 0, B = 0 }`, the second arm of
  `_ => 1, _ => 2` and of `let u: T => 1, _ => 2` is
  `ty::unreachable-pattern`; with a variant or a constant pattern before
  `_ => 2, _ => 3`, or without arms, nothing beyond the enum's
  `ty::duplicate-discriminant` is reported.

## Consequences

- Functions that run while an enum's values are computed may `match` on
  that enum with `_` and `let` arms, which inspect no variant.
- On an enum with a value that failed or repeats, the dead `_` arms of a
  `match` that needs no value are reported along with the enum's error.
  They are real errors whatever the enum's values, so nothing reported is
  spurious.
- The result is asymmetric on such an enum: the second `_` of
  `match t { _ => 1, _ => 2 }` is reported, but the third `_` of
  `match t { T::A => 1, _ => 2, _ => 3 }` is not, since that `match`
  needs the values and is skipped. It is reported once the enum's error
  is fixed.
- Whether a `match` needs the values is decided from the kinds of its
  patterns alone, before any value is computed.
- The rule is stated for a `match` on an enum without payloads, whose
  values are those of its integer type. With payloads and nested
  patterns (M2.4b), an enum is examined at inner positions too
  (`Msg::Set(Mode::Run)` against `Msg::Set(let m: Mode)`), and an enum
  with payloads is no integer; M2.4b restates the rule per enum examined
  at each position.

## Alternatives considered

- **Keep the literal rule, and record that every `match` on an enum is a
  `ty::cycle` in a function computing one of its values** — simple to
  state, but rejects `match`es that name no value of the enum at all.
- **With a catch-all arm, need only the values the patterns name**, and
  skip the checks only when one of those failed or two of them repeat —
  would also accept `match G::B { G::B => 0, _ => 1 }` in `h`, whose
  checks do not depend on `G::A`. Rejected: the skip of ADR 0035 is a
  property of the whole enum, and this makes it a second, per-variant
  rule that depends on whether a catch-all is present, for a narrow case
  that a `let` arm with `==` already expresses
  (`let g: G => if g == G::B { 0 } else { 1 }`).
- **Keep the skip for every `match`, without reading the values for those
  with only `_` and `let` arms** — impossible: whether the enum has a
  value that failed or repeats is only known by computing all its values,
  which is exactly what makes the cycle.
- **Report a `match` without arms without reading the values**, listing
  every variant as missing by name — would let it need no value either,
  but builds that diagnostic apart from every other `match`. A `match`
  without arms on an enum is always an error (an enum has at least one
  variant, ADR 0035), so its needing the values costs no correct program
  anything.
