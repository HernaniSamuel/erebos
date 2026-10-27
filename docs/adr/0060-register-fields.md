# 0060. Register fields

- Status: Accepted
- Date: 2026-10-26
- Amends: 0058 (a register's type may also be a bit-field struct or an
  enum without payloads on `u8`, `u16` or `u32`; a register's fields are
  read and written through it; a register may declare that its reads
  have effects)

## Context

ADR 0058 declares registers as `u8`, `u16` or `u32` and left to M3.3
fields at bit positions, enums in fields, write-1-to-clear fields, reads
with side effects and reset values. ADR 0059 gives fields at bit
positions, as bit-field structs, with enums in fields and default values.
What remains is how such a value behaves in a register.

Changing one field of a register means reading the register and writing
it back. That is wrong for two kinds of hardware behaviour, which SVD
files describe:

- **Write effects** (`modifiedWriteValues`): writing a 1 to a
  write-1-to-clear (`oneToClear`) bit clears it, and a 0 does nothing.
  Interrupt and status flags are commonly cleared this way. Writing back
  the value just read writes a 1 to every flag that was set, and clears
  them all, though the program changed only an enable bit. SVD also has
  `oneToSet`, `oneToToggle`, `zeroToClear`, `zeroToSet` and
  `zeroToToggle`. `svd2rust` gives each such field the value whose write
  has no effect, 0 for the `oneTo*` ones and 1 for the `zeroTo*` ones,
  both in its `modify` and in its `write`, whatever the reset value.
- **Read effects** (`readAction`): reading the 16550's line status
  register clears its error bits, reading its receive buffer takes a
  byte from the FIFO, and reading its interrupt identification register
  clears a pending interrupt. A read the program does not see, such as
  the read of a read-modify-write, loses that information or that byte.

SVD gives a register a reset value, the value it holds after reset;
`svd2rust`'s `write` starts from it, so that a program writes only the
fields it changes.

## Decision

### Register types

A register's type is one of:

- `u8`, `u16` or `u32`, as in ADR 0058;
- a bit-field struct (ADR 0059) whose integer type is one of those;
- an enum without payloads whose integer type is one of those.

The register's width is that integer type's. Anything else is
`ty::register-type`, as before: a `u64` bit-field struct, an enum on
`i8`, an enum with payloads.

```
peripheral Uart16550 {
    pub read rbr: u8 at 0x00 read modifies,
    pub write thr: u8 at 0x00,
    read write ier: Ier at 0x01,
    write fcr: Fcr at 0x02,
    read write lcr: Lcr at 0x03,
    read lsr: Lsr at 0x05,
}
```

### Accesses

As in ADR 0058, each read of a register is one volatile load of its
width and each write one volatile store.

- `UART0.lcr` read is one load, giving an `Lcr`; `UART0.lcr = v` is one
  store of `v`'s bits, reserved bits included.
- `UART0.lsr.data_ready` reads the register, one load, and gives the
  field.
- **A field assignment**, `UART0.lcr.dlab = true`, or a compound
  assignment to a field, is one load of the register, then one store, as
  ADR 0059 says for any place: the value and its check run first, then
  the load, then the store, with nothing between them but the change of
  the field. It is not atomic (ADR 0023). It reads and writes, so it
  needs a `read write` register: on a `read` one it is
  `ty::write-to-read-only`, on a `write` one `ty::read-of-write-only`,
  labeled as read by the field assignment.
- A register whose type is an enum is read and written whole; a value
  read that is no variant is defined (ADR 0036).

Fields have no access modes of their own: every field of a register is
read and written as its register allows. Whether fields need modes is
M3.4's question.

### Write effects

A field of a bit-field struct may declare what a write does to it, after
its bits:

```
struct Status: u32 {
    pub enable: bool at 0 = false,
    pub overrun: bool at 3 write 1 clears,
    pub done: bool at 4 write 1 clears,
}
```

```
write_effect = "write" INTEGER ( "clears" | "sets" | "toggles" )
```

`write 1 clears` is SVD's `oneToClear`, `write 0 sets` its `zeroToSet`,
and so on for the six combinations. The bit written is an integer
literal whose value is 0 or 1 (`write 0x1 clears` is `write 1 clears`).
A field has at most one write effect, and no default (ADR 0059).

**In a field assignment to a volatile place, every other field with a
write effect is written with the value that has no effect**: all zeros
for `write 1 ...`, all ones for `write 0 ...`. A volatile place is a
register, or a place reached through a volatile pointer (ADR 0023),
whose bits are the device's or another agent's. The assigned field gets
the value assigned; reserved bits and fields without a write effect keep
the value read.

```
TIMER.status.enable = true;     // overrun and done written 0: unchanged
TIMER.status.overrun = true;    // clears overrun alone
let s: Status = TIMER.status;
TIMER.status = s;               // clears every flag that was set
```

- Writing a whole value writes it as it is: what it clears is what the
  program wrote. A value built with `..` gives every field with a write
  effect that it leaves out the value that has no effect (ADR 0059), so
  it clears, sets or toggles nothing it does not name.
- A **compound assignment to a field with a write effect** in a volatile
  place, `TIMER.pending.mask |= 1 << 3` with `mask` a `write 1 clears`
  field, is `ty::compound-write-effect`: it would write back every bit
  it read, and clear them all. The program writes the bits it means.
- In ordinary memory, a local or a static, the effects mean nothing: a
  field assignment changes that field alone, so that a value read from a
  register can be changed and written back as the program sees it.

### Read effects

A register, after its offset, or a field of a bit-field struct, after
its bits, may declare what a read does:

