# 0047. Zero-sized payloads have an offset

- Status: Accepted
- Date: 2026-10-19
- Amends: 0045 (the `Option(())` row of the layout table)

## Context

ADR 0045's layout table gives the payload of `Option(())` as "none":

| Type | Size | Alignment | Payload at |
|------|------|-----------|------------|
| `Option(())` | 1 | 1 | none |

ADR 0039, which ADR 0045 follows for layout, lays each variant out as a
struct of the tag followed by its payload values, each value as close
after the previous one as its alignment allows, and counts zero-sized
types among payload types: they take no bytes (ADR 0022), but nothing
exempts them from having a place. The compiler follows ADR 0039: the
`typeck` dump shows `enum Option(()): u8 { None = 0, Some(() at 1) = 1 }
size 1, align 1`, and `docs/reference.md` gives the payload as "1, of
size 0". The size and alignment in the table agree with this; only the
offset cell disagrees.

The offset is not a formality. A zero-sized payload of larger alignment
moves and grows the enum: `Option([u32; 0])` has its payload at 4, and
size 4 and alignment 4, as ADR 0039 says of `B(u8, [u32; 0])`. A rule
in which zero-sized payloads have no offset would leave that case
without an answer.

Accepted ADRs are not edited (ADR 0001, `docs/adr/README.md`), so the
table is corrected here.

## Decision

A zero-sized payload value has an offset, given by ADR 0039's struct
rule like any other payload value, and takes no bytes there. ADR 0045's row
becomes:

| Type | Size | Alignment | Payload at |
|------|------|-----------|------------|
| `Option(())` | 1 | 1 | 1, of size 0 |

Nothing else in ADR 0045 changes: the size, the alignment, the calling
convention (one register, ADR 0025) and the C twin. As ADR 0039 says, a
C twin leaves out a zero-sized payload of alignment 1, which changes no
offset:

```c
union OptionUnit {
    uint8_t tag;                   /* 0: None */
    struct { uint8_t tag; } some;  /* 1: Some */
};
```

## Consequences

- ADR 0045 now agrees with ADR 0039, the compiler and
  `docs/reference.md`. The compiler does not change; tests are added.
- The offset of a zero-sized payload is not observable from a program
  today (payload bindings are copies, ADR 0040, and `offset_of` does not
  take an enum, ADR 0039); it shows in the dumps and decides the layout
  of a zero-sized payload of larger alignment.
- The layout test keeps a member for the zero-sized payload:
  `tests/layout/structs.h` writes `struct {} p0`, a GNU C extension of
  size 0 that clang accepts. `tests/c_layout.rs` checks the offset of
  every payload value of the dump against clang, so this member is what
  checks the offset this ADR is about; the twin a program would publish
  leaves it out, as above.

## Alternatives considered

- **Keep "none" and give zero-sized payloads no offset in the
  compiler.** An exception to ADR 0039's rule, needing its own case in
  the layout, the dumps and MIR places, and no answer for a zero-sized
  payload whose alignment is larger than the tag's.
- **Correct the table in ADR 0045 itself.** Accepted ADRs are immutable;
  only their status line changes.

## Conformance tests

- **ui (typeck):** `typeck_option_result.ers` already shows `Some(() at
  1)` and asserts `size_of(Option(())) == 1`. To add there:
  `size_of(Option([u32; 0])) == 4` and `align_of(Option([u32; 0])) ==
  4`, with the dump showing `Some([u32; 0] at 4)`.
- **Layout against clang:** `tests/layout/structs.h` already checks the
  offset of `Option(())`'s payload (see Consequences). To add: a field
  `Option([u32; 0])` in `WithOptions`, whose twin gives the payload a
  zero-length array member, `uint32_t p0[0]` (GNU C), at offset 4.
