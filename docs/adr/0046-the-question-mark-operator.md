# 0046. The `?` operator

- Status: Accepted
- Date: 2026-10-19

## Context

With `Result(T, E)` and `Option(T)` (ADR 0045), a function that calls
other fallible functions passes their failures on:

```
let d: u32 = match digit(c) {
    .Ok(let v: u32) => v,
    .Err(let e: ParseError) => return .Err(e),
};
```

Written for every call, the failure handling hides the computation.
Rust's `?` shortens this to `digit(c)?`, converting the error with
`From`; Zig's `try digit(c)` does the same with error sets merged by the
compiler. The roadmap fixes that in Erebos `?` requires the same error
type, with no conversion (ADR 0008).

## Decision

### Syntax

`e?` is a **postfix operator**, binding as tightly as indexing and
field access: `parse(s)?.len` is `(parse(s)?).len`, `-f()?` is
`-(f()?)`, `*p?` is `*(p?)`. As for any operator, a statement that
starts with a block-like expression ends with it (`if c { } - 1` is not
a subtraction), so a `match` whose result goes to `?` at the start of a
statement is written `(match r { ... })?`. A type ends at its last
token, so `x as T?` is a parse error after the type; `(x as T)?` is the
spelling, though no cast gives an `Option` or a `Result`.

### Types

- The operand is typed **on its own**, without an expected type, and
  must be an `Option(T)` or a `Result(T, E)`; anything else is
  **`ty::try-operand`**. A `.V` operand is `ty::untyped-variant`
  (ADR 0044).
- The enclosing function must return the same kind:
  - on an `Option(T)`, an `Option(U)`, for any `U`;
  - on a `Result(T, E)`, a `Result(U, E)` with **the same `E`**, for
    any `U`. Same means identical: none of the implicit conversions of
    ADR 0024 and 0031 applies, so a `Result(T, *mut u8)` operand in a
    function returning `Result(U, *const u8)` is an error, although the
    `match` below would accept it.

  Otherwise, including in a function returning `!`, `()` or anything
  else, it is **`ty::try-return`**, which shows the operand's type and
  the return type, and for a different `E` says that errors are never
  converted (ADR 0008).
- `e?` has type `T`.
- An operand of type `!` (`(loop {})?`) gives `e?` the type `!`, and an
  erroneous operand the erroneous type; neither reports anything, and
  the return type is not checked. An erroneous return type, or one
  whose type arguments are in error, reports nothing more either.

### Meaning

`e?` evaluates `e` once, then reads its tag:

- `Some` or `Ok`: the value of `e?` is a copy of the payload;
- `None`: the function returns `.None`;
- `Err`: the function returns `.Err(x)`, `x` a copy of the payload;
- a tag that is no variant (ADR 0036, 0045): the panic function is
  called with the cause "enum value is no variant", as a `match` without
  a catch-all does (ADR 0040), and at compile time the evaluation fails
  with `eval::panic`.

So `e?` behaves as

```
match e {
    .Ok(let v: T) => v,
    .Err(let x: E) => return .Err(x),
}
```

(with `.Some` and `.None` for an option). It is an operand evaluated
where it appears (ADR 0018): in `f(a()?, b())`, when `a()` fails, `b()`
does not run.

### At compile time

`?` returns from the enclosing function, so it cannot appear in code
that runs at compile time on its own: a `comptime` expression or block,
or the initializer of a constant or static, or an array length. There
it is `resolve::return-in-comptime`, as `return` is (ADR 0029), with a
message naming `?`. A function using `?` may be called at compile time
like any other.

### Code

A switch on the operand's tag with two cases, the default calling the
panic function. As in ADR 0036, nothing tells LLVM that the tag is a
variant.

### Diagnostic codes

| Code | Meaning |
|------|---------|
| `ty::try-operand` | `?` on a value that is neither an `Option` nor a `Result` |
| `ty::try-return` | `?` in a function whose return type is not an `Option` (for an `Option` operand), or a `Result` with the operand's error type (for a `Result` operand) |

### Conformance tests

- **ui (parser):** `f()?`, `a[i]?`, `s.f?`, `f()?.x`, `f()??`, `-f()?`,
  `*p?`; `match r { ... }?` at the start of a statement, rejected, and
  in parentheses, accepted; `x as T?`, rejected.
- **ui (resolve):** `?` in a `comptime` expression, a `comptime` block,
  a constant's initializer and an array length
  (`resolve::return-in-comptime`).
- **ui (typeck):** `ty::try-operand` on a `u32`, a struct and an enum
  with payloads; `ty::try-return` for an option in a function returning
  a result and the reverse, a result with another error type, and `?`
  in functions returning `u32`, `()` and `!` (the entry function);
  `.Ok(1)?` (`ty::untyped-variant`); a `Result(T, *mut u8)` operand in
  a function returning `Result(U, *const u8)` (`ty::try-return`);
  `(loop {})?` and an erroneous operand, reporting nothing; accepted: `Option(T)?` returning
  `Option(U)` with `U` other than `T`, `Result(T, E)?` likewise, and the
  value of `e?` used where `T` is expected, with a mismatch reported
  where it is not.
- **MIR and LLVM IR snapshots:** the tag switch, the return of `.Err(x)`
  and `.None`, and the panic default; no range metadata on the tag
  load.
- **ui (eval):** a function using `?` evaluated at compile time, in
  both outcomes; a tag that is no variant reaching `?` at compile time
  (`eval::panic` with the cause).
- **e2e:** errors propagated through three calls; `f(a()?, b())` not
  calling `b` when `a` fails, observed through a counter; a tag that is
  no variant reaching `?` panics, at both optimization levels.
- **Differential test** (ADR 0027): functions using `?` on options and
  results, at compile time and at run time.

## Consequences

- Passing a failure on costs one character, and the failure's type
  stays visible in every signature it crosses.
- A function that mixes error types converts them itself, with a
  `match`; there is no `From`, as there are no implicit conversions.
- Calls through `?` cost a tag compare and a branch, plus the check for
  a tag that is no variant. Across a call LLVM cannot see the tag, so
  the check stays unless the callee is inlined.

## Alternatives considered

- **Converting the error, as Rust's `From`** — would be an implicit
  conversion (ADR 0008), and needs traits or overloading Erebos does not
  have.
- **A prefix `try e`, as Zig** — reads well alone, but chains poorly
  (`(try parse(s)).len`), and Zig's error sets have no Erebos
  counterpart.
- **`?` only on `Result`** — `Option` would need a `match` for the same
  pattern; both cases are one rule.
- **Treating a tag that is no variant as a failure** — would return a
  `.None` or an `.Err` the operand never held, and for `Result` there is
  no `E` to return.
