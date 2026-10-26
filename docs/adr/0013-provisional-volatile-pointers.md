# 0013. Provisional volatile pointers

- Status: Accepted; amended by 0058 (accesses through a pointer to a peripheral type are volatile without `volatile`, which is an error there)
- Date: 2026-10-12

## Context

M1 writes to memory-mapped devices (UART, test finisher) through raw
pointers. Without volatile semantics, LLVM may delete, merge or reorder those
accesses: to an optimizer, writing the same address twice is redundant.
Native peripheral declarations, where the compiler knows a place is a device
register, only arrive in M3.

## Decision

Volatility is part of the pointer type:

```
*const T            *mut T              // ordinary memory
*const volatile T   *mut volatile T     // every access is a volatile access
```

- Reads and writes through a `volatile` pointer are emitted as volatile loads
  and stores, exactly once each, in program order relative to other volatile
  accesses.
- Converting between volatile and non-volatile pointers requires `as`.
- Integers convert to pointers (and back) only with `as`.

## Consequences

- An access cannot be "forgotten" as non-volatile at the use site; the
  declaration decides.
- This is a stopgap in the spirit of ADR 0002: once `peripheral` blocks exist
  (M3), MMIO should be written through them, and this ADR will be revisited
  (volatile pointers may remain for rare cases or be removed).

## Alternatives considered

- **Intrinsics (`volatile_write(p, v)`)** — easy to forget at one call site;
  exactly the workaround style the language wants to replace.
- **All raw pointer accesses volatile** — simple, but pessimizes ordinary
  memory accesses once arrays and structs exist.
