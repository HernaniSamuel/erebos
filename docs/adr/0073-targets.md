# 0073. Targets

- Status: Accepted; corrected on 2026-11-03 (the example origin of the third consequence)
- Date: 2026-11-03
- Amends: 0010 (riscv32 is the first target, no longer the only one; the
  target is chosen per build)

## Context

ADR 0010 made riscv32 on QEMU `virt` the only target and allowed its
values to live in one `target` module until a second target arrived.
The roadmap gives that second target to M3.5, before interrupts and
vector tables (M4), the most target-specific part of the language, are
designed. M3.1 already took the board out of the target: where memory
is belongs to the program's `memory` block (ADR 0057).

What still assumes riscv32 is spread over the compiler: `POINTER_BITS`,
`WIDEST_ACCESS_BITS`, `STACK_ALIGN` and `CODE_ALIGN` are constants of
`src/target.rs`; the calling convention of `src/codegen/abi.rs` is
riscv32's alone (ADR 0025); `_start` is RISC-V assembly (ADR 0012,
0030); the linker script names `OUTPUT_ARCH(riscv)` and
`.riscv.attributes`; LLVM is initialized for RISC-V only; and `core`'s
64-bit division exists because RV32 lacks it (ADR 0020). The driver
picks the one `Target` value itself.

## Decision

### One description per target

A target is a value of `target::Target`, and the compiler has a fixed
list of them, `target::TARGETS`. A description holds everything that
differs between targets:

| Field | What it is | `riscv32imac-unknown-none-elf` | `thumbv7m-none-eabi` (ADR 0074) |
|-------|------------|--------------------------------|---------------------------------|
| `name` | what `--target` names | as in the heading | as in the heading |
| `arch` | the architecture, which picks the code below | `Riscv32` | `Armv7m` |
| `llvm_triple`, `llvm_cpu`, `llvm_features` | LLVM's machine | `riscv32-unknown-none-elf`, `generic-rv32`, `+m,+a,+c` | `thumbv7m-none-eabi`, `cortex-m3`, none |
| `pointer_bits` | width of pointers, `usize`, `isize` | 32 | 32 |
| `widest_access_bits` | the widest access one instruction makes (ADR 0023) | 32 | 32 |
| `stack_align` | alignment of the stack pointer the C ABI requires (ADR 0057) | 16 | 8 |
| `code_align` | alignment of the code's region origin, where the reset entry is (ADR 0057) | 4 | 128 |
| `div64` | whether LLVM's 64-bit division may be emitted, its calls all having the C ABI (ADR 0076) | yes | no |

What is code rather than a number is chosen by `arch`, each in the
layer that owns it: the calling convention in `codegen::abi` (ADR 0025,
0075), the reset entry in codegen (ADR 0012, 0030, 0074), LLVM's
initialization in codegen, and the linker script's architecture and
sections in `link`. A `match` on `arch` is the one place to add an
architecture to; there is no other test of the target's name.

**Pointer width and widest access are shared.** Layout, the type
checker, MIR building and the compile-time interpreter use the pointer
width and the widest access as constants (`target::POINTER_BITS`,
`target::WIDEST_ACCESS_BITS`), in types such as `layout::MAX_SIZE`. They
stay constants: every target in `TARGETS` has 32 for both, and a test
checks each description against the constants. A description still
states its values, so that a target with others is a visible change of
the constants and of every use, not a silent one. A 64-bit target, which
would make them parameters, is not planned (ADR 0002, 0010; roadmap).

`stack_align` and `code_align` stop being constants: the memory map's
checks (`ty::misaligned-code`, `ty::stack-size`) and the linker script
read them from the build's target, and the diagnostics name its values.

### Choosing the target

`erebos build` takes `--target <name>`, where the name is one of
`TARGETS`. Without it the target is `riscv32imac-unknown-none-elf`, so
every existing command and test builds as before. Any other name is an
error that lists the known ones:

```text
error: unknown target `cortex-m3`; the targets are riscv32imac-unknown-none-elf, thumbv7m-none-eabi
```

The names are those Rust gives the same targets; the ARM one is also
clang's triple. They name an instruction set and an ABI, not a board:
the board is the program's `memory` block and its peripherals.

Only `erebos build` takes the flag; `erebos svd` writes source, which
does not depend on the target. Objects given with `--link` must be
built for the same target; nothing checks it before linking, and a
mismatch is reported in the linker's own words.

The target reaches the front end as well as codegen and linking: type
checking checks the memory map against `code_align` and `stack_align`.
The driver passes the chosen `&'static Target` to the front end, to
codegen and to `link`; `target` stays shared vocabulary, which every
layer may name (ADR 0063). `--emit` stages are those of the chosen
target. The ui tests take a header, `// target: <name>`, defaulting to
riscv32.

A build is a function of its `.ers` files and its flags, as with `-O`.
The language is the same on every target: a program that compiles for
one has the same types, layouts and values on another (with 32-bit
pointers); what changes is the machine code, the calling convention
with C, and the memory map checks above.

## Consequences

- Adding an architecture is a description, an `arch` variant and its
  four pieces of code (ABI, reset entry, LLVM initialization, linker
  script), each found by the compiler's `match` on `arch`.
- Every build that does not say `--target` is riscv32. A program written
  for an ARM board and built without the flag gets the riscv32 checks
  and code; nothing in the program says which target it needs.
- A program that is correct for two targets may still fail the memory
  map checks of one: a code origin at 0x40 is aligned for riscv32 and
  not for thumbv7m.

  **Note, 2026-11-03:** the example said 0x100, which is a multiple of
  128 and so is accepted on thumbv7m; 0x40 is the origin that only
  riscv32 accepts.
- 32-bit pointers stay a fact of the language, now stated per target and
  checked.

## Alternatives considered

- **`--target` required, with no default** — more explicit, but every
  command, test and document gains the flag while riscv32 is still the
  usual target.
- **The target declared in the source, in the `memory` block** — the
  board would say what core it has, and a build would depend on its files
  alone; but the same program could not be built for two targets without
  conditional compilation, which the language does not have, and the
  front end would need the block evaluated before it knows the target.
- **Short names (`riscv32`, `cortex-m3`)** — shorter, but they name less
  (no ABI, no instruction set extensions) and match no other tool.
- **Pointer width as a parameter now** — the work of a 64-bit target,
  which is not planned; both targets would take the same value.
