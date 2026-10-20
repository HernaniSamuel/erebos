# 0020. Runtime support functions written in Erebos

- Status: Accepted; amended by 0052 (the runtime functions move into `core`)
- Date: 2026-10-13

## Context

Some operations have no instruction on the target. On RV32, `/` and `%` on
64-bit integers are lowered by LLVM to calls to `__udivdi3`, `__umoddi3`,
`__divdi3` and `__moddi3`; aggregates (M2) will add `memcpy` and `memset`.
Hosted toolchains get these from libgcc or compiler-rt. Erebos had nothing,
so 64-bit division compiled but failed to link.

ADR 0002 forbids hidden runtime: whatever runs must be visible.

## Decision

The support functions are written **in Erebos**, in `rt/builtins.ers`:

- The file is part of the repository and embedded in the compiler binary, so
  the compiler stays self-contained.
- `erebos build` compiles it together with every program. `--emit` dumps
  show the program alone, since the runtime is not called from IR; LLVM's
  backend inserts the calls.
- Names starting with `__` are reserved for it; a program defining one is an
  error.
- The functions use only operations the target has natively. In particular
  they never use 64-bit `/` or `%`, which LLVM would lower to a call back to
  themselves.

## Consequences

- Programs pay a few hundred bytes for functions they may not call, until
  unused-section removal is added.
- The runtime is tested by every end-to-end program that is built, and
  directly by one that divides 64-bit values.
- With modules and `core` (M2), this file becomes part of `core`.

## Alternatives considered

- **LLVM's compiler-rt** — would have to be built for bare-metal riscv32 from
  the LLVM sources, and is a large external dependency.
- **Generating the functions in the compiler** — no extra file, but the code
  would only be visible in `--emit=llvm-ir`, against the spirit of ADR 0002.
- **Expanding division inline at every use** — no calls, but much larger code.
