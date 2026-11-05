# 0085. Vector tables, the trap entry and default handlers

- Status: Accepted
- Date: 2026-11-05
- Amends: 0030 (`_start` also sets the trap vector on riscv32), 0053 (a
  new `PanicReason`, with a `PanicInfo` that locates nothing), 0074 (the
  table's words past the second exist; a fault no longer locks up; the
  table is no longer in the dump of a program without handlers), 0083 (the
  table grows to 16 words, so `_start` is at origin plus 64 on thumbv7m;
  riscv32 has a table too; `_start` sets `mtvec`; the table is no longer
  in the dump of a program without handlers)

## Context

ADR 0084 declares handlers. Something must reach them from the machine:
on thumbv7m the core reads a word of the vector table, whose first two
words the compiler already emits (ADR 0083); on riscv32 the core jumps to
the one address in `mtvec`, undefined at reset (0 on QEMU `virt`), which
nothing sets yet, so a trap jumps to address 0 today. The roadmap agrees
that the compiler generates the table or trap entry from the program's
handlers, and that a vector without a handler calls the panic function,
with a new `PanicReason`, instead of locking up.

ADR 0083 moved the startup code out of the compiler into `core::arch`,
where a reader sees it, written in the language. A trap entry is the same
kind of code. What must be generated is only what depends on the program:
which handler each vector calls.

ADR 0078 leaves to this ADR whether generated handlers are contexts. If
they were, the panic function would run in an unmaskable context in every
program, and by rule 2 of that ADR it could never name a `static mut`,
not even one of its own, since it also runs from every check.

ADR 0053 charges a program for panic information only if its panic
function takes it. A vector without a handler has no file or line.

## Decision

### What the compiler generates

On each target, one table of words, and the default handlers its words
name. Everything else is `core`'s, in Erebos. `Mir` holds the table as
data, one entry per word, in the order of the words, and code generation
emits it as module assembly, as ADR 0083 emits the first two words. Default
handlers and their information are emitted in the same order.

### The table on thumbv7m

`.vector_table` has **16 words**, 64 bytes:

| Word | Holds |
|------|-------|
| 0 | `__stack_end` |
| 1 | `_start` |
| 2, 3, 4, 5, 6, 11, 12, 14, 15 | the handler at that `Vector`, or its default handler |
| 7, 8, 9, 10, 13 | 0: reserved by the architecture, never read by the core |

```asm
    .section .vector_table, "a", %progbits
    .p2align 2
    .word __stack_end
    .word _start
    .word __erebos_unhandled_NMI
    ...
```

The linker sets bit 0 of each handler's word, as it does for `_start`.
`_start` follows, at the origin plus 64. `code_align` stays 128 (ADR 0074):
a table of 64 bytes fits the alignment VTOR already needs. M4.4 sizes the
table with the chip's interrupts and decides what follows from it for
`code_align`. Until then, a program that enables an NVIC interrupt through
registers makes the core read a word of `_start`'s code, as ADR 0074 said
of the words past the second.

Handlers need no entry code on this target: the core stacks the registers
that the C ABI does not preserve. They rely on `CCR.STKALIGN` being 1, so
that the core aligns the stack to 8 on entry. It is 1 at reset on QEMU and
on the Cortex-M3 from revision r2p0, and `_start` does not write it: a
Cortex-M3 before r2p0 is not supported for programs that take interrupts.

### The table on riscv32

The table has **33 words**, in read-only data, under the symbol
`__erebos_trap_table`, in a section `.rodata.__erebos_trap_table`:

| Word | Holds |
|------|-------|
| 0–15 | the exception of that `mcause`: its handler, its default handler, or the default of word 32 if `Vector` has no such variant |
| 16–31 | the interrupt of that cause (`mcause` without bit 31) in the same way |
| 32 | the default handler of any other cause |

### The trap entry on riscv32

`mtvec` is **direct**: every trap enters at one address, with mode bits 0.
The entry is a private naked function of `rt/arch/riscv32.ers`:

