# 0078. Concurrency with interrupt handlers

- Status: Accepted; amended by 0093 (a `&raw` evaluated at compile time names no static)
- Date: 2026-11-03

## Context

M4 brings interrupt and exception handlers. From then on, code may stop
between any two instructions while a handler runs and changes memory. A
`static mut` is read and written with ordinary loads and stores
(ADR 0030), which the compiler and LLVM assume nothing else changes
between two accesses of the program: a loop `while !DONE {}` may read
`DONE` once. ADR 0030 left to M4 how a `static mut` is shared with a
handler, and the roadmap asks for the model in an ADR written before M4
starts, so that M4 decides nothing M5 (ownership and borrow checking,
ADR 0009) must break.

What other languages do does not carry over:

- C shares globals with handlers through `volatile` and by disabling
  interrupts by hand, with no check.
- Rust checks sharing with the `Send` and `Sync` traits and closures
  (`interrupt::free(|cs| ...)`), and RTIC computes, in macros, which
  resources each task reaches and locks them by priority ceilings.
  Erebos has no traits, no closures and no destructors (ADR 0002, 0027),
  so neither a guard value nor a function taking the protected code
  exists, and a pair of calls that disable and enable interrupts is left
  unbalanced by any `return`, `break` or `?` between them.

The two targets (ADR 0073) behave alike when nothing changes their
reset state:

- **riscv32** in machine mode, on QEMU `virt` (CLINT and PLIC, no
  CLIC): a trap clears `mstatus.MIE`, and `mret` restores it, so a
  handler is never interrupted by another unless it sets `MIE` itself.
  Synchronous exceptions (an illegal instruction, a misaligned access,
  `ecall`) are taken whatever `MIE` says.
- **ARMv7-M**: every exception of configurable priority resets to
  priority 0, and an exception preempts only code of a higher priority
  number (a lower priority), so handlers of priority 0 never preempt
  each other. `PRIMASK`
  (`cpsid i`) masks all of them. NMI (priority −2) and HardFault (−1)
  are never masked and preempt everything; a fault of configurable
  priority that is masked escalates to HardFault.

## Decision

### Contexts

Every function runs in one or more **contexts**:

- the **thread context**: the entry function and every function it
  reaches through calls;
- the **handler context**: the handlers of maskable interrupts, which
  M4.3 declares in the language, and every function they reach;
- an **unmaskable context** per handler of an event that masking does
  not stop: faults, synchronous exceptions and non-maskable interrupts,
  with every function it reaches. Each such handler is a context of its
  own, since they may preempt each other (NMI preempts HardFault; on
  riscv32 an exception inside a trap handler enters the trap again).
  M4.3's ADR lists, per target, which declared handlers are of this
  kind.

A function reached from several contexts runs in each of them. The calls
the compiler inserts count as calls: every check that may call the
panic function (ADR 0014) reaches it from the context of the check.
Whether the handlers the compiler generates for vectors without one
(M4.3) are contexts, and so whether the panic function they call runs
in an unmaskable context in every program, M4.3's ADR decides, before
M4.5 checks the rule.

**One level.** Until a later step adds priorities, every maskable
handler has the same priority: a handler never preempts another
handler, and preempts the thread context anywhere outside a critical
section. An unmaskable handler preempts any code, critical sections and
handlers included. This is what both targets do from reset (see
Context); a program that changes priorities or re-enables interrupts
inside a handler, through registers or assembly, leaves this model, and
nothing checks it.

### Critical sections

```
let ticks: u32 = critical { TICKS };
critical {
    QUEUE[TAIL] = byte;
    TAIL = (TAIL + 1) % 16;
}
```

`critical BLOCK` is an expression, the only way the language masks
interrupts with a guarantee:

- On entry it records whether maskable interrupts were enabled and
  masks them; on **every** exit it puts back what it recorded: the end of
  the block, `break` or `continue` to a loop outside it, `return`, and
  `?`. Its value and type are the block's. A call of the panic function
  inside it is not an exit: the panic function runs with interrupts as
  the block left them.
- It is a compiler fence at entry and at exit: no access to memory that
  other code can see (statics, memory reached through pointers) moves
  into or out of it. Accesses inside it stay in program order relative
  to each other only as far as ADR 0018 and 0023 already say.
- Blocks nest: an inner block finds interrupts masked and leaves them
  masked. In a handler, interrupts are already masked under the one
  level, and the block changes nothing.
- It does not mask unmaskable events.
- At compile time (ADR 0027) it runs its block: there are no interrupts
  to mask.
