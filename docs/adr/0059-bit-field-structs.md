# 0059. Bit-field structs

- Status: Accepted; amended by 0062 (`ty::cycle` applies to the struct's bits needing those bits and to a default needing its own value; a variant value that fails leaves its field a failed value)
- Date: 2026-10-26
- Amends: 0017 (a literal operand of `as` with a bit-field struct as its
  target takes the struct's integer type), 0019 (a bit-field struct
  converts to integers and from its own integer type), 0021 (a literal
  of a bit-field struct may leave out fields with `..`; a bit field is a
  place that `&raw` does not take), 0022 (a bit-field struct has its
  integer type's layout, and no field has a byte offset), 0023 (a
  bit-field struct is a scalar), 0025 (a bit-field struct crosses calls
  as its integer type, not as an aggregate), 0032 (a value known at
  compile time that does not fit the field it is written to is an
  error; no value of a bit-field struct is known), 0035 and 0036 (an
  enum field of a bit-field struct made from an integer with `as` holds
  any value of its bits, so a field read is a source of values that are
  no variant besides memory), 0048 (a bit position or a bit field's type
  in error is a failed value), 0050 (`..` may leave out a field that is
  not visible when it has a default or a write effect), 0053 (a new panic reason,
  `FieldOutOfRange`)

## Context

A device register packs several values into one integer: the 16550
UART's line control register holds the word length in bits 0 and 1, the
number of stop bits in bit 2, parity in bits 3 to 5 and the divisor
latch access bit (DLAB) in bit 7. Today a program writes such a
register as an integer and builds it with shifts and masks:

```
const DLAB: u8 = 1 << 7;
UART0.lcr = UART0.lcr | DLAB;
UART0.lcr = 0x03;                      // 8 data bits, 1 stop bit, no parity
```

Nothing ties `DLAB` to `lcr`, a value of two bits can be shifted into
three, and `0x03` says nothing about what it sets.

ADR 0058 left what a register holds beyond its bits to M3.3, and the
roadmap asks for fields at bit positions as Ada writes them with
representation clauses (`for Lcr use record Dlab at 0 range 7 .. 7;`),
with enums in fields and reset values. C has bit-fields, whose order and
packing the ABI chooses, so they cannot describe a register portably.
Zig's `packed struct(u8)` places fields one after the other from bit 0,
so a gap needs a padding field and a position is a sum. Rust has no
bit-fields in the language; `svd2rust` generates a reader and a writer
type per register, with a method per field.

A register is read and written whole, so changing two fields should cost
one read and one write. The fields' values must therefore live in a
value that a program can hold, change and write back, not only in the
register.

This ADR covers bit-field structs as values, in any memory. ADR 0060
makes them register types and adds what only devices need (write and
read effects); ADR 0061 covers registers that share an address.

## Decision

### Declaration

```
enum WordLength: u8 { Five = 0, Six = 1, Seven = 2, Eight = 3 }

struct Lcr: u8 {
    pub word_length: WordLength at 0..=1 = .Five,
    pub two_stop_bits: bool at 2 = false,
    pub parity: bool at 3 = false,
    pub even_parity: bool at 4 = false,
    pub break_control: bool at 6 = false,
    pub dlab: bool at 7 = false,
}
```

Bit 5, stick parity, is left reserved in this example.

A **bit-field struct** is a struct whose name is followed by `:` and an
**integer type**, `u8`, `u16`, `u32` or `u64`, in which it is stored. Each
field gives the **bits** it occupies after `at`, and may give a
**default** after `=`:

```
bit_struct = "struct" NAME ":" type "{" [ bit_field { "," bit_field } [ "," ] ] "}"
bit_field  = [ "pub" ] NAME ":" type "at" bits [ read_effect ] [ write_effect ] [ "=" expr ]
bits       = end [ "..=" end ]
```

(The effects are ADR 0060's; an ordinary struct's grammar is unchanged.)
An `end` is an expression without assignment: the `=` after the bits
always starts the default, so `at 7 = false` is bit 7 with the default
`false`, and `at BASE + 1 = false` too.

- **Bits** are numbered from 0, the least significant. `at 7` is bit 7
  alone; `at 0..=1` is bits 0 to 1, both included, lowest first, as
  Ada's `range 0 .. 1` and a datasheet's "bits 1:0". Both ends are `u32`
  values computed at compile time (ADR 0029), in the scope of the
  item's module, so constants name them.
- **A field's type** is `bool`, one of the integer types `u8`, `u16`,
  `u32` and `u64`, or an enum without payloads whose integer type is one
  of those four. Its **width** must suit its bits: a `bool` takes exactly
  one bit; an integer type, and an enum's integer type, need at least as
  many bits as the field has, so that a read of the field is a value of
  the type; and every variant of an enum must fit in the field's bits.
- Fields may not share a bit. Bits that no field covers are **reserved**.
- A bit-field struct needs at least one field
  (`resolve::empty-bit-struct`): with none it would be its integer under
  another name, as an enum without variants would (ADR 0035).
- Any other integer type after `:` (`i8`, `usize`), or any other type, is
  `resolve::bit-struct-type`, as `resolve::enum-type` is for enums.
- In an ordinary struct, a field with `at` or `=` is
  `parser::unexpected-token`, whose help names bit-field structs; in a
  bit-field struct, a field without `at` is too.
- Fields are private to the struct's module unless `pub`, as other
  fields (ADR 0050); the struct is an item like any other (ADR 0049).
  `#[packed]` and `#[align(N)]` do not apply
  (`resolve::misplaced-attribute`): its layout is its integer's.
- A bit position or a default whose computation needs the struct's own
  bits, as `at L as u32` with `const L: Lcr`, is `ty::cycle`, as a
  variant's value needing itself is (ADR 0035). `size_of(Lcr)` inside
  `Lcr`'s bits is a cycle too: under ADR 0048 it asks whether `Lcr`
  reaches a failed value, which needs its bits.

### Values and literals

A bit-field struct is a type of values, as an ordinary struct is: a
local, a parameter, a return value, a field, an element, a payload, a
constant or a static may have it.

```
let l: Lcr = Lcr { word_length: .Eight, two_stop_bits: false, parity: false,
                   even_parity: false, break_control: false, dlab: false };
let m: Lcr = Lcr { word_length: .Eight, .. };
```

- A literal names every field once, as for any struct (ADR 0021), or ends
  with `..`, after a comma when fields come before it, which gives every
  field it leaves out its **default**. A field left out without a
  default is `resolve::missing-default`, and so is `..` in a literal of
  an ordinary struct, whose fields have none. `..` is allowed even where
  it leaves out no field.
- A field with a write effect (ADR 0060) has no default
  (`resolve::write-effect-default`): `..` gives it the value whose write
  has no effect, all zeros for `write 1 ...` and all ones for
  `write 0 ...`, so that a literal never triggers an effect it does not
  name. This holds wherever the value goes, a register or a local.
- **Visibility.** `..` may leave out a field that is not visible where
  the literal is written, when that field has a default or a write
  effect: the literal is then complete without naming it (amends ADR
  0050, whose `resolve::private-field` covered any literal of a struct
  with an invisible field). A literal that names an invisible field, or
  leaves out one with neither, is still `resolve::private-field`.
- A default is a value of the field's type computed at compile time,
  once, in the scope of the item's module, as an enum variant's value
  (ADR 0035). A default reported in error, because it fails to evaluate,
  has the wrong type or does not fit (`ty::field-value-out-of-range`),
  is reported once, at the default: a literal that takes it reports
  nothing more, and the body holding that literal is not used, as a body
  naming a constant that failed.
- Reserved bits are 0 in a value built by a literal.
- `l.f` reads the field: its bits, as the field's type (an integer
  zero-extended, a `bool` that is `true` when the bit is 1, an enum whose
  value is those bits).
