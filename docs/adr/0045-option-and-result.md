# 0045. `Option(T)` and `Result(T, E)`

- Status: Accepted
- Date: 2026-10-19

## Context

A function that can fail, such as a parser, a checked multiplication or
a read from a device that may time out, returns either a value or the
reason it has none. ADR 0039 gives Erebos enums with payloads, so each
program can declare its own `enum ParsedU32: u8 { Ok(u32) = 0, Err(ParseError) = 1 }`;
but every such enum is a new type with its own name, written again for
every value type, and the `?` operator (ADR 0046) needs to know which
variant is the value and which the failure.

The roadmap fixes the shape: `Option(T)` and `Result(T, E)` are
compiler-known types, as `[T; N]` is, written with parentheses like any
comptime call. In M6 they become ordinary comptime functions in `core`
returning enums (ADR 0027), with the same syntax, so programs do not
change. `Option(*mut T)` takes 8 bytes: address 0 is valid (ADR 0019),
and there are no niches (ADR 0039).

The roadmap leaves to this step whether `!` may be a type argument, as
in an infallible `Result(u32, !)` (ADR 0034).

## Decision

### The types

`Option(T)` and `Result(T, E)` are enums with payloads (ADR 0039), as if
declared

```
enum Option(T): u8 { None = 0, Some(T) = 1 }
enum Result(T, E): u8 { Ok(T) = 0, Err(E) = 1 }
```

(not valid syntax: an enum with parameters needs M6). Everything ADR
0039 and 0040 say of an enum with payloads holds for them: their layout,
how they cross calls, that they are aggregates (no `==`, no `as`, no
fields, no whole volatile access), that they may be constants, statics,
fields, elements, parameters and return values, that no value of them is
known at compile time, what a value that is no variant is, and how a
`match` reads them.

### Writing them

- `Option(T)` and `Result(T, E)` are written wherever a type is: `let`,
  parameters, return types, fields, array elements, pointees
  (`*const Option(u32)`), statics, constants, payloads, `size_of`,
  `extern fn` signatures, and as type arguments themselves
  (`Option(Option(u8))`).
- A **type argument** is a type, and must be one a payload may have
  (ADR 0039): integers, `bool`, pointers, slices, arrays, structs, enums
  with or without payloads, and `()` and other zero-sized types, which
  take no bytes (`Result((), ParseError)`). `[T]` is `ty::unsized`.
- **`!` is not a type argument** (`ty::never-in-memory`): it is a
  payload type, and ADR 0039 rules `!` out there, so `Result(u32, !)` is
  an error. Without generics, no code is written for a `Result(T, E)`
  whose `E` is chosen later, so an infallible result has no use yet; M6
  revisits this with generic code.
- A type containing itself through a type argument, as `struct Node {
  next: Option(Node) }`, is `ty::infinite-size` (ADR 0039);
  `Option(*const Node)` is the way to write it. A type larger than the
  target can hold is `ty::too-large`. Since `Option(T)` has no
  declaration, these are reported where it is written: `ty::too-large`
  at each place an `Option(T)` or `Result(T, E)` that is too large is
  written, as for an array type, when every argument has a layout; one
  whose argument has none, as `Option(Big)` with `struct Big` too large,
  or `Option(Node)` with `Node` in a cycle, reports nothing more, the
  argument's error having been reported at its declaration.
  `ty::infinite-size` is labelled at the first declared struct or enum
  of the cycle, counted from where the cycle was found, as today (a
  cycle always goes through one, since type arguments alone are
  finite), with the `Option(T)` in the path it shows.
- A type argument in error, such as `!` or an unknown name, is reported
  once, and the whole type then counts as erroneous: its uses report
  nothing more because of it, as for a struct with such a field
  (ADR 0034).
