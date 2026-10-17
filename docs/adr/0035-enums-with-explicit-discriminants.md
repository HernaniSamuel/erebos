# 0035. Enums with explicit discriminants

- Status: Accepted; amended by 0038 (a `match` that needs no value is checked on an enum with a failed or repeated value)
- Date: 2026-10-16
- Amends: 0023 (an enum is a scalar), 0032 (enum variants are known at
  compile time)

## Context

Firmware is full of named integers: the states of a state machine, the
values of a register field, command codes in a protocol. Today they are
`const` items of an integer type, which nothing ties together: a function
taking a `u8` state accepts any `u8`, and a `match` (ADR 0037) could not
know which values exist.

In C, enumeration constants are `int`, and the enum type itself is an
integer type the implementation chooses (`-fshort-enums` and some
embedded ABIs change it) unless C23's `enum E : uint8_t` fixes it; the
constants are numbered implicitly, so inserting one renumbers those after
it. Rust's enums choose their own size unless `#[repr(u8)]` is given, and
number implicitly too. On bare metal the values often come from a
datasheet or from C code, so they are part of the interface.

ADR 0022 left the layout of enums to M2.4. This ADR covers enums whose
variants carry no data (M2.4a); variants with payloads come in M2.4b.

## Decision

### Declaration

```
enum Mode: u8 {
    Off = 0,
    Idle = 1,
    Run = 3,
}
```

- An enum is an item, in the one namespace of items (ADR 0029). Its name
  is followed by `:` and its **integer type**, any of `u8 u16 u32 u64 usize
  i8 i16 i32 i64 isize`, and then its variants, separated by commas, with
  an optional trailing comma. Any other type there (`bool`, a struct,
  another enum, a pointer, an array, `()`, `!`) is `resolve::enum-type`.
- **Both are required.** Without the integer type, the declaration is
  `parser::enum-without-type`; a variant without `= value` is
  `parser::variant-without-value`. No value is ever implied, so inserting
  a variant never renumbers the others.
- A variant's value is an expression of the enum's integer type,
  evaluated at compile time like an array length (ADR 0029): `= 1 << 3`,
  `= CMD_BASE + 1`, `= Mode::Idle as u8 + 1`. A literal takes the integer
  type, so `= 300` in a `u8` enum is `ty::literal-out-of-range`, and
  `= -1` needs a signed type. A value that needs itself, directly or
  through other variants or constants, is a `ty::cycle`.
- Two variants with one name are `resolve::duplicate-variant`; two with
  one value are `ty::duplicate-discriminant`, naming both.
- A value that fails to evaluate is reported once, like a constant
  (ADR 0029), and so is a duplicate value. In either case uses of the
  enum report nothing more because of it: a `match` on that enum is not
  checked for exhaustiveness or dead arms (ADR 0037).
- An enum needs at least one variant (`resolve::empty-enum`): it names
  values, and with none it would be its integer under another name.
- No attribute applies to an enum (`resolve::misplaced-attribute` for
  `#[packed]` and `#[align(N)]`): its layout is its integer type's.
- `enum` becomes a keyword.

### Naming a variant

`Mode::Run` is the variant `Run` of `Mode`, an expression of type `Mode`.
`::` is a new token; modules (M2.6) reuse it for paths.

- A variant is a **value, not a place**, like a constant: `&raw const
  Mode::Run` is `ty::not-a-place`, and it cannot be assigned.
- `Mode::Walk`, with no such variant, is `resolve::unknown-variant`; a
  name before `::` that is not an enum is `resolve::not-an-enum`, and one
  that names nothing is `resolve::unknown-type`. The name before `::` is
  looked up among items only, as the name of a struct literal is, so a
  local variable of the same name does not hide the enum. `Mode` alone is
  not an expression, and reports what a struct's name alone reports
  today (`resolve::unknown-name`).
- A variant is **known at compile time** (amends ADR 0032), and so is a
  variant converted with `as`: `[u8; Mode::Run as usize]` is fine, and
  `x >> (Mode::Run as u32)` is checked like any known shift amount.

### Operations

- `==` and `!=` compare two values of the same enum type by their
  integers. `<`, `<=`, `>`, `>=`, arithmetic and bitwise operators do not
  apply (`ty::invalid-operand`): what a number means for ordering is up
  to the program, which can compare `x as u8`.
- **`as` converts an enum to any integer type**, as its integer type
  would convert (ADR 0019): `Mode::Run as u32` is 3, and with
  `enum Level: i8 { Low = -1 }`, `Level::Low as u32` is `0xFFFF_FFFF`.
