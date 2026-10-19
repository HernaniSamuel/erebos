# 0044. Variants named by the expected type

- Status: Accepted
- Date: 2026-10-19
- Amends: 0035 (a variant may also be named `.V`), 0037 and 0040 (`.V`
  patterns, and the enums a `match` needs), 0039 (`.V(args)` builds a
  variant)

## Context

A variant is named by its enum and its name, `Mode::Run` (ADR 0035), or
built with its payload, `Msg::Write(3, 4)` (ADR 0039). `Option(T)` and
`Result(T, E)` (M2.5, ADR 0045) are enums whose name is a type with
arguments: written that way, a variant would be
`Result(u32, ParseError)::Err(ParseError::Empty)`, in every `return` and
every pattern. In M6 they become ordinary enums returned by comptime
functions in `core` (ADR 0027), and the roadmap requires that programs
written in M2.5 do not change then: whatever spelling they use must be
one any enum can have.

There is no type inference (ADR 0008), but type checking is
bidirectional: a literal takes the type expected where it appears,
`let a: u8 = 200;`. The enum of a variant can be taken the same way.
Zig does this with enum literals (`.run`, checked against the result
type); Swift with implicit member expressions (`.run`).

## Decision

### `.V` in expressions

```
let m: Mode = .Run;
let c: Msg = .Write(3, 0x1234);
fn parse(s: *const [u8]) -> Result(u32, ParseError) {
    if len(s) == 0 { return .Err(.Empty); }
    ...
}
```

- `.V` and `.V(a, b)`, a `.` followed by a variant's name and, for a
  variant with a payload, its values, name a variant of **the enum that
  is the expected type** at that position, with or without payloads,
  declared or built in (ADR 0045). The result is what `E::V` or
  `E::V(a, b)` would be: same type, same value, same rules (ADR 0035,
  0039). It is a value, not a place.
- `.V` takes its enum **exactly where an integer literal takes its
  type** (ADR 0008), except as the operand of `as` (below), from the
  same expected type, including one taken
  from a sibling: an earlier branch of an `if`, an earlier arm of a
  `match`, an earlier `break` of the same loop, another element of an
  array, the other operand of a binary operator. For example: the
  initializer of a `let`, a constant or a static; the right side of an
  assignment; an argument; a payload value; a struct literal's field;
  `return`'s value and a function's tail; `comptime`'s operand;
  `if c { m } else { .Off }`; `m == .Run` and `.Run == m`.
- With no expected type, `.V` is **`ty::untyped-variant`**: the
  scrutinee of a `match`, the operand of `as` (ADR 0017 gives a literal
  the target type, and nothing converts to an enum), both operands of
  one `==`, the operand of `?` (ADR 0046), an expression statement.
  When exactly one *declared* enum has a variant of that name, the help
  suggests its `E::V`; for `.None`, `.Some`, `.Ok` and `.Err`, which
  name variants of `Option` and `Result` (ADR 0045), it suggests writing
  the type, as in `let x: Option(u32) = .None;`. When both apply, as for
  `.None` with a declared enum that has a `None`, the help shows both.
- When the expected type is not an enum, `.V` is `ty::mismatched-types`
  ("expected `usize`, found variant `.Run`"): an index, a condition, an
  operand of arithmetic against an integer. Where the expected type is
  an enum but the operator does not apply to it, the operator's error
  is reported, as for any operand of that enum: `.Run < m` is
  `ty::invalid-operand`. When the enum has no variant of that name, it
  is **`ty::unknown-variant`**: it is found by type checking, as an
  unknown field is, not by name resolution as `E::V`'s
  `resolve::unknown-variant` is. When the expected type is erroneous,
  nothing is reported.
- The number of values is checked as in ADR 0039
  (`ty::wrong-payload-count`), and so are the values, each against its
  payload type, which is their expected type. When the `.V` itself is
  in error (no expected type, not an enum, no such variant), its values
  are still checked, with an erroneous expected type, so that a literal
  among them reports nothing more.

### `.V` in patterns

```
match r {
    .Ok(let v: u32) => v,
    .Err(.Overflow) => 0xFFFF_FFFF,
    .Err(_) => 0,
}
```

- `.V` and `.V(p, ...)` are variant patterns of **the enum that is the
  position's type** (ADR 0040). They mean what `E::V` and `E::V(p, ...)`
  mean there, for exhaustiveness, dead arms, bindings and code.
