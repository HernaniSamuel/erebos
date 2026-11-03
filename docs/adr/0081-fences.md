# 0081. Fences

- Status: Accepted
- Date: 2026-11-03
- Amends: 0023 (the compiler provides a compiler fence and a hardware
  fence; the hand-off of memory to a device can be written with a
  guarantee)

## Context

ADR 0023 orders volatile accesses among themselves, not relative to
ordinary ones, and emits no fences. It names the case that needs more:
a driver fills a descriptor or a buffer in RAM with ordinary writes,
then writes a device's "doorbell" register. The compiler may move the
ordinary writes after the volatile one, and the hardware may make them
visible to the device after it: RISC-V orders I/O and memory accesses
only through `fence`, and ARMv7-M orders accesses to Normal and Device
memory, as seen by other bus masters, only through `dmb` or `dsb`. The
reverse case, reading a buffer after a device's "done" status, is the
same. ADR 0023 asked M4 for a compiler fence and the target's hardware
fences.

The handlers of M4.3 need compiler fences too, and `critical` blocks
(ADR 0078) are compiler fences.

LLVM's `fence` instruction orders memory among threads; on riscv32 it
becomes `fence rw,rw`, which leaves out I/O, and it says nothing about
devices.

## Decision

`core::arch` (ADR 0080) has two portable functions, written with
`asm` (ADR 0079):

```
pub fn compiler_fence()
pub fn fence()
```

- **`compiler_fence()`** emits no instruction. No access to memory that
  other code can see (statics, memory reached through pointers),
  ordinary or volatile, moves across it in the generated code. It is an
  `asm` with an empty template and the options `nostack` and
  `preserves_flags` only, none about memory, so it is ordered like any
  `asm` (ADR 0079). It orders nothing in the hardware: enough for
  code and a handler on one core, which see memory in program order.
- **`fence()`** is a compiler fence and a hardware fence: every access
  to memory or to a device before it, ordinary or volatile, is observed
  by every other observer (a device, a DMA engine, another bus master)
  before any access after it. On riscv32 it is `fence iorw, iorw`; on
  thumbv7m, `dmb sy`, with the same options. It orders; it does not
  wait for a write to reach a device, nor make a cache coherent (neither target of the tests has
  a data cache).

The hand-off of ADR 0023 is written:

```
DESCRIPTORS[0] = Descriptor { addr: buf, len: 64, flags: 1 };
core::arch::fence();
DEVICE.doorbell = 1;
```

Finer fences, which order only some kinds of access (`fence w,o` on
RISC-V, `dmb st` or `dsb` on ARM), are not portable items: a program
that needs one writes its `asm`, and `core::arch` may gain
target-specific ones with the step that needs them. Calling either
function at compile time is `eval::asm`.

## Consequences

- The known limitation of ADR 0023 is lifted: drivers can hand memory to
  devices with a guarantee, in the code and on the bus.
- `fence()` costs one instruction that may stall the pipeline; using it
  where a compiler fence suffices is correct but slower.
- `fence()` is stronger than most hand-offs need, chosen so that one
  portable function is right everywhere.
- QEMU does not reorder, so the tests can check the instruction is
  there, not that it is needed.

## Alternatives considered

- **LLVM's `fence` instruction** — leaves I/O out on riscv32, and its
  meaning is ordering among threads, not devices.
- **Built-in functions known to the compiler** — the same instructions
  with more code generation; `asm` in `core` does it in the language.
- **Only `asm` in programs** — every driver would repeat the instruction
  of its target, and portable drivers could not exist.
- **Several portable fences (`read_fence`, `write_fence`)** — the
  kinds of ordering do not map one to one between RISC-V and ARM; one
  full fence is right on both, and finer ones stay specific.