```
#[naked]
#[align(4)]
fn __erebos_trap() -> ! {
    asm(
        "addi sp, sp, -64",
        "sw ra, 0(sp)",
        "sw t0, 4(sp)",
        "sw t1, 8(sp)",
        "sw t2, 12(sp)",
        "sw a0, 16(sp)",
        "sw a1, 20(sp)",
        "sw a2, 24(sp)",
        "sw a3, 28(sp)",
        "sw a4, 32(sp)",
        "sw a5, 36(sp)",
        "sw a6, 40(sp)",
        "sw a7, 44(sp)",
        "sw t3, 48(sp)",
        "sw t4, 52(sp)",
        "sw t5, 56(sp)",
        "sw t6, 60(sp)",
        "csrr t0, mcause",
        "slli t1, t0, 1",
        "srli t1, t1, 1",
        "li t2, 16",
        "bltu t1, t2, 1f",
        "li t1, 32",
        "j 2f",
        "1:",
        "bgez t0, 2f",
        "addi t1, t1, 16",
        "2:",
        "slli t1, t1, 2",
        "la t2, __erebos_trap_table",
        "add t1, t1, t2",
        "lw t1, 0(t1)",
        "jalr t1",
        "lw ra, 0(sp)",
        ...
        "lw t6, 60(sp)",
        "addi sp, sp, 64",
        "mret",
        options(noreturn),
    );
}
```

- It saves the **16 registers the C ABI does not preserve**, in 64 bytes,
  so the stack stays aligned to 16 (`stack_align`, ADR 0073) if it was. The
  handler, a plain function of the C ABI, saves the others if it uses
  them. Interrupted code cannot tell it ran.
- It runs on the stack of the interrupted code; there is no stack for
  traps.
- `mepc` and `mstatus` are **not saved**. A trap clears `MIE`, so no
  maskable handler enters while one runs, and an exception inside a
  handler never returns (ADR 0084), so nothing that returns overwrites
  them. A program that sets `MIE` inside a handler leaves the model
  (ADR 0078) and must save them itself.
- It is private and the compiler does not name it: `_start` names it with
  `sym`. `#[align(4)]` (ADR 0087) is there because `mtvec` holds multiples
  of 4 and, with the C extension, LLVM may align a function to 2.

**`_start` sets the vector.** After the stack pointer, before anything can
fault, `rt/arch/riscv32.ers`'s `_start` adds:

```
        "la t0, {trap}",
        "csrw mtvec, t0",
        ...
        trap = sym __erebos_trap,
```

### Default handlers

Each word whose vector has no handler names a **default handler**, which
calls the panic function, with the same call a check makes (ADR 0053), and
never returns.

- **A panic function without a parameter:** one shared function,
  `__erebos_unhandled`. No information is built, as ADR 0053 asks.
- **A panic function with `*const core::PanicInfo`:** one function per
  vector, `__erebos_unhandled_<variant>`, and `__erebos_unhandled_other`
  for the words of riscv32 that have no variant, each passing read-only
  information of its own:

  | Field | Value |
  |-------|-------|
  | `reason` | `PanicReason::UnhandledVector`, a new variant with the value 5 |
  | `message` | the variant's name, as bytes: `b"HardFault"`, `b"IllegalInstruction"`; `b"unknown"` for `__erebos_unhandled_other` |
  | `file` | an empty slice |
  | `line`, `column` | 0 |

  That costs, per vector left empty, the 28 bytes a check costs (ADR
  0053), the name, and the small function. These `PanicInfo`s are the one
  case where `line` and `column` are not counted from 1, `file` names no
  file, and `message` is not an `assert`'s, as ADR 0053 and the comments of
  `rt/core.ers` otherwise say.

The names are reserved already (ADR 0020). A fault in a program without any
handler reaches the panic function, on both targets.

