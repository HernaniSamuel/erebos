# Roadmap

Each milestone runs **end to end** through every compiler layer (ADR 0003)
and ends with a program running on QEMU, verified by an automated test.

| Milestone | Delivers |
|-----------|----------|
| **M0** | `entry fn main() -> ! { loop {} }` compiles to an object file, links, and boots on QEMU. Every layer exists, even if trivial. |
| **M1** | Functions, integers, typed `let`, `if`/`while`, raw pointers. Prints "hello" over the QEMU UART via raw pointer writes. |
| **M2** | Structs, arrays, `const` and const evaluation, modules. |
| **M3** | `memory { ... }` blocks generating the linker script; `peripheral` blocks with native MMIO. The UART is declared in the language. |
| **M4** | Interrupt handlers, vector tables, inline `asm`. |
| **M5** | Moves/ownership, then references and borrow checking (ADR 0009). |
| **M6** | Generics via monomorphization. |

## Pending decisions

- First target: ARM Cortex-M (`lm3s6965evb` on QEMU) or RISC-V (`riscv32 virt`).
- ADR 0007 open questions (shifts, division by zero).
- LLVM version pinned for development (ADR 0005).
