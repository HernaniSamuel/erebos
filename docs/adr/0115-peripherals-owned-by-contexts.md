# 0115. Peripherals owned by contexts

- Status: Accepted
- Date: 2026-11-14
- Amends: 0058 (a peripheral is checked across contexts), 0078 ("What the check
  does not see": peripherals are seen), 0093 (the rule's places include those
  naming a peripheral; two codes; the tests of the inserted functions and of
  `_start`), 0094 ("Contexts": how a context owns a peripheral or what a static
  holds), 0110 (a borrow of a peripheral names it)

## Context

ADR 0078 sets the rule for memory shared between the thread context, the
handler context and the unmaskable contexts, and leaves peripherals out: "a
read-modify-write of one register from two contexts races, and is left to the
ownership of peripherals in M5". Its alternatives add that checking peripherals
"catches register races, but most registers are read in one context and written
in another without harm, and the false positives would wait for ownership
anyway". ADR 0093 checks the rule for `static mut` and `extern static mut` only.

ADR 0094 leaves to M5.6 "giving a context ownership of a value, a peripheral's
handle above all", and says that "the handles of a program come from functions
and statics (M5.6)". ADR 0110 lets a program borrow a peripheral (`&mut UART0`),
and says a borrow of one "names no static", leaving to M5.6 "how a context comes
to own a peripheral". ADR 0101 makes a static of a linear type an error, since
the value a static holds is never moved out of (ADR 0094), and lets M5.6 relax
it.

The roadmap asks M5.6 for: handles to peripherals and how a program gets them
once; how a context comes to own a value a static holds; the races on registers;
and what happens to programs that name peripherals directly.

Where the language stands:

- A peripheral is a place named like a static, by any function that can see it
  (ADR 0058). In MIR, every access to one starts from an operand
  `Const::Peripheral(p)`, its address, as an access to a static starts from
  `Const::Static(s)` (ADR 0093). A peripheral has no symbol, so no `sym` operand
  names one.
- Inside one context, borrows of a peripheral are checked: within a body (ADR
  0106, peripherals being roots, ADR 0111) and across calls, a callee that
  touches a peripheral its caller lent `&mut` being
  `analysis::borrow-conflict-in-call` (ADR 0111). Two peripherals whose bytes
  meet (a peripheral `over` another, ADR 0066) conflict there, and the registers
  of one peripheral always overlap one another.
- Across contexts nothing is checked: a handler and the thread may both write
  one register.
- `&raw` of a peripheral in a constant, a static's initializer or a `comptime`
  block is a pointer made from an address (ADR 0058), as for a static, and code
  using it names nothing (ADR 0093).
- The boards of the tests (`tests/boards/`) reach the UART and the test
  finisher through `const` volatile pointers, not peripherals, and `core`
  declares no peripheral: `core::arch`'s NVIC functions use addresses.
- The programs with handlers that name a peripheral are three, all correct
  today:

  | Program | What it shares | Why it is correct |
  |---------|----------------|-------------------|
  | `tests/e2e/uart_interrupt_riscv32/` | `PLIC`: set up by the thread, claimed by the handler; `TEST`: written by `exit`, called from both | the setup runs before interrupts are enabled; `exit` never returns |
  | `tests/e2e/uart_interrupt_armv7m/` | `UART0`: set up by the thread, written by the handler | the setup runs before interrupts are enabled |
  | `tests/e2e/uart_loopback_riscv32.ers` | `UART0`: set up and written by the thread, read by the handler; `PLIC`: set up by the thread, claimed by the handler | each thread access is before the interrupt is enabled or after its last run |

What others do:

- **Rust's `svd2rust` and `cortex-m`**: each peripheral is a zero-sized
  singleton, obtained once from `Peripherals::take()`, which returns `None`
  the second time (a flag checked at run time). A handler reaches one through
  a `static Mutex<RefCell<Option<T>>>`, borrowed inside `interrupt::free`.
- **RTIC**: resources, peripherals among them, are declared per task in a
  macro; a resource of one task is used freely, one shared by tasks of several
  priorities is locked up to the ceiling.
- **C**: anything reaches any register, unchecked.

## Decision

### A context owns what only it reaches

A context **owns** a peripheral, or a `static mut`, when it is the only context
that reaches it, in the sense of ADR 0078 and 0093. Ownership is not declared,
is not a value and is checked by nothing of its own: it is the name for a
peripheral or a static that is not shared, read off the call graph as the rule
of ADR 0078 already reads sharing. The rule below is what does the work: what
one context owns needs nothing written, and what two contexts share obeys the
rule.