```
read_effect = "read" ( "clears" | "sets" | "modifies" )
```

`read clears` and `read sets` are SVD's `readAction` `clear` and `set`;
`read modifies` is `modify` or `modifyExternal`, any other change. For
the compiler, all three mean that **a read has effects**. A register's
reads have effects when it declares a read effect, or when its type is a
bit-field struct with a field that does; so do reads through a volatile
pointer of a place whose type is such a struct. In ordinary memory, a
local or a static, read effects mean nothing, as write effects do not.
A read effect on a `write` register, which is never read, is accepted:
it documents the device, as an SVD file may, and the reads of a `write`
register never count as having effects, so a field assignment to it is
`ty::read-of-write-only` alone.

```
struct Lsr: u8 {
    pub data_ready: bool at 0,
    pub overrun: bool at 1 read clears,
    pub parity_error: bool at 2 read clears,
    pub framing_error: bool at 3 read clears,
    pub break_interrupt: bool at 4 read clears,
    pub thr_empty: bool at 5,
    pub transmitter_empty: bool at 6,
}
```

**Such a place is never read without the read being written**: a field
assignment or a compound assignment to it, or to one of its fields, is
`ty::read-with-effects`. The program reads the register into a local,
which shows the read, keeps what it returned, and writes what it means.
Explicit reads are unchanged: each is one volatile load, never removed or
duplicated (ADR 0023), so `UART0.lsr.thr_empty` reads the register, and
its effects happen, where it is written.

A field has at most one read effect, and a register at most one. The
grammar of ADR 0058 and 0059 becomes:

```
register  = [ "pub" ] access NAME ":" type "at" expr [ read_effect ] [ "in" NAME ]
bit_field = [ "pub" ] NAME ":" type "at" bits [ read_effect ] [ write_effect ] [ "=" expr ]
```

(`in NAME` is ADR 0061's.) `read`, `write`, `clears`, `sets`, `toggles`
and `modifies` are recognized there by position only; elsewhere they are
ordinary names.

### Reset values

**A field's default (ADR 0059) is its reset value.** A tool generating
declarations from SVD writes each field's reset bits as its default, so
that `Fcr { enable: true, .. }` writes the register as it is after reset
but for `enable`, and for the fields with write effects, which have no
default and are written with the value that has no effect, as
`svd2rust`'s `write` does. A register's reset value is not declared
apart from its fields' defaults: an integer register has none, and
reserved bits are 0 in a literal (ADR 0059).

### Compile time

A register access stays `eval::volatile-access` at compile time
(ADR 0058). Effects only change what the compiler accepts and how a
field assignment to a volatile place is built.

### What is checked

Once the values are computed:

| Code | Case |
|------|------|
| `ty::register-type` | a register whose type is not `u8`, `u16` or `u32`, or a bit-field struct or an enum without payloads on one of them |

When code is type checked:

| Code | Case |
|------|------|
| `ty::write-to-read-only` | a field assignment to a `read` register, as any write |
| `ty::read-of-write-only` | a field assignment to a `write` register, which reads it, or a field read from one |
| `ty::read-with-effects` | a field assignment or a compound assignment to a place whose reads have effects, or to one of its fields |
| `ty::compound-write-effect` | a compound assignment to a field with a write effect in a volatile place |

When several apply, each is reported, as ADR 0058 reports a write of a
`read` register through a `*const` pointer: each is a fault of its own,
and fixing one leaves the other. So a field assignment to a `read`
register whose reads have effects is `ty::write-to-read-only` and
`ty::read-with-effects`, and a compound assignment to a write-effect
field of a register whose reads have effects is
`ty::compound-write-effect` and `ty::read-with-effects`. `&raw` of a
register's bit field is `ty::bit-field-address` alone (ADR 0059).

Syntax errors in effects (a second effect of a kind, a word that is no
effect, `write 2 clears`) are `parser::unexpected-token`.

## Consequences

- A register is configured by fields, and one field changed in one read
  and one write, without clearing flags it did not name.
- Registers whose reads consume data cannot be read behind the
  program's back; reading them takes a line of its own.
- The six write effects cover SVD's `oneTo*` and `zeroTo*` values; SVD's
  `clear`, `set` and `modify` write effects (any write clears or sets)
  have no neutral value, are not declared, and are M3.4's to check
  against a real chip.
- An integer register's reset value is not declared. M3.4, checking
  against SVD, may add one.
- A program polling `UART0.lsr.thr_empty` loses the error bits that read
  cleared, as C code doing the same does; a program that wants them
  reads `lsr` into a local.
- The effects of a type apply through volatile pointers too, so memory
  shared with a device that is not a register behaves as a register of
  that type would.

## Alternatives considered

- **Only whole registers read and written** — every field change would
  take a local and three lines, and the compiler would know nothing of
  effects, so a program writing back the local would clear flags anyway.
- **Only `write 1 clears`** — the other five follow the same rule; adding
  them later would amend this ADR for no new idea.
- **An attribute (`#[w1c]`)** — attributes say how something is laid out
  or named; an effect is what the hardware does, and reads as words.
- **Read effects as documentation only** — the compiler would accept the
  read-modify-write that loses a received byte.
- **Effects applied in ordinary memory too** — a field assignment to a
  local would change other fields of that local, which the program did
  not write.
- **Reset values on registers, read with a query** — see ADR 0059.
- **Field access modes now** — a field written in a `read write`
  register that hardware ignores (SVD's read-only fields) would be
  caught, but modes in a bit-field struct would also apply, or not, to
  locals of that type; left to M3.4 with the SVD of a real chip.