- `critical` becomes a keyword with M4.3, which implements the block.
- Enabling interrupts inside a critical section, by assembly or by a
  function that does it, ends its protection; the check below assumes it
  does not happen. M4.3's ADR says how a program enables interrupts.

How it is lowered on each target (saving `mstatus.MIE` or `PRIMASK`) is
M4.3's.

### Shared statics

A `static mut` (or `extern static mut`) is **reached** from a context
when a function running in that context names it: reads it, writes it,
takes its address with `&raw`, or names it in a `sym` operand of `asm`
(ADR 0079), which gives its address to the assembly. It is **shared**
when it is reached from two contexts.

The rule:

1. A shared static reached from the thread context and from the handler
   context must be **protected** at every place the thread context
   reaches it: every path of calls from the entry function to that
   place passes through a `critical` block, around the place itself or
   around one of the calls on the path. A path that recurses through a
   function outside any block is still protected if the call that
   entered the recursion was.
2. A static reached from an unmaskable context and from any other
   context, another unmaskable one included, is an error at each place
   an unmaskable context reaches it: nothing can protect it.
3. Places in the handler context need no protection: under one level,
   no maskable handler runs while another does.

Two accesses that obey the rule never overlap, and the critical
sections' fences keep the compiler from moving them; a program that
obeys it has no data race on a static it names, unless an unmaskable
handler is entered again while it runs (a fault inside the handler of
that fault), which this model leaves out. A racy access is not
defined: the compiler may use a value it read before the handler ran,
or a store may be lost.

**M4.5 checks the rule.** From M4.3 to M4.5 it is not checked, as in C.
The check may reject programs that are correct (a shared static written
only before interrupts are enabled, outside a critical section); a later
step may accept more, never less.

**What the check does not see:**

- Accesses through raw pointers, including a pointer obtained with
  `&raw` from a static (taking it is an access; using it is not). Raw
  pointers are unchecked until ownership and references (M5, ADR 0009).
- Peripherals and their registers (ADR 0058): a read-modify-write of one
  register from two contexts races, and is left to the ownership of
  peripherals in M5.
- Code outside the program: C and assembly reached through `extern fn`
  or `asm`.

**Calls through function pointers.** Erebos has none. The step that adds
them must keep the rule sound: a call through a function pointer reaches
every function whose address the program takes, unless its type says
more, which that step's ADR decides.

### What later steps may add

- **Priorities** (not scheduled): handlers of several priorities and
  nesting, with critical sections that mask up to a ceiling, as RTIC's
  stack resource policy. One level is the case where every handler has
  the same priority and every `critical` masks all of them, so programs
  written now keep their meaning.
- **M5**: ownership of peripherals and other values that must not be
  shared or copied, and references, whose sharing with handlers the M5
  ADRs decide. They add checks; they change no meaning of this ADR.
- **Several cores** need atomics and a memory model of their own
  (roadmap, "Deferred ideas"); a critical section protects nothing from
  another core.

## Consequences

- Sharing a `static mut` with a handler is written with `critical`, and
  from M4.5 checked by the compiler over the call graph, with no traits,
  closures or destructors.
- `critical` is a keyword, so it can no longer name an item, local or
  field.
- A handler is never preempted by another handler in M4: a slow handler
  delays every other one. Priorities are a later step.
- A fault or NMI handler cannot read a static that other code writes,
  another unmaskable handler included. A fault handler that wants to
  print what happened must use statics of its own or raw pointers.
- Peripherals stay unchecked across contexts until M5.
- Spinning on a flag in the thread context reads it inside a critical
  section at each test (`while !critical { DONE } {}`); waiting for an
  interrupt without missing one is M4.3's to show.

## Alternatives considered

- **`static mut` unchecked, as in C, until M5** — the simplest, but the
  compiler would know every handler and every access and still say
  nothing; M4.5 is small with one level.
- **Functions to mask and unmask interrupts** — no structure the check
  can see, and the unmask is skipped by any early exit.
- **Volatile accesses for shared statics** — order the accesses in the
  code but not a read-modify-write: `COUNT += 1` in the thread context
  still loses an increment made by a handler between its load and its
  store.
- **Atomics** — protect single words, not structures such as a queue's
  buffer and indices; riscv32 has them (the A extension), ARMv7-M has
  exclusive loads and stores; left with several cores.
- **Priorities and ceilings in M4** — more of what RTIC does, but both
  targets would need priorities programmed before the entry function
  runs, and the riscv32 machine of the tests has no nesting in
  hardware; one level comes first and extends to them.
- **Checking peripherals too** — catches register races, but most
  registers are read in one context and written in another without
  harm, and the false positives would wait for ownership anyway.
