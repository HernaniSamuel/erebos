# Roadmap

Each milestone runs **end to end** through every compiler layer (ADR 0003)
and ends with a program running on QEMU, verified by an automated test.

| Milestone | Delivers |
|-----------|----------|
| **M0** ✅ | `entry fn main() -> ! { loop {} }` compiles to an object file, links, and boots on QEMU. Every layer that has work to do exists, even if trivial (const evaluation waits for M2). |
| **M1** ✅ | Functions, integers, `bool`, typed `let`, `if`/`while`/`loop`, raw and volatile pointers (ADR 0013), byte literals (ADR 0016), checked division (ADR 0007), the panic function (ADR 0014). Prints "hello" over the QEMU UART via volatile pointer writes. |
| **M2** | Compound data, enums and modules, in the steps below. |
| **M3** | `memory { ... }` blocks generating the linker script; `peripheral` blocks with native MMIO. The UART is declared in the language. |
| **M4** | Interrupt handlers, vector tables, inline `asm`, compiler and hardware fences (ADR 0023). |
| **M5** | Moves/ownership, then references and borrow checking (ADR 0009); methods (`impl`, `self`, `&self`, `&mut self`, `x.f()`). |
| **M6** | Types as values and `comptime` parameters: generics in the Zig model, monomorphized (ADR 0027). |

## M2 steps

M2 is split into steps that each end like a milestone: ADRs, the reference
updated, and a program on QEMU.

| Step | Delivers | Program on QEMU |
|------|----------|-----------------|
| **M2.1** ✅ Structs and arrays | `struct`, `[T; N]`, fields, checked indexing, `&raw const/mut` (ADR 0021); C layout, `size_of`/`align_of`/`offset_of`, `#[packed]`, `#[align(N)]`, `bool` loads (ADR 0022); unchecked pointer indexing (ADR 0021); volatile access widths (ADR 0023); `memcpy`/`memset` in `rt/` | a ring buffer on the stack |
| **M2.2** ✅ Aggregates across calls | passing and returning structs and arrays by value with the riscv32 C ABI (small ones in registers, large ones through a hidden pointer) | calls a C function built with clang, to prove the ABI is the real one |
| **M2.3a** ✅ Compile-time execution | the MIR interpreter and on-demand checking (ADR 0027, 0028); `const` items; `comptime` expressions and blocks; array lengths and attribute arguments computed at compile time | a lookup table computed by the compiler, and `comptime` checks of struct layouts |
| **M2.3b** ✅ Statics and byte strings | `static`/`static mut` with compile-time initializers, `#[align(N)]` on statics; `_start` initializes `.data` and `.bss`; `b"..."`; slices `*const [T]` | prints with a real `print` function |
| **M2.4a** ✅ Enums and `match` | enums with explicit discriminants (`enum Mode: u8 { Off = 0, Run = 3 }`), `match` on integers, `bool` and enums, with exhaustiveness and dead arms checked | a state machine |
| **M2.4b** ✅ Enum payloads | variants carrying data, their layout and ABI, patterns nested in payloads | a command decoder: messages with payloads, decoded from bytes and dispatched with `match` |
| **M2.4c** Cost of `match` | code of a `match` on an enum with payloads tested arm by arm, linear in its patterns (ADR 0042); checks of exhaustiveness and dead arms that examine once the values the arms do not tell apart; at most four witnesses searched (ADR 0043) | matches whose code and checks grew exponentially, built and run |
| **M2.5** `Option` and `Result` | built into the compiler until generics exist; the `?` operator | a number parser returning `Result` |
| **M2.6** Modules and panic information | `mod`, `use`, `pub`, `core`; `panic fn on_panic(info: *const PanicInfo) -> !` with file, line and a `PanicReason` enum (ADR 0014) | a panic prints `division by zero at main.ers:12` |

Decisions already agreed for the later steps (their ADRs come with the step):

