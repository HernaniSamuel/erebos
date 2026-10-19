# 0040. Patterns in payloads

- Status: Accepted; amended by 0042 (code generation), 0043 (witnesses listed without a count), 0044 (`.V` patterns, and the enums a `match` needs)
- Date: 2026-10-17
- Amends: 0036 (which values make a `match` call the panic function),
  0037 (what can be matched, variant patterns with payloads, nested
  patterns, exhaustiveness and dead arms over them, at most three missing
  values listed, code generation), 0038 (which enums a `match` needs the
  values of, per position, and when MIR building does not use a body)

## Context

ADR 0039 lets enum variants carry payload values. A `match` must reach
them, to tell variants apart and to read their values, as in
`Msg::Write(0x10, let v: u16)`. ADR 0037 matched integers, `bool` and
enums without payloads, where every pattern stands for a set of values
of one integer; with payloads, a pattern describes a tree of values, and
exhaustiveness and dead arms must range over those trees.

ADR 0038 decides which `match`es need the values of an enum, for a
scrutinee whose values are those of its integer type, and asks this step
to restate the rule "per enum examined at each position".

## Decision

### What can be matched

The scrutinee is an integer, a `bool` or an enum, **with or without
payloads** (amends ADR 0037). Structs, arrays, pointers, slices, `()` and
`!` remain `ty::match-type`.

### Variant patterns with payloads

The examples use these enums:

```
enum Mode: u8 { Off = 0, Run = 1 }

enum Msg: u8 {
    Nop = 0,
    Led(bool) = 1,
    Write(u8, u16) = 2,
    Wide(u32) = 3,
    SetMode(Mode) = 4,
}

match m {
    Msg::Nop => 0,
    Msg::Led(true) => 1,
    Msg::Led(false) => 2,
    Msg::Write(0x10 | 0x11, let v: u16) => special(v),
    Msg::Write(let r: u8, let v: u16) => write(r, v),
    Msg::Wide(_) => 3,
    Msg::SetMode(Mode::Run) => 4,
    Msg::SetMode(_) => 5,
}
```

- `Msg::Write(p, q)` matches a value whose tag is the value of
  `Msg::Write` and whose payload values match `p` and `q`, in order. Each
  of them is a pattern, checked against the type of its payload value:
  any pattern of ADR 0037, a variant pattern of another enum, with its own
  payload, or an or-pattern of these. Patterns nest to any depth.
- The number of patterns must be the payload's: `Msg::Write(_)` and
  `Msg::Nop(_)` are `ty::wrong-payload-count` (ADR 0039). There is no
  `..`: `Msg::Write(_, _)` writes every position, so adding a value to a
  variant makes every pattern naming it an error.
- A variant without payload is written without parentheses, as in
  ADR 0037; `Msg::Nop()` and `Mode::Off()` are `ty::wrong-payload-count`,
  as in expressions (ADR 0039).
- At a position whose type is a struct, an array, a pointer, a slice or
  `()`, only `_`, `let` and constants apply, and a constant there is the
  error below; a literal or a variant there is `ty::mismatched-types`, as
  against any type it does not have.
- A **constant pattern** keeps the types of ADR 0037: an integer, `bool`
  or an enum without payloads, at any position. A constant of any other
  type, such as an enum with payloads, a struct or a pointer, is
  `ty::constant-pattern-type`, whatever the type of its position, and
  only that is reported about the constant itself; it still counts as a
  constant pattern for the enums a `match` needs (below), so in a
  function computing a value of its position's enum it is a `ty::cycle`
  too. For an enum with payloads its help
  suggests the variant pattern. Patterns compare only integers, and a
  payload or a struct may hold values no pattern compares, such as
  pointers.
- A **`let`** at any position binds a copy of the value at that position,
  whose type it writes and which must be the type of that position
  (`ty::mismatched-types`, ADR 0037). One name bound twice in one
  pattern, as in `Msg::Write(let a: u8, let a: u16)`, is
  `resolve::duplicate-binding`. Bindings inside or-patterns are ADR 0041.

A **position** of a pattern is the scrutinee, or a payload value of a
variant named by the pattern at an enclosing position. Its type is the
scrutinee's type, or the type the variant's declaration gives that payload
value.

### Choosing an arm

As in ADR 0037: the scrutinee runs once, before any arm, and is read where
it is written; the arms are tried in order and the first that matches
runs. Every pattern tests the one value the scrutinee gave, so a pattern
at any depth reads no memory: the order in which a pattern tests its
positions cannot be observed.

### Exhaustiveness

Every `match` is exhaustive (ADR 0037): its arms together match every
value of the scrutinee's type, where at every position the values of an
enum are its variants, each with every value of its payload, and not the
values that are no variant (ADR 0036). A `bool` needs `true` and
`false`, an integer every value of its type.

`ty::non-exhaustive-match` lists the missing values as patterns,
**witnesses**. They are found by examining positions in the order they
are written: the scrutinee first, and a variant's payload positions, left
to right, in its place. At each position, the **remaining arms** are those
that match what the witness has so far:

