# 0042. `match` code tested arm by arm

- Status: Accepted
- Date: 2026-10-18
- Amends: 0040 (code generation of a `match` on an enum with payloads:
  the arms are tested in order, its size is linear in its patterns, and
  where a test that fails goes), 0041 (code generation: a `let` is copied
  where its position is reached, and when several alternatives match, the
  first one binds)

## Context

ADR 0040 says that a `match` on an enum with payloads becomes a switch on
the tag, then, for each variant, tests of the payload positions its arms
examine, "in arm order", a failed test jumping "to the next arm that can
still match", and that the default of a switch on an enum position goes
"to the arms that match there with `_` or `let`". It says nothing about
the size of that code.

The M2.4b compiler builds a decision tree instead: a switch on a position
sends each value to the arms that can still match it, so an arm with `_`
at that position is copied under every case and under the default, and
each alternative of an or-pattern becomes an arm of its own. The code
then grows with the product of the patterns, not their sum. Measured on
that compiler:

- `enum P: u8 { A(bool, ..., bool) = 0 }` with `2n` positions, one arm
  per `i < n` with `true` at positions `i` and `i + n` and `_` elsewhere,
  and a `_` arm: 140 MIR blocks for `n = 6`, 526 for `n = 8`, 2064 for
  `n = 10`;
- one arm `Q::A(1 | 2 | 3, ..., 1 | 2 | 3)` over `k` positions of type
  `u8`, and a `_` arm: 49 blocks for `k = 4`, 373 for `k = 6`, 3289 for
  `k = 8`.

The patterns are small; the code is not. Both shapes are what a decoder
of flags or of register fields writes.

## Decision

### The arms are tested in order

A `match` on an enum with payloads becomes:

- a switch on the tag of the scrutinee's copy, with a case for each
  variant that an arm names at the top, as its pattern or as an
  alternative of its or-pattern;
- under each such case, the **chain** of that variant: in arm order, each
  top-level alternative that is the variant, and then the first
  **catch-all arm** (ADR 0037: its pattern is `_`, a `let`, or an
  or-pattern with a `_` alternative), which ends it, or, without one, the
  call to the panic function. Each alternative of the chain is tested in
  turn; the first that passes runs its arm, and one that fails goes to
  the next alternative of the chain. In `Msg::Write(1, _) => 0,
  Msg::Led(true) | _ => 1`, the chain of `Msg::Write` is `Msg::Write(1,
  _)`, then the second arm, and that of `Msg::Led` is `Msg::Led(true)`,
  then the second arm;
- the default of the tag switch goes to the first catch-all arm, or else
  to the call to the panic function: it is reached by the variants no arm
  names and by the tags that are no variant.

A catch-all arm is entered at one place, whichever chain or default
reaches it; a `let` there copies the scrutinee's copy into its variable
before the arm's expression.

An alternative is tested by its payload positions, in the order they are
written, depth first:

- `_` tests nothing; `let` copies its position into the arm's variable
  where the position is reached, before the tests after it, and tests
  nothing;
- a literal or a constant is a switch on its position with one case;
- a variant of an enum is a switch on its position, the integer or the
  tag of the enum, with one case, then the tests of its own payload
  positions;
- an or-pattern tests its alternatives in order: the first that passes
  goes on to the positions after the or-pattern, and one that fails goes
  to the next alternative; when the last fails, the pattern that contains
  the or-pattern fails. Alternatives written next to each other that are
  literals, constants or variants written without a payload are one
  switch with one case per value, as in `0x10 | 0x11` or
  `Mode::Off | Mode::Run`.

A test that fails goes to the failure of the alternative it belongs to,
never back into an or-pattern before it: the positions of a pattern are
disjoint, and a pattern reads no memory (ADR 0040), so which alternative
of an earlier or-pattern passed does not change whether a later position
matches.

### Where bindings are copied

ADR 0041 copies what an alternative binds where the `match` finds that
the alternative matches. Here a `let` is copied where its position is
reached, so that the positions after an or-pattern can be generated once
for all its alternatives (below): in `Msg::Write(let r: u8, 0) |
Msg::Clear(let r: u8)`, the value `Msg::Write(5, 1)` copies 5 into `r`
before the `0` fails. Such a copy is never read: a variable is visible
only in its arm's expression, which runs only when a whole alternative
passed, and that alternative copied every variable it binds on its way.