- **M2.2:** the earlier idea of never passing aggregates by value is
  dropped; `Result` is only useful if functions can return it.
- **M2.3b:** `b"abc"` has type `[u8; 3]`, its length counted by the compiler.
  It is a place in read-only memory, so `&raw const b"abc"` points at it
  without a copy, while `let s: [u8; 3] = b"abc";` copies it. This revisits
  ADR 0016 (no strings).
- **M2.4:** every `match` is exhaustive; on integers that means a
  catch-all arm (`_` or a `let` binding, ADR 0037).
- **M2.5:** `Option(T)` and `Result(T, E)` are compiler-known types, as
  `[T; N]` is; in M6 they become ordinary comptime functions in `core`
  returning enums (ADR 0027), with the same syntax, so programs do not
  change. They are written with parentheses, like any comptime call. `?` requires the same error
  type, with no conversion. `Option(*mut T)` takes 8 bytes: address 0 is
  valid (ADR 0019), so there is no null niche for `None`.

Until M5, there are no methods: functions are free and take their receiver
explicitly (`fn rect_area(r: *const Rect) -> u32`), and modules (M2.6)
provide namespacing. Method receivers are references (`&self`,
`&mut self`), and taking an address implicitly at `x.f()` would be an
implicit conversion (ADR 0008), so methods wait for references instead of
being designed twice.

## Deferred ideas

Agreed as useful, not yet scheduled:

- `for i in 0..n` over integer ranges, without general iterators.
- Loop labels (`'outer: loop`) to break out of nested loops.
- Warnings (unreachable code, unused variables); today there are only errors.

## Notes for M2

- Struct and array literals are built in a temporary and then copied into
  their variable. Building them in place would save a copy at `-O0`; `-O`
  already removes it.
- A large argument is copied by the caller and, if it is a variable,
  that copy is all there is; a temporary argument could be passed without
  the copy. `-O` removes most of it already.

## Pending decisions

Questions accepted ADRs leave to a later step, to settle in that step's
ADRs:

- **M2.5:**
  - `!` as a type argument of `Option(T)` and `Result(T, E)`, such as
    the infallible `Result(u32, !)` (ADR 0034).
- **M2.6:**
  - Symbol names with modules: functions and statics are emitted under
    their own names (ADR 0006, 0026, 0030). With modules, names must be
    unique across the program, or be mangled with an attribute to export
    them.
  - The panic handler's signature: location, reason and message
    (ADR 0014, 0031).
- **M4:**
  - Sharing a `static mut` with interrupt handlers (ADR 0030).
  - Compiler and hardware fences (ADR 0023).
  - Moving `_start` into the language (ADR 0012, 0030).
- **M5:**
  - Constants and statics of types that must not be copied: duplicating a
    peripheral handle is a bug ownership should catch (ADR 0021), but a
    constant is copied at each use and reading a static copies it.
  - Copy versus move (ADR 0021), and borrow checking (ADR 0009).
  - The `(*s)[i]` syntax for slice elements (ADR 0031).
  - Whether the MIR analyses run before compile-time evaluation, which
    today runs MIR they have not checked (ADR 0033).
- **M6:**
  - `compile_error` (ADR 0031).
  - Requirements on type arguments (ADR 0027).
  - ADR 0027 writes return types Zig-style (`fn max(comptime T: type,
    a: T, b: T) T`); by ADR 0015 a function without `->` returns `()`, so
    the M6 ADR writes `-> T`.

Documentation owed:

- The informal grammar reference ADR 0004 asks for in `docs/`; with
  `comptime` precedence and expressions inside types, it is overdue.

## Status

- Environment ready and verified (ADR 0011): an `inkwell` + LLVM 22 smoke test
  emitted a valid `riscv32` object file, and a hand-written assembly program
  linked with `ld.lld` booted on QEMU `virt`, printed over the UART at
  `0x1000_0000` and exited through the test finisher at `0x0010_0000`
  (ADR 0010).
