# 0077. Programs tested on every target

- Status: Accepted
- Date: 2026-11-03
- Amends: 0010 (test programs stop QEMU through their board module: on
  `virt` still the test finisher, on `mps2-an385` semihosting)

## Context

The roadmap's program for M3.5 is "the e2e programs on both targets".
Each program of `tests/e2e/` declares, inside itself, the
devices of QEMU `virt` it uses (the 16550's transmit register at
`0x1000_0000`, the test finisher at `0x0010_0000`) and a `memory` block
for `virt`'s RAM. The generated programs of ADR 0068 do the same. None
of that exists on `mps2-an385` (ADR 0074): its UART is a CMSDK UART at
`0x4000_4000` that must be enabled before it transmits, its memory is
elsewhere, and it has no device that stops QEMU with an exit code. On
Cortex-M, test programs stop QEMU through semihosting: the instruction
`bkpt 0xab` with a request in `r0` and its argument in `r1`, which QEMU
serves when started with `-semihosting-config enable=on`.

The language has no conditional compilation, and no inline assembly
before M4.

## Decision

### A board module per target

The tests hold one **board module** per target, in
`tests/boards/<target name>.ers` (`riscv32imac-unknown-none-elf.ers`,
`thumbv7m-none-eabi.ers`), with a C file of the same name beside it when
the board needs one. A board module describes the QEMU machine the
target's tests run on, and offers programs:

```
pub fn putc(byte: u8)          // one byte to the UART
pub fn exit(code: u32) -> !    // stops QEMU with `code` as its exit status
pub const SCRATCH: usize       // 64 KiB of RAM nothing is linked into
```

It also holds the machine's `memory` block (ADR 0057 lets it sit in any
module). `SCRATCH` is the address of 64 KiB of RAM that no section
occupies and `_start` never writes, zero when QEMU starts, for programs
that need memory at a fixed address: a marker that survives a jump
back to `_start`, or a misaligned buffer.

- **riscv32, QEMU `virt`:** the `memory` block the programs have today
  (64 MiB for the image at `0x8000_0000`, 64 MiB at `0x8400_0000` for a
  1 MiB stack, `.data` and `.bss`), `putc` writing the 16550's transmit
  register, `exit` writing the test finisher (`0x5555` for 0,
  `(code << 16) | 0x3333` otherwise), and `SCRATCH` at `0x8100_0000`,
  in the image's region well above the program, as the programs use it
  today.
- **thumbv7m, QEMU `mps2-an385`:** a `memory` block with 4 MiB at
  `0x0` for the code and 4 MiB at `0x2000_0000` for a 1 MiB stack,
  `.data` and `.bss`; `putc` writing the UART's baud divisor
  (`BAUDDIV`, offset `0x10`) with 16, the smallest QEMU accepts, then
  setting its transmit enable bit (QEMU reports enabling it with a
  smaller divisor as a guest error), waiting while its transmit buffer
  is full, and writing the byte; `exit` calling a C function of
  `tests/boards/thumbv7m-none-eabi.c` that makes the semihosting
  request `SYS_EXIT_EXTENDED` (`0x20`) with the reason
  `ADP_Stopped_ApplicationExit` (`0x20026`) and `code`, in inline
  assembly (`bkpt 0xab`), which QEMU turns into its exit status; and
  `SCRATCH` at `0x2100_0000`, in the machine's 16 MiB of RAM that the
  `memory` block leaves out.

The board modules are test infrastructure, not part of the compiler or
of `core`. A program for a real board writes its own the same way.

### Portable programs

A program reaches the machine only through `mod board;`: its output
through `board::putc`, its end through `board::exit`, its memory map
through the board's block, and any fixed address through
`board::SCRATCH`. Its panic function and its printing helpers stay in
the program and call the board. Moving the devices and the `memory`
block out changes the lines of a program; the expected panic locations
(`file:line:column`) of the programs that print them change with them,
and nothing else in an expectation does.

The test harness builds every program for every target it runs on, at
both optimization levels, in a work directory: it copies the program
(its root file, or its directory with `main.ers` and the files below,
keeping their names, which panic messages print), adds the target's
board as `board.ers` beside the root file, compiles the board's C file
if there is one, and links it with the program's own C files
(`// link-c:`), compiled with that target's clang flags. A program that
already has a file named `board.ers` is an error of the harness.

A program that needs one machine says so in a header:

```text
// targets: riscv32imac-unknown-none-elf
```

and runs only on the targets it lists. Without the header, it runs on
every target, with the same expected output and exit code on each.
A program lists only riscv32 for exactly one of three reasons, which
its header comment names:

