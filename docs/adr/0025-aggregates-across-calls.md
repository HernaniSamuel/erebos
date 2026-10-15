# 0025. Aggregates across calls on riscv32

- Status: Accepted; amended by 0031 (slices across calls)
- Date: 2026-10-14

## Context

ADR 0006 makes the target's C ABI the only ABI, and notes that LLVM does
not lower aggregates for it: the front end must classify how each struct is
passed and returned, as clang does. Until now, arrays and structs could not
be parameters or return types at all (`ty::aggregate-by-value`). M2.2 lifts
that restriction: `Result` (M2.5) is only useful if functions can return it.

The rules below are the integer calling convention of the RISC-V psABI for
ilp32 (no floating-point registers on our target, ADR 0010). They were
checked against clang 22 for `riscv32-unknown-elf`, `-mabi=ilp32`.

## Decision

Every function, Erebos or C, passes and returns aggregates (structs and
arrays) by value as the riscv32 C ABI says. With XLEN = 32 bits:

| Aggregate | As an argument | As a return value |
|-----------|----------------|-------------------|
| size 0 | not passed at all | nothing is returned |
| size up to 4 bytes | one register, holding the bytes as laid out in memory | `a0` |
| size up to 8 bytes, alignment 8 | a register pair, as a 64-bit integer | `a0`, `a1` |
| size up to 8 bytes, alignment up to 4 | a register pair, as two 32-bit words | `a0`, `a1` |
| larger than 8 bytes | a pointer to a copy that the caller makes | into memory the caller provides, whose address is passed as a hidden first argument |

- A pair that finds only one argument register left goes half in that
  register and half on the stack; with no register left, it goes on the
  stack. Larger aggregates follow the same rule for their pointer.
- A large argument is a copy: the callee may write to it without the
  caller seeing the change, as with any parameter.
- Arrays pass exactly like a struct of the same size and alignment. C has
  no arrays by value; a C twin wraps the array in a struct.
- Zero-sized types (ADR 0022) take no register and no stack space, as C
  compilers do with empty structs, which they support as an extension. A
  C declaration of the same function leaves such parameters out.
- Scalars are unchanged: integers narrower than 32 bits are extended as
  before (ADR 0006), and `u64`/`i64` use a register pair.
- Values are copied in their layout (ADR 0022); bytes of padding and of the
  unused part of a register are unspecified, as in memory.

This is target-specific code in code generation, next to the other target
details; MIR stays target-independent and passes aggregates as ordinary
values.

## Consequences

- Structs and arrays become ordinary parameters and return values, and
  Erebos functions can exchange them with C functions compiled by clang or
  GCC.
- Large aggregates cost a copy per call; returning one writes it straight
  into the caller's memory.
- `ty::aggregate-by-value` disappears.
- A second target needs its own classification; the table above is the
  riscv32 one.
- The guarantee is tested against clang itself: the M2.2 program calls C
  functions with aggregates of every row of the table, and C functions call
  Erebos ones.

## Alternatives considered

- **Always by pointer, for both arguments and results** — simple, but not
  the C ABI, so calls to and from C would silently disagree.
- **An internal Erebos convention plus the C ABI at boundaries** — what
  ADR 0006 rejected: two ABIs to understand and test.
