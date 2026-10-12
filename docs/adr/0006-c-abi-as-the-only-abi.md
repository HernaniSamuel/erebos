# 0006. C ABI as the only ABI

- Status: Accepted
- Date: 2026-10-12

## Context

Designing a custom calling convention is significant work with little benefit
for bare-metal code, and interoperating with existing C code (vendor HALs,
startup files, assembly) is valuable.

## Decision

All Erebos functions use the **target's C ABI** (AAPCS on ARM, the RISC-V
psABI on RISC-V, etc.). There is no separate "Erebos ABI".

- Any function can be called from C and any C function can be declared and
  called from Erebos (FFI comes for free).
- Data layout is deterministic and C-compatible by default (declaration order,
  natural alignment), so structs can be shared with C as-is.
- Special conventions (interrupt handlers, naked functions) are explicit
  attributes, not separate ABIs.

## Consequences

- LLVM does **not** perform C ABI lowering for aggregates: the frontend must
  classify how structs are passed and returned per target (as clang does).
  Until a per-target ABI module exists, **only scalars and pointers** may be
  passed and returned by value across functions; aggregates go by pointer.
- No ABI-level optimizations (e.g. struct field reordering) are available.
  This is consistent with the language's "what you write is what the hardware
  sees" goal.

## Alternatives considered

- **Unspecified internal ABI + `extern "C"`** (Rust's model) — more freedom
  for the compiler, but two ABIs to understand and test.
