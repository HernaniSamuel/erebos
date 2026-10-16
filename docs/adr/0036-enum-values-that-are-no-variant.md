# 0036. Enum values that are no variant

- Status: Accepted
- Date: 2026-10-16

## Context

An enum is stored as its integer type (ADR 0035), so memory can hold a
value of the enum's type that is none of its variants: a device register
reporting a mode the datasheet calls reserved, a C function returning a
value the Erebos declaration does not list, a byte buffer read through a
`*const Mode`, a `static mut` written by C code.

Rust makes such a value undefined behaviour the moment it is produced,
which lets the compiler assume every enum holds a variant. ADR 0022 faced
the same question for `bool` and chose the other way: every byte is a
valid `bool` in memory, at the cost of a compare per load. Nothing outside
the program forces enums to be undefined either.

## Decision

### Every value of the integer type is a value of the enum

A value of an enum type is any value of its integer type. The variants
name some of them; the others are **values that are no variant**. Reading,
writing and copying such a value, comparing it with `==` and `!=`, and
converting it with `as` are defined and behave as for its integer, at run
time and at compile time. Nothing is checked at those points.

Since nothing converts to an enum (ADR 0035), these values only come from
memory written by other means: a pointer cast, a volatile read, an
`extern static`, the result of an `extern fn`, code outside the program.
A program that only writes variants never holds one.

### `match` checks

Only `match` (ADR 0037) tells the values apart:

- A variant pattern never matches a value that is no variant. A
  catch-all arm (`_`, `let x: Mode`) does, and is how a program handles
  one: a `_` after every variant is not a dead arm while values that are
  no variant remain (an enum on `u8` with 256 variants has none). A
  constant pattern matches its constant's value, whatever it is: a
  constant of type `Mode` may hold a value that is no variant, and then
  matches exactly that value (ADR 0037).
- A `match` on an enum **without a catch-all arm calls the panic function**
  (ADR 0014) when the value is no variant, as a failed bounds check does.
  The cause is "enum value is no variant"; M2.6 gives it a `PanicReason`.
- At compile time the same `match` fails the evaluation with
  `eval::panic` and that cause (ADR 0029): the compiler and the target
  agree.

This check is the only cost: the default case of the `switch` a `match`
compiles to. With `-O`, LLVM removes it where it can see the value is a
variant, for instance when it was just written.

### What the compiler never assumes

Code generation never tells LLVM that an enum holds a variant: no range
metadata on loads, no `assume`, no `unreachable` default in a `switch` on
an enum. The value range of an enum, as LLVM sees it, is that of its
integer type.

### Conformance tests

- **e2e:** a program reads the bytes 0, 1, 3 (variants) and 2, 7 (no
  variant) through a `*const Mode` and through a `*const volatile Mode`;
  a `match` with a `_` arm sends 2 and 7 there, `==` and `as` give their
  integers, and a `match` listing every variant panics on 7, observed
  through the exit code, at both optimization levels.
- **ui (eval):** a compile-time `match` without a catch-all on a value
  that is no variant, read from a `static` of bytes through a cast
  pointer, is `eval::panic` with the cause; with a `_` arm it evaluates.
- **LLVM IR snapshot:** a load of an enum without range metadata, and the
  `switch` of a `match` without a catch-all whose default calls the panic
  function.

## Consequences

- Reading an enum from a register or from C is never undefined, whatever
  the hardware or the other code put there, as with `bool`.
- Exhaustive matches stay short: listing every variant is enough, and a
  value that is no variant stops the program at the `match` instead of
  running an arbitrary arm.
- A program that wants to handle such values writes `_`, which documents
  that it expects them.
- Each `match` without a catch-all pays a compare and a branch that Rust
  would not; a `switch` has a default case anyway, so on riscv32 this is
  usually one more branch target.
- LLVM cannot use "an enum is always a variant" for optimization.

## Alternatives considered

- **Undefined behaviour (Rust)** — no default case, but a reserved value
  from a register makes the program undefined, which the language cannot
  prevent; ADR 0022 refused this for `bool`.
- **A check at every load from memory** — catches the value where it
  enters the program, but puts a check on every read, makes reads panic,
  and gains nothing for code that only copies or compares the value.
- **Normalizing at load, as for `bool`** — `bool` has an obvious mapping
  (nonzero is `true`); an enum has none, and any choice would hide the
  value that was really there.
