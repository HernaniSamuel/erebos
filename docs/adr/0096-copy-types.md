# 0096. Copy types and `#[copy]`

- Status: Accepted
- Date: 2026-11-09

## Context

ADR 0094 makes every type copy, affine or (from M5.3) linear, with copy
opt-in: scalars, raw pointers, slices, arrays of a copy type, `Option(T)` and
`Result(T, E)` of copy arguments, and a struct or an enum with payloads
declared `#[copy]`. It leaves to M5.2 how the attribute is written and
checked, and says that a `const` or `comptime` value of a non-copy type is an
error, since evaluating it at each use would forge the handle the type stands
for.

Today every value is copied (ADR 0021), and nothing in the compiler knows
whether a type is copy. Attributes are parsed generically and checked in HIR
lowering (`src/hir/lower/item.rs`), per kind of item, with
`resolve::unknown-attribute`, `resolve::misplaced-attribute`,
`resolve::attribute-argument` and `resolve::duplicate-attribute`.

One construct duplicates a value that ADR 0094 does not name: `[v; N]`
evaluates `v` once and copies it into every element (ADR 0021).

## Decision

### The attribute

`#[copy]` takes no argument. It may be written on a struct (not a bit-field
struct) and on an enum with payloads:

```
#[copy]
struct Point { x: i32, y: i32 }

#[copy]
enum Shape: u8 { Dot(Point) = 0, Line(Point, Point) = 1 }
```

- On any other item, a bit-field struct and an enum without payloads included,
  it is `resolve::misplaced-attribute`, whose help says where it applies. Those
  two are copy already (ADR 0094), and one way of writing each thing is kept.
- With an argument it is `resolve::attribute-argument`; twice, it is
  `resolve::duplicate-attribute`.
- It goes with `#[packed]` and `#[align(N)]`, and changes neither the layout
  nor the ABI.

### Which types are copy

A type is **copy** when it is: an integer, `bool` or `()`; an enum without
payloads; a bit-field struct; a raw pointer, a slice pointer included; an
array whose element type is copy; `Option(T)` with `T` copy; `Result(T, E)`
with `T` and `E` copy; a struct or an enum with payloads declared `#[copy]`.
Every other type with values is **non-copy** (affine in M5.2). `!` and
peripheral types have no values and no kind (ADR 0094).

Whether a struct or enum is copy is what it declares; the check below makes
the declaration true, so no type is computed recursively.

### The check of `#[copy]`

Every field of a `#[copy]` struct, and every payload value of a `#[copy]`
enum, must be of a copy type. One that is not is `ty::copy-field-not-copy`, at
the attribute, labeling the field or payload value and naming its type, once
per field. The help, when that type is a struct or an enum with payloads of
the program, suggests marking it `#[copy]`.

### Values computed at compile time

- A `const` whose type is non-copy is `ty::non-copy-constant`, at its type.
- A `comptime` expression whose type is non-copy is `ty::non-copy-constant`,
  at the expression.
- A static's initializer is not one (ADR 0094): `static S: Driver = ...;`
  with `Driver` non-copy is valid.
- A `comptime` function may build, move and return non-copy values while it
  runs; only a value that leaves evaluation as a constant must be copy.

### Repeat expressions

`[v; N]` with `v` of a non-copy type is `ty::non-copy-repeat`, at `v`, whatever
`N` is, `N` being computed at compile time and possibly 0 or 1. The help
suggests `#[copy]` as above, or an array literal listing each element.

The exception is a `v` that is a variant written without payload values, as
`.None` or `State::Free` of an enum with payloads: it holds no non-copy value,
so copying it forges nothing. `static mut SLOTS: [Option(Handle); 32] =
[.None; 32];` is valid; `[.Some(h); 32]` is not.

## Consequences

- Programs that use a struct or an enum with payloads after copying it, or
  that hold one in a `const`, need `#[copy]`; M5.2 adds it to the tests where
  they need it, and leaves the other types non-copy so that the tests exercise
  moves (ADR 0097). `core` has one struct, `PanicInfo`, which it passes by
  pointer only, and needs nothing.
- The kind of a type is known to type checking and, through the flag the type
  carries, to MIR (ADR 0098).
- A table of copy values with handles in it cannot be a `const`; it can be a
  static, from which copy fields are read (ADR 0097).
- `#[copy]` on a type of M6 made by a `comptime` function is M6's (ADR 0094).

## Alternatives considered

- **Accept `#[copy]` on types that are already copy, without effect** — eases
  code copied between types, but gives two spellings of one type and hides
  that a bit-field struct is always copy.
- **Derive copy from the fields where `#[copy]` is absent** — ADR 0094 rejects
  it: a handle whose fields are plain integers would be copied in silence.
- **`[v; N]` allowed for `N` of 0 or 1, as Rust does** — `N` is computed at
  compile time, so whether an expression is valid would depend on a value
  that may change with a `const`; the literal `[v]` already says one element.
- **`[v; N]` of a non-copy type always an error, `.None` included** — one rule
  fewer, but a table of optional handles in a static would list each `.None`.
  Rust's way, `[const { None }; N]`, needs a constant of a non-copy type, which
  ADR 0094 rules out.
