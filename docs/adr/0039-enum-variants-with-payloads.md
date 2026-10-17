# 0039. Enum variants with payloads

- Status: Accepted
- Date: 2026-10-17
- Amends: 0023 (an enum with payloads is an aggregate for volatile
  accesses), 0025 (enums with payloads cross calls as aggregates), 0032
  (no value of an enum with payloads is known at compile time), 0034 (a
  payload value is memory), 0035 (an enum with payloads is not its
  integer type), 0036 (a value of an enum with payloads is any bytes of
  its size)

## Context

A message in a protocol, a decoded command or the result of an operation
is often one of several cases, each with its own data: `Nop`, `Led(on)`,
`Write(register, value)`. ADR 0035 gives the cases names, but no data;
today the data goes in a struct next to the enum, and nothing ties a
field to the case that uses it.

C writes such values by hand, as a tagged union: a struct holding a tag
and a union of the cases' data, or a union of structs that each start
with the tag. Nothing checks that the code reads the case the tag names.
Rust's enums carry data; their layout is unspecified unless an attribute
chooses one of the two C forms (RFC 2195): `#[repr(C, u8)]` the first,
`#[repr(u8)]` the second, and `#[repr(C)]` alone the first with a tag of
the size of C's `int`. Zig's tagged unions are similar, and also choose their own
layout by default.

The roadmap leaves three questions to this step: the shape of a payload
(one value per variant or several), the layout and calling convention of
enums with payloads, and payloads of type `!` (ADR 0034). `Option(T)` and
`Result(T, E)` (M2.5) are, in M6, functions returning enums (ADR 0027),
so this design must be able to express a variant with one value of any
type.

This ADR covers declaring, building, storing and passing enums with
payloads. Matching them is ADR 0040, and bindings in or-patterns ADR 0041.

## Decision

### Declaration

```
enum Msg: u8 {
    Nop = 0,
    Led(bool) = 1,
    Write(u8, u16) = 2,
    Wide(u32) = 3,
}
```

- A variant may carry a **payload**: one or more types in parentheses
  after its name, separated by commas, with an optional trailing comma,
  before its `= value`. The payload's values are **positional**: they have
  no names, and are told apart by their order.
- What ADR 0035 says about the declaration still holds: the integer
  type and every value are required, values are computed at compile time,
  are unique, and are reported once when they fail. The integer type is
  that of the **tag**, which holds the variant's value. Since `as` does
  not apply to an enum with payloads (below), a value cannot be computed
  from another variant of the same enum, as `= Mode::Idle as u8 + 1` is
  in ADR 0035; a constant holding the number serves both.
- `Led()`, parentheses without a type, is `parser::empty-payload`: a
  variant without payload is written without parentheses, so it has one
  spelling.
- A payload type is any type a struct field may have (ADR 0021): integers,
  `bool`, pointers, slices, arrays, structs and enums, with or without
  payloads, and zero-sized types, which take no bytes (ADR 0022).
- **`!` is not a payload type** (`ty::never-in-memory`; amends ADR 0034,
  which lists the positions where `!` is the type of memory). A payload is
  memory: a tag read from memory may name the variant, and its payload
  would then be a `!` read from memory, which ADR 0034 rules out. Whether
  `Result(u32, !)` is infallible is left to M2.5. `[T]` is not a payload
  type either (`ty::unsized`, ADR 0031).
- An enum that contains itself by value, directly or through structs,
  arrays and other enums, is `ty::infinite-size`, as a struct is
  (ADR 0021): `Cons(u8, List)` in `enum List` would have no size, and
  `Cons(u8, *const List)` is the way to write it.
- An enum whose size, the largest variant's rounded up to the enum's
  alignment, is larger than the target can hold is `ty::too-large`, as a
  struct is (ADR 0022).
- An enum with a payload type in error, such as `!`, `[u8]` or an unknown
  name, is reported once, at that type, and the enum then **counts as
  erroneous**, as a struct with such a field does (ADR 0034): its uses,
  in `size_of`, in building a variant, in a `match` or in a call, report
  nothing more because of it.
