# Roadmap

Each milestone runs **end to end** through every compiler layer (ADR 0003)
and ends with a program running on QEMU, verified by an automated test.

| Milestone | Delivers |
|-----------|----------|
| **M0** ✅ | `entry fn main() -> ! { loop {} }` compiles to an object file, links, and boots on QEMU. Every layer that has work to do exists, even if trivial (const evaluation waits for M2). |
| **M1** | Functions, integers, typed `let`, `if`/`while`, raw pointers. Prints "hello" over the QEMU UART via raw pointer writes. |
| **M2** | Structs, arrays, `const` and const evaluation, modules. |
| **M3** | `memory { ... }` blocks generating the linker script; `peripheral` blocks with native MMIO. The UART is declared in the language. |
| **M4** | Interrupt handlers, vector tables, inline `asm`. |
| **M5** | Moves/ownership, then references and borrow checking (ADR 0009). |
| **M6** | Generics via monomorphization. |

## Pending decisions

- How panics work (needed by M1 for division by zero, ADR 0007).

## Status

- Environment ready and verified (ADR 0011): an `inkwell` + LLVM 22 smoke test
  emitted a valid `riscv32` object file, and a hand-written assembly program
  linked with `ld.lld` booted on QEMU `virt`, printed over the UART at
  `0x1000_0000` and exited through the test finisher at `0x0010_0000`
  (ADR 0010).
- **M0 done.** `erebos build` runs lexer, parser, HIR lowering, type
  checking, MIR building, LLVM codegen and `ld.lld`; the result boots on QEMU
  through the provisional `_start` (ADR 0012) and is checked by `tests/e2e.rs`.
- **Next: M1.** It also lets the end-to-end tests check UART output and exit
  through the test finisher.