When several alternatives of an or-pattern match the same value, the
first one, left to right, binds, since it is the one tested first: with
`Msg::Pair(let a: u8, 7) | Msg::Pair(5, let a: u8)`, `Msg::Pair(5, 7)`
gives `a` the value 5. The M2.4b compiler already behaves so, and the
reference says it; this ADR makes it a decision.

### The code is linear in the patterns

Each pattern written is tested once, in one place: the positions after an
or-pattern are generated once, and every alternative that passes jumps to
them. A `match` thus has code proportional to the size of its patterns,
plus one jump per variant to its first catch-all arm or to a panic
call.
The arm's expression is generated once, as ADR 0041 says.

### Values that are no variant

A test at an enum position fails on a value that is no variant, as on any
variant it does not name, and the failure goes where any failure goes: to
the next alternative of an enclosing or-pattern, or else to the next
alternative of the chain. A chain without a catch-all arm ends in a call
to the panic function with the cause "enum value is no variant". Since
ADR 0040 makes every `match` exhaustive, only a nested value that is no
variant reaches it; at a variant whose arms test only `bool` and integer
positions no value reaches it at all, and the call is there all the
same: only a wrong exhaustiveness check would reach it, and the program
then panics rather than have no defined behaviour. This replaces ADR 0040's rule that the default of a
switch on an enum position goes to the arms that match there with `_` or
`let`: those arms are the rest of the chain. As in ADR 0036, nothing tells
LLVM that a tag or a payload value holds a variant.

`match`es on integers, `bool` and enums without payloads keep their single
switch (ADR 0037).

### Conformance tests

- **MIR and LLVM IR snapshots:** the tag switch with a case per named
  variant; a chain where a failed nested test goes to the next arm; the
  tag default going to a `_` arm, to an arm `Msg::Led(true) | _`, and
  to the panic call without a catch-all arm; a `let` arm reached from the
  tag default and from two chains through one entry; an
  or-pattern of values as one switch; an or-pattern of variants with
  payloads whose alternatives jump to one block for the positions after
  it; a chain ending in the panic call at a variant whose arms test only
  `bool` positions; two alternatives binding one variable, and the arm's
  expression generated once.
- **Size:** generated `match`es of the two shapes in "Context", with
  `n = 32` and `k = 32`, and an or-pattern of variants nested three deep
  (`enum N: u8 { A(N1) = 0, B(N1) = 1 }`, `N1` likewise over `N2`, `N2`
  over `u8`, the arm `N::A(N1::A(N2::A(1 | 2) | N2::B(3)) |
  N1::B(N2::A(4))) | N::B(_)` and a `_` arm), have at most 4 × (p + v)
  MIR blocks,
  where p counts every pattern written, each alternative of an
  or-pattern, `_` and `let` included, and the or-pattern itself, and v
  counts the variants of the scrutinee's enum.
- **Differential test** (ADR 0027): the shapes above, an arm
  `Msg::Led(true) | _`, and two overlapping alternatives binding one
  variable, at compile time and at run time.
- **e2e:** the same shapes on QEMU, at both optimization levels, and the
  existing tests of tags and nested values that are no variant.

## Consequences

- A `match` costs code in proportion to what it writes, whatever the
  shape of its patterns.
- An arm may test again a position an earlier arm of the chain already
  tested, as in `Msg::Write(0x10, 7)` followed by `Msg::Write(0x10 | 0x11,
  let v: u16)`; a decision tree tests each position at most once on any
  path. At `-O`, LLVM merges much of it; at `-O0` it costs what a chain of
  C `if`s costs.
- The end of each chain without a catch-all arm holds a call to the
  panic function that is often unreachable; it costs a few bytes.
- A `let` copy may be made for an alternative that then fails; it costs
  a move, and never changes what the arm sees.

## Alternatives considered

- **The decision tree, with identical subtrees shared** — tests each
  position at most once per path, but the number of distinct subtrees can
  still grow with the product of the patterns, so the size would stay
  exponential in the worst case.
- **A limit on the size of a `match`**, as an error — keeps the decision
  tree and its run-time cost, but rejects programs for the compiler's
  choice of code, not for anything they say.
- **`unreachable` at the end of a chain whose arms examine no enum
  position** — saves the call, but turns a wrong exhaustiveness check into
  undefined behaviour instead of a panic.
