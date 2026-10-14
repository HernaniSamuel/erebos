# 0029. Constants and comptime expressions

- Status: Accepted
- Date: 2026-10-14

## Context

ADR 0027 adopts compile-time execution and ADR 0028 the on-demand
pipeline that makes it possible. This ADR fixes what M2.3a gives programs:
the syntax and meaning of constants, of `comptime` expressions and blocks,
of `assert`, and of the places where a value computed at compile time is
now accepted instead of a literal. It also names the evaluator's errors.
Statics, byte strings and types as values come later (M2.3b, M6).

## Decision

### `const` items

```
const BAUD: u32 = 115_200;
const DIVISOR: u32 = clock_hz() / (16 * BAUD);
const UART: *mut volatile Uart = 0x1000_0000 as *mut volatile Uart;
const CTRL: *mut volatile u32 = &raw mut (*UART).ctrl;
const CRC_TABLE: [u32; 256] = crc_table(0xEDB8_8320);
```

- A `const` is an item, at the top level, with an explicit type
  (ADR 0008). Its initializer is any expression, run at compile time
  (ADR 0027); it may call functions and use other constants, declared
  before or after it.
- A constant is a **value, not a place**. It has no address, so
  `&raw const` of a constant is an error (`ty::not-a-place`), and it cannot
  be assigned (`ty::not-assignable`). Data that needs an address is a
  `static` (M2.3b).
- Using a constant whole gives a copy of its value. Indexing it or reading
  a field (`CRC_TABLE[i]`, `LIMITS.max`) reads only that part: an
  aggregate constant is stored once, in read-only data, and those reads go
  straight to it. Since a constant has no address and never changes, the
  program cannot tell the difference from a copy.
- Its type is any type with values: integers, `bool`, pointers, arrays and
  structs.
- Every constant is evaluated, used or not (ADR 0028; Zig evaluates only
  what is used). A constant that fails to evaluate is reported once; its
  uses are not reported again.
- A `const` inside a function body is an error (`parser::local-const`)
  suggesting `let name: T = comptime ...;`.

### One namespace for items

Functions, structs and constants share one namespace, so `struct Foo` and
`const Foo: u32` cannot coexist (`resolve::duplicate-item`, which replaces
`resolve::duplicate-function` and the struct case of
`resolve::duplicate-type`; `resolve::duplicate-type` remains for items
named like built-in types). In M6 types are values (`const Word: type =
u32;`), and a single namespace keeps today's programs valid then.

A local variable shadows an item of the same name, as it already shadows
functions. When constants become patterns (M2.4), this is the classic
ambiguity of `match x { MAX => ... }`: binding a new variable or comparing
with the constant. M2.4 must decide it; this ADR only flags it.

### Pointers at compile time

ADR 0027 forbids **reading and writing** through a pointer made from an
integer, nothing else. Computing a place from one is allowed: field
projections, indexing and `&raw` compute an address, with arithmetic that
wraps as at run time (ADR 0021), and access no memory. So
`&raw mut (*UART).ctrl` and `&raw mut BASE[1]` are valid at compile time,
give pointers made from integers, and are not volatile accesses. The
result can be converted to an integer, since its address is known.

A pointer into memory the compiler holds cannot be the result of any
evaluation (a constant, a `comptime` expression, an array length) until
statics exist to give that memory an address (`eval::pointer-escapes`).

### `comptime` expressions and blocks

```
let divisor: u32 = comptime baud_divisor(48_000_000, 115_200);

comptime {
    assert(size_of(Header) == 7);
    assert(offset_of(Header, len) == 1);
}
```

- `comptime expr` evaluates `expr` at compile time, and is that value.
  Like `return`, it applies to **the whole expression to its right**:
  `comptime f(1) + y` is `comptime (f(1) + y)`. Parentheses limit it:
  `(comptime f(1)) + y`. It has the lowest precedence, with `return` and
  `break` in the table of the reference.
- It passes the expected type through, so `let x: u8 = comptime 300;` is
  `ty::literal-out-of-range`.
- A `comptime { ... }` item runs its block at compile time, once, when the
  program is compiled. The block has type `()`.

### Where compile-time values are required

The length of an array type, the count of `[v; N]` and the argument of
`#[align(N)]` become expressions of type `usize`, evaluated at compile
time: `[u8; 2 * SECTOR]`, `[0; LEN]`, `#[align(LINE)]`. With no implicit
conversions, `const SECTOR: u32 = 512;` needs `[u8; SECTOR as usize]`.
Two array types are the same when their element types and the **values**
of their lengths are: `[u8; 4]` and `[u8; 2 + 2]` are one type, and
diagnostics show the value (`[u8; 4]`).

These rules apply to **every** evaluation, wherever it is asked for:

- It cannot use the enclosing function's variables or parameters, which
  exist only at run time (`ty::not-comptime`): not in `comptime expr`, nor
  in `let b: [u8; n]` with a local `n`, nor in `#[align(n)]`.
- Its result cannot hold a pointer into compiler memory
  (`eval::pointer-escapes`).

