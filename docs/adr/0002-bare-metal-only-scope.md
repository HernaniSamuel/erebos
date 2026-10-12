# 0002. Bare-metal-only scope

- Status: Accepted
- Date: 2026-10-12

## Context

Systems languages such as C and Rust serve both hosted (OS-backed) and
freestanding environments. Supporting both forces compromises: a standard
library that assumes an OS, a runtime, `no_std` as an opt-out, and hardware
concepts (MMIO, interrupts, memory maps) expressed through library workarounds.

## Decision

Erebos targets **bare metal only**. From the language's point of view, the
physical machine is the only thing that exists:

- No operating system, syscalls, processes, files or environment.
- No hidden runtime. Nothing runs before or after user code unless the program
  (or an explicit, visible part of `core`) says so.
- No heap or allocator in the language. Allocation, if ever, is a library concern.
- No unwinding. Panics go to a user-defined handler.
- Hardware concepts become **language constructs** over time instead of
  library patterns: memory maps, peripherals/registers (MMIO), bitfields,
  interrupts, vector tables, sections and placement.

## Consequences

- The language can make strong guarantees about hardware access (e.g. reject
  writes to read-only registers at compile time).
- Hosted use (CLI tools, tests running on the dev machine) is out of scope;
  testing happens on emulators (QEMU) and real boards.
- Every feature proposal must answer: "does this help writing code for the
  bare machine?" If not, it is out of scope.

## Alternatives considered

- **General-purpose with a freestanding mode** — this is exactly the
  compromise the language exists to avoid.
