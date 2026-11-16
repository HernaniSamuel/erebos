# 0121. A second panic stops

- Status: Accepted
- Date: 2026-11-16
- Amends: 0014 (the panic function's code begins with a guard the compiler
  inserts), 0085 (a fault inside the panic function no longer re-enters it
  without end on riscv32)

## Context

M5.11 bounds the worst-case stack depth of a program (ADR 0123). ADR 0120
left one path without a bound: a check that fails inside the panic function,
or in a function it calls, calls the panic function again, which may fail the
same check again, without end. Checks are no edge of ADR 0120's graph, so the
rule against recursion does not see this cycle, and it must not: a panic
function that prints its message reads a slice, and the read has a bounds
check (ADR 0120, "The call graph").

Faults take the same path. A vector without a handler names a default handler
that calls the panic function (ADR 0085). On riscv32 a fault inside the panic
function enters the trap entry again, on the same stack, and the panic function
with it; "nothing guards against that recursion" (ADR 0085), and
`tests/e2e/fault_in_panic_riscv32.ers` shows it. On thumbv7m a fault at
HardFault's level locks the core up, but a fault in the default handler of an
enabled `MemManage`, `BusFault` or `UsageFault` escalates to HardFault, whose
default handler calls the panic function again.

Rust stops at a panic while panicking: the runtime aborts. Erebos has no
runtime that could abort, and `core` does not know the board, so it cannot
end the program the way a board can (`board::exit` in the tests).

## Decision

### The guard

The compiler inserts a **guard** at the start of the panic function. Code
generation emits it, before the first statement of the panic function's body:

1. It reads a flag, `__erebos_panicking`, a byte in `.bss` that the compiler
   defines (reserved by ADR 0020's rule that names starting with `__` belong
   to the compiler and `core`). `core`'s `_start` zeroes `.bss` before the
   entry function (ADR 0030, 0083), and every program starts there, so the
   flag is 0 until the first panic.
2. If the flag is set, the panic function spins for ever: a loop of one
   branch, with interrupts left as they are. Nothing else of its body runs.
   LLVM may not remove the loop: Erebos functions carry no `mustprogress`, so
   an infinite loop without side effects is defined, and the guard relies on
   that staying so.
3. Otherwise it sets the flag, and the body runs as written.

The panic function is marked `noinline`, so that one copy of it, with one
guard and its own frame, exists in the object (ADR 0123, which counts that
frame).

The read and the write are volatile, so that a handler that preempts the
panic function sees the flag as set. They are not atomic, and need not be:
a context that preempts between the read and the write and panics never
returns to the one it preempted, since the panic function returns `!`.

The guard is the same for every way the panic function is entered: a check
(ADR 0014, 0053), a default handler (ADR 0085) and an explicit call
(ADR 0053). The checks keep calling the panic function directly; what changes
is the panic function's own code. The guard is code generation's, like the
default handlers: MIR does not show it, and neither the flag nor the guard's
accesses are seen by the analyses of ADR 0093, as `__critical_enter` is not.
`--emit=llvm-ir` shows it, since the flag and the loop are part of the
program.

### What a program sees

- The first panic runs the panic function as before.
- A second panic, from a check, a fault or an explicit call, while the first
  has not ended (and it never ends, ADR 0014), spins inside the panic
  function. A handler that interrupts the spin still runs: interrupts are as
  the second panic left them. On riscv32 a trap clears `MIE`, so a second
  panic reached through a fault spins with interrupts masked; one reached
  through a check in code that runs with interrupts enabled does not.
- A panic function that wants to report the second panic cannot: it never
  learns of it. It may still guard against its own faults as it did before,
  but only before it panics a second time.

### The bound this gives

With the guard, the panic function runs its body at most once. A second
entry costs the panic function's frame, which the prologue sets up before
the guard runs, and nothing more. ADR 0123 counts it so.

### Tests

- **LLVM IR snapshots:** the guard and the flag in a panic function without a
  parameter and in one with `&core::PanicInfo`, on both targets. Every
  snapshot that shows a panic function changes; the update is reviewed for
  that change alone.
- **End to end, on both targets at both levels:** `panic_in_panic`: the thread
  arms the timer (the CLINT's on riscv32, SysTick on thumbv7m) with a handler,
  enables interrupts, and fails a check. The panic function prints `first`
  and fails a check of its own; the second panic spins, the timer's handler
  prints `stopped` and exits with 1. Without the guard, the program would
  print `first` again.
- `tests/e2e/fault_in_panic_riscv32.ers`, whose second pass the guard now
  stops, is rewritten. A second fault would spin with `MIE` clear, which
  nothing can end, and ADR 0088 asks a program that takes exceptions to
  exit. So the first panic still comes from a fault; the panic function
  prints `first`, arms the timer, enables interrupts (`enable_interrupts`,
  ADR 0119) and fails a check. The second panic spins, and the timer's
  handler prints `stopped` and exits with 1. The fault reaches the guard
  through the default handler; the check reaches it as in `panic_in_panic`.

## Consequences

- A check, or a fault, in the panic function or in what it calls can no longer
  re-enter it without end. The stack bound of ADR 0123 has a value for the
  panic function.
- Every program pays one byte of `.bss`, a load, a compare, a store and a loop
  in the panic function.
- A panic function can no longer handle a second panic itself, as
  `fault_in_panic_riscv32.ers` did. No other e2e program does.
- A program whose second panic happens with interrupts masked hangs, instead
  of looping through the trap entry; on hardware that is what a watchdog is
  for. A program that wants another end (a reset, a breakpoint) has none yet:
  a hook for it would be a later ADR.
- ADR 0014's "Operations that panic call it directly; nothing else runs first"
  stays true of the call; the panic function's own first instructions are
  now the guard. ADR 0085's "Nothing guards against that recursion" no longer
  holds: the guard does.

## Alternatives considered

- **Count one re-entry and no more in the bound.** No code changes, but a check
  that keeps failing still overflows the stack the bound claims enough.
- **Reject a panic function that reaches a check.** Sound with no cost at run
  time, but a panic function could not read a slice, and the e2e panic
  functions that print their message would be rejected; ADR 0120 turned the
  same rule down.
- **The guard in `core`, called by each check.** `core` would hold the flag in a
  `static mut`, which the analyses of ADR 0093 would then see written from every
  context; each check would make two calls instead of one; and `core` cannot
  name the panic function, whose signature varies (ADR 0053). Inside the panic
  function, one guard covers checks, faults and explicit calls.
- **Mask interrupts before spinning.** A spin with interrupts masked cannot be
  ended by a handler, so a test could not see it, and a program could not
  use a timer to recover. Leaving them as they are does not weaken the bound:
  a handler that preempts the spin is counted as a handler (ADR 0123).
- **A target-specific stop (`ebreak`, `bkpt`, `udf`).** Each raises an
  exception, which reaches a default handler and the panic function again, on
  top of the stack: the loop the guard is there to end.
