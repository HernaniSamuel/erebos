# 0084. Interrupt handlers

- Status: Accepted
- Date: 2026-11-05
- Amends: 0052 (the compiler knows `core::arch::Vector` by path), 0079
  (`#[naked]` does not apply to a handler)

## Context

ADR 0078 sets the concurrency model: contexts, one priority level, and a
rule for shared statics. It leaves to this step how a handler is
declared, which handlers are of each kind, and whether fault handlers
return `!`. The roadmap fixes the form: `interrupt fn NAME() at VECTOR`,
with no parameters, that cannot be called.

The two targets (ADR 0073) name their events differently:

- **ARMv7-M** numbers its system exceptions from 1 (reset) to 15. Word
  `n` of the vector table holds the handler of exception `n`. The core
  stacks `r0`–`r3`, `r12`, `lr`, `pc` and `xPSR` on entry, so a handler is
  a plain function of the C ABI, returning with `bx lr` to the
  `EXC_RETURN` value in `lr`. `MemManage`, `BusFault` and `UsageFault` are
  disabled at reset and escalate to HardFault until a program enables
  them in `SHCSR`; a fault that its priority or `PRIMASK` masks also
  escalates, and so does `svc` under `PRIMASK`.
- **RISC-V** in machine mode has one register, `mcause`: bit 31 tells an
  interrupt from an exception, and the rest is the cause. An interrupt and
  an exception may have the same number. `mepc` holds the instruction
  that trapped, also for `ecall` and `ebreak`.

`rt/arch/` is where target-specific items live (ADR 0080), and
`core::PanicReason` shows how `core` names the compiler's cases
(ADR 0052, 0053). A step-by-step probe on QEMU 11.1, written in assembly
while planning, confirmed what this ADR and ADR 0085 and 0086 rely on:
`PRIMASK` is 0 and `CCR.STKALIGN` is 1 at reset on `mps2-an385`;
`mstatus` and `mtvec` are 0 at reset on `virt`; `udf` reaches HardFault
with `HFSR.FORCED` and `IPSR` 3; an all-zero word traps with `mcause` 2.

## Decision

### Declaration

```
use core::arch::Vector;

interrupt fn tick() at Vector::SysTick {
    TICKS += 1;
}

interrupt fn crash() -> ! at Vector::HardFault {
    board::exit(1);
}
```

```
fn_decl = { attr } [ "pub" ] [ "entry" | "panic" | "interrupt" ] "fn" NAME
          "(" params ")" [ "->" type ] [ "at" path ] block
```

- `interrupt` is a keyword, as `entry` and `panic` are. `at` stays a word
  recognized only in its positions (ADR 0058): it follows the signature of
  an `interrupt fn`, where it is required, and nothing else takes it.
  Without it, or on another function, the parser's
  `parser::unexpected-token` reports what it expected.
- **The vector is a path to a variant of `core::arch::Vector`**, in full
  or through `use`, resolved in the module of the handler like any path.
  It is not an expression: `at` takes no constant, and the shorthand of
  ADR 0044 does not apply. A path that names nothing is the code that
  path already has (ADR 0080: a variant of the other target is one of
  these); a path that names anything else is `resolve::not-a-vector`.
- **At most one handler per vector** in a program. Each further handler
  of a vector is one `resolve::duplicate-vector`, labelling it and the
  first, in the order of the functions' ids, which follows the source.
  Handlers may be in any module.
- **Handlers are roots** (ADR 0028), as the entry and the panic function
  are: each is type checked and lowered whether or not anything names it.
- **It takes no parameters** (`ty::interrupt-params`) and returns `()` or
  `!` (see below; anything else is `ty::interrupt-return`).
- **It cannot be called**, from Erebos or at compile time:
  `resolve::call-interrupt`, as `resolve::call-entry`. A `sym` operand
  (ADR 0079) may name it, since that gives only its address.
- **It is a function of the C ABI**, with a symbol by ADR 0051. `#[export]`
  and `#[align(N)]` (ADR 0087) apply. `#[naked]` does not
  (`resolve::misplaced-attribute`): a naked handler would find a different
  state on each target (the core's frame on thumbv7m, the trap entry's on
  riscv32), and no contract says what either holds. Handlers with a frame
  are the step that writes one (roadmap, "Deferred ideas"). The compiler
  keeps every handler in the object.