A panic function reached this way runs **with what the trap left**: `MIE`
clear on riscv32, and at the priority of the exception on thumbv7m. It may
read `mcause`, `mepc`, `IPSR`, `HFSR` with `asm`. If it faults in turn: on
thumbv7m at HardFault's level, the core locks up; on riscv32 the trap entry
runs again, on the same stack, and the panic function with it. Nothing
guards against that recursion (ADR 0078 leaves an unmaskable handler
entered again out of its model).

### Generated handlers are not contexts

A default handler is **not a context of ADR 0078**, maskable or not, and
M4.5 does not count what it reaches. Only handlers that a program declares
are contexts.

Rule 2 exists because code that an unmaskable handler interrupted will
continue and see the handler's writes, or the handler sees half of its
writes and both carry on. A default handler never returns: the interrupted
code never continues, and nothing else runs but an NMI. There are no two
activities to order; the panic function may read a value that the
interrupted code was about to write, or a structure half updated, which is
all a panic function can know at that moment in any case. Its reads are
compiled like any others, and there is no later access for an optimization
to invalidate. So a panic function may name statics under the rules that
its other contexts give, and this path adds nothing. The exception is an NMI
or fault handler the program declares, which may preempt the panic function
here and write a static it reads; M4.5 sees that pair only if the panic
function is also reached from a check or a declared handler. This ADR
accepts that, for a program already crashing.

### Dumps

`--emit=mir` lists the program's handlers and their vectors, in the order of
the table's words. `--emit=llvm-ir` shows the table and the default handlers
when the program declares a handler. When it declares none, `leave_out`
(ADR 0052) also drops them, though they are the compiler's and not `core`'s:
this is a choice for readers, since such a table says nothing the program
wrote, and it would add 33 lines to every riscv32 snapshot. So the IR of a
program without handlers has no `module asm` on either target, which reverses
ADR 0074 and 0083 for thumbv7m, whose dumps showed the first two words.

### Conformance tests

- **ui snapshots:** the LLVM IR of a program with one handler, on each
  target, with a panic function with and without the parameter: the table,
  the handler's word, the default handlers and their information; a handler
  with `#[export]`, whose word holds its own name, and one in a nested
  module, whose word holds its dotted symbol; the IR of a program without
  handlers has no `module asm` on either target; the MIR dump lists handlers;
  `PanicReason::UnhandledVector` matched by name.
- **ELF:** on both targets at both levels: on thumbv7m `.vector_table` is
  64 bytes at the origin, `_start` at origin plus 64 with bit 0 set, every
  word as the table says, the reserved ones 0, a handler's word its symbol
  with bit 0 set; on riscv32 `__erebos_trap_table` is 33 words, each the
  address of the expected function, and `core.arch.__erebos_trap` is a
  multiple of 4.
- **End to end,** on each target, at both levels:
  - the timer of the milestone: SysTick on `mps2-an385`, the CLINT on
    `virt`, counting ticks in a `static mut` read in a `critical` block by
    the entry function, which waits with `wait_for_interrupt` (ADR 0086)
    and exits after 5;
  - a fault without a handler reaches the panic function, which prints the
    reason and the message and shows an empty file and line 0: `udf` as
    `HardFault`, an illegal instruction as `IllegalInstruction`;
  - a maskable vector without a handler (SysTick, the machine timer)
    reaches it with its name; on riscv32 a cause without a variant (the
    supervisor software interrupt, set in `mip`) as `unknown`;
  - a panic function without a parameter reached from a fault;
  - a fault with a declared handler runs that handler and not the panic
    function, and on riscv32 `ecall` with a declared `EnvironmentCall`;
  - on thumbv7m, a returning `SVCall` handler, and PendSV and NMI pended by
    the program in `ICSR`, each resuming the code they interrupted;
  - a handler in a nested module with a dotted symbol runs, on thumbv7m with
    its word's bit 0 set. The other vectors, which the tests do not raise,
    are covered by the ELF tests' check of every word;
  - a handler that interrupts code holding known values in every register
    the C ABI saves and every one it does not, written with `asm`, which
    finds them unchanged (a loop that spins until the handler has run);
  - a handler that reads `sp` with `asm` and finds it aligned to
    `stack_align`, and calls a function with a `u64` local;
  - a maskable handler that faults (a check in it), reaching the panic
    function.

