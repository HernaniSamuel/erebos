# 0083. Startup code in `core::arch`

- Status: Accepted; amended by 0085 (the table has 16 words, `_start` at origin plus 64 on thumbv7m; `_start` sets `mtvec`; the table is not in the dump of a program without handlers), 0086 (`_start` masks interrupts first), 0090 (`_start` is further from the origin with a list)
- Date: 2026-11-04
- Supersedes: 0012
- Amends: 0030 (`_start` is `core`'s, and is read in `rt/arch/`, not in
  `--emit=llvm-ir`), 0052 (the compiler knows a third item of `core` by
  path), 0057 (on thumbv7m the reset entry is two sections, the vector
  table's and `_start`'s), 0074 (the two words of the vector table are
  data the compiler emits in a section of their own; `_start` is
  `core`'s and loads addresses without a literal pool), 0079 (`sym
  entry` names the entry function)

## Context

ADR 0012 made `_start` provisional: the compiler emits it as module-level
assembly because the language could not express it. ADR 0030 gave it the
loops that fill `.bss` and copy `.data`, ADR 0057 its place at the
origin of the code's region and its stack from the linker script, and
ADR 0074 the two words of a vector table before it on thumbv7m. Today
it is a string in `src/codegen/mod.rs`, one per target.

The language can now express it: a naked function (ADR 0079) is a
function with no code of the compiler's, whose body is one `asm`, and
`core::arch` (ADR 0080) is the module of `core` whose file depends on
the target. What it cannot express yet is how `core`, compiled with
every program, reaches the program's entry function, which it cannot
name by path (ADR 0052): the entry function may be in any module and
have any name, and its symbol follows ADR 0051.

Two more questions come with it: whether a program may supply reset
code of its own, as C startup files allow and Rust's `cortex-m-rt` does
with a hook run before initialization (its `#[pre_init]` in Rust,
deprecated in 0.7.6; a `__pre_init` in assembly, still supported); and
where the two words of
the thumbv7m vector table go, which M4.3 will grow into the full table
the compiler generates from the program's handlers.

## Decision

### `_start` is a naked function of `core::arch`

Each file of `core::arch` (ADR 0080) defines the reset entry:

```
#[export]
#[naked]
fn _start() -> ! {
    asm(
        "la sp, __stack_end",
        "la t0, __bss_start",
        "la t1, __bss_end",
        "1:",
        "bgeu t0, t1, 2f",
        "sw zero, 0(t0)",
        "addi t0, t0, 4",
        "j 1b",
        "2:",
        "la t0, __data_start",
        "la t1, __data_end",
        "la t2, __data_load",
        "3:",
        "bgeu t0, t1, 4f",
        "lw t3, 0(t2)",
        "sw t3, 0(t0)",
        "addi t0, t0, 4",
        "addi t2, t2, 4",
        "j 3b",
        "4:",
        "tail {}",
        sym entry,
        options(noreturn),
    );
}
```

That is `rt/arch/riscv32.ers`; `rt/arch/armv7m.ers` has the same steps
in Thumb-2, loading each address with a `movw`/`movt` pair (ARMv7-M has
both) instead of `ldr r0, =sym` and a literal pool, and jumping to the
entry function with `bx` through a register:

```
        "movw r0, :lower16:{0}",
        "movt r0, :upper16:{0}",
        "bx r0",
        sym entry,
```

- It does exactly what ADR 0030 lists, in that order: set the stack
  pointer to `__stack_end`, fill `.bss` with zeros, copy `.data` from
  its load address, jump to the entry function. The loops work a word
  at a time between the symbols of the linker script, test their bounds
  before the first word and compare unsigned, as before. Nothing else
  runs before the entry function. This is what `_start` does as of
  this ADR; a later step may add to it with its own ADR (M4.3 decides
  whether `_start` sets up the riscv32 trap entry or masks interrupts
  on thumbv7m).
- The linker script's symbols are written in the template, by name.
  Declaring them as `extern static`s to name them with `sym` would check
  nothing more: the linker defines them, and a misspelt name is an
  undefined symbol either way.