- if every remaining arm has `_` or `let` there, or none remains, the
  witness has `_` there; the scrutinee of a `match` without arms is the
  exception: an enum scrutinee lists every variant, as below, and a
  `bool` one `true` and `false`, as ADR 0037 does;
- at an enum position, each variant gets its witnesses in turn, in
  declaration order, and the remaining arms for it are those whose
  pattern there is that variant, a constant holding its value, `_` or
  `let`. A variant with no remaining arm is written with `_` at each of
  its payload positions (`Msg::Wide(_)`); one with remaining arms
  continues with its payload positions;
- at a `bool` position, `true` and then `false` are examined in turn;
- at an integer position, each value the remaining arms name is examined
  in turn, in the order they first name it, and then one value they do
  not name, if one exists: the one closest to zero, the non-negative one
  first.

A witness is complete once every position is written, and is missing
when no arm remains then; a complete one that some arm still matches is
dropped.

An integer scrutinee keeps the message of ADR 0037, which asks for a `_`
or `let` arm. The witnesses are listed in the order found; when there are
more than three, the first three are shown, followed by "and N more"
(amends ADR 0037, which lists every missing variant). The help suggests an
arm for each witness shown, or a `_` arm.

With the enum above:

```
match m {
    Msg::Nop => 0,
    Msg::Led(true) => 1,
    Msg::Write(0, 5) => 2,
}
// missing: `Msg::Led(false)`, `Msg::Write(0, 0)`, `Msg::Write(1, _)`
// and 2 more
```

The two more are `Msg::Wide(_)` and `Msg::SetMode(_)`. With
`enum P: u8 { Pair(Mode, bool) = 0 }` and the arms `P::Pair(Mode::Off,
true)` and `P::Pair(_, false)`, the second arm remains for `Mode::Run`,
and the witness is `P::Pair(Mode::Run, true)`.

### Dead arms

As in ADR 0037: an arm, or an alternative of an or-pattern, that matches
no value the arms and alternatives before it leave is
`ty::unreachable-pattern`, labelled with the first earlier arm that covers
it alone, if one does. This holds for alternatives at any depth: in
`Msg::Led(true | true)`, the second `true` is dead.

Here the values of an enum at every position include those that are no
variant (ADR 0036): `Msg::SetMode(_)` after `Msg::SetMode(Mode::Off)` and
`Msg::SetMode(Mode::Run)`, which list every variant of `Mode`, is not dead
while `Mode` has values that are no variant, and neither is a `_` arm
after every variant of `Msg`.

### Which enums a `match` needs

This restates ADR 0038 per position. A `match` **needs the values of an
enum** `E` when:

- some alternative, at a position whose type is `E`, is a variant or a
  constant pattern, whatever its own type; or
- the `match` has no arms, and its scrutinee is of type `E`.

As in ADR 0038, the kind of a pattern is read before it is checked
against its position's type. The positions inside a variant pattern have
the types its own declaration gives, even when that variant does not fit
its position: with `H` another enum, `H::X(Mode::Run)` at a position of
type `G` needs the values of `G` and of `Mode`, and is
`ty::mismatched-types`.

A `match` that needs the values of an enum one of whose values failed or
repeats another is not checked for exhaustiveness or dead arms
(ADR 0035, 0038). Any other `match` is checked normally, and adds no
dependence on the values of the enums it needs none of.

**MIR building** does not use a body containing a `match` that needs the
values of an enum in that state, whether or not the `match` has a
catch-all arm: its checks were skipped, so a variant may lack an arm at
some position, and the `match` would panic on it as if it were no
variant. The build stops at the enum's error anyway; this only means that
such a function is not evaluated at compile time either. MIR building
asks only about enums type checking already asked about, so this adds no
dependence and no cycle. It replaces ADR 0038's rule that MIR building
asks only for a `match` without a catch-all.

### Values that are no variant

A `match` calls the panic function when no arm matches its value
(amends ADR 0036). Since it is exhaustive, that only happens when a
position that an arm examines with a variant pattern holds a value that
is no variant: a tag, or a payload value of an enum type. The cause is
"enum value is no variant", and at compile time the evaluation fails with
`eval::panic` (ADR 0036). Positions matched only by `_` and `let` are
never examined: `Msg::SetMode(_)` matches a `Mode` value that is no variant.

### Values known at compile time

ADR 0037's rule for scrutinees known at compile time is unchanged. A
value of an enum with payloads is never known (ADR 0039), so every arm of
a `match` on one is checked by ADR 0032's rules.

### Code generation

A `match` on an enum with payloads becomes a switch on the tag of the
scrutinee's copy, then, for each variant, tests of the payload positions
that its arms examine, in arm order: a switch on an integer, `bool` or
enum position, and, when a test fails, a jump to the next arm that can
still match. The default of a switch on an enum position goes to the arms
that match there with `_` or `let`, or to the panic call. It costs what
nested C `switch`es cost.

As in ADR 0036, nothing tells LLVM that a tag or a payload value holds a
variant.