A layout that depends on itself, such as `struct S { a: [u8; size_of(S)] }`
or `#[align(align_of(S))] struct S`, is a `ty::cycle`.

### `assert`

`assert(cond)` is a built-in function taking a `bool`. At run time, a false
condition calls the panic function (ADR 0014). At compile time, it fails
the evaluation (`eval::assertion-failed`), pointing at the assertion. It
works in any module (M2.6), unlike a call to the program's own panic
function, whose name it cannot know. A message comes with byte strings
(M2.3b). `assert` becomes a reserved name.

At compile time, the panic function's **body never runs**: reaching any
call to it, explicit or from a failed check, is itself the failure.

### Evaluation

- Evaluation runs MIR in an interpreter that models the target
  (ADR 0027): memory is bytes with the layouts of ADR 0022; a pointer is
  an object and an offset, never a host address.
- **Each evaluation is independent.** Every constant, array length,
  `comptime` expression and block is evaluated with its own budget and
  call depth, starting from zero. When one needs a constant not yet
  computed, that constant is evaluated separately, and its cost is not
  charged to the evaluation that asked for it. Results never depend on
  which was asked for first (ADR 0028).
- Limits: **10,000,000 steps** per evaluation, where every executed MIR
  statement and every executed terminator is a step, so `loop {}` spends
  one step per iteration; and **256 nested calls**.
- An error names what was being evaluated (a constant, an array length, a
  `comptime` block) and points at every call on the way to the failure.
- In the program, a constant of scalar type becomes an immediate value;
  one of aggregate type becomes read-only data whose padding is zero
  (ADR 0027).

### Diagnostic codes

Errors found by running code use the prefix `eval::`, for the "const
evaluation" layer of ADR 0003; errors found by checking code keep `ty::`.

| Code | Meaning |
|------|---------|
| `eval::panic` | the evaluation reached the panic function: a zero divisor, an index out of bounds, an explicit call |
| `eval::assertion-failed` | `assert` with a false condition |
| `eval::step-limit` | more than 10,000,000 steps |
| `eval::call-depth` | more than 256 nested calls |
| `eval::volatile-access` | a volatile read or write |
| `eval::extern-call` | a call to an `extern fn` |
| `eval::invalid-access` | reading or writing through a pointer made from an integer |
| `eval::pointer-to-int` | converting a pointer into compiler memory to an integer |
| `eval::pointer-compare` | ordering pointers into different objects, or comparing one with a pointer made from an integer |
| `eval::uninit-read` | reading bytes never written, such as padding, as a value |
| `eval::pointer-escapes` | a result holding a pointer into compiler memory |
| `eval::static-mut` | reading or writing a `static mut` (M2.3b) |
| `ty::not-comptime` | a run-time variable where a compile-time value is required |
| `ty::cycle` | an item whose value needs itself (ADR 0028) |
| `parser::local-const` | `const` inside a function body |
| `resolve::duplicate-item` | two items with one name |

### Conformance tests

- **Differential tests** (ADR 0027): an end-to-end program compares
  `comptime f(x)` with `f(x)` on QEMU, at both optimization levels. The
  run-time inputs come from an `extern fn` identity function written in C,
  so LLVM cannot fold `f(x)` itself: otherwise, at `-O`, the "run-time"
  side would be LLVM's constant folder rather than code on the target.
- **ui snapshots** for every code above except `eval::static-mut` (M2.3b),
  including `comptime { loop {} }` for the step limit; `&raw const` of a
  constant; assignment to a constant; `ty::cycle` between constants,
  through an array length and through a layout; a failing `comptime`
  block; `[u8; SECTOR]` with a `u32` constant; and an evaluation whose
  dependencies are processed in a different order, showing the same
  output.
- An **LLVM IR snapshot** showing a scalar constant as an immediate, an
  aggregate constant as read-only data with zero padding, and
  `CRC_TABLE[i]` reading one element from it.
- **The milestone program:** a CRC-32 table computed by the compiler,
  used at run time to check a message against its known CRC, with
  `comptime` assertions on struct layouts.

## Consequences

- Tables, divisors and sizes are computed by the compiler from ordinary
  Erebos code, and checked by it with `assert`.
- Literal-only positions now take expressions, which the parser accepts in
  types and attributes.
- Until statics exist (M2.3b), a constant table can be indexed or copied,
  but not passed by pointer, since it has no address.
- `resolve::duplicate-function` becomes `resolve::duplicate-item`, and the
  existing snapshots that show it change.
- Unused constants cost compile time and report their errors.

## Alternatives considered

- **Constants with an address (C's `const` globals)** — that is a `static`;
  constants as values can be used like literals.
- **`const` items inside functions** — `let ... = comptime ...` covers them
  with one construct.
- **Lazy evaluation of constants, as in Zig** — unused constants would
  cost nothing, but their errors would go unreported, as with uninstantiated
  generics; evaluating all of them matches how the rest of the program is
  checked (ADR 0028).
- **`comptime` binding tightly, like a unary operator** — `comptime f(1) + y`
  would mix compile-time and run-time evaluation in one expression without
  parentheses showing where the line is.
- **Separate namespaces for types and values** — breaks once types are
  values (M6).
