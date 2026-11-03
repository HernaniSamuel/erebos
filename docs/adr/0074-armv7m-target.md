# 0074. The ARMv7-M target

- Status: Accepted
- Date: 2026-11-03
- Amends: 0012 (on ARMv7-M the reset entry is two words of a vector
  table before `_start`), 0030 (the linker script discards the unwind
  tables of ARM objects)

## Context

The roadmap's second target is ARM Cortex-M on QEMU, so that the
riscv32 assumptions are found before M4 designs interrupts and vector
tables (ADR 0073). Cortex-M is where most microcontrollers are, and it
is 32-bit like riscv32, so `usize` keeps its width.

The cores differ in what they lack: Cortex-M0 and M0+ (ARMv6-M) have no
32-bit division instruction, so LLVM calls `__aeabi_uidivmod` and
`__aeabi_idivmod`, which return quotient and remainder in `r0` and
`r1`, a convention no C function has; Cortex-M3 and later (ARMv7-M)
divide 32-bit values in hardware. All of them lack 64-bit division and
use the run-time functions of the ARM EABI for copies and fills.

Two QEMU machines with a Cortex-M3 fit: `lm3s6965evb`, a TI chip with
256 KiB of flash and 64 KiB of RAM, and `mps2-an385`, Arm's FPGA image
for the Cortex-M3 with 4 MiB of SRAM at `0x0` and 4 MiB at
`0x2000_0000`. Neither has a device to stop QEMU with an exit code, as
`virt` has; on Cortex-M, test programs stop QEMU through semihosting.

A Cortex-M does not start by jumping to an address. At reset it reads
two words at the address in VTOR, whose reset value the chip fixes: the
initial stack pointer, then the address of the reset handler, whose
bit 0 must be set to mark Thumb code. The rest of that vector table
holds the handlers of exceptions and interrupts, which are M4's.

## Decision

### The target

`thumbv7m-none-eabi` (ADR 0073) is ARMv7-M, as on the Cortex-M3:

- LLVM triple `thumbv7m-none-eabi`, CPU `cortex-m3`, no extra features.
  Thumb-2 only, little-endian, no floating-point unit; the C ABI is the
  AAPCS with its base (soft-float) procedure call standard, which is
  what clang uses for this triple. Aggregates cross calls as ADR 0075
  says; scalars as on riscv32 (ADR 0006): integers narrower than 32 bits
  and `bool` are extended (`signext`, `zeroext`), and `u64`/`i64` take
  an even–odd register pair or 8 aligned bytes of stack.
- The pointer width and widest access are 32 bits. `LDRD` and `STRD`
  are single instructions, but ARMv7-M makes them two accesses of a word
  each, so a 64-bit volatile access stays an error (ADR 0023).
