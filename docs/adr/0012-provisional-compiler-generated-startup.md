# 0012. Provisional compiler-generated startup code

- Status: Accepted
- Date: 2026-10-12

## Context

Before the entry function can run, the processor needs a stack pointer, and
something must sit at the reset address. ADR 0002 forbids hidden runtime, but
the language cannot yet express this itself: inline `asm` arrives in M4 and
memory maps in M3.

## Decision

Until the language can express startup code, the compiler emits a minimal
`_start` as module-level assembly in section `.text.start`, which the
generated linker script places first in RAM (the reset address on QEMU
`virt` with `-bios none`, ADR 0010):

```asm
_start:
    li sp, <end of RAM>
    j  <entry function>
```

Rules that keep it honest:

- It does nothing else: no `.bss` zeroing, no `.data` copy, no trap setup.
  M0 has no statics, so none of that is needed yet.
- It is visible in `--emit=llvm-ir` (`module asm` lines).
- `_start` is a reserved symbol name while this ADR is in effect.

## Consequences

- M0 can boot real code without inventing language features early.
- The stack size is implicit (all RAM above the program). Explicit stack
  placement comes with `memory { ... }` blocks in M3.
- This ADR must be superseded when M3/M4 land, by one that moves startup into
  the language (or into a visible part of `core`).

## Alternatives considered

- **A hand-written assembly file shipped with the compiler** — same behavior,
  but adds an assembler step and a file to locate at runtime.
- **Waiting for inline `asm`** — would block M0 on M4.