### Diagnostic codes

| Code | Meaning |
|------|---------|
| `ty::constant-pattern-type` | a constant pattern of a type other than an integer, `bool` or an enum without payloads |
| `resolve::duplicate-binding` | one name bound twice in one pattern |

`ty::wrong-payload-count` (ADR 0039) also covers patterns.

### Conformance tests

The values that are no variant below are read through a cast pointer
from a `static` byte array with `#[align(4)]`, the alignment of `Msg`,
since the target assumes pointers are aligned (ADR 0019).

- **ui (parser):** nested variant patterns, an or-pattern inside a
  payload, a trailing comma inside a payload pattern.
- **ui (resolve):** `resolve::duplicate-binding`, also at two depths;
  `resolve::unknown-variant` and `resolve::not-a-constant` inside a
  payload.
- **ui (typeck):** a scrutinee of an enum with payloads, accepted, and of
  a struct, still `ty::match-type`; `Msg::Write(_)`, `Msg::Nop(_)`,
  `Msg::Write()`, `Msg::Nop()`, `Mode::Off()` and a bare `Msg::Write`
  (`ty::wrong-payload-count`); a literal at a struct position; a `let` of
  the wrong type inside a payload; constants of an enum with payloads, of
  a struct at a struct position and of a pointer at a pointer position,
  and a constant of an enum with payloads at a `u8` position
  (`ty::constant-pattern-type` alone); `H::X(Mode::Run)` at a position of
  type `G` (`ty::mismatched-types`); `ty::non-exhaustive-match` with the
  witnesses of the example above ("and 2 more"), `Msg::Write(0, 5)` as
  the only `Write` arm, the `P::Pair` example, `match b {}` on a `bool`
  (still `true` and `false`), `match m {}` on an enum without payloads of
  five variants (three, "and 2 more"), an `i8` position missing `-1` and `1` (shows `1`),
  a missing variant nested two deep, and `match m {}` on `Msg` listing
  its first three variants; dead arms: a nested repeated literal,
  `Msg::Led(true | true)` (the alternative), a whole nested arm covered
  by two earlier arms, `_` after every variant of `Msg`, and
  `Msg::SetMode(_)` after every variant of `Mode` (both not dead); a
  nested enum with a repeated value skips both checks, and a `match`
  examining it only with `_` is checked.
- **ui (MIR):** in a function computing a value of an enum `G`, one
  `ty::cycle` each for a variant of `G` nested in a payload, for a
  constant of type `G` nested in one, and for `H::X(Mode::Run)` at a
  position of type `G` (also `ty::mismatched-types`); no cycle when `G`'s
  positions hold only `_` and `let`. On a `Mode` with a repeated value,
  a `comptime` block calling a function whose `match` has
  `Msg::SetMode(Mode::Run)` and a `_` arm reports only the enum's error,
  with an assert that would fail if the function were evaluated.
- **MIR and LLVM IR snapshots:** the tag switch and a nested switch on a
  payload value; a failed nested test falling to a later arm; the panic
  default for a nested enum position; no range metadata on the tag load.
- **ui (eval):** a compile-time `match` whose nested `Mode` is no
  variant (`eval::panic` with the cause), and the same with
  `Msg::SetMode(_)`.
- **Differential test** (ADR 0027): nested matches on integers of every
  width, `bool`, enums with and without payloads, or-patterns inside
  payloads and `let`, at compile time and at run time.
- **e2e:** a tag that is no variant and a nested enum value that is no
  variant panic in a `match` without a catch-all and are handled with
  `_`, at both optimization levels.

## Consequences

- Programs read payloads only through patterns, so a payload value is
  read only under the tag of the variant that has it.
- A nested pattern is checked like a top-level one: exhaustiveness and
  dead arms cover every position.
- The checks are no longer a set of values per `match`, but an analysis
  over trees of values, which the compiler must implement; it subsumes
  ADR 0037's checks, whose diagnostics stay as they are, except that a
  `match` missing more than three variants shows three and a count.
- A missing integer value inside a payload is shown as one example, not as
  every missing value: Erebos has no range patterns to write them.
- Adding a value to a variant breaks every pattern naming it, which is the
  intent: there is no `..` to hide it.

## Alternatives considered

- **`..` for the rest of a payload**, or a bare variant name matching any
  payload — shorter, but a second spelling of `(_, _)`, and the second
  would look like a variant without payload.
- **Constants of enums with payloads as patterns**, when every payload
  type is an integer, `bool` or enum — one more rule over the whole
  enum's types, for values a variant pattern already writes.
- **Listing every witness**, as ADR 0037 lists every missing variant —
  complete, but a payload of two enums of 100 variants each can miss
  some 200 values; three show what to add, and the count how much is
  left.
- **Witnesses as ranges** (`Msg::Write(1..=255, _)`, as Rust prints them)
  — exact, but in a syntax Erebos does not have.
- **Payload values readable outside `match`**, as `m.0` — would read a
  payload whatever the tag says.