- **Handles.** Inside its context, a peripheral is reached by naming it, or
  through a reference to it or to a cluster of it (`&mut UART0`,
  `&mut PLIC.ctx[0]`, ADR 0110), passed down as a parameter or a receiver (ADR
  0113). That reference is the handle to a peripheral. No token type, no
  singleton taken once and no flag at run time is added: the peripheral is the
  one object, its uses inside a context are made exclusive by the borrow check
  (ADR 0106, 0111), and its uses across contexts by the rule below.
- **Getting it once.** There is nothing to get: a program names its
  peripherals, as it names its statics. A value that stands for a configured
  device (the pins of M5.2, the transfer of M5.3) is made by functions of the
  module that defines its type, whose private fields (ADR 0050) keep other
  modules from making one; whether such a value is made only once is the
  module's business, not a check of the language.
- **What a static holds.** A context owns the value a `static mut` holds when
  it is the only context that reaches the static. There is no hand-off of
  ownership: when the thread makes a value and a handler uses it, both reach
  the static, which is then shared, not owned. The thread assigns the whole
  static inside `critical`, as rule 1 asks of any access to a shared static,
  and the handler uses it, borrowing it (ADR 0110) as it needs; no check is
  added for this. Nothing moves out of a static (ADR 0094, unchanged), and a
  static of a linear type stays `ty::linear-static` (ADR 0101, unchanged).
- **Moves across contexts.** ADR 0094 says that moves do not cross contexts
  "in M5.2 to M5.5" and leaves giving a context a value to M5.6. This ADR adds
  no such move: moves still do not cross contexts, and a construct that moves
  a value from one context to another (a `replace` on statics, a parameter of a
  handler) is left to a later step.

### Peripherals in the rule of ADR 0078

The rule of ADR 0078 holds for peripherals as written for statics, with
"peripheral" for "static":

- A peripheral is **reached** from a context when a function running in that
  context names it. In MIR, a place naming a peripheral `p` is a statement or
  terminator that holds an operand `Const::Peripheral(p)`, in a block reachable
  from the start block of its body, as in ADR 0093, "What names a static". That
  covers a read, a write and a compound assignment of a register, of a field of
  one (ADR 0060), of an element of a register array, and of any of them in a
  cluster (ADR 0064); `&P`, `&mut P` and the borrows of clusters (ADR 0110);
  `&raw` of a peripheral or of a cluster; and a method called on a peripheral
  or a cluster (ADR 0114), whose receiver is such a borrow.
- **A use of a reference or of a pointer names nothing**, as for statics (ADR
  0110): `(*u).thr = b` and `self.thr = b` through a parameter are no places.
  The place is where the reference or the pointer was made.
- **Compile time does not name**, as in ADR 0093: `&raw mut UART0` in a
  `const`, a static's initializer or a `comptime` block is an address, and the
  code using it reaches the device unchecked. So does a pointer cast from an
  integer, `0x1000_0000 as *mut Uart16550`, at run time or not. These are the
  way out, as for statics.
- **Peripherals whose bytes meet are one device.** A peripheral `p` is
  **shared** with a context `C` when `C` reaches `p`, or reaches a peripheral
  whose bytes meet `p`'s: the same bytes reached under two names are the same
  registers. Two peripherals meet when their ranges `[address, address +
  extent)` (ADR 0058) share a byte, the words of ADR 0058 and 0111. A
  peripheral of extent 0 has no byte and meets no other peripheral, wherever
  it lies; it is still shared with a context that reaches it by its own name.
  The borrow check of ADR 0111 compares ranges with a test that counts an empty
  range strictly inside another as meeting it, against ADR 0111's words; M5.6
  corrects it, so that both analyses use one comparison (plan, step 2). The
  relation is not closed transitively: with `B` over the first half of a device
  `A` and `C` over its second half, `B` and `C` do not meet, and a context
  reaching `B` and another reaching `C` share nothing unless one of them also
  reaches `A`.
- **The whole peripheral is the unit.** Registers and clusters of one
  peripheral are not told apart: a context that reaches one register of
  `UART0` reaches `UART0`. A static is checked whole in the same way (ADR 0093).

With these words, the rules of ADR 0078 and ADR 0093 apply unchanged:

1. A place in the thread context, outside `critical` (ADR 0093, "Inside or
   outside `critical`"), naming a peripheral shared with the handler context, is
   an error, `analysis::unprotected-peripheral`.
2. A place in an unmaskable context naming a peripheral shared with any other
   context, another unmaskable one included, is an error,
   `analysis::unmaskable-peripheral`, `critical` or not.
3. Places in the handler context need nothing.

The search over contexts, the paths, the calls that are edges (calls and the
checks that may call the panic function) and what is inside or outside
`critical` are those of ADR 0093. A place that names a static and a peripheral,
or two peripherals, is one place for each.

### Diagnostics

| Code | Case |
|------|------|
| `analysis::unprotected-peripheral` | a place in the thread context, outside `critical`, naming a peripheral shared with the handler context (rule 1) |
| `analysis::unmaskable-peripheral` | a place in an unmaskable context naming a peripheral shared with another context (rule 2) |

Each error has what an error of ADR 0093 has: the primary label on the place,
a label for each step of the path, a label on one place of the other context,
and a help.

- The **title** names the peripheral of the primary place. When the other
  context reaches only peripherals meeting it, the title says so: "`UART0` is
  reached outside `critical` in the thread context, and a handler reaches
  `UART0_DLAB`, whose bytes meet it".
- The **other place** is chosen as ADR 0093 chooses it, by `BodyId`, block and
  statement, among the places naming the peripheral or one meeting it; within
  one statement naming several of them, the peripheral first in
  `Mir::peripherals` is the one labelled. When it is another peripheral, its
  label says so: "the handler `tick` reaches `UART0_DLAB` here, whose bytes
  meet `UART0`'s".

```
error[analysis::unprotected-peripheral]: `UART0` is reached outside `critical` in the thread context, and a handler reaches it too
   ╭─[main.ers:30:5]
30 │     UART0.ier.thri = true;
   ·     ───────┬──────
   ·            ╰── reached here, outside `critical`
 ...
12 │     UART0.thr = byte;
   ·     ────┬────
   ·         ╰── the handler `external` reaches it here
   ╰────
  help: put the access, or one call on the path to it, in a `critical` block, or leave the device to one context
```

The help of rule 2 reads "leave the device to this handler, or reach it
through a pointer from a `const`". Errors are emitted after those of ADR 0093
for the same rule, by `BodyId`, block, statement and the peripheral's index in
`Mir::peripherals`; `Diagnostics::into_vec` then orders them by primary label,
as for statics. Names are shown as paths, as in ADR 0093. The texts are
snapshots and may be reworded without a new ADR; the codes and what each error
labels may not.

The codes are new rather than those of ADR 0093 because the message names a
device and the help differs; a reader of a code knows which kind of place it
is.

### Programs that name peripherals directly

They keep their meaning. A program whose every peripheral is reached by one
context compiles as before; a program whose handler and thread share one is
checked, and the thread's accesses go inside `critical`, as ADR 0093 asked of
statics. The three programs of the table above are rewritten so:
`critical` around the thread's setup of `PLIC` and `UART0` and around the
thread's accesses of `uart_loopback_riscv32.ers`; and in
`uart_interrupt_riscv32/`, the program's own `exit`, which writes `TEST` from
both contexts, gives way to the board's, which writes the finisher through a
`const` pointer and so names no peripheral, as in the other programs of the
tests (a `critical` around each of the thread's calls of `exit` would do too).
Their output does not change.

### What the check still does not see

- A peripheral reached through a pointer made at compile time or from an
  integer, as above.
- Code outside the program: C and assembly (ADR 0078).
- A configured-device value made twice inside one context: it is no check of
  the language (above).

### Priorities and several cores

As in ADR 0094: with priorities, `critical` masks up to a ceiling and the rule
holds per ceiling; with several cores, a critical section protects nothing from
another core, and a peripheral shared between cores needs what that step
brings. Ownership read off the call graph extends to both: a context of each
priority or core is one more root.

### Conformance tests

- **ui, rejected** (`// emit: mir`), riscv32 unless named:
  - rule 1 at each way of naming: a register read, written and compound
    assigned; a field of a register; an element of a register array; a register
    of a cluster and of an element of a cluster array; `&P`, `&mut P`, `&mut
    P.c`; a method called on `P`; `&raw` of `P` and of a cluster; in the entry
    function and in a callee, with its path;
  - a peripheral `over` another (ADR 0066), the thread naming one and a handler
    the other, the title and the label naming the other; the same with the
    roles swapped;
  - a device `A` with `B` over its first half and `C` over its second half,
    the thread naming `A` and `B`, a handler naming `C`: the thread's places
    of `A` are errors, its places of `B` are not;
  - rule 2: a fault handler and the thread, the thread's access inside
    `critical`; on thumbv7m, NMI and the thread; two unmaskable handlers, both
    reported; a panic function writing a peripheral the thread writes, with a
    fault handler that has a check;
  - one statement naming a shared static and a shared peripheral, reported
    under each code;
  - the program on QEMU of M5.6 (`tests/e2e/uart_owned_*`) with the thread
    writing the UART outside `critical`.
- **ui, accepted** (`// emit: mir`): the thread's accesses inside `critical`,
  directly and through a call; a peripheral reached by the thread only, by the
  handler context only, by one unmaskable handler only; two maskable handlers
  sharing one; a reference to a peripheral taken inside `critical` and lent to
  a function that names none; a pointer from a `const` and one cast from an
  integer, used in every context; two peripherals that do not meet, declared
  apart, one per context; `B` over the first half of a device `A` and `C`
  over its second half, `B` in the thread and `C` in a handler, `A` named by
  neither; a peripheral of extent 0 inside another's range, one per context.
- **ui, stage:** a program that breaks rule 1 for a peripheral compiled to
  `--emit=typeck` has a dump; to `--emit=llvm-ir` at both levels, it reports
  the same error.
- **Unit tests:** the functions the compiler inserts and `_start` reach no
  peripheral, and the walk finds a peripheral in a function that names one, so
  that the test can fail (ADR 0093's tests, extended).
- **`tests/rejected.rs`:** the two codes are exempt, for the reason ADR 0093
  gives for its own: a snippet cannot break either rule alone.
- **End to end:** the three programs of the table, rewritten, with the same
  output; and the program on QEMU of M5.6, a UART owned by its handler.

## Consequences

- A register written by the thread and by a handler, with no `critical`, is an
  error with the path to fix, as for a static. A device that one context alone
  uses needs nothing written: the UART of the program below is owned by the
  handler context because only handlers name it.
- Correct programs pay, as ADR 0078 foresaw: a device set up by the thread
  before interrupts are enabled, or a status register read by the thread while
  a handler writes another register, needs `critical` anyway. A later step may
  tell registers and clusters apart, as the borrow check may (ADR 0111); either
  accepts more programs and rejects none.
- A panic function that writes a peripheral meets the rule like any function:
  if a handler has a check, the thread's other accesses to that device go inside
  `critical`, and if a fault handler has one, the device is reached through a
  `const` pointer, as the boards of the tests do.
- There is no token for a peripheral, so the language does not stop two values
  standing for one configured device from being made in one context; modules
  stop it with private fields. Typestate over the device's registers (M5.2's
  pins) stays a convention of the module.
- A context cannot take a linear value out of a static: a value two contexts
  use is shared through a static, assigned in `critical` by the thread and
  borrowed by the handler, with no transfer of ownership. A `replace` that moves the old value out, which would let
  statics hold linear values, is not scheduled; nothing needs it yet, and it
  could be added without breaking programs. A driver that starts a linear
  transfer in the thread and finishes it in a handler (M5.3's `Transfer`, a
  DMA driver) cannot be written with a linear value until then: the roadmap
  records it.
- `src/analysis/shared_statics.rs` handles both kinds of place; no new analysis
  and no change to MIR.
- The test of ADR 0093 that the functions the compiler inserts and `_start`
  reach no `static mut` also checks that they reach no peripheral, so that a
  peripheral declared in `core` later is noticed.

## Alternatives considered

- **Tokens taken once, as `svd2rust`** — a value per peripheral, obtained once,
  makes a driver built from it unique, which typestate wants. But peripherals
  would stop being places, so every program that names one (124 files of the
  tests) changes; "once" is a flag at run time or a parameter of the entry
  function; and a handler, which takes no parameters (ADR 0084), gets its token
  only through a static that values are moved out of, a construct that does not
  exist.
- **Owners declared, as RTIC** (`interrupt fn f() at V owns UART0`) — the owner
  is written where the reader sees it, and a use in another context is an error
  at that use. But it is new syntax for what the call graph already tells, the
  peripherals with no declared owner still need the rule above, and with one
  level of priority every maskable handler is one context anyway.
- **Registers and clusters as the unit** — fewer false positives (the PLIC set
  up by the thread and claimed by a handler, a status register read by the
  thread), but a borrow of the peripheral covers every register, the
  diagnostics must name parts, and the borrow check compares whole peripherals
  too (ADR 0111). It can be added later without breaking programs.
- **The codes of ADR 0093** — one code per rule, but the message and the help
  would have to cover both kinds, and a reader of the code would not know which
  it is.
- **A `replace` built-in on statics** — moves the old value out and puts a new
  one in, letting a handler consume a linear handle the thread left in a
  static, and letting ADR 0101 allow linear statics. No program needs it yet.
- **Peripherals still unchecked across contexts** — no program changes, but the
  compiler knows every handler and every access to a device and would still say
  nothing of a race on a register.