- `Option` and `Result` are **names of built-in types**: **any item**
  with one of these names, a struct, enum, function, constant or static,
  is `resolve::duplicate-type`, as ADR 0029 asks of items named like
  built-in types. Items share one namespace (ADR 0029), and in M6
  `Option(u32)` is a call resolved in it, so a `fn Option` written now
  would change what that type means then. Either name
  with the wrong number of arguments, `Option`, `Option()`,
  `Option(u8, u8)`, `Result(u32)`, is **`resolve::type-argument-count`**,
  which says how many it takes; so are arguments after any other type
  name, `u32(u8)` or `Point(u8)`, which take none.
- **One type per set of arguments**: two `Option(T)` are the same type
  when their arguments are, compared after array lengths are computed
  (ADR 0029): `Option([u8; 4])` and `Option([u8; 2 + 2])` are one type.
  Diagnostics show it as written with computed lengths:
  `Result([u8; 4], ParseError)`.

### Their variants

Their variants are named by the expected type (ADR 0044): `.None`,
`.Some(v)`, `.Ok(v)`, `.Err(e)`, in expressions and in patterns. There
is no path to them: `Option::None` is `resolve::not-an-enum`, whose help
shows `.None`, and `Option(u32)::None`, which parses as a call followed
by `::`, stays `parser::unexpected-token`, with the same help when the
name before the parentheses is `Option` or `Result` and its arguments
parse as expressions; `Option(*const u8)::None`, and the same path in a
pattern, fail earlier with the parser's usual message. In M6, when
`Option(u32)` is a value, a path may follow.

```
fn checked_mul10(v: u32) -> Option(u32) {
    if v > 429_496_729 { return .None; }
    .Some(v * 10)
}

match parse(b"42") {
    .Ok(let v: u32) => print_u32(v),
    .Err(.Empty) => print(b"empty"),
    .Err(.BadDigit) => print(b"bad digit"),
    .Err(.Overflow) => print(b"overflow"),
}
```

Witnesses of `ty::non-exhaustive-match` show them the same way: `.None`,
`.Err(_)`, `.Some(Mode::Run)`.

### Layout and calls

As enums with payloads (ADR 0039): a `u8` tag at offset 0, and the
payload value as close after it as its alignment allows.

| Type | Size | Alignment | Payload at |
|------|------|-----------|------------|
| `Option(u8)` | 2 | 1 | 1 |
| `Option(u32)`, `Option(*mut T)` | 8 | 4 | 4 |
| `Option(())` | 1 | 1 | none |
| `Option(u64)` | 16 | 8 | 8 |
| `Result(u32, ParseError)`, `ParseError` on `u8` without payloads | 8 | 4 | `Ok` 4, `Err` 1 |

Their C twins are unions of structs that each start with the tag, as
for any enum with payloads:

```c
union OptionU32 {
    uint8_t tag;                                  /* 0: None */
    struct { uint8_t tag; uint32_t value; } some; /* 1: Some */
};
```

They cross calls as aggregates (ADR 0025): `Option(u8)` in one
register, `Option(u32)` in a register pair, `Option(u64)` in memory.

### In M6

`Option` and `Result` become comptime functions in `core` returning
enums declared with these variants, values, tag type and order. Their
layout, calling convention and C twins do not change, and neither does
any program: types are still written `Option(T)`, variants `.V`. Whether
`!` may then be a type argument, for generic code instantiated with an
infallible error type, is decided there.

### In the compiler

Each distinct `Option(T)` or `Result(T, E)` is an enum of its own,
made when a type is normalized (its array lengths computed), with the
variants above. From type checking on, the compiler sees an enum with
payloads, so layout, MIR, code generation and the evaluator need nothing
new.

Nothing the compiler shows depends on the order in which these enums
are made, which follows the on-demand schedule (ADR 0028): the
`--emit=typeck` and `--emit=mir` dumps list them after the declared
enums, sorted by their displayed name, and the help of
`ty::untyped-variant` (ADR 0044) looks only at declared enums.

### Diagnostic codes