- `stack_align` is 8, the AAPCS's alignment of the stack at calls.
- `code_align` is 128. The reset entry is a vector table. At reset the
  chip fixes where it looks (VTOR's reset value, 0 on `mps2-an385`),
  which the compiler cannot check, as on riscv32 (ADR 0057); but a
  table at that origin is also the one a program points VTOR at, and
  VTOR only holds multiples of 128 (its `TBLOFF` field starts at bit 7).
  So a code region whose origin is not a multiple of 128 cannot hold a
  vector table on any ARMv7-M chip. `ty::misaligned-code` reports it as
  for riscv32.
- `div64` is false: LLVM would call the EABI's 64-bit division
  functions, so 64-bit `/` and `%` call `core` instead (ADR 0076).

### The reset entry

The reset entry (ADR 0057) is two words, the start of a vector table,
followed by `_start`, all in the section `.text.start` that the linker
script places at the origin of the code's region:

```asm
    .syntax unified
    .thumb
    .section .text.start, "ax", %progbits
    .p2align 2
    .word __stack_end           @ initial stack pointer
    .word _start                @ reset handler; bit 0 set: Thumb
    .globl _start
    .type _start, %function
    .thumb_func
_start:
    ldr r0, =__stack_end
    mov sp, r0
    ldr r0, =__bss_start
    ldr r1, =__bss_end
    movs r2, #0
.Lzero_bss:
    cmp r0, r1
    bhs .Lcopy_data
    str r2, [r0], #4
    b .Lzero_bss
.Lcopy_data:
    ldr r0, =__data_start
    ldr r1, =__data_end
    ldr r2, =__data_load
.Lcopy_word:
    cmp r0, r1
    bhs .Lenter
    ldr r3, [r2], #4
    str r3, [r0], #4
    b .Lcopy_word
.Lenter:
    ldr r0, =<entry function>
    bx r0
    .ltorg
```

`_start` does exactly what ADR 0012 and 0030 let it do on riscv32: set
the stack pointer, fill `.bss` with zeros, copy `.data`, jump to the
entry function. The processor has already loaded the stack pointer from
the first word; `_start` sets it again so that code jumping back to
`_start`, as a startup test does (ADR 0030), starts from a fresh stack.
The comparisons are unsigned (`bhs`), as `bgeu` is on riscv32, and the
entry function is reached through a register, so it may be anywhere in
the address space. `.ltorg` keeps the literal pool of the `ldr =` loads
inside `.text.start`. It is visible in `--emit=llvm-ir` (`module asm`),
and `_start` stays a reserved name.

The table has no other entry. An exception (a fault, an NMI) reads its
handler from the words after the second, which are `_start`'s code: the
processor jumps to an address that is no handler, faults again, and
locks up. QEMU logs the exception, which the test harness reports
(ADR 0077). Exception handlers and the rest of the table are M4's.

### The linker script

The script of ADR 0057 and 0030 changes in three places on this target:

- `OUTPUT_ARCH(arm)` instead of `OUTPUT_ARCH(riscv)`.
- `.ARM.attributes`, which describes the object and is not loaded,
  joins the unloaded sections placed explicitly, in place of
  `.riscv.attributes`.
- `.ARM.exidx*` and `.ARM.extab*` are **discarded** (`/DISCARD/`). LLVM
  and clang emit an unwind table entry for every function, saying that
  it cannot be unwound. Nothing unwinds (ADR 0002), and placing them
  would cost eight bytes per function in the code's region. This amends
  ADR 0030, whose script discards nothing: these sections are named,
  and any other unknown section is still an orphan and an error
  (`--orphan-handling=error`).

The small-data sections of ADR 0030 stay in the script; ARM objects do
not have them, and they cost nothing there.

### Run-time functions

On ARM EABI targets LLVM copies, moves and fills memory through the
EABI's functions instead of `memcpy`, `memmove` and `memset`:
`__aeabi_memcpy`, `__aeabi_memmove`, `__aeabi_memset` and
`__aeabi_memclr`, each also with the suffixes `4` and `8`, which promise
a destination (and source) aligned to 4 or 8. `core::builtins`
(ADR 0020, 0052) defines the twelve, in Erebos, on every target: each
calls the general function, ignoring the alignment promise. None
returns a value. Their signatures, with `n` a `usize`:

| Function | Parameters |
|----------|------------|
| `__aeabi_memcpy`, `4`, `8` | `dest: *mut u8, src: *const u8, n` |
| `__aeabi_memmove`, `4`, `8` | `dest: *mut u8, src: *const u8, n` |
| `__aeabi_memset`, `4`, `8` | `dest: *mut u8, n, c: i32` (in that order, not `memset`'s); only the low byte of `c` is stored, as by `memset` |
| `__aeabi_memclr`, `4`, `8` | `dest: *mut u8, n` |

They cost a few hundred bytes on riscv32, where nothing
calls them, as the other run-time functions do where they are unused
(ADR 0020).

ARMv7-M divides 32-bit values in hardware. For 64-bit division LLVM
would call `__aeabi_uldivmod` and `__aeabi_ldivmod`, which return the
quotient in `r0`–`r1` and the remainder in `r2`–`r3`, a convention no
Erebos function can have under the C ABI; code generation calls
`core`'s division functions instead (ADR 0076), and `core` does not
define the two EABI ones.

### The machine of the tests

Programs run on QEMU's `mps2-an385` (`qemu-system-arm -machine
mps2-an385`): a Cortex-M3 that boots from address 0 (VTOR's reset
value), 4 MiB of SRAM at `0x0` for the code, 4 MiB at `0x2000_0000` for
data and the stack, and the CMSDK APB UART0 at `0x4000_4000`. They stop
QEMU through semihosting (ADR 0077). C files linked into these tests are
compiled with `clang --target=thumbv7m-none-eabi -mcpu=cortex-m3
-mfloat-abi=soft -ffreestanding`.

## Consequences

- Programs run on a Cortex-M3 and on any ARMv7-M or later M-profile core
  in Thumb mode (Cortex-M4, M7, M33 run ARMv7-M code), without their
  FPUs or DSP instructions.
- C code linked into a `thumbv7m-none-eabi` program cannot divide
  64-bit values: clang calls `__aeabi_uldivmod` and `__aeabi_ldivmod`,
  which no one defines. The linker reports the undefined symbol.
  Defining them needs assembly (M4) or a convention attribute.
- A fault on this target locks the processor up instead of reaching a
  handler. On QEMU the log shows it; on a board the program stops.
- M4 may raise `code_align`. ARMv7-M requires a relocated vector table
  to be aligned to its size rounded up to a power of two, at least 128
  bytes; a full table with a chip's interrupts is larger than 128 bytes,
  so a code origin accepted now may be rejected once M4 sizes the
  table. Real chips put their code at addresses aligned far beyond
  that (`0x0`, `0x0800_0000`), so few programs should notice.
- Cortex-M0 (ARMv6-M) is another target, for later: it needs the 32-bit
  EABI division functions, with the same problem as the 64-bit ones.
- The language has no function pointers yet. When it gets them, their
  value on this target has bit 0 set, which matters to any comparison
  or conversion of one to an integer.

## Alternatives considered

- **`lm3s6965evb`** — a real chip's memory map, but 64 KiB of RAM is
  tight for the test programs, which declare stacks of up to 1 MiB,
  and the extra realism does not test the compiler more.
- **Cortex-M0, `thumbv6m-none-eabi` (QEMU `microbit`)** — the smallest
  core, but its missing 32-bit division brings the EABI's register-pair
  return into M3.5 for every division, not only 64-bit ones.
- **A table with the 16 system exceptions pointing at a loop** — a fault
  would stop in a known place, but that is code the program did not ask
  for, which M4's vector tables replace.
- **Placing `.ARM.exidx` in the code's region, as GNU scripts do** — it
  keeps information nothing reads, at eight bytes per function.
- **A triple without `eabi` (`thumbv7m-unknown-none-elf`)** — LLVM would
  call the libgcc names (`__udivdi3`, `memcpy`), but objects would not
  be EABI objects and would differ from clang's for the same board.