- `l.f = v` writes the field when `l` is a place that can be written:
  the other fields and the reserved bits keep their values. A compound
  assignment, `l.count += 1`, reads the field and writes it.
- **A bit field is not a place in memory**: `&raw const l.f` and
  `&raw mut l.f` are `ty::bit-field-address`, also when `l` is a register
  (not also `ty::register-address`: what is addressed is the bit field),
  and `offset_of(Lcr, f)` is `ty::bit-field-offset`. `&raw` of the whole
  value is a pointer to its integer's bytes, as for any value.
- `==`, `!=`, arithmetic and bitwise operators do not apply
  (`ty::invalid-operand`), as for structs: a program compares fields, or
  integers made with `as`, which shows that reserved bits take part. A
  `match` on a bit-field struct is `ty::match-type`; a `match` on one of
  its fields is a `match` on that field's type.

### Enum fields and values that are no variant

ADR 0035 makes nothing convert to an enum, so that a program that writes
only variants never holds a value that is no variant, and ADR 0036
defines those that come from memory. An enum field of a bit-field struct
holds whatever its bits hold: a value read from a register, and also a
value made from an integer with `as` (below), `(0x83 as Lcr).word_length`
being whatever bits 0 and 1 of `0x83` are. A field read is therefore a
second source of values that are no variant, besides memory (amends ADR
0035 and 0036). Such a value is defined as ADR 0036 says: a `match`
without a catch-all arm calls the panic function on it. A field whose
every bit pattern is a variant, as `word_length`, never gives one.

