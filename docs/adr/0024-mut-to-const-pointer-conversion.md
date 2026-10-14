# 0024. Implicit conversion from `*mut T` to `*const T`

- Status: Accepted
- Date: 2026-10-14
- Amends: 0008

## Context

ADR 0008 forbids every implicit conversion, so that conversions which
change a value (widening, narrowing, reinterpreting the sign) are always
visible. M2.1 showed one conversion where the rule only adds noise:
passing a `*mut T` where a `*const T` is expected, as in
`memcpy(dest, src as *const u8, n)` with `src: *mut u8`. It is the most
common conversion in low-level code, and the cast tells the reader nothing:
the address, the pointee and the volatility stay the same, and the pointer
only loses permission to write. Rust performs it implicitly.

## Decision

Wherever a type is expected, a value of type `*mut T` is accepted as
`*const T`, with the same `T` and the same volatility:

```
fn checksum(data: *const u8, n: usize) -> u32 { ... }

let buf: *mut u8 = ...;
checksum(buf, 16);                    // ok, no cast
let view: *const u8 = buf;            // ok
```

- It applies wherever bidirectional checking supplies an expected type:
  `let`, arguments, assignments, `return`, `break` values, struct fields,
  array elements and `if` branches with an expected type.
- Comparisons accept a `*mut T` and a `*const T` in either order.
- Only the outermost pointer converts: `*mut *mut u8` becomes
  `*const *mut u8`, never `*mut *const u8`, which would allow storing a
  `*const` where a `*mut` is expected to be.
- Volatility never changes implicitly (ADR 0013), and nothing converts from
  `*const T` to `*mut T`.

Every other conversion stays explicit (ADR 0008).

## Consequences

- No casts that only drop write permission.
- The conversion is free: all pointers have one representation, so MIR and
  code generation are unchanged.
- An `if` without an expected type still takes the type of its first
  branch; its branches must then agree exactly.

## Alternatives considered

- **Keep every conversion explicit** — consistent, but pays for the
  harmless case with noise in exactly the code that is hardest to read.
- **Also convert between volatile and non-volatile pointers** — would hide
  whether accesses are volatile, which ADR 0013 wants decided at the
  declaration.