| Code | Meaning |
|------|---------|
| `resolve::type-argument-count` | `Option` or `Result` with the wrong number of type arguments, or type arguments after a type that takes none |

### Conformance tests

- **ui (parser):** `Option(u32)`, `Result(u32, E)` with a trailing
  comma, nested, behind pointers and in array types, in every position
  a type is written.
- **ui (resolve):** `resolve::type-argument-count` for `Option`,
  `Option()`, `Option(u8, u8)`, `Result(u32)`, `u32(u8)` and `Point(u8)`;
  `struct Option {}` and `enum Result: u8 { A = 0 }`
  (`resolve::duplicate-type`), and so are `fn Option() {}` and
  `const Result: u32 = 0;`; `Option::None`
  (`resolve::not-an-enum` with the `.None` help) and
  `Option(u32)::None` (`parser::unexpected-token` with the same help);
  an unknown type as an argument, reported once.
- **ui (typeck):** `Option(!)`, `Result(u32, !)` (`ty::never-in-memory`,
  once, also in a function nothing calls) and `Option([u8])`
  (`ty::unsized`); `struct Node { next: Option(Node) }`
  (`ty::infinite-size`), accepted with `Option(*const Node)`;
  `struct A { x: [u8; size_of(Option(Node))] }` declared before that
  `Node`, labelled at `Node`; `Option([u8; 0x7FFF_FFFF])`
  (`ty::too-large`), written twice, reported at both; `Option(Big)`
  with `struct Big` too large (one error, at `Big`), and a parameter of
  type `Option(Node)` beside the `Node` cycle (nothing more); `==` between two
  options, `o as u8`, `o.f`, a volatile read of an `Option(u8)`;
  `Option([u8; 4])` passed where `Option([u8; 2 + 2])` is expected,
  accepted; a mismatch between `Option(u32)` and `Option(u16)` showing
  both; witnesses `.None`, `.Some(_)`, `.Err(_)` and a nested one;
  accepted: options and results as fields, elements, parameters, return
  values, constants and statics, `size_of` and `align_of` of each row of
  the table above, checked in a `comptime` block.
- **Layout against clang** (`tests/layout/`): a struct with fields of
  `Option(u8)`, `Option(u32)`, `Option(u64)`, `Option(())` and
  `Result(u32, Mode)` against its C twin with the unions.
- **Calls against clang:** an e2e program passes and returns
  `Option(u8)`, `Option(u32)`, `Option(u64)` and `Result(u32, E)` to and
  from C functions using the C unions, in both directions.
- **e2e:** options and results built with `.V`, stored, passed and
  matched; a tag that is no variant, read through a cast pointer from
  an aligned byte array, panics in a `match` without a catch-all, at
  both optimization levels.
- **Differential test** (ADR 0027): options and results built and
  matched at compile time and at run time.

## Consequences

- A failure is a value of one type, written once (`Result(u32,
  ParseError)`), that `?` (ADR 0046) and `match` understand.
- `Option` and `Result` cost what an enum with payloads costs: a tag
  byte and padding, never a niche. `Option(u32)` takes 8 bytes and two
  registers.
- Programs written now do not change in M6, and neither do their C
  twins.
- `Option` and `Result` are reserved type names until M6 makes them
  functions in `core`.
- `Result(T, !)` is an error until M6 says otherwise.

## Alternatives considered

- **Each program declares its own result enums** — no compiler support
  needed, but one enum per value type, and `?` could not tell which
  variant is the failure.
- **A tag of `u32`**, the size of a C `int` — no gain in calls or
  alignment for small payloads, and `Option(u8)` would take 8 bytes
  instead of 2.
- **`Result(T, !)` with no `Err` variant**, whose tag value 1 would be
  no variant — sound, since its payload is never read, but a rule with
  no use before generics, which M6 can add when it has one.
- **Niches for pointers, as Rust's `Option<&T>`** — no null address to
  use (ADR 0019), and ADR 0039 refuses niches.