1. it uses a device of `virt` other than the board's (the PLIC, virtio,
   the 16550's other registers, the SVD of `virt`);
2. it uses a feature of one target's C compiler (riscv32's small-data
   sections);
3. it tests a `memory` block of its own, laid out for `virt`'s RAM
   (`memory_map`, `memory_map_ram`); the board's block cannot be the
   one under test. The thumbv7m board's own map, code in one region and
   data and stack in another, is such a layout, and the ELF checks below
   cover it.

Such programs keep declaring their devices and their `memory` block
themselves. Every other program is portable, including those of the
startup code: `startup_data` and `startup_bss` (which jumps back to
`_start` and marks its second run at `board::SCRATCH`) run on both
targets, so that ARM's `_start` is tested at run time as riscv32's is.

### Running each target

The harness starts, per target:

| Target | QEMU |
|--------|------|
| riscv32 | `qemu-system-riscv32 -machine virt -nographic -bios none -kernel <elf>` |
| thumbv7m | `qemu-system-arm -machine mps2-an385 -nographic -semihosting-config enable=on,target=native -kernel <elf>` |

with the event log of today (`-d int,guest_errors`), which must stay
empty. On `mps2-an385`, QEMU 11 logs lines of a correct run under
`-d int`, which the harness removes before checking, and only these,
each matched from the start of the line:

| Line | Why QEMU logs it |
|------|------------------|
| `Loaded reset SP 0x… PC 0x… from vector table` | at every reset, twice (before and after the image is loaded) |
| `Taking exception 16 [Semihosting call] …` | at each semihosting request |
| `...handling as semihosting call 0x…` | the same request, served |

Any other line fails the run, including any other `Taking exception`.
If a QEMU version words them differently, the harness fails with the
unknown lines shown, and the table is updated with the version.

### Other tests on both targets

- The generated programs of ADR 0068 use `mod board;` and run on both
  targets, against the same compile-time oracle.
- The layout tests (`tests/c_layout.rs`, `tests/layout/`) compile their
  C twins with each target's clang flags: the layouts must agree with
  clang on both.
- The ELF checks of the startup guarantees and of the memory map
  (ADR 0030, 0057) run on a program built for each target, with the
  target's stack alignment and its reset entry: on riscv32 `_start` is
  at the code's origin, on thumbv7m the two words of the vector table
  are, followed by `_start`.
- The reproducible-build test (`tests/reproducible.rs`) builds its
  programs for each target.
- The IR invariants of `tests/architecture.rs` (no `nsw` or `nuw`, the
  panic counts) hold for thumbv7m's snapshots too.
- ui snapshots of `--emit=llvm-ir` and `--emit=linker-script` for
  thumbv7m, chosen with `// target:` (ADR 0073), show its reset entry,
  its calls with aggregates (ADR 0075) and its 64-bit division
  (ADR 0076).
- An e2e program with a C twin calls in both directions with the
  aggregates of ADR 0075's table, placed so that they meet the
  boundaries of the AAPCS: an `[N x i64]` after an odd number of words,
  an argument split between `r3` and the stack, a `byval` argument that
  starts in registers, a top-level and a nested `#[align(16)]` struct,
  returns of 1 to 5 bytes. It is portable, so it also runs on riscv32.
- The tests that only run the front end (`tests/rejected.rs`, most ui
  snapshots) stay on riscv32: the front end differs between targets only
  in the memory map checks, which ui snapshots cover per target.

## Consequences

- One program text runs on both machines, so a difference in behavior
  between the targets is a test failure, not a second program to keep in
  step.
- The end-to-end and generated tests take about twice as long.
- Every portable program changes once: its devices and `memory` block
  move to the board, and its writes to `UART0.thr` and `FINISHER.status`
  become calls. Programs no longer show the devices they write to; the
  board module does, in one place per machine.
- Expected outputs and exit codes cannot differ per target. A program
  whose behavior should differ is two programs, each with `// targets:`.
- Semihosting needs QEMU's option; a program built for a real board with
  this test board would stop at the `bkpt` without a debugger.

## Alternatives considered

- **Conditional compilation in the language** (`#[cfg(target = ...)]`,
  or a `comptime` value naming the target) — programs could choose their
  devices themselves, but it is a language feature of its own size, and
  a test need does not justify designing it now.
- **A compiler flag mapping a module to a file** (`--module
  board=<path>`) — no copying, but a build input of the compiler that
  only the tests would use.
- **Semihosting written in the board module** — needs `bkpt`, which
  Erebos cannot write before M4's inline assembly.
- **Leaving the existing programs on riscv32 and writing new ones for
  ARM** — two sets to keep in step, and the riscv32 programs would never
  test the second target.