- At a position whose type is not an enum, `.V` is
  `ty::mismatched-types`, as a variant of another enum is; with no such
  variant, `ty::unknown-variant`; at a position whose type is erroneous,
  nothing is reported. In each of these cases its sub-patterns are
  still checked, against an erroneous type: a `let` inside binds its
  written type, and reports nothing more.
- A bare name stays a constant pattern (ADR 0037): the leading `.` tells
  the two apart, so no name in a pattern changes meaning.

### Which enums a `match` needs

ADR 0040 counts a variant pattern at a position whose type is `E` as
needing the values of `E`. A `.V` pattern needs the values of the enum
that is its position's type, if that type is an enum, and its payload
positions have the types that enum's variant `V` gives them, if it has
one (amends ADR 0040). Otherwise its payload positions have no type,
and, as for the extra values of a variant pattern in ADR 0040, a
position with no type needs nothing: a constant or a `.V` there needs
no enum, and an `E::V` there needs `E` and gives its own payload
positions their declared types.

### `E::V` stays

`E::V` keeps its meaning (ADR 0035), and is what a program writes where
no type is expected, or where naming the enum reads better. Diagnostics
keep showing variants of declared enums as `E::V`, witnesses included;
the variants of `Option` and `Result`, which have no such path, are
shown as `.V` (ADR 0045).

### Diagnostic codes

| Code | Meaning |
|------|---------|
| `ty::untyped-variant` | `.V` where no type is expected |
| `ty::unknown-variant` | `.V` where the expected enum, or the position's enum, has no variant of that name |

### Conformance tests

- **ui (parser):** `.V`, `.V(a, b)` with a trailing comma, in
  expressions and patterns, nested (`.Some(.Run)`), in or-patterns; a
  statement starting with a block-like expression followed by `.V` on
  the next line, which starts a new expression; `x.V` stays a field
  access.
- **ui (typeck):** `.V` accepted at every position listed above, for an
  enum without payloads and one with payloads, including types taken
  from a sibling: `if c { m } else { .Off }`, a later `match` arm, a
  later `break`, `[m, .Off]`; `m == .Run` and `.Run == m`;
  `ty::untyped-variant` for a scrutinee, `.Run as u8`, `.A == .B` and
  an expression statement, with the `E::V` help, without it (two
  declared enums have the name), and for `.None`;
  `let x: u32 = .Run;`, `a[.Run]` and `if .Run {}`
  (`ty::mismatched-types`); `.Run < m` (`ty::invalid-operand`); an
  unknown name (`ty::unknown-variant`); `.Write(3)` and `.Nop()`
  (`ty::wrong-payload-count`); a literal out of range in a payload;
  `let x: u32 = .Write(3, 4);` and `.Bogus(1)` with an expected enum,
  each reporting one error; `.V` patterns: accepted, at a `u8` position
  (`ty::mismatched-types`), unknown, with the wrong count, at a
  position of erroneous type (nothing), and `.Bad(let v: u32)`
  (one error); exhaustiveness and dead arms with `.V` patterns, and
  mixed with `E::V`, giving the diagnostics the same match written
  with `E::V` gives.
- **ui (MIR):** a `.V` pattern of `G` in a function computing a value of
  `G` is a `ty::cycle`, as `G::V` is.
- **MIR snapshot:** a function written with `.V` gives the MIR of the
  same function written with `E::V`.

## Consequences

- `Option` and `Result` are usable without writing their type
  arguments at every variant, and the spelling stays valid when they
  become ordinary enums in M6.
- Every enum gets the shorter spelling where its type is written
  nearby, as literals do, and diagnostics name the enum when it is not.
- A variant has two spellings, `E::V` and `.V`. The first is needed
  where no type is expected, the second where the enum has no name to
  write before `::`.
- A `.V` is read by looking at the expected type, which is written
  somewhere nearby (a `let`, a parameter, a return type), as for
  literals; never at other uses of the value.

## Alternatives considered

- **Only the path, `Result(u32, E)::Err(e)`** — no new rule, but every
  `return` and pattern repeats the type arguments, and expressions would
  need a type before `::`.
- **`.V` only for `Option` and `Result`** — one spelling for declared
  enums until M6, but M6 would have to extend it to every enum anyway,
  since `Option` then is one.
- **Built-in names `Some`, `None`, `Ok`, `Err`** (Rust's prelude) —
  short, but reserves four names, and no ordinary enum could offer the
  same in M6.
- **A bare variant name, `Run`, taken from the expected type** — shorter
  still, but in patterns it collides with constants (ADR 0037), and in
  expressions with locals.
