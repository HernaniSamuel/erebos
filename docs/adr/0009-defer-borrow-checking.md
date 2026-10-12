# 0009. Defer borrow checking

- Status: Accepted
- Date: 2026-10-12

## Context

A Rust-like ownership model is a goal, and `polonius-engine` was considered
for borrow checking. Polonius is only a Datalog solver: the frontend must
produce its input facts (loans, regions, CFG points) from a complete MIR, and
it is built around rustc's region model.

## Decision

- Early milestones use raw pointers and plain values; no borrow checking.
- MIR is designed from the start to support it later: explicit places,
  explicit moves/copies, explicit borrows, and a CFG with addressable points.
- Whether to use `polonius-engine` or a simpler in-house analysis is decided
  in a future ADR, once MIR exists.

## Consequences

- Faster path to running code on hardware.
- Early programs are memory-unsafe in the same way C is. Accepted for now.