- An **enum with payloads** is one with at least one variant that has a
  payload. The others are **enums without payloads**, those of ADR 0035,
  and nothing in this ADR changes them.

### Building a value

```
let m: Msg = Msg::Write(3, 0x1234);
let n: Msg = Msg::Nop;
```

- `Msg::Write(a, b)` builds the variant `Write` with the payload values
  `a` and `b`, in order. Each expression is checked against its payload
  type as an argument is against its parameter: a literal takes that type,
  and the implicit conversions of ADR 0024 and 0031 apply. They are
  evaluated left to right (ADR 0018).
- A variant without payload is named as in ADR 0035, `Msg::Nop`, also in
  an enum with payloads.
- The number of values must be the payload's: `Msg::Write(3)`,
  `Msg::Write` and `Msg::Nop(1)` are `ty::wrong-payload-count`, which says
  how many values the variant carries. There are no function values, so
  `Msg::Write` alone is no function either. A variant without payload
  written with parentheses, `Msg::Nop()` or `Mode::Off()` in an enum
  without payloads, is the same error: it carries no values, and is
  named without parentheses.
- The result is a value, not a place, like a struct literal: `&raw const
  Msg::Write(3, 4)` is `ty::not-a-place`, and it cannot be assigned.
- A value of an enum with payloads is never **known at compile time** in
  the sense of ADR 0032, not even `Msg::Nop`, nor a constant or a
  `comptime` expression of such a type (amends ADR 0032, which makes every
  constant known): like a struct, it is an aggregate, and ADR 0032's
  checks concern integers and `bool`. (In ADR 0035, a variant is known
  because it is an integer.) So a `match` on a constant of an enum with
  payloads is checked in every arm (ADR 0037, 0040).
- Constants, statics and `comptime` values may have such a type, as they
  may have a struct type. Their bytes that evaluation never wrote, such as
  those outside the tag and the payload of the variant built, are zero,
  as padding is (ADR 0027), and a `static mut` whose bytes are all zero
  goes to `.bss` (ADR 0030).

### Operations

An enum with payloads is an aggregate, as a struct is (ADR 0021):

- `let`, assignment and reading a place copy the whole value.
- `==` and `!=` are not defined on it (`ty::invalid-operand`); compare
  with a `match` (ADR 0040).
- `as` converts it to nothing and nothing to it (`ty::invalid-cast`): it
  is not an integer, and its tag is read with a `match`.
- It has no fields: `m.f` and `offset_of(Msg, f)` are `ty::no-fields`.
  Payload values are reached only by patterns (ADR 0040).
- **Through a volatile pointer** it is an aggregate, not a scalar: reading
  or writing it whole is `ty::volatile-aggregate` (amends ADR 0023 and
  0035, which made every enum a scalar there). Since it has no fields, a
  volatile pointer to one reaches nothing; device registers are described
  with structs, integers and enums without payloads.

### Layout

**Each variant is laid out as a struct** (ADR 0022) whose first field is
the tag, of the enum's integer type, followed by the payload values in
order; a variant without payload is a struct holding only the tag. The
enum's alignment is the largest alignment of these structs, and its size
is the largest of their sizes, rounded up to a multiple of that
alignment. The tag is at offset 0 in every variant, and holds the
variant's value.

```
enum Msg: u8 { Nop = 0, Led(bool) = 1, Write(u8, u16) = 2, Wide(u32) = 3 }
// Nop:   tag at 0                       size 1, alignment 1
// Led:   tag at 0, bool at 1            size 2, alignment 1
// Write: tag at 0, u8 at 1, u16 at 2    size 4, alignment 2
// Wide:  tag at 0, u32 at 4             size 8, alignment 4
// Msg:   size 8, alignment 4
```

- A payload value never shares bytes with the tag, and starts as close
  after it as its alignment allows, as a field does.
- The bytes of the enum outside the tag and the payload of the variant it
  holds, including padding, are **unspecified**, as padding is
  (ADR 0022): building or writing a variant may leave anything there.
