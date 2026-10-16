# 0034. `!` is not a type of memory

- Status: Accepted
- Date: 2026-10-16
- Amends: 0021 (field and element types, and the type of array
  expressions), 0022 (`!` has no layout a program can ask for), 0029 and
  0030 (enforces that constants, `comptime` values and statics have types
  with values)

## Context

`!` is the type of an expression that does not finish: `loop {}` without
`break`, `return`, a call to a function returning `!`. No value of type
`!` ever exists.

Parameters (`ty::param-type`) and cast targets (`as !`, `ty::invalid-cast`)
already reject `!`, but the compiler accepts it as the pointee of a
pointer (`*const !`), a struct field, an array element, and the type of a
`static`, `extern static` or `const`. ADR 0029 limits constants to "any
type with values", and ADR 0030 statics to the types of constants, but
nothing enforces it. Through these positions a program can read a `!`
from memory:

```
fn f() -> u32 {
    let x: u32 = 1;
    *(&raw const x as *const !)
}
```

At run time the compiler emits `unreachable` after such a read, so what
happens next is undefined. At compile time the interpreter reaches that
`unreachable` and the compiler panics. Nothing outside the program forces
this case to be undefined.

## Decision

`!` is a type of expressions, never of memory.

### Where `!` is an error

It is an error (`ty::never-in-memory`) as:

- the pointee of a pointer (`*const !`, `*mut volatile !`), or the element
  type of a slice (`*const [!]`, ADR 0031);
- the type of a struct field;
- the element type of an array type (`[!; 0]` included);
- the type of a `static`, `extern static` or `const`;
- the type of a `comptime` expression's value (`comptime loop {}`),
  since ADR 0029 requires a type with values. A `comptime { ... }` item
  is a check of type `()` (ADR 0029) and is not affected: one that never
  finishes still stops at the step limit;
- the type operand of `size_of`, `align_of` or `offset_of`.

`&raw const` or `&raw mut` of a place of type `!` is the same error, since
its result would be a `*const !`. Such places are a `let x: !`, and places
formed from an expression of type `!`: `*(loop {})`, `(loop {})[0]`.

Each `!` written in one of these positions is reported once, at its span,
also in declarations nothing uses. The whole type that contains it
(`*const !`, `[!; 0]`, the struct with the field) then counts as
erroneous, which produces no further diagnostics, such as a mismatch in
`let p: *const ! = &raw const x;` (no `ty::too-large` or `ty::unsized`
for the same type, no error in each `size_of` of a struct that contains
it).

### Where `!` stays valid

Where no memory is involved:

- as the type of an expression, including places formed from one, read
  but never stored: `*(loop {})`, `(loop {})[0]`, `on_panic().b`
  (`tests/ui/codegen_deref_never.ers`);
- as a function's return type (`-> !`, including `extern fn`);
- in a `let` whose initializer never finishes (`let x: ! = loop {};`).

**Array expressions.** An array literal or repeat expression in which
any element has type `!` (`[loop {}; 3]`, `[loop {}, loop {}]`,
`[one(), loop {}]`) has type `!`, like a block that does not finish, not
`[!; N]`. Its other elements are still checked, against the expected
element type if there is one, otherwise against the type of the first
element whose type is not `!`: `[loop {}, one(), true]` reports the
`bool` against `u32`. A literal with no type to take, as in
`[loop {}, 1]` with no expected type, is `ty::untyped-literal`, as
anywhere else.

### Layout

A program cannot ask for the layout of `!` (above). Inside the compiler,
locals and return slots of type `!`, such as a `let x: !` or the result
of a `-> !` function, take no storage, as zero-sized places do
(ADR 0022). This amends ADR 0022's "every type has a size and an
alignment" for `!`.

### Why this is enough

Since no program can store a `!` or point to one, a read of type `!`
only happens in code that is never reached, and the `unreachable` after
it is never reached either, at run time or at compile time. Reads of `!`
are no longer undefined, and nothing needs checking at run time.

### Diagnostic codes

| Code | Meaning |
|------|---------|
| `ty::never-in-memory` | `!` as a pointee, slice element, field, array element, the type of a static, `extern static`, `const` or `comptime` value, a layout query operand, or under `&raw` |

The help says that `!` is the type of expressions that do not finish. It
suggests `()` for a type without data, and, for a `static` or
`extern static`, a type of non-zero size such as `u8`, whose address is
a real one
(a zero-sized place's address is unspecified, ADR 0022).

### Conformance tests

- **ui snapshots** for each position above: `*const !`, `*mut volatile !`,
  a struct with a `!` field (unused, and used in `size_of`, reported
  once), `[!; 0]`, `static N: ! = loop {}`, `extern static X: !`,
  `const C: ! = loop {}`, `comptime loop {}`, `size_of(!)`,
  `offset_of(!, f)`, `&raw const x` with `x: !`, and
  `&raw const *(loop {})`. `*const [!]` comes with slices (ADR 0031).
- **ui snapshots** for what stays valid: `-> !`, `extern fn f() -> !`,
  `let x: ! = loop {};`, `*(loop {})`, `(loop {})[0]`, `on_panic().b`,
  `[loop {}; 3]` where a `[u32; 3]` is expected (it has type `!`), and
  `comptime { loop {} }`, still `eval::step-limit`.
- **ui snapshots** for array expressions: `[loop {}, one(), true]`
  reporting the `bool`, and `[loop {}, 1]` with no expected type,
  `ty::untyped-literal`.
- The program in the Context, which panicked the compiler, is now a type
  error.

## Consequences

- No program can read a `!` from memory, so that undefined behaviour
  disappears at no run-time cost, and the evaluator's `unreachable` holds
  by construction. (An `extern fn ... -> !` that returns stays undefined:
  that is code outside the program breaking its declaration, ADR 0026.)
- `*const !`, `[!; N]`, fields, statics and constants of type `!` are no
  longer accepted. They describe memory that can hold nothing; `()` and
  zero-sized structs remain for that.
- The rule reaches every later construct that puts a type in memory:
  enum payloads (M2.4), the type arguments of `Option(T)` and
  `Result(T, E)` (M2.5, where `Result(u32, !)` would be an infallible
  result), and type arguments in general (M6). Each of those steps
  decides how `!` fits.

## Alternatives considered

- **Reading `!` calls the panic function at run time and is an error at
  compile time** — defined, but pays a check for code that should not
  exist, and keeps types that describe nothing.
- **Leaving it undefined, with a compile-time error only** — the run-time
  case stays undefined, which nothing outside the program forces.
- **Checking only written types, and leaving `[loop {}; 3]` the type
  `[!; 3]`** — harmless, since that array never exists, but two rules
  for one type; giving the expression type `!` keeps `[!; N]` out of the
  checker entirely.
