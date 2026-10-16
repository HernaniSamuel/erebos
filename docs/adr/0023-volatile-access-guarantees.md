# 0023. Volatile access guarantees

- Status: Accepted; amended by 0031 (slices are aggregates for volatile accesses), 0035 (an enum is a scalar)
- Date: 2026-10-14

## Context

ADR 0013 promises that volatile accesses happen exactly once and in program
order, but says nothing about their **width**, or about what happens when a
single access is impossible. In M1 that never came up: every volatile access
was a whole scalar of at most 32 bits. M2 adds fields, elements and
`#[packed]` structs (ADRs 0021, 0022), which make the question concrete.

Device registers care about both width and count. A 32-bit register written
as four byte stores may latch a partial value; reading a FIFO data register
twice pops two entries. C leaves this implementation-defined, and compilers
silently split accesses they cannot perform in one instruction (a 64-bit
volatile access on a 32-bit core becomes two).

This ADR refines ADR 0013; it does not replace it.

## Decision

### The guarantee

Every volatile access is an access to a **scalar** (an integer, `bool` or a
pointer), performed as **exactly one load or store instruction of the
scalar's width**. The compiler never splits, merges, widens, narrows,
duplicates, removes or reorders volatile accesses relative to each other.

**If the compiler cannot keep this promise, the program is rejected**:

| Error | Case |
|-------|------|
| `ty::volatile-aggregate` | reading or writing a whole struct or array through a volatile pointer |
| `ty::volatile-too-wide` | a scalar wider than the target's widest single access; on riscv32, `u64` and `i64` |
| `ty::volatile-packed-field` | a scalar with alignment above 1 reached through a field of a `#[packed]` struct at any depth (`(*p).len`, `(*p).inner.x`, `(*p).arr[i]`), since it may be misaligned and would be split (ADR 0022) |

A 64-bit device register on riscv32 is read as two explicit 32-bit accesses,
so their order is visible in the source. That order matters, as in the
classic `mtime` read: high, low, then high again to detect a carry.

### Places

Volatility flows from the pointer through field and index projections down
to the scalar finally accessed:

```
(*p).ctrl            // one access of ctrl's width
(*p).chan[i].status  // one access of status's width, at the computed offset
```

- An index is evaluated, and its bounds check performed, before the access.
  The bounds check reads no memory.
- Each occurrence in the source is one access: `(*p).x + (*p).x` performs two
  loads, left to right (ADR 0018).
- `(*p).x += 1` performs one volatile load, then one volatile store. The pair
  is not atomic: an interrupt may run between them.
- An access to a zero-sized type (`()`) performs no memory access.
- Unchecked pointer indexing keeps volatility: with
  `p: *mut volatile u32`, `p[i]` is one volatile 32-bit access at
  `p + 4 * i` (ADR 0021).
- A volatile `bool` is one byte access. Any nonzero byte reads as `true`
  (ADR 0022), so a device returning another value does not make the program
  undefined. Device registers are still better declared as `u8` or wider and
  tested explicitly, since a flag is rarely a whole byte.

### Ordering

Volatile accesses are emitted in program order relative to each other. They
are **not** ordered relative to ordinary (non-volatile) accesses, which the
optimizer may move across them. The compiler emits no fences.

**Known limitation until M4.** The classic device hand-off (fill a DMA
descriptor or buffer in RAM, then write a "doorbell" register to start the
device) cannot be written with a guarantee: the compiler may move the
ordinary writes to RAM after the volatile doorbell write. Two partial
workarounds exist until fences arrive:

- writing the descriptor through a volatile pointer too orders it with the
  doorbell **in the generated code**, at the cost of unoptimized accesses;
- it does **not** order them **on the bus**: on RISC-V, RAM and I/O accesses
  may still be reordered by the hardware without a `fence`. QEMU does not
  reorder, so tests pass anyway, which is exactly why this is written down.

M4 must provide a compiler fence and the target's hardware fences.

### What depends on the target

- **The widest single access:** 32 bits on riscv32. A 64-bit target would
  allow `u64`.
- **Misaligned pointers:** the compiler assumes a pointer is aligned for its
  pointee (ADR 0019) and still emits one instruction; whether a misaligned
  address traps, is emulated or works is up to the hardware.
- **Ordering as observed by the device:** whether the bus and the device
  see accesses in program order depends on the memory model and the region.
  On RISC-V, I/O regions are usually strongly ordered through their physical
  memory attributes, but communication through RAM, such as DMA buffers,
  needs `fence` instructions. Fences arrive with inline assembly (M4) or
  intrinsics.
- **Which widths a register accepts:** for example, whether a byte store to
  a 32-bit register is legal is a property of the device, not the language.
  `peripheral` blocks (M3) will be able to state it.

### Conformance tests

M2.1 ships with:

- **LLVM IR snapshots**, at both optimization levels, showing one volatile
  load or store of the right width for: a scalar, a field, a nested field,
  an array element, a field inside an array element, a compound assignment
  (one load, one store) and an operand read twice (two loads, in order).
  With `-O`, none of them may be merged, removed or reordered.
- **ui snapshots** for `ty::volatile-aggregate`, `ty::volatile-too-wide` and
  `ty::volatile-packed-field`, the last one for a direct field and for a
  nested path.
- **LLVM IR snapshots** for volatile pointer indexing and for a volatile
  `bool` read (an 8-bit load and a compare).
- **An e2e program** that drives the UART through a struct describing its
  registers.

## Consequences

- MMIO through volatile pointers has a precise, checkable meaning.
- A few programs are rejected that C would compile into split accesses.
- The new errors arrive in M2.1; no existing program uses volatile 64-bit
  accesses.
- Codegen tests can verify the guarantee at the LLVM IR level.
- Until M4, programs cannot order RAM writes before a volatile access with
  a guarantee; drivers that hand memory to devices wait for fences.

## Alternatives considered

- **Split silently, as C compilers and Rust do for wide types** —
  convenient, but the split order and width are invisible, and for devices
  they are the whole point.
- **Allow wide volatile accesses and document that they split** — still
  turns a register access into two accesses without a sign at the use site.
