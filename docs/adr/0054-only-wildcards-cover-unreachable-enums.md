# 0054. Only `_` covers an enum that is not reachable

- Status: Accepted
- Date: 2026-10-20
- Amends: 0050 ("Variants": a `let` cannot cover a value whose type
  cannot be named, and the help of a non-exhaustive `match` names `_`
  only)

## Context

ADR 0050, "Variants", says that where an enum is not reachable, "`_` and
`let` patterns name no variant and stay allowed", and that the help of a
non-exhaustive `match` on such an enum "says that a `_` or `let` arm is
the way to cover them". Its conformance tests follow it.

A `let` pattern writes its type (`let n: T`, ADR 0008: types are never
inferred), and no path written where an enum is not reachable can name
it: each segment is checked (ADR 0050, "Items"), so writing it is
`resolve::private-item`. At a pattern position whose type is that enum,
or a type that can only be written by naming it, such as
`Option(uart::Mode)`, a `let` pattern therefore cannot be written, and a
help proposing one points at code that does not compile. A `let` of a
reachable type that holds the enum is another matter: with `pub enum Msg:
u8 { Clear = 0, Set(Mode) = 1 }` in `uart` and `Mode` private, the arm
`let m: crate::uart::Msg` written in a sibling covers every `Mode` inside
a `Msg`.

Accepted ADRs are not edited (ADR 0001, `docs/adr/README.md`), so the
rule is corrected here.

## Decision

- At a pattern position whose type is an enum that is not reachable, or
  a type that can only be written by naming such an enum, **only `_`**
  matches without naming a variant: as the whole pattern of an arm, or
  inside a payload (`.Some(_)`). A `let` pattern names no variant, so it
  is never `ty::private-variant`, but its type cannot be written at such
  a position. A `let` of a reachable type that holds the enum, written at
  a position of that type, still covers the enum's values inside it.
- The help of `ty::non-exhaustive-match`, when a shown witness names a
  variant of an enum not reachable from the module of the `match`, at any
  depth (`.Some(uart::Mode::Run)` as well as `uart::Mode::Run`), names `_`
  only: "`uart::Mode` cannot be named in module `sibling`, so neither can
  its variants: cover them with `_`, which names no variant (ADR 0037,
  0050, 0054)". `_` covers the missing values at any position, where a
  `let` of an enclosing type exists only at some.
- When the shown witnesses mix such values with writable ones (`match
  uart::msg() { .Set(.Off) => 0 }` misses `uart::Msg::Clear` and
  `uart::Msg::Set(uart::Mode::Run)`), the help still names only `_`, on
  purpose: one `_` arm covers all of them, while the writable arms alone
  would leave the others uncovered.
- Unlike the help of `ty::private-variant` in a pattern, this help does
  not suggest offering the enum with `pub` or `pub use`, on purpose: it
  tells how to complete the `match` where it is written, which the
  module of the `match` can do alone; `ty::private-variant` reports a
  pattern that names what it cannot, which either side can fix.

Nothing else in ADR 0050 changes.

## Consequences

- ADR 0050 now agrees with ADR 0008 and with the compiler, which gives
  this help (M2.6, step 6). The compiler does not change.
- A module that matches a value of an enum it cannot name treats all its
  values alike at that position, through `_`, or through a `let` of a
  reachable type that holds it; the module of the enum, or one that
  reaches it through `pub use`, can tell them apart.

## Alternatives considered

- **Keep "`_` or `let`" in the help.** It would propose an arm that is
  `resolve::private-item` at the positions the help is about.
- **Let a `let` pattern omit its type where it cannot be written.** Type
  inference in patterns, which ADR 0008 rules out.
- **List the writable missing arms beside `_`.** The values that cannot
  be written would still need `_`, which alone covers them all.

## Conformance tests

ADR 0050's item "a non-exhaustive `match` on a private enum in another
module, with its help" becomes the following; its item "`.V` of a private
enum in an argument and in a pattern, from its module and from another"
is kept as it is.

- **ui (typeck)** `typeck_private_variants.ers`, from a sibling of the
  enum's module:
  - accepted: a `_` arm, `.Some(_)` on an `Option` of the enum, and the
    arm `let m: crate::uart::Msg` on a reachable `pub enum Msg: u8 {
    Clear = 0, Set(Mode) = 1 }`, a `let` of a reachable type that holds
    the enum;
  - a non-exhaustive `match` on the enum, and one on an `Option` of it,
    each with the help naming `_` only;
  - mixed witnesses: `match crate::uart::msg() { .Set(.Off) => 0 }`
    misses `uart::Msg::Clear`, which could be written, and
    `uart::Msg::Set(uart::Mode::Run)`, which cannot; the help names `_`
    only.

  A witness inside a payload (`.Some(uart::Mode::Run)`,
  `uart::Msg::Set(uart::Mode::Run)`) arises only next to an arm that names
  a variant of the enum (`.Some(.Off)`, `.Set(.Off)`), itself
  `ty::private-variant`; without one the witness is `.Some(_)` or
  `uart::Msg::Set(_)`, which names no variant and keeps the usual help.