- **There are no niches**: the tag is always stored, whatever the payload
  types, and no payload value stands for a variant. This is what the
  roadmap fixed for `Option(*mut T)`.
- `size_of` and `align_of` give the size and alignment above.
- No attribute applies to an enum, with or without payloads
  (ADR 0035). A payload type with `#[align(N)]` raises the enum's
  alignment as it raises a struct's, and such an enum cannot be a field
  of a packed struct (`ty::aligned-in-packed`, ADR 0022).

**C compatibility.** The layout is that of a C union of structs that each
start with the tag; Rust's `#[repr(u8)]` on an enum with fields gives the
same one. The C twin of `Msg` is:

```c
union Msg {
    uint8_t tag;
    struct { uint8_t tag; bool on; } led;
    struct { uint8_t tag; uint8_t reg; uint16_t value; } write;
    struct { uint8_t tag; uint32_t value; } wide;
};
```

Zero-sized payload types have no C equivalent (ADR 0022). A C twin
leaves out those of alignment 1, which change no offset; a zero-sized
payload of larger alignment, as `B(u8, [u32; 0])`, raises the variant's
alignment and size, and such an enum has no exact C twin.

### Values that are no variant

ADR 0036 extends to enums with payloads. A value of such an enum is any
bytes of its size. Its tag is any value of the integer type; when the tag
is the value of a variant, its payload values are read from their places
as that variant lays them out, each as a value of its type, as a field
is. When the tag is no variant's value, the whole value is **no variant**,
and nothing but its tag is ever read. Reading, writing and copying such a
value is defined, as in ADR 0036; ADR 0040 says what a `match` does with
it.

### Calls

An enum with payloads crosses calls **as an aggregate** of its size and
alignment, by the table of ADR 0025, which now covers structs, arrays,
slices and enums with payloads (amends ADR 0025). `Msg`, of size 8 and
alignment 4, goes in a register pair, as the C `union Msg` does: the
RISC-V psABI classifies unions like structs, by size and alignment. An
enum with payloads is never zero-sized, since its tag has a size. Enums
without payloads stay scalars (ADR 0035).

So giving one variant a payload changes the layout and the calling
convention of the whole enum: from an integer passed extended by its
signedness (ADR 0006) to an aggregate, passed in a register whose unused bits are
unspecified (ADR 0025), or in two registers or in memory as it grows. Its
C twin changes from its integer type to a union.

### In the compiler

MIR keeps enums without payloads as their integer type (ADR 0035). An
enum with payloads is a MIR type of its own, with its layout, and two
projections reach into a place of that type: its tag, an integer place,
and a payload value of a given variant. Code generation and the
evaluator reach both by offset, as they reach fields; codegen still sees
only MIR (ADR 0003).

### Diagnostic codes

| Code | Meaning |
|------|---------|
| `parser::empty-payload` | `V()`, a payload without types |
| `ty::wrong-payload-count` | a variant built, or matched (ADR 0040), with a number of values other than its payload's |

### Conformance tests

- **ui (parser):** a payload with one and with several types, a trailing
  comma in a payload, `V()`, a payload before and after `= value` (the
  latter rejected).
- **ui (typeck):** `!` as a payload type, also in an enum nothing uses
  (reported once); `[u8]` as a payload type; `enum List: u8 { Nil = 0,
  Cons(u8, List) = 1 }` and a cycle through a struct (`ty::infinite-size`),
  accepted with `*const List`; `B([u8; 0x7FFF_FFFF])`, over the limit
  only through its tag, and `A([u8; 0x7FFF_FFFE])` next to `B(u32)`,
  over it only through the rounding (`ty::too-large`); an enum with a `!` payload used in `size_of`, in
  building a variant, in a `match` and as a parameter, reported once;
  `Msg::Write(3)`, `Msg::Write`, `Msg::Nop(1)`, `Msg::Nop()` and
  `Mode::Off()` (`ty::wrong-payload-count`); `= Msg::Nop as u8 + 1` as a
  value (`ty::invalid-cast`); a literal out of range for its payload
  type; `&raw const Msg::Write(3, 4)`; `==` between two `Msg`s; `m as u8`
  and `3 as Msg`; `m.f` and `offset_of(Msg, f)`; a `Msg` read and written
  through a volatile pointer (`ty::volatile-aggregate`); a
  payload type with `#[align(8)]` raising the enum's alignment, and that
  enum as a field of a packed struct (`ty::aligned-in-packed`); accepted:
  payloads of every kind of type, `*mut T` passed where a payload is
  `*const T` and `*const [u8; 4]` where it is `*const [u8]`, an enum
  with payloads as a field, an array element, a
  parameter, a return value, a constant and a static, `size_of` and
  `align_of`.