- **`pub` is accepted**, as on `entry fn` and `panic fn`; since a handler
  cannot be called, it only affects who may name it in `sym`.
- `core` declares none. Programs only.

### `core::arch::Vector`

Each file of `core::arch` declares a public `enum Vector: u32`, in Erebos.
It has the same name in both, and its variants differ, so it is an item
specific to each target (ADR 0080), not a portable one. The compiler names
it by path, the fourth item of `core` it knows (ADR 0052, amended; ADR 0086
adds two more). Its value is what the machine calls the event, so a program
can compare it with a register:

| `rt/arch/armv7m.ers` | Value (exception number, table word) | Kind | Handler returns |
|----------------------|--------------------------------------|------|-----------------|
| `NMI` | 2 | unmaskable | `()` or `!` |
| `HardFault` | 3 | unmaskable | `!` |
| `MemManage` | 4 | unmaskable | `!` |
| `BusFault` | 5 | unmaskable | `!` |
| `UsageFault` | 6 | unmaskable | `!` |
| `SVCall` | 11 | unmaskable | `()` or `!` |
| `DebugMonitor` | 12 | unmaskable | `!` |
| `PendSV` | 14 | maskable | `()` or `!` |
| `SysTick` | 15 | maskable | `()` or `!` |

| `rt/arch/riscv32.ers` | Value (`mcause`) | Kind | Handler returns |
|-----------------------|------------------|------|-----------------|
| `InstructionMisaligned` | 0 | unmaskable | `!` |
| `InstructionFault` | 1 | unmaskable | `!` |
| `IllegalInstruction` | 2 | unmaskable | `!` |
| `Breakpoint` | 3 | unmaskable | `!` |
| `LoadMisaligned` | 4 | unmaskable | `!` |
| `LoadFault` | 5 | unmaskable | `!` |
| `StoreMisaligned` | 6 | unmaskable | `!` |
| `StoreFault` | 7 | unmaskable | `!` |
| `EnvironmentCall` | 11 (from machine mode) | unmaskable | `!` |
| `MachineSoftware` | `0x8000_0003` | maskable | `()` or `!` |
| `MachineTimer` | `0x8000_0007` | maskable | `()` or `!` |
| `MachineExternal` | `0x8000_000B` | maskable | `()` or `!` |

A program runs in machine mode (ADR 0010), so the causes of the lower
privilege modes and of paging do not arise and have no variant. Reset has
none either: `_start` serves it, and a program cannot replace it (ADR 0083).
M4.4 adds the chip's interrupts, in an enum of the chip's, and says how the
compiler knows it; the path after `at` stays a path to a variant.

The compiler holds the same list with what the language needs of each
vector: `src/target.rs` has, per target, the variant's name, its value, its
word in the target's table (ADR 0085), its kind and whether it must diverge.
The compiler looks a vector up by the variant's name. A test checks the enum
of each file against that list, name and value, and each word against the
value: on thumbv7m the word is the value; on riscv32 a value `n` is word `n`,
and a value `0x8000_0000 + n` is word `16 + n`.

### The kinds of vector

These are the lists ADR 0078 asks of this step:

- **Maskable** handlers are the handler context of ADR 0078: PendSV and
  SysTick on thumbv7m, the three machine interrupts on riscv32.
- **Unmaskable** handlers are each a context of their own. That is every
  other vector in the tables. Some can be masked in a sense: under
  `PRIMASK`, MemManage, BusFault, UsageFault and SVCall do not wait, they
  escalate to HardFault. Since masking does not delay them, a critical
  section does not protect anything from what they run, and the rule of
  ADR 0078 treats them as it treats NMI. DebugMonitor is the conservative
  case: an asynchronous debug event may wait under `PRIMASK`, but a `bkpt`
  cannot, and one kind per vector is what the check of M4.5 can read.

Handlers the compiler generates for vectors the program left empty are not
contexts; ADR 0085 says why.

### What a handler returns

A handler returns `()` or `!`. It **must** return `!`
(`ty::interrupt-return`) where returning would run the instruction that
trapped again, with nothing changed, since a handler without parameters
cannot see the frame:

- on thumbv7m, HardFault, MemManage, BusFault and UsageFault, and
  DebugMonitor, since a `bkpt` stacks its own address (a watchpoint would
  not, but the handler cannot tell which it was);
