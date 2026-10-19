# 0048. Types with a failed length or alignment are erroneous

- Status: Accepted
- Date: 2026-10-19

## Context

Array lengths and `#[align(N)]` arguments are values required at compile
time (ADR 0029), and either can fail. The failure is reported where the
value is written. No ADR said what then happens to the types that
contain such an array, or to a struct whose alignment failed.

ADR 0034 settles the same question for `!`: the whole type that
contains it (`*const !`, `[!; 0]`, the struct with the field) counts as
erroneous, which produces no further diagnostics, "no error in each
`size_of` of a struct that contains it".

For a failed length the compiler did something else. It laid the array
out as an erroneous type of size 0, so a struct containing it kept a
layout computed without the array:

```
const N: usize = 1 / 0;
struct Broken { x: [u8; N], y: u32 }
const S: usize = size_of(Broken);      // was 4
comptime { assert(size_of(Broken) == 5); }
```

This reported the division by zero, then `assertion failed`, an error
caused by the wrong size, not by the program. An enum with a payload
`*const Broken` was still checked for coverage, so a `match` reported
`E::A(_)` as not covered. A function using such a type, even only
through a pointer, reached MIR building, which assumes well-typed input,
and the compiler panicked. Commit a47eb48 fixed the panic by treating
such types as erroneous; this ADR records that rule and defines it.

A failed `#[align(N)]` still behaves as lengths did: `#[align(1 / 0)]
struct T { x: u32 }` is laid out as if it had no attribute, so
`size_of(T)` has a value and an `assert` on `align_of(T)` fails as well.

## Decision

