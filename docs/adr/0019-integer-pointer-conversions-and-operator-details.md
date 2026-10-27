# 0019. Conversions, pointer representation and pointer validity

- Status: Accepted; amended by 0031 (only thin pointers are addresses), 0059 (a bit-field struct converts to integers, and from its own integer type)
- Date: 2026-10-13

## Context

Conversions with `as` were implemented in M1 without a record, and some of
their semantics would be hard to change later: how integers widen and
narrow, what a pointer is as a number, and what converting an integer to a
pointer does and does not promise.

Until this decision, any integer type converted to and from a pointer, and
integer-to-pointer conversion **zero-extended** (LLVM `inttoptr`) whatever
the signedness: on a 64-bit target, `-1_i8 as *mut u8` would have been `0xFF`,
while Rust sign-extends signed integers. It went unnoticed only because the
one target is 32-bit.

## Decision

### Integer conversions

`x as T` between integer types is defined for every pair, never panics and
depends only on the source's signedness and the two widths:

| Case | Result | Example |
|------|--------|---------|
| Widening, signed source | sign-extends | `-1_i8 as u32 == 0xFFFF_FFFF` |
| Widening, unsigned source | zero-extends | `255_u8 as i32 == 255` |
| Narrowing | keeps the low bits (truncates) | `300_u32 as u8 == 44`, `-1_i32 as u8 == 255` |
| Same width | reinterprets the bits | `-1_i32 as u32 == 4_294_967_295`, `65535_u16 as i16 == -1` |
| `bool` to integer | `false` is 0, `true` is 1 | `true as u8 == 1` |

There is no conversion to `bool` (write `x != 0`). This matches Rust and is
what two's-complement hardware does (ADR 0007).

### Integers and pointers

Only `usize` converts to a pointer, and a pointer only converts to `usize`
(error `ty::pointer-cast-width` otherwise). Other integer types first convert
to `usize` under the rules above, so any extension is always visible:

```
let addr: u32 = 0x8000_1234;
let p: *mut u8 = addr as usize as *mut u8;
```

A literal operand already takes `usize` when cast to a pointer (ADR 0017), so
`0x1000_0000 as *mut volatile u8` is unaffected.

### Pointer representation

On every target Erebos supports, a pointer is a plain address in a single,
flat address space, and **`usize` is defined to be exactly as wide as a
pointer**. Therefore:

- `p as usize` is the numeric address of `p`;
- `a as *mut T` (with `a: usize`) is the pointer to address `a`;
- the round trip is lossless in both directions.

This is a property each target must have to be supported: a target whose
pointers are not plain addresses (capabilities, segmented or multiple
address spaces, Harvard data/program memories) requires a new ADR before
Erebos supports it. Today the only target is riscv32 (ADR 0010).

### Pointer validity

**A conversion produces a pointer value; it does not make the pointer valid
to dereference.** The compiler does not and cannot check, at conversion or
at `*p`, that:

- something exists at that address (RAM, ROM or a device register);
- the address is aligned for the pointee type (a `*mut u32` must be 4-byte
  aligned; misaligned accesses may trap on real hardware);
- the access is allowed (writing to ROM, reading a write-only register);
- reading has no side effects (device registers often have them; use
  `volatile`, ADR 0013).

Dereferencing an invalid pointer behaves as the hardware does (a fault, a
bus error, or silent garbage); it is the programmer's responsibility. Until
ownership and borrowing exist (ADR 0009), raw pointers are as unchecked as
in C.

**Address 0 is an ordinary address.** On bare metal it may be real memory,
for example a vector table. The compiler tells LLVM that null accesses are
valid (`null_pointer_is_valid`), since by default LLVM treats them as
undefined and optimizes them away.

### Operator details

- Pointers of the same type compare with all comparison operators, by
  address, as unsigned values.
- `!` is logical NOT on `bool` and bitwise NOT on integers.
- A shift amount may be any integer type, independent of the shifted type,
  and is masked to the shifted type's width (ADR 0007).
- Pointer-to-pointer casts may change mutability, volatility and pointee.

## Consequences

- No conversion between integers and pointers extends a value silently.
- Code holding an address in a `u32` writes `addr as usize as *mut T`.
- Supporting a target with non-address pointers is an explicit decision.
- Pointer validity stays unchecked; later milestones may add safer ways to
  reach memory (`peripheral` blocks in M3, references in M5).

## Alternatives considered

- **Any integer width, Rust's extension rules** — familiar, but the extension
  is invisible at the conversion.
- **Keep the previous behavior** — zero-extending signed values differs from
  Rust and surprises.
