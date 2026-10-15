# 0027. Compile-time execution (comptime)

- Status: Accepted; amended by 0030 (comparing pointers at compile time)
- Date: 2026-10-14

## Context

Bare-metal programs need values computed before they run: buffer sizes,
lookup tables (CRC, sine, baud-rate divisors), register layouts, checks
that a struct matches a datasheet. M2.3 was to add `const` with a small
evaluator of constant expressions, and M6 a separate generics system.

Zig shows a single mechanism covering both: ordinary code executed by the
compiler, where types are values. Generics are functions that take types
and return types, written in the same language as everything else, with
type arguments always written out. That matches Erebos: explicit types,
no inference (ADR 0008), and hardware concepts expressed in the language
rather than in a separate macro or template language.

## Decision

Erebos adopts Zig's model of compile-time execution, **comptime**.

### Any function can run at compile time

The compiler executes ordinary Erebos code wherever a value must be known
before the program runs:

- the initializer of a `const` item, and of a `static`;
- array lengths (`[u8; BUFFER_SIZE]`) and attribute arguments
  (`#[align(LINE)]`);
- `comptime expr`, which forces an expression to be evaluated by the
  compiler;
- `comptime { ... }` blocks, at item level, for checks such as
  `comptime { assert(size_of(Header) == 7); }`;
- (M6) arguments of `comptime` parameters.

There is no `const fn` marker: any function may be called at compile
time. Whether it can actually run there is found out by running it.

### What compile-time code can and cannot do

Compile-time execution follows the **target's** semantics exactly, not
the host's: `usize` is 32 bits, arithmetic wraps (ADR 0007), layouts are
those of ADR 0022. A program computes the same value at compile time as
it would at run time on the target.

There is no hardware at compile time, so these are compile-time errors,
reported where the evaluation was asked for, with the chain of calls that
led to them:

- volatile accesses (ADR 0013, 0023);
- calls to `extern fn` (ADR 0026), and later inline assembly;
- dereferencing a pointer that does not point into memory the compiler
  itself holds, such as an address made from an integer
  (`0x1000_0000 as *mut u8` is a valid value, but cannot be read through);
- converting a pointer into memory the compiler holds to an integer
  (`&raw const TABLE as usize`): that address exists only after linking,
  as a relocation. For the same reason, ordering comparisons (`<`) between
  pointers into different objects, and comparisons between such a pointer
  and one made from an integer, are errors; pointers into one object
  compare by offset;
- reading or writing a `static mut`, whose value is only known at run time;
- reading bytes never written, such as padding (ADR 0022), as a value;
  copying them along with an aggregate is fine;
- anything that would call the panic function at run time: a zero
  divisor, an index out of bounds, an explicit call.

Execution is deterministic and sees nothing of the host:

- A budget of execution steps stops an evaluation that does not end, and
  a limit on the depth of nested calls stops runaway recursion before it
  exhausts the compiler's own stack. A way to raise them comes when a
  program needs it.
- Memory produced at compile time that ends up in the program (a
  `static`'s initial value, data a constant points to) has its unwritten
  bytes, padding included, **set to zero**. Padding is unspecified at run
  time (ADR 0022); in data the compiler emits it is zero, so builds are
  reproducible byte for byte.

### Types as values (M6)

`type` is a type whose values are types, and which exists only at compile
time. Functions may take `comptime` parameters and return types:

```
fn Ring(comptime T: type, comptime N: usize) type {
    return struct { slots: [T; N], head: usize, tail: usize };
}

fn max(comptime T: type, a: T, b: T) T {
    if a > b { a } else { b }
}

let uart_rx: Ring(u8, 64) = ...;
let m: u32 = max(u32, x, y);
```

- **Generics are this mechanism.** There is no separate generics system;
  each distinct set of comptime arguments gives one instance, compiled
  like ordinary code (monomorphization).
- The same arguments give the same type: `Ring(u8, 64)` is one type
  wherever it is written.
- Comptime arguments are always written out; they are never inferred
  from the other arguments (ADR 0008).
- What a function requires of a type argument is checked when it is
  instantiated, and errors show the chain of instantiations. A function
  that is never instantiated is not type checked at all, so its errors go
  unreported (ADR 0028). A way to state requirements up front may come
  later.
- Instantiations nest only to a fixed depth (ADR 0028).
- `Option(T)` and `Result(T, E)` become ordinary comptime functions in
  `core`; until then they are built in (M2.5), with the same syntax.

### Delivery

- **M2.3a:** compile-time execution of values: the MIR interpreter, `const`
  items, `comptime` expressions and blocks, constant array lengths and
  attribute arguments.
- **M2.3b:** statics whose initializers run at compile time.
- **M6:** `type`, `comptime` parameters and types as values.

### Conformance tests

The promise is "the same value at compile time and at run time", and the
only way to check it is to compare the two:

- **Differential tests.** A program on QEMU calls the same functions twice,
  once inside `comptime` and once at run time, and compares the results:
  wrapping `+ - *` at every width, signed and unsigned `/` and `%`
  (including `MIN / -1`), masked shifts, every `as` conversion, `bool` in
  memory, struct and array copies, field and element access, and
  `size_of`/`align_of`/`offset_of`. It runs at both optimization levels.
  Mismatches print what differs.
- **Reproducibility.** Compiling the same program twice gives identical
  object files; a `static` with padding has zeros there.
- **ui snapshots** for each error above: volatile access, `extern fn`
  call, dereferencing an integer address, pointer to integer, ordering
  across objects, `static mut`, reading padding, each panic cause, the
  step budget and the call depth limit, each showing the chain of calls.

## Consequences

- One mechanism for constants, checks, tables and generics; the language
  to learn is Erebos itself.
- The compiler needs an interpreter for MIR that models the target
  precisely, including pointers into memory it holds, which become
  relocations when such memory ends up in the program. This interpreter is
  the "const evaluation" layer of ADR 0003.
- Type checking and evaluation depend on each other, so the pipeline runs
  on demand rather than strictly layer after layer (ADR 0028).
- Errors in generic code appear at instantiation, as in Zig and C++
  templates, rather than at the definition, as with Rust's traits; generic
  code that is never instantiated is never checked.
- Borrow checking (M5) runs on instantiated code.
- Compile times grow with the work done at compile time; the step budget
  keeps them bounded.

## Alternatives considered

- **Constant expressions only (C, early Rust `const`)** — simple, but every
  table or derived value must be written by hand or generated outside the
  language.
- **`const fn` plus Rust-style generics with traits** — checks generic code
  at its definition, but two mechanisms, a trait system, and a marker on
  every function usable at compile time.
- **Macros or a template language** — a second language inside the first.
