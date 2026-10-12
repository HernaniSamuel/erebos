# 0007. Wrapping integer arithmetic

- Status: Proposed
- Date: 2026-10-12

## Context

On real hardware, integer addition, subtraction and multiplication wrap
around (two's complement). C makes signed overflow undefined behavior; Rust
panics in debug and wraps in release. Both diverge from what the silicon does.

## Decision

Integer types mirror hardware behavior:

- `+`, `-`, `*` **wrap** for both signed and unsigned types, in every build
  mode. In LLVM terms: never emit `nsw`/`nuw` flags on these operations.
- Signed integers are two's complement.
- No implicit widening, narrowing or sign conversion; use explicit `as`.

## Open questions

These operations have no single "hardware behavior" because targets differ,
and LLVM treats them as undefined or poison. A rule must be chosen before
the operators are implemented:

- **Shift by >= bit width** — x86 masks the amount, ARM does not. Options:
  mask (`amount % width`), or saturate (result 0 / sign fill).
- **Division / remainder by zero** — ARM `udiv` returns 0, RISC-V returns all
  ones, x86 traps. Options: trap via panic handler, or define a fixed result.
- **`MIN / -1`** for signed division.

## Consequences

- No hidden overflow checks or undefined behavior in arithmetic.
- Optional checked/saturating operations may be added later as intrinsics or
  operators, but wrapping stays the default.
