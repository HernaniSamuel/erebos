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
| **M5** | Moves/ownership, then references and borrow checking (ADR 0009). |
| **M6** | Generics via monomorphization. |

## M2 steps

M2 is split into steps that each end like a milestone: ADRs, the reference
updated, and a program on QEMU.

| Step | Delivers | Program on QEMU |
|------|----------|-----------------|
| **M2.1** Structs and arrays | `struct`, `[T; N]`, fields, checked indexing, `&raw const/mut` (ADR 0021); C layout, `size_of`/`align_of`/`offset_of`, `#[packed]`, `#[align(N)]`, `bool` loads (ADR 0022); unchecked pointer indexing (ADR 0021); volatile access widths (ADR 0023); `memcpy`/`memset` in `rt/` | a ring buffer on the stack |
| **M2.2** Aggregates across calls | passing and returning structs and arrays by value with the riscv32 C ABI (small ones in registers, large ones through a hidden pointer) | calls a C function built with clang, to prove the ABI is the real one |
| **M2.3** Constants, statics and byte strings | `const` with compile-time evaluation; `static`/`static mut`, `#[align(N)]` on statics; `_start` initializes `.data` and `.bss`; `b"..."`; slices `*const [T]` | prints with a real `print` function |
| **M2.4** Enums and `match` | enums with explicit discriminants (`enum Mode: u8 { Off = 0, Run = 3 }`), `match` on integers, `bool` and enums; then payloads, nested patterns and exhaustiveness | a state machine |
| **M2.5** `Option` and `Result` | built into the compiler until generics exist; the `?` operator | a number parser returning `Result` |
| **M2.6** Modules and panic information | `mod`, `use`, `pub`, `core`; `panic fn on_panic(info: *const PanicInfo) -> !` with file, line and a `PanicReason` enum (ADR 0014) | a panic prints `division by zero at main.ers:12` |

Decisions already agreed for the later steps (their ADRs come with the step):

- **M2.2:** the earlier idea of never passing aggregates by value is
  dropped; `Result` is only useful if functions can return it.
- **M2.3:** `b"abc"` has type `[u8; 3]`, its length counted by the compiler.
  It is a place in read-only memory, so `&raw const b"abc"` points at it
  without a copy, while `let s: [u8; 3] = b"abc";` copies it. This revisits
  ADR 0016 (no strings).
- **M2.4:** every `match` is exhaustive; on integers that means an explicit
  `_` arm.
- **M2.5:** `Option<T>` and `Result<T, E>` are compiler-known types, as
  `[T; N]` is; with generics (M6) they move to `core` as ordinary enums with
  the same syntax, so programs do not change. `?` requires the same error
  type, with no conversion. `Option<*mut T>` takes 8 bytes: address 0 is
  valid (ADR 0019), so there is no null niche for `None`.

## Deferred ideas

Agreed as useful, not yet scheduled:

- `for i in 0..n` over integer ranges, without general iterators.
- Loop labels (`'outer: loop`) to break out of nested loops.
- Warnings (unreachable code, unused variables); today there are only errors.

## Notes for M2

- With arrays, LLVM's `-O` pipeline turns copy and fill loops into calls to
  `memcpy`/`memset`. `rt/` must provide them (ADR 0020), marked so LLVM does
  not turn their own loops back into calls to themselves.

## Pending decisions

- None.

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
- **Next: M2.1** (structs and arrays). ADRs 0021, 0022 and 0023 are accepted.
