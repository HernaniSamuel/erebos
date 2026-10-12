# 0016. Literals: integers and bytes; no chars, strings or floats

- Status: Accepted
- Date: 2026-10-12

## Context

Bare-metal code talks to hardware in bytes and integers: registers, UART
data, protocols. Unicode characters and strings bring encodings, variable
widths and several meanings of "length". Floating point brings IEEE 754
semantics, and our target (RV32IMAC, ADR 0010) has no floating-point unit.

## Decision

**Integer literals:**

- Bases: `0b1010`, `0o17`, `42`, `0x2A`. Digits may be separated by `_`.
- A decimal literal with a leading zero (`017`) is an error suggesting `0o17`,
  avoiding C's silent octal (`010 == 8`).
- A literal's type comes from the expected type (ADR 0008); a value that does
  not fit that type is an error.

**Byte literals:**

- `b'h'` has type `u8`.
- `b"hello"` has type `[u8; 5]` (with arrays, M2).
- Only printable ASCII may be written directly. Escapes: `\n \r \t \\ \' \" \0`
  and `\xNN` for any byte.

**Not provided:** `char`, Unicode strings and floating-point types. Text is
bytes; fractional math uses fixed point.

## Consequences

- One simple, exact model of text: what is written is what goes over the wire.
- Non-ASCII text is spelled as UTF-8 bytes (`b"caf\xC3\xA9"`), which is
  clumsy but explicit.
- Unicode text or floats can be proposed later in a new ADR if a real need
  appears.