A **failed value** is an array length, or the `#[align(N)]` argument of
a struct, that is reported in error, whatever the error: an evaluation
failure (a division by zero, a failed `assert`, the step limit, a
cycle), a type error (`[u8; true]`, a length of type `u32`), or, for an
alignment, a value that evaluates but is not a valid alignment
(`ty::invalid-align`: not a power of two, too large, or below the
struct's natural alignment, as `#[align(3)]`).

The `#[align(N)]` argument of a static is not a failed value in this
sense: a static's alignment is part of no type, so no wrong layout can
spread from it. A static whose alignment is in error is compiled with
the natural alignment of its type, after the error, and the code that
uses it is not affected.

A type **reaches** a failed value when it is, or contains at any depth,
an array whose length failed or a struct whose `#[align(N)]` failed: as
the type itself (`[u8; N]`), a field, an array element, a payload
value, behind a pointer or a slice, or in a type argument
(`*const Broken`, `[Broken]`, `Option(*const Broken)`). Such a type
counts as erroneous, as one containing `!` does (ADR 0034):

- The failed value's own error is the only one reported for it.
- Type checking of code that uses the type runs to the end and reports
  its own errors. A mismatch is still a mismatch: `let p: *const Broken
  = 0 as *const u8;` is `ty::mismatched-types`, and `(*p).z` is
  `ty::unknown-field`. Only checks that depend on the type's variants
  or layout report nothing: `match` coverage of such an enum, the types
  in its payload patterns, and `?` with such an operand or return type
  (ADR 0046). A type that is itself `{error}`, such as `[u8; N]`, stays
  silent everywhere, as any erroneous type does.
- `size_of`, `align_of` and `offset_of` of such a type have no value.
  What depends on them, a constant, another length, an `assert`, is not
  evaluated and reports nothing.
- A function, constant, static or `comptime` block is not built into
  MIR when its return type, one of its locals, the type of one of its
  expressions, or the operand of a `size_of`, `align_of` or `offset_of`
  in it reaches a failed value. It is therefore neither evaluated nor
  checked for the values known at compile time of ADR 0032.

## Consequences

- A program with a failed length or alignment shows one error at it,
  instead of a cascade computed from a wrong layout.
- Programs without errors are not affected: the rule applies only once
  a value has failed, which is always reported.
- Errors that only MIR building or evaluation find in code using such a
  type, such as `7 / 0` or an index out of range (ADR 0032), are
  reported only once the failed value is fixed. Type errors are reported
  at once.
- `size_of(*const Broken)` has no value although a pointer's size does
  not depend on its pointee. This keeps one rule for the whole type, as
  ADR 0034 does for `*const !`.
- Because whether code mentioning `*const S` is compiled depends on the
  lengths of `S`, a length of `S` computed by a function that only takes
  a `*const S` is a `ty::cycle`, although the layout of `S` does not
  depend on that function:

  ```
  const N: usize = f(0 as *const S);
  fn f(p: *const S) -> usize { 4 }
  struct S { x: [u8; N] }
  ```

  This predates this ADR: the check already looked through pointers for
  structs without a layout. It becomes more common in M6, where
  `comptime` functions over self-referential types are ordinary, and is
  revisited there (roadmap, pending decisions for M6).
- The compiler applies the rule to lengths since a47eb48. For a
  struct's `#[align(N)]` it must change: a struct whose alignment failed
  to evaluate or to type check, or is `ty::invalid-align`, becomes
  erroneous instead of being laid out without the attribute. Statics do
  not change: one whose alignment is in error already gets the natural
  alignment of its type.
- `docs/reference.md` gains the rule, next to what it says of payloads
  and type arguments in error.

## Alternatives considered

- **Lay the array out with size 0 and continue** (lengths before
  a47eb48, `#[align(N)]` today): reports errors that come from a wrong
  layout, and lets code reach stages that assume well-typed input.
- **Silence every diagnostic on such a type, mismatches included,** by
  making it `{error}` wherever it is written: `*const Broken` against
  `*const u8` is a real error, independent of the length, and would be
  lost along with the type's identity.
- **Count a pointer as erroneous only where its pointee is reached**
  (a dereference, a field, `size_of`): removes the false cycle above
  and reports more errors in code that only passes pointers, but gives
  two answers to one situation, since ADR 0034 makes `*const !`
  erroneous, and is a larger change. Revisited in M6.
- **Leave `#[align(N)]` out:** two rules for values of the same kind
  (ADR 0029), and a cascade kept for one of them.
- **Count only alignments that fail to evaluate or type check, not
  `ty::invalid-align`:** the struct would get a layout the program did
  not ask for, from which further errors could follow; "reported in
  error, whatever the error" is one rule, as for lengths.
- **Make a static whose alignment failed erroneous, with the code using
  it:** there is no cascade to prevent, since a static's alignment is
  part of no type, and real errors in that code would go unreported.

## Conformance tests

- **ui (MIR):** `mir_failed_length_inside.ers` (exists): functions,
  constants, statics and `comptime` code using such a type by value,
  behind pointers, arrays, slices, fields, payloads and `Option`, at
  compile time and at run time: one error, no panic. To add: the
  `ty::cycle` example above.
- **ui (typeck):** `typeck_failed_length_inside.ers` (exists): a
  non-exhaustive `match`, a wrongly typed payload pattern, and `?` with
  such an operand and with such a return type report nothing more. To
  add: `size_of(Broken)` in a constant used as a static's length and in
  an `assert`, `size_of(*const Broken)`, `align_of(Broken)` and
  `offset_of` of a struct with a `*const Broken` field, reporting only
  the length's error; a mismatch and an unknown field on `*const Broken`,
  still reported; `7 / 0` in a function taking `*const Broken`, not
  reported; a length that fails to type check (`[u8; true]`), silencing
  its uses the same way.
- **ui (typeck), `#[align(N)]`, to add:** `#[align(1 / 0)] struct T {
  x: u32 }`: only the division by zero; `size_of(T)` and `align_of(T)`
  without a value, an `assert` on them silent; a function taking
  `*const T` not built into MIR; no panic at any `--emit` level. The
  same for `#[align(true)]` and for `#[align(3)]` (only
  `ty::invalid-align`). For a static, `#[align(1 / 0)] static X: u32`:
  only the division by zero, and a function using `X` still built and
  checked (its `7 / 0` reported).