### Values that do not fit

**A value written to a field must fit in its bits.** A field of `n`
bits holds the values from 0 to 2ⁿ − 1:

- A value known at compile time (ADR 0032) that does not fit is
  `ty::field-value-out-of-range`: in a literal, in an assignment, in a
  default. A known enum variant always fits, being checked at the
  declaration. A compound assignment is never reported at compile time:
  the value it writes back depends on the field's value, which is not
  known (`l.count |= 16` on a four-bit field always overflows,
  `l.count &= 16` never does); it is checked when it runs.
- Any other value is checked when it is written: if it does not fit,
  the program calls the panic function (ADR 0014), as for a zero divisor
  (ADR 0007), with the new reason `core::PanicReason::FieldOutOfRange = 4`
  (amends ADR 0053), located at the field's name in the literal or the
  assignment. An enum value that is no variant can fail the check too.
  A `bool` always fits; so does an integer whose type has exactly the
  field's bits. With `-O`, LLVM removes checks it proves redundant, so a
  value already masked (`x & 0xF`) costs nothing.
- A compound assignment checks the value it writes back: `l.count += 1`
  on a field holding its maximum panics instead of wrapping into the
  neighbouring bits.

A program that wants the low bits of a wider value masks them itself.

### Conversions

- `as` converts a bit-field struct to any integer type, as its integer
  type would convert (ADR 0019): `l as u32` is the bits of `l`,
  zero-extended.
- `as` converts to a bit-field struct only from its own integer type:
  `0x83 as Lcr`, `x as Lcr` with `x: u8`. A literal operand takes that
  integer type, as a literal operand of `as` takes the target type
  (amends ADR 0017). Every integer is a value of the struct; reserved
  bits keep what the integer holds. From any other type, another integer
  type included, it is `ty::invalid-cast`: a wider value is narrowed
  first, visibly. A bit-field struct does not convert to itself
  (`l as Lcr`), as an enum does not (ADR 0035).
- A bit-field struct converts to nothing else: not to `bool`, a pointer,
  an enum or another bit-field struct.

### Layout, calls and volatile accesses

- **Layout:** a bit-field struct has the size and alignment of its
  integer type (amends ADR 0022). Its C twin is that integer type: a
  `uint8_t` for `Lcr`. A field of an ordinary struct may have a
  bit-field struct type, laid out as that integer; in a `#[packed]`
  struct, such a field of more than one byte is reached through a
  volatile pointer as any field needing alignment is
  (`ty::volatile-packed-field`, ADR 0023).