- The labels are local (`1:`, `2f`), as ADR 0079 asks of every template.
- The entry function is reached from anywhere in the address space: on
  riscv32 with `tail` (`auipc` and `jalr`), where ADR 0012's `j` reached
  only ±1 MiB; on thumbv7m through a register, as ADR 0074 had it. A
  `movw`/`movt` pair of a Thumb function's symbol has bit 0 set, as
  `bx` needs.
- `_start` is private and `#[export]`, so its symbol is `_start` and no
  program can name it by path. It is no portable item of ADR 0080: it
  exists in every file, but only the compiler names it.

**How the compiler knows it.** The compiler names `core::arch::_start` by
path, as it names `core::PanicInfo` and `core::PanicReason`
(ADR 0052, amended: three items). It puts the function in the section
`.text.start`, which the linker script places at the origin of the
code's region, alone, as ADR 0057 says; `ENTRY(_start)` stays. A file of
`core::arch` without a function `_start` is a bug of the compiler, not a
diagnostic.

**Where it is read.** `--emit=llvm-ir` leaves `core` out (ADR 0052), so
`_start` is no longer in it: the rule of ADR 0012 and 0030 that it is
visible there becomes "it is visible in `rt/arch/`", in the language.
The program's LLVM IR has no `module asm` on riscv32, and on thumbv7m
only the two words below.

**`_start` stays reserved.** A program cannot define `_start`: a
function, `extern fn` or `extern static` of that name in any module,
with or without `#[export]`, is `resolve::reserved-name`, as today; the
help now says that `core::arch` defines it. `core`'s own `_start` is
exempt. It is the only diagnostic: the comparison of symbols
(`resolve::duplicate-symbol`) leaves out a program's item named
`_start`, as it leaves out one whose name starts with `__` (ADR 0080),
so the program is not also told that its `_start` collides with a
private item of `core` it cannot name.

### `sym entry`

A `sym` operand (ADR 0079) may name the entry function with the keyword
`entry` in place of a path:

```
operand = ... | "sym" ( path | "entry" ) | ...
```

It is replaced by the entry function's symbol (ADR 0051), whatever the
function's module and name. `entry` is a keyword, so no path can mean
it, and nothing follows it: `sym entry::x` is a
`parser::unexpected-token` at `::`. It may be named like any operand
(`main = sym entry`). It is allowed in any `asm`, of `core` or of the
program: the program can already write any symbol into a template, so allowing it everywhere
adds no power, and a restriction would be one more rule and code. Like
any template, a jump to the entry function is the code's business:
`resolve::call-entry` is about calls written in Erebos and does not
apply. Every program has one entry function (`resolve::missing-entry`
otherwise), so `sym entry` has nothing of its own to report.

### The vector table on thumbv7m

The two words that start the vector table (ADR 0074), the initial stack
pointer `__stack_end` and `_start` with bit 0 set, are emitted by the
compiler as module-level assembly into a section of their own,
`.vector_table`, visible in `--emit=llvm-ir`:

```asm
    .section .vector_table, "a", %progbits
    .p2align 2
    .word __stack_end
    .word _start
```

On thumbv7m the linker script places `.vector_table` at the origin of
the code's region, then `.text.start` with `_start` alone, then the rest
as before; so `_start` is at the origin plus 8, as today, as long as
the function's alignment is at most 8. LLVM chooses it, and no
documented rule fixes it (a naked Thumb function was aligned to 4 in
a build tried while planning); the ELF test that checks the address catches a
change, and the fix is then an explicit alignment on the function,
which code generation sets. The riscv32
script has no `.vector_table`. The reset entry of ADR 0057 is, on
thumbv7m, these two sections. `code_align` stays 128 (ADR 0074).

The table stays the compiler's because M4.3 has the compiler generate
the full table from the program's handlers (roadmap); M4.3 grows this
section and decides its size.

### No reset code of the program

A program cannot replace `_start` nor run code before it. Code that
must run early, such as raising a clock, runs first in the entry
function, after `.data` is copied; the copy is then slower, which
matters only to large `.data` images. Memory left uninitialized across
a reset needs statics placed in a named region, already deferred by
ADR 0057. Code that must run before `.data` exists, such as setting up
an external memory controller that holds `.data`, is recorded in the
roadmap's deferred ideas. A hook of compiled code run before
initialization reads statics not yet set, which is why `cortex-m-rt`
deprecated its Rust `#[pre_init]`; the form it kept, a hook in
assembly, would here be a naked function of the program that
`_start` calls before the loops. No board of the tests needs it, so it
is the open variant the deferred idea names.

