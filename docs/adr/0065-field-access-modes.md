# 0065. Field access modes

- Status: Accepted
- Date: 2026-10-28
- Amends: 0059 (a bit field may declare an access mode), 0060 (fields
  of a register read or written through a volatile place follow their
  own access modes, not only their register's)

## Context

ADR 0058 gives each register an access mode, and ADR 0060 reads and
writes every field of a register as its register allows, leaving to
M3.4 whether fields need modes of their own. SVD gives fields an
`access` too, and real chips use it: the ESP32-C3's SVD states an
access for 5 of its 1150 registers and none for the others, and gives
848 fields `read-only` and 294 `write-only` (ADR 0067). The fields' modes
say what the hardware does: a `read-only` status bit ignores a write,
and a `write-only` trigger bit reads as nothing meaningful.

ADR 0060 rejected field modes until then because a bit-field struct is
also a type of locals: modes would apply, or not, to a local of that
type. Effects (ADR 0060) settled the same question: they apply in
volatile places only, and a local can be changed and written back as the
program sees it.

Most such fields sit in registers whose fields are all of one mode,
which a register's own mode already says: of the ESP32-C3's 848
read-only fields, 794 are in its 281 registers whose fields are all
read-only, and 237 of its 294 write-only fields in 71 registers whose
fields are all write-only (ADR 0067 gives such a register its fields'
mode). What only field modes can say is the rest: 61 ESP32-C3 registers
mix modes, holding 54 read-only and 57 write-only fields next to
read-write ones, as do 33 GD32VF103 registers and 11 CH32V003 ones.

## Decision

A bit field may declare an access mode before its name:

```
struct Status: u32 {
    pub read busy: bool at 0,
    pub write start: bool at 1,
    pub mode: u8 at 4..=5 = 0,
}
```

```
bit_field = [ "pub" ] [ "read" | "write" ] NAME ":" type "at" bits [ read_effect ] [ write_effect ] [ "=" expr ]
```

- `read` makes the field **read-only** and `write` **write-only**. A
  field without a mode is read and written as its register allows, as in
  ADR 0060. `read write` is not written on a field: it is what no word
  says.
- As for registers, `read` and `write` are recognized there by position
  only: `read: bool at 0` is a field named `read`, `read write: bool at
  0` a read-only field named `write`.

**In a volatile place, a register or a place reached through a volatile
pointer (ADR 0060), a field's mode is checked as a register's is**
(ADR 0058):

- A field assignment or a compound assignment to a `read` field is
  `ty::write-to-read-only`.
- A read of a `write` field, `STATUS.start`, or a compound assignment to
  one, is `ty::read-of-write-only`.

The register's own mode is checked too. When both forbid the access
with the same code, as a field assignment to a `read` field of a `read`
register, or a read of a `write` field of a `write` register, it is one
diagnostic labeling the register and the field: one code for one
access, with both causes shown, since fixing one leaves the other. When
they forbid it with different codes, each is reported, as ADR 0060
reports a register's mode and a read effect: a field assignment to a
`read` field of a `write` register is `ty::read-of-write-only` for the
register (the assignment reads it) and `ty::write-to-read-only` for the
field, and a compound assignment to a `write` field of a `read`
register is `ty::write-to-read-only` for the register and
`ty::read-of-write-only` for the field.

Everything else is unchanged:

- A field assignment to another field keeps the bits of `read` and
  `write` fields without a write effect as read, as reserved bits and
  fields without a write effect are kept (ADR 0060): the hardware
  ignores the write of a read-only field, and what a write-only field
  reads back is written back. **A field with a write effect gets the
  value that has no effect, whatever its mode** (ADR 0060): a `write`
  field with `write 1 clears`, the usual interrupt-clear bit, is written
  0 by a field assignment to another field, and clears nothing.
- A whole read gives every field, and a whole write writes every field:
  a literal may name a `read` field, and `..` gives it its default
  (ADR 0059). What a whole value holds is the program's to choose, as
  for reserved bits.
- In ordinary memory, a local or a static, modes mean nothing, as
  effects do not: a value read from a register can be changed and
  written back as the program sees it.
- A mode combines with effects without a check: a `read` field with a
  write effect, or a `write` field with a read effect, is accepted, as a
  read effect on a `write` register is (ADR 0060); it documents the
  device. So does a field whose mode its register lacks, such as a
  `write` field in a `read` register, which no access reaches.
- The dumps show a field's mode.

### What is checked

When code is type checked:

| Code | Case |
|------|------|
| `ty::write-to-read-only` | a field assignment or a compound assignment to a `read` field in a volatile place |
| `ty::read-of-write-only` | a read of, or a compound assignment to, a `write` field in a volatile place |

Syntax errors (`read write` or two modes on a field, a mode after the
name) are `parser::unexpected-token`.

## Consequences

- A generator from SVD keeps each field's `access`, and gives a register
  without one its fields' mode (ADR 0067), so that a status register is
  `read` and a whole write to it is an error; in the registers that mix
  modes, the fields' modes are what the compiler checks.
- Writing a status bit through a field assignment, which the hardware
  ignores, and reading a trigger bit, which gives nothing meaningful, are
  compile errors.
- A literal written whole can still set bits of `read` fields, which the
  hardware ignores; the compiler does not see where a value goes.
- A write-only field that reads back as something other than what has
  no effect is written back with that value by a field assignment to
  another field, as `svd2rust`'s `modify` does. No surveyed SVD marks
  such a field (ADR 0067).

## Alternatives considered

- **No field modes, the generator writing them as comments** — on the
  ESP32-C3 nothing the hardware does with a field would be checked.
- **Modes checked in every place, locals included** — a value read from
  a register into a local could not have a read-only field changed
  before a comparison, for no fault; effects were confined to volatile
  places for the same reason (ADR 0060).
- **Literals naming a `read` field rejected** — a literal is a value,
  which may be stored in a local or compared, not only written to a
  register.
- **`read write` allowed on a field** — two spellings of one meaning.
- **A field assignment writing `write` fields with their default or
  with 0** — the value that has no effect is not known for a field
  without a write effect; SVD does not state it.
