# 0018. Evaluation order

- Status: Accepted; amended by 0031 (the message of `assert` runs only if it fails)
- Date: 2026-10-13

## Context

With volatile accesses (ADR 0013), function calls and assignments inside
expressions, the order in which operands run is observable: a register read
twice, or a variable read after an assignment in the same expression, gives
different results under different orders. The compiler already follows one
order (M1); it was decided in code without a record.

## Decision

Erebos follows Rust's order:

- **Operands run left to right**: both sides of a binary operator, the
  arguments of a call, and `&&`/`||` (whose right side may not run at all).
- **Assignment runs the value first, then the place**: in `*p = f()`, `f()`
  runs before `p` is read. Compound assignment (`x += e`) runs `e`, then reads
  and writes the place.
- **A read happens where it is written.** Reading a local is deferred to its
  use only while nothing in between can change it; reading through a pointer
  (`*p`) always happens at its position, so a volatile read runs exactly once,
  in order.

## Consequences

- Expressions with side effects have one meaning, independent of target or
  optimization level.
- MIR building must preserve this; it copies a local early when a later
  operand could assign it.

## Alternatives considered

- **Unspecified order (C)** — leaves room for optimizers, but makes code with
  volatile reads or calls in one expression ambiguous.
- **Place before value in assignment** — also defensible; Rust's choice was
  kept for familiarity.