- on riscv32, every exception, `ecall` and `ebreak` included, since `mepc`
  points at the instruction itself. Advancing `mepc` is not offered: the
  trap entry does not save it (ADR 0085), so a returning exception inside
  a handler would lose the handler's own `mepc`.

A later step may allow a return where it gives a handler what it needs to
fix the cause; that accepts more programs, and breaks none.

### Diagnostic codes

| Code | Meaning |
|------|---------|
| `resolve::call-interrupt` | a handler called |
| `resolve::not-a-vector` | the path after `at` names something other than a variant of `core::arch::Vector` |
| `resolve::duplicate-vector` | two handlers at one vector |
| `ty::interrupt-params` | a handler with parameters |
| `ty::interrupt-return` | a handler returning anything other than `()` or `!`, or returning `()` at a vector that must diverge |

`resolve::misplaced-attribute` also covers `#[naked]` on a handler.

### Conformance tests

- **ui snapshots:** the AST of a handler returning `()` and `!`, with the
  full path and through `use`; `at` missing, and `at` on a plain function
  (`parser::unexpected-token`); `extern interrupt fn`; `interrupt` as a
  name; each code above, with three handlers at one vector for
  `resolve::duplicate-vector`, and a handler called from a function and
  from a `const` for `resolve::call-interrupt`; `#[naked]` on a handler; a
  variant of the other target's `Vector`, with `// target:`; a shorthand and
  an integer after `at`; a handler in a module; `pub` on a handler; `sym`
  naming a handler; a handler no code names is still type checked (an error
  in its body is reported). `#[export]` keeping a handler's name in the
  table shows in the snapshots of ADR 0085's tables.
- **Unit test:** each `core::arch::Vector` agrees with the list of
  `src/target.rs`, in names, values and words, on both targets.
- **End to end:** see ADR 0085, whose tables make handlers run.

## Consequences

- A handler is visible in the source with the vector it serves, and the
  compiler checks that the vector exists on the target.
- `interrupt` is a keyword, so it can no longer name an item, a local or a
  field; `erebos svd` renames it (ADR 0071), as every keyword of the lexer.
- `Vector` values can be compared with `mcause` or `IPSR`, but a program
  that names a variant builds for one target. Handlers are specific to a
  machine; a portable timer program does not exist, and the programs of the
  tests say their target in a header (ADR 0077).
- No handler sees the interrupted state. A fault handler can report and
  stop, not repair and resume; the faulting address is available through
  `mtval`, `BFAR` or `MMFAR`, and on riscv32 `mepc`. The language offers no
  way to the stacked `pc` on thumbv7m; `asm` that walks the stack would have
  to know the handler's own frame. Handlers with a frame are a deferred idea
  (roadmap).
- SVCall is a context of its own, so from M4.5 a static shared between an
  SVCall handler and the thread that runs `svc` is an error, although the
  `svc` is synchronous and could not race with it. That is the rule of ADR
  0078 as written; a later step may accept more.
- A handler is one more thing `core::arch` and `src/target.rs` must list
  alike; the test catches a difference.

## Alternatives considered

- **Bare names after `at`, resolved in a table of the compiler** (`at
  SysTick`) — no item in `core`, but a namespace of its own that M4.4 would
  have to combine with the chip's enum.
- **An integer after `at`** (`at 15`) — direct, but it says nothing to a
  reader, and on riscv32 one number is both an exception and an interrupt.
- **Any handler may return `()`** — a fault that returns becomes a silent
  loop.
- **`!` for HardFault only**, as `cortex-m-rt` — no analogue on riscv32,
  and the other three faults still loop.

Defaults assumed while writing this ADR, for correction when accepting it:

- **Every handler of an unmaskable vector returns `!`** — NMI and SVCall
  return normally on hardware that supports it.
- **`#[naked]` handlers** — the way a handler could reach its frame, but
  each target would hand it a different state with no contract for it; left
  with handlers with a frame.
- **DebugMonitor free to return** — useful with a watchpoint, but a `bkpt`
  would loop, and a later step can relax it.
- **`interrupt` recognized only before `fn`** — keeps the name free, but
  `entry` and `panic` are keywords, and the SVD generator already renames
  keywords.
- **Variants for causes that cannot arise in machine mode** (page faults,
  `ecall` from a lower mode) — they would be vectors that never fire,
  each with a default handler to pay for.
