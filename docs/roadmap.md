# Roadmap

Each milestone runs **end to end** through every compiler layer (ADR 0003)
and ends with a program running on QEMU, verified by an automated test.

| Milestone | Delivers |
|-----------|----------|
| **M0** ✅ | `entry fn main() -> ! { loop {} }` compiles to an object file, links, and boots on QEMU. Every layer that has work to do exists, even if trivial (const evaluation waits for M2). |
| **M1** ✅ | Functions, integers, `bool`, typed `let`, `if`/`while`/`loop`, raw and volatile pointers (ADR 0013), byte literals (ADR 0016), checked division (ADR 0007), the panic function (ADR 0014). Prints "hello" over the QEMU UART via raw pointer writes. |
| **M2** | Structs, arrays, byte strings, `const` and const evaluation, modules; panic location and reason (ADR 0014). |
| **M3** | `memory { ... }` blocks generating the linker script; `peripheral` blocks with native MMIO. The UART is declared in the language. |
| **M4** | Interrupt handlers, vector tables, inline `asm`. |
| **M5** | Moves/ownership, then references and borrow checking (ADR 0009). |
| **M6** | Generics via monomorphization. |

## Deferred ideas

Agreed as useful, not yet scheduled:

- `match` on integers (states, commands, register codes) — likely M2.
- `for i in 0..n` over integer ranges, without general iterators.
- Loop labels (`'outer: loop`) to break out of nested loops.
- Warnings (unreachable code, unused variables); today there are only errors.

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
- **Next: M2.** Before starting, revisit the panic function's signature
  (location and reason, ADR 0014) and decide whether `match` joins M2.
