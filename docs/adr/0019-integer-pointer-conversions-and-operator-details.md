# 0019. Integer/pointer conversions and operator details

- Status: Proposed
- Date: 2026-10-13

## Context

Several typing rules were implemented in M1 without a record. Most are
uncontroversial; one, how integers of other widths become pointers, is
semantics that is hard to change later.

Today any integer type converts to and from a pointer with `as`, and the
conversion **zero-extends** (LLVM `inttoptr`), whatever the signedness: on a
64-bit target, `-1_i8 as *mut u8` would be `0xFF`, while Rust sign-extends
signed integers and gives `0xFFFF_FFFF_FFFF_FFFF`.

## Decision

**Integer/pointer conversions (proposed change):** only `usize` converts to a
pointer and a pointer only converts to `usize`. Other integer types first
convert to `usize`, which makes the extension explicit and follows the
integer rules (`x as usize` sign-extends a signed `x`). A literal operand
already takes `usize` when cast to a pointer (ADR 0017), so the common
`0x1000_0000 as *mut volatile u8` is unaffected.

**Recorded as implemented:**

- Pointers of the same type compare with all comparison operators, by
  address, as unsigned values.
- `!` is logical NOT on `bool` and bitwise NOT on integers.
- A shift amount may be any integer type, independent of the shifted type,
  and is masked to the shifted type's width (ADR 0007).
- Pointer-to-pointer casts may change mutability, volatility and pointee.

## Consequences

- With the proposed change, no conversion between integers and pointers
  extends a value silently.
- Code converting a `u32` address must write `addr as usize as *mut T`.

## Alternatives considered

- **Any integer width, Rust's extension rules** — familiar, but the extension
  is invisible at the conversion.
- **Keep the current behavior** — zero-extension of signed values differs
  from Rust and surprises; it only goes unnoticed because the target is 32-bit.