### Conformance tests

- **ui snapshots:** the AST of `sym entry`, plain and named; `sym
  entry::x` as `parser::unexpected-token`; the LLVM IR of a program
  with an `asm` holding `sym entry`, which names the entry function's
  symbol, on both targets, in the root module and with the entry
  function in a module; the LLVM IR of a riscv32 program without
  `module asm`, and of a thumbv7m program with only the vector table's
  words; `_start` defined by a program, as function, `extern fn` and
  `extern static`, still `resolve::reserved-name`, with the new help,
  and no other diagnostic.
- **ELF:** on both targets at both levels, `_start` is a function
  symbol with a size (the naked function, not a label of module
  assembly) at the code's origin (riscv32) or at origin plus 8 with bit
  0 set (thumbv7m), with the vector table's words before it; a
  function aligned to 64 in an object given with `--link` does not move
  it.
- **End to end:** every e2e program boots through `core`'s `_start` on
  both targets at both levels; the startup tests of ADR 0030 (`.data`
  values on entry; `.bss` zero again after a jump back to `_start`)
  keep passing; a program whose entry function is in a module boots;
  on riscv32, a program whose entry function lies more than 1 MiB
  after `_start` (a naked function of 1.5 MiB of `.space` before it)
  boots, and the ELF test checks that distance.

## Consequences

- No startup code is hidden in the compiler: `_start` is Erebos source
  in `core`, written with the same `asm` a program can use. ADR 0012's
  provisional state ends.
- The compiler still emits two things of its own: the thumbv7m vector
  table's two words, which M4.3 turns into the table generated from the
  handlers, and the section `_start` goes in.
- A reader of `--emit=llvm-ir` no longer sees `_start`; it is in
  `rt/arch/riscv32.ers` and `rt/arch/armv7m.ers`.
- Programs whose code region is larger than 1 MiB on riscv32 can place
  the entry function anywhere in it.
- A program cannot run code before `.bss` and `.data` are initialized;
  boards that need it wait for the deferred idea.
- A change to `_start` is a change to `core`, built with every program
  and tested on QEMU like the rest of `core`.

## Alternatives considered

- **The entry function under a fixed symbol** (`__erebos_entry`), which
  `core` declares as an `extern fn` — amends ADR 0051, and the debugger
  shows the fixed name instead of the function's own, for nothing
  `sym entry` does not give.
- **`sym entry` only inside `core`** — one more rule and diagnostic,
  while the program can write any symbol into a template anyway.
- **A hook of compiled code the program defines, called before
  initialization** (as `cortex-m-rt`'s Rust `#[pre_init]`) — runs
  compiled code while statics hold garbage; `cortex-m-rt` deprecated it
  in 0.7.6.
- **A hook in assembly, a naked function of the program called before
  the loops** (as `cortex-m-rt`'s `__pre_init`) — avoids that problem,
  but no board of the tests needs it; deferred, not rejected (roadmap).
- **A program-defined `_start` replacing `core`'s** — every program
  doing so repeats the stack, the loops and the jump, and a mistake
  there corrupts every static; no board of the tests needs it.
- **A general `#[section("...")]` attribute** — would need a string
  outside `asm` (ADR 0079) and settle how items name sections and
  regions, which ADR 0057 left open for statics in named regions.
- **A `#[reset]` attribute that only `core` may use** — explicit in the
  source, but an attribute and a diagnostic for one item the compiler
  can name by path.
- **The vector table's words in `_start`'s template**, with
  `.pushsection` — all of the startup in `core`, but M4.3 would move
  them back to the compiler when it generates the table.
- **The loops in Erebos, called from a short `asm`** — one copy of the
  loops for both targets, but compiled code would run before `.data`
  exists, and a pointer walking from `__bss_start` to `__bss_end` leaves
  the one-byte object the linker's symbol is declared as.
- **Keeping `ldr r0, =sym` and `.ltorg` on thumbv7m** — works, but the
  literal pool must be kept inside `.text.start` by hand; `movw`/`movt`
  has no pool.