- **M0 done.** `erebos build` runs lexer, parser, HIR lowering, type
  checking, MIR building, LLVM codegen and `ld.lld`; the result boots on QEMU
  through the provisional `_start` (ADR 0012) and is checked by `tests/e2e.rs`.
- **M1 done.** Programs with functions, recursion, loops, branches, integer
  arithmetic and volatile pointers print over the UART and exit QEMU through
  the test finisher; end-to-end tests compare their output and exit code.
  The language is Turing-complete.
- **M2.1 done.** Structs, arrays, checked and unchecked indexing, `&raw`,
  C layout with `#[packed]` and `#[align(N)]`, layout queries, and the
  volatile guarantees of ADR 0023. The milestone program, a ring buffer on
  the stack, runs on QEMU; layouts are checked against clang, and volatile
  accesses in LLVM IR with and without `-O`. `rt/` provides `memcpy`,
  `memmove` and `memset`.
- **M2.2 done.** Structs and arrays cross calls by value with the riscv32 C
  ABI (ADR 0025), and `extern fn` declares functions defined elsewhere
  (ADR 0026). The milestone program exchanges aggregates of every size
  class with C code compiled by clang, in both directions.
- **M2.3a done.** Erebos runs code at compile time in the Zig model
  (ADR 0027): a MIR interpreter with the target's semantics computes
  constants, `comptime` expressions and checks, array lengths and
  `#[align]` arguments, on demand and with cycles reported (ADR 0028,
  0029). The milestone program computes a CRC-32 table at compile time and
  checks a message on QEMU; a differential test compares 21 functions run
  by the compiler and on the target, and builds are reproducible.
- **M2.3b done.** Statics (`static`, `static mut`, `extern static`) get
  their initial values at compile time and may point at each other through
  relocations; `_start` zeroes `.bss` and copies `.data` from an image laid
  out as on a board with flash (ADR 0030). Byte strings and slices, with
  checked element access, `len`, `slice` and messages in `assert`
  (ADR 0031). Shift amounts, divisors and indices known at compile time are
  checked, not only literals (ADR 0032); `!` is no longer a type of memory
  (ADR 0034); pointer comparisons at compile time give the run-time answer
  or an error. The milestone program prints byte strings and numbers on
  QEMU through a `print` function taking a slice, with its messages in a
  static table; an ELF test checks where statics go and the symbols
  `_start` relies on.
- **M2.4a done.** Enums with an explicit integer type and explicit values
  (ADR 0035), stored, passed and accessed through volatile pointers as
  that integer, checked against C23 enums in layout and across calls.
  `match` on integers, `bool` and enums (ADR 0037), with literal, constant,
  variant, `let` and or-patterns, compiled to a switch; a differential
  test checks that it runs alike at compile time and on the target. Every
  `match` is exhaustive, and dead arms are errors. An enum value that is
  no variant is defined, and a `match` without a catch-all arm calls the
  panic function on one (ADR 0036). The milestone program runs a state
  machine over a byte string of events on QEMU, printing each transition,
  and checks the same machine in a `comptime` block.
- **M2.4b done.** Enum variants carry payloads of one or more positional
  values (ADR 0039), laid out as a C union of structs that each start with
  the tag, checked against clang in layout and across calls, where they
  pass as aggregates; `!` is no payload type. `match` reaches payloads
  through nested variant patterns (ADR 0040), with exhaustiveness and dead
  arms checked over every position by the usefulness of patterns, at most
  three missing values listed, and compiles to a decision tree of
  switches; or-patterns may bind variables when every alternative binds
  the same ones (ADR 0041). A differential test runs nested matches alike
  at compile time and on the target. The milestone program decodes a
  stream of command bytes into messages with payloads on QEMU, dispatches
  them with nested and or-patterns, printing each action, and checks the
  decoder in a `comptime` block.
- **Next: M2.4c** (cost of `match`), then M2.5 (`Option` and `Result`).