## Consequences

- A fault no longer locks up or jumps nowhere, in any program. ADR 0074's
  consequence that a fault locks the processor up holds only for a fault
  inside the panic function reached from HardFault.
- Every program pays: on thumbv7m 56 more bytes of table; on riscv32 132
  bytes of table and the trap entry's code, an estimated 120 bytes; one
  default handler, or one per vector when the panic function takes
  information.
- The trap entry is readable in `rt/arch/riscv32.ers`, in the language; a
  reader of `--emit=llvm-ir` sees the table only when the program has
  handlers.
- A stack overflow on riscv32 faults inside the trap entry, which then
  faults again, forever. On thumbv7m a stacking failure is a HardFault,
  whose own stacking may lock up. Stack limits are for later; on QEMU the
  harness's timeout catches a hang.
- A panic function may name statics, and M4.5's rule sees only its other
  contexts. It may read a half-updated structure when reached from a fault,
  as with any crash handler; nothing else can run into it.
- Word 32 and the default of unnamed causes mean an unexpected platform
  cause reaches the panic function as `unknown`, rather than a wrong
  handler.
- `_start` is one step longer on riscv32, and a program's traps go to `core`
  until the program writes `mtvec` itself with `asm`, which then leaves this
  ADR's handlers and defaults behind.

## Alternatives considered

- **`mtvec` vectored** — the core jumps to `base + 4 * cause` for
  interrupts, but every exception enters at word 0, so the dispatch on
  `mcause` stays; the words are `j` instructions, reaching ±1 MiB, a limit
  ADR 0083 just removed from `_start`. The gain is a few cycles per
  interrupt.
- **LLVM's `interrupt` attribute on each handler** — saves only what the
  handler uses and ends in `mret`, but needs vectored mode, leaves the
  dispatch of exceptions open, and hides the save in the compiler, the
  opposite of ADR 0083.
- **The trap entry generated by the compiler** — the roadmap's wording
  allows it, but only its table depends on the program, and code that is
  the same for every program belongs in `core`.
- **A chain of comparisons on `mcause` instead of a table** — smaller with
  few handlers, but every program's default handlers fill the table anyway.
- **Saving `mepc` and `mstatus`, or a separate trap stack** — needed only
  by traps that return inside a handler, which ADR 0084 does not allow, or
  by a reliable report of a stack overflow. Both can be added.
- **Generated handlers as contexts** — the model has no exception, but no
  panic function could name a `static mut` from M4.5 on, for a race that
  cannot resume.
- **The vector's number in `line`** — no strings, but it overloads `line`
  and a handler has to translate it.
- **A new field `vector: u32` in `PanicInfo`** — explicit, but every check
  of every program pays 4 bytes for it, and the layout of ADR 0053 changes.
- **One `PanicInfo` for every default, the program reading `mcause` or
  `IPSR`** — the cheapest, but the roadmap agrees the vector is passed.
- **A default handler in `core` reading the cause and calling the panic
  function through a new `sym panic`** — no generated functions, but the
  information would be in the image of programs whose panic function does
  not take it, against ADR 0053.

Defaults assumed while writing this ADR, for correction when accepting it:

- **The table and defaults always shown in `--emit=llvm-ir`** — complete,
  but 33 identical lines in every riscv32 snapshot, for nothing specific to
  the program.
- **Word 7, 8, 9, 10, 13 holding a default handler** — a reserved exception
  never enters; 0 says so in a dump, and costs nothing.
- **`_start` writing `CCR.STKALIGN`** — helps only Cortex-M3 before r2p0;
  one more register for a program to read about.