- **Nothing converts to an enum** (`ty::invalid-cast`): not its own
  integer type, not another enum, not the enum itself (as `bool` does not
  convert to `bool`). An enum converts neither to `bool` nor to a pointer.
  The help suggests a `match` from the integer (`3 => Mode::Run`). A
  literal cast to an enum, `3 as Mode`, has no type to take (ADR 0017),
  and reports only this `ty::invalid-cast`. So a program that only writes
  variants never makes a value that is no variant; ADR 0036 says what
  happens with those that come from memory.
- A literal compared with an enum, as in `m == 3`, is checked against the
  enum's type like any literal operand, and is `ty::mismatched-types`,
  whose help suggests a variant or `m as u8 == 3`.
- `match` (ADR 0037) tells the variants apart.

### Layout and calls

An enum **is stored as its integer type**: it has that type's size and
alignment (ADR 0022), and its value is that integer. `size_of(Mode)` is 1.

- **C compatibility.** Its layout and calling convention are those of its
  integer type, so the C twin of `Mode` is `uint8_t`, or C23's
  `enum Mode : uint8_t { ... }`, as a field, a parameter or a return
  value.
- **Across calls** it is a scalar, not an aggregate (ADR 0025): passed and
  returned as its integer type, extended by that type's signedness
  (ADR 0006).
- **Through a volatile pointer** it is a scalar, which ADR 0023 defines as
  an integer, `bool` or a pointer; this adds enums to that list (amends
  ADR 0023). An access is one access of its integer type's width. An enum on `u64` or `i64` cannot be accessed
  through one (`ty::volatile-too-wide`), like the integer itself.
- An enum has no fields: `x.f` and `offset_of(Mode, f)` are
  `ty::no-fields`.
- In constants and statics, its value is emitted as its integer; a
  `static mut` holding a variant whose value is 0 goes to `.bss`
  (ADR 0030).

### Diagnostic codes

| Code | Meaning |
|------|---------|
| `parser::enum-without-type` | an enum without `: Type` |
| `parser::variant-without-value` | a variant without `= value` |
| `resolve::enum-type` | an enum whose type after `:` is not an integer type |
| `resolve::duplicate-variant` | two variants with one name |
| `resolve::empty-enum` | an enum without variants |
| `resolve::unknown-variant` | `E::V` where `E` has no variant `V` |
| `resolve::not-an-enum` | `X::V` where `X` is not an enum |
| `ty::duplicate-discriminant` | two variants with one value |

### Conformance tests

- **ui snapshots** for each code above, with duplicate values both
  written as literals and computed from a constant; `= 300` in a `u8`
  enum; `= -1` in a `u8` enum; a cycle between two variants' values;
  `#[packed]` on an enum; `enum E: bool` and `enum E: Other`; a failed
  value (`= 1 / 0`) reported once; casts from an integer variable, from a
  literal (`3 as Mode`, one error), to `bool` and to another enum
  (`ty::invalid-cast`); `m == 3` (`ty::mismatched-types`); `<` between
  variants (`ty::invalid-operand`); `Mode` alone; a local named like the
  enum, which does not hide it before `::`;
  `&raw const Mode::Run`; and what is accepted: `==`, `as` to every integer
  type, a variant in an array length, an enum as a field, parameter,
  return value, constant and static.
- **Layout against clang** (`tests/layout/`): a struct with enum fields on
  `u8`, `u16` and `i32` against the same struct in C23 with
  `enum E : uint8_t` and friends.
- **Calls against clang:** an e2e program passes enums on `u8` and `i8`
  (a negative value, for the sign extension) to a C function and gets them
  back, in both directions.
- **LLVM IR snapshots:** a variant as an integer immediate, `==` as an
  integer compare, and a volatile access of an enum with its integer's
  width.
- **Differential test** (ADR 0027): `as` from enums on signed and
  unsigned types, at compile time and at run time, the run-time values
  coming from an `extern fn` identity on the enum written in C, so that
  LLVM cannot fold them.

## Consequences

- States, register values and command codes get names and a type of their
  own, at no cost: in memory and in calls an enum is its integer.
- Every value is written out, which costs a few characters per variant
  and makes renumbering impossible by accident.
- A C header and an Erebos declaration agree byte for byte when the C side
  uses the same integer type.
- Ordering and arithmetic on enums go through `as`, visibly.
- Values that are no variant can still appear through memory; ADR 0036
  makes them defined.

## Alternatives considered

- **Implicit numbering (C, Rust)** — shorter state machines, but
  inserting a variant renumbers the rest, silently, which breaks values
  shared with hardware or C.
- **An implicit integer type** (C's `int`, or the smallest that fits) —
  the layout would depend on the values, invisibly.
- **`Mode.Off`, as in Zig** — fits types as values (M6), but `.` already
  reads a field of a value.
- **`as` from an integer to an enum** — shorter decoding, but makes values
  that are no variant as easy to write as variants; a `match` states which
  integers mean which variant.
- **Ordering enums by value** — rarely what the program means, and `as`
  makes it explicit when it is.
