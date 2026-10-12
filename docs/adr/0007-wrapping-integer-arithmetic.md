# 0007. Wrapping integer arithmetic

- Status: Accepted
- Date: 2026-10-12

## Context

On real hardware, integer addition, subtraction and multiplication wrap
around (two's complement). C makes signed overflow undefined behavior; Rust
panics in debug and wraps in release. Both diverge from what the silicon does.

Some operations have no single "hardware behavior", and LLVM treats them as
undefined or poison:

| Operation              | RISC-V              | ARM                        | x86   |
|------------------------|---------------------|----------------------------|-------|
| shift by >= width      | masks the amount    | 8-bit amount (`<< 32` = 0) | masks |
| division by zero       | all ones            | 0, or trap if configured   | traps |
| signed `MIN / -1`      | `MIN`, remainder 0  | `MIN`                      | traps |

## Decision

Integer types mirror hardware behavior:

- `+`, `-`, `*` and unary `-` **wrap** for signed and unsigned types, in every
  build mode. In LLVM terms: never emit `nsw`/`nuw` flags.
- Signed integers are two's complement.
- No implicit widening, narrowing or sign conversion; use explicit `as`.

Where hardware disagrees:

- **Shifts mask the amount** to the operand width: `x << n` means
  `x << (n % width)`. This is what RISC-V (our target) and x86 do, so it is
  free. A **constant** shift amount `>= width` is a compile-time error,
  catching the common mask-building bug (`(1 << 32) - 1`).
- **Division and remainder by zero trap**: they call the panic mechanism
  (defined in M1). Hardware disagrees and the result would be garbage
  anyway, so failing loudly beats target-dependent values. The check costs a
  compare and branch, removed by LLVM when the divisor is a nonzero constant.
- **Signed `MIN / -1` wraps to `MIN`, and `MIN % -1` is `0`**. The true
  result does not fit and wraps like `MIN * -1`; unlike division by zero, it
  has a mathematical answer. This matches RISC-V and ARM. Because LLVM's
  `sdiv`/`srem` are undefined for it, codegen guards the `-1` divisor.

## Consequences

- No hidden overflow checks and no undefined behavior in integer arithmetic.
- Division is not a single instruction: it carries a zero check and, for
  signed types, a `-1` guard. Unchecked division may be offered later as an
  intrinsic for code that proves its divisor nonzero.
- `x << 32` on a `u32` is `x`, which can surprise; the constant case is
  rejected at compile time, the dynamic case is the programmer's to handle.
- Checked or saturating operations may be added later as intrinsics or
  operators, but wrapping stays the default.