- **Calls:** it is a scalar, passed and returned as its integer type,
  zero-extended (ADR 0006, amends 0025).
- **Volatile pointers:** it is a scalar (amends ADR 0023), accessed in
  one load or store of its integer's width; a `u64` one cannot be
  accessed through a volatile pointer (`ty::volatile-too-wide`). Reading
  a field through a volatile pointer, `(*p).dlab`, is one load of the
  whole value. Writing one, `(*p).dlab = true`, is one load and then one
  store of the whole value, as `x += 1` is (ADR 0023), not atomic; ADR
  0060 adds what fields with effects need.

### Evaluation order

In an assignment the value runs before the place (ADR 0018). Writing a
field with `=` runs: the value, its check, then whatever the place needs
(a pointer, an index and its bounds check), then the read of the whole
value, then the write. A compound assignment, `l.f op= v`, runs the
value, then the place, then the read of the whole value, then the new
field value and its check, then the write: on a volatile place, a value
that does not fit calls the panic function after the load and before
the store. A literal's field values run in the order written, each
followed by its check, and the defaults need no code.

### Compile time

Bit-field structs work in constants, statics, `comptime` blocks and
expressions as any value. **No value of a bit-field struct is known at
compile time** in the sense of ADR 0032, as aggregates are not (amends
ADR 0032): not a literal, not a constant of the type, not `0x83 as Lcr`,
and neither is a field read from one nor an integer made from one with
`as`. So `x << (L as u32)` with `const L: Lcr` is checked as a shift
amount that is not known.

### Failed values

A bit position, and a field's type that is in error, are **failed
values** in the sense of ADR 0048 when they are reported in error: an
evaluation failure, a type error, or for a type,
`ty::bit-field-type` or an erroneous type (amends ADR 0048). So is a
field whose bits are reversed or reach past the integer
(`ty::reversed-bit-range`, `ty::bit-field-outside`), or whose width does
not suit its type (`ty::bit-field-width`): its bits cannot be read or
written as declared. A bit-field struct with a failed value **reaches a
failed value**, with what ADR 0048 says of such types: its own error is
the only one reported, and a body whose locals or expressions have the
type is not built into MIR. Overlapping fields are reported but are no
failed value: each field can still be read and written. A default in
error is no failed value of the type; it affects only the literals that
take it (above).

### What is checked

When names are resolved:

| Code | Case |
|------|------|
| `resolve::bit-struct-type` | the type after `:` is not `u8`, `u16`, `u32` or `u64` |
| `resolve::empty-bit-struct` | a bit-field struct without fields |
| `resolve::duplicate-field` | two fields with one name, as in any struct |
| `resolve::misplaced-attribute` | `#[packed]` or `#[align(N)]` on a bit-field struct |
| `resolve::write-effect-default` | a default on a field with a write effect |
| `resolve::missing-fields` | a literal without `..` that leaves fields out; its help offers `..` when every field left out has a default or a write effect |
| `resolve::missing-default` | a literal whose `..` leaves out a visible field with neither a default nor a write effect, or `..` in a literal of an ordinary struct |
| `resolve::private-field` | a literal naming a field not visible here, or leaving one out without `..`, or with `..` when it has neither a default nor a write effect |

Once the values are computed, with checks that need a failed value
skipped:

| Code | Case |
|------|------|
| `ty::bit-field-type` | a field whose type is not `bool`, `u8`, `u16`, `u32`, `u64` or an enum without payloads on one of these four |
| `ty::reversed-bit-range` | `at a..=b` with `a > b` |
| `ty::bit-field-outside` | a field reaching past the last bit of the integer type |
| `ty::bit-field-width` | a `bool` field of more than one bit, an integer field wider than its type or than its enum's integer type, or an enum variant that does not fit in its field |
| `ty::bit-field-overlap` | two fields sharing a bit; once per pair, at the later field, labeling the earlier |
| `ty::field-value-out-of-range` | a default that does not fit in its field |
| `ty::cycle` | a bit position or default that needs the struct's own bits |

When code is type checked and built:

| Code | Case |
|------|------|
| `ty::bit-field-address` | `&raw` of a bit field, from a register too |
| `ty::bit-field-offset` | `offset_of` of a bit field |
| `ty::invalid-cast` | `as` to a bit-field struct from anything but its integer type (itself included), or from one to anything but an integer type |
| `ty::invalid-operand` | an operator on bit-field structs |
| `ty::match-type` | a `match` on a bit-field struct |
| `ty::field-value-out-of-range` | a value known at compile time, written to a field by a literal or an assignment, that does not fit (with ADR 0032's other checks, when the body is built) |

## Consequences

- Register values get names, types and checked widths, at no cost: in
  memory and across calls a bit-field struct is its integer, and a field
  access is a shift and a mask.
- Changing several fields of a register is one read and one write
  through a local, and a field written alone is one read and one write.
- Positions are written as datasheets give them; nothing is placed by
  order, so inserting a field moves no other.
- With defaults, a register value is written as the fields that differ
  from reset plus `..` (ADR 0060 uses the defaults as reset values); a
  driver may keep fields private and still let other modules write
  values with `..`.
- A field write may call the panic function where an integer write
  would not; the check is gone when the value is known to fit.
- `as` and enum fields make values that are no variant without memory;
  a `match` on such a field takes a catch-all arm, or panics on one.
- Reserved bits are 0 in a literal. A device that wants reserved bits
  kept as read is written with a field assignment or through a local
  read from it.
- Default values exist only for bit-field structs; ordinary structs
  could get them later with the same syntax.
- Fields have no access modes of their own: a field is read and written
  as its register allows (ADR 0060). Whether fields need modes is left
  to M3.4, which checks the design against a real chip's SVD file.
- Signed fields, nested bit-field structs and range types (the roadmap's
  Ada-inspired types) are not part of this ADR.

## Alternatives considered

- **A new keyword (`bits Lcr: u8`)** — the same meaning under another
  word. `struct NAME: u8` follows `enum NAME: u8`, and a bit-field struct
  is read, written and built as a struct is.
- **Fields declared inside a register** — the value read would have no
  type to name, so a program could not keep it in a local to change
  several fields with one write.
- **Fields placed in order from bit 0, as Zig** — gaps need padding
  fields, and a position is a sum to compute against the datasheet.
- **C bit-fields** — order and packing are the ABI's choice.
- **Half-open bit ranges (`at 0..2`)** — the end is no bit of the field,
  which reads wrongly next to a datasheet's "bits 1:0".
- **`msb:lsb` (`at 1:0`), as SVD's `bitRange`** — `:` already separates a
  name from its type; and offset with width (`at 0 width 2`, SVD's
  `bitOffset` and `bitWidth`) — the datasheet's numbers have to be
  converted.
- **Masking a value that does not fit** — never panics, but loses bits
  silently; a program masks explicitly when it means to.
- **Integer types of any width (`u1` to `u31`), as Zig** — a field of four
  bits would be a `u4` and fit by construction; a large change to the
  type system, and conversions to every other integer type.
- **`==` on bit-field structs** — compares reserved bits too, which the
  expression would not show.
- **`as` from any integer type** — narrowing a `u32` to a `u8` struct would
  drop bits inside one cast.
- **No `as` to a bit-field struct with an enum field**, keeping ADR
  0035's promise that values that are no variant come only from memory —
  a register gives such values anyway, and a program decoding a value
  received as an integer would have to store it to memory first.
- **Defaults allowed on fields with write effects** — a generator writing
  each field's reset value would give a `write 0 clears` flag that resets
  to 0 the default `false`, and every literal with `..` would clear it.
- **`..` never filling invisible fields, as ADR 0050 had it** — a driver
  could not keep a register's reserved-like fields private and still let
  callers write values with `..`.
- **Defaults on the register (`reset` after its offset, read with a
  query like `offset_of`)** — every write of some fields would need a
  local and several lines; defaults on fields give `Fcr { enable: true, .. }`.