- **e2e:** the payload values of `Msg::Write(f(), g())` are evaluated
  left to right, observed through a counter.
- **Layout against clang** (`tests/layout/`): enums with payloads on `u8`,
  `u16` and `u32`, with payload values of alignment 1, 2, 4 and 8 and a
  struct payload, against their C unions: size, alignment and the offset
  of every payload value.
- **Calls against clang:** an e2e program passes and returns enums with
  payloads of each row of ADR 0025's table (one register, a register
  pair of alignment 4, a register pair of alignment 8 with
  `enum E: u64 { A = 0, B(()) = 1 }` against `union { uint64_t tag; }`,
  and memory) to and from C functions using the C unions, in both
  directions.
- **LLVM IR snapshots:** building a variant writes its tag and its
  payload values at their offsets; a variant without payload in an enum
  with payloads writes only its tag; a constant of such an enum, with
  zero bytes outside its variant; a `static mut` of zero bytes in `.bss`.
- **Differential test** (ADR 0027): variants of every kind built at
  compile time in constants and at run time, compared through their tag
  and payload bytes, read through a `*const u8`.

## Consequences

- Messages, commands and results carry their data in the variant that
  uses it, and the compiler checks that each variant is built with the
  values it declares.
- A C header describing a tagged union as a union of structs matches an
  Erebos enum byte for byte, and both pass it alike across calls.
- The layout is compact: a payload starts right after the tag, not at the
  alignment of the largest payload; but values of different variants do
  not start at one common offset, as they would after a tag followed by a
  union.
- `Option(*mut T)` and similar types pay for their tag; there is no niche
  optimization to rely on or to break.
- Adding a payload to a variant changes the whole enum's layout and
  calling convention, which a C twin must follow.
- `!` is no payload type, so when `Option(T)` and `Result(T, E)` become
  enums in M6, `Result(u32, !)` is `ty::never-in-memory` unless M2.5 or
  M6 amends this ADR.
- Payload values have no names: a variant with many values of one type,
  such as `Rect(u32, u32, u32, u32)`, is better written with a struct
  payload, `Rect(Rect)`.

## Alternatives considered

- **Exactly one payload value per variant** — the smallest rule, and
  extending it later breaks nothing; but every variant with two values
  needs a struct of its own, and without struct patterns its values could
  not be matched inside the payload.
- **Named payload fields** (`Write { reg: u8, value: u16 }`), as Rust's
  struct variants — the most explicit, but every pattern must name every
  field, and `Option` would need `Some { value: T }`.
- **A tag followed by a union of the payloads** (Rust's `#[repr(C, u8)]`)
  — the most common C idiom, but every payload starts at the alignment of
  the largest one: with `Write(u8, u32)`, 12 bytes instead of 8.
- **Niches**, letting a payload value stand for a variant (Rust's
  `Option<&T>`) — smaller values, but layouts that depend on payload
  types invisibly, and nothing in Erebos has an invalid value to use
  (ADR 0019, 0022, 0036).
- **`as` giving the tag of an enum with payloads** — useful for tables
  indexed by variant, but would make an aggregate convertible with `as`;
  a `match` gives the same number visibly.
- **`==` comparing tag and payload** — would compare aggregates, which
  structs do not (ADR 0021), and would have to skip bytes outside the
  variant.
- **Allowing `!` payloads as variants that never exist** — would serve
  `Result(u32, !)`, but a tag read from memory can name such a variant,
  and its payload would then be a `!` in memory (ADR 0034).
