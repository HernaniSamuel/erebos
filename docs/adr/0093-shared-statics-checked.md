# 0093. Shared statics checked

- Status: Accepted
- Date: 2026-11-06
- Amends: 0063 (the tables gain the module `analysis`, which uses `mir` and
  which `driver` uses), 0078 (a `&raw` evaluated at compile time names no static)

## Context

ADR 0078 sets the rule for shared statics and says M4.5 checks it. The roadmap
leaves to this step where the check runs, its diagnostics, and what counts as
naming a static. ADR 0084 lists the kind of each vector, ADR 0085 says that
default handlers are no contexts, and ADR 0089 puts the handlers of the chip's
list in the handler context. The rule is not restated here. ADR 0078 counts a
function that "takes its address with `&raw`" as reaching a static, without
saying when that runs; "What names a static" below makes that the run time.

What MIR holds decides much of this:

- A read, a write and a `&raw` of a static all start from one statement,
  `_7 = const static A`, followed by a dereference of `_7` (`static_place` in
  `src/mir/build.rs`). A value leaving a `critical` block is copied before the
  block's exit (ADR 0086), so a statement inside the block never hands a place
  to one outside it.
- Each statement and terminator has a span, and `Statement::Call` and
  `Terminator::Panic` are the calls MIR shows. `CriticalEnter` and `CriticalExit`
  mark where each block starts and every way out of it.
- A `const` or a `comptime` block is a body of its own, run at compile time; the
  function that uses its value holds the result as read-only data with
  relocations, with no `const static` operand.
- MIR is built once, whatever the optimization level.

A survey of the repository, with this ADR's reading of the rule run over the
MIR dump of each program, finds the existing programs that it rejects. All of them
are correct:

| Program | Rule | Why it is correct today |
|---------|------|-------------------------|
| `tests/e2e/irq_disabled_armv7m.ers`, `irq_handler_in_module_armv7m.ers`, `irq_pended_armv7m.ers`, `irq_unpended_armv7m.ers`, `pendsv_returns.ers` | 1 | reads before the interrupt is enabled or after its one run |
| `tests/e2e/uart_interrupt_armv7m/`, `uart_interrupt_riscv32/` | 1 | `SENT` read after the last interrupt |
| `tests/e2e/nmi_returns.ers`, `svcall_returns.ers` | 2 | a count written by the handler and read by the thread |
| `tests/ui/interrupt_codegen_thumbv7m.ers` | 2 | never run; three handlers write one static |

ADR 0078 accepts rejecting such programs, and ADR 0084 names the SVCall case.

## Decision

### Where it runs

The check is the first **MIR analysis** of ADR 0033's pipeline: a new layer,
`src/analysis/`, with one public function that the driver calls.

```rust
pub fn check(mir: &Mir, diags: &mut Diagnostics)
```

- It reads the MIR of the whole program, `core` included, after `into_mir` and
  before `leave_out`. It names only `mir` and the shared modules (ADR 0063, whose
  tables gain `("analysis", &["mir"])` and `analysis` in the row of `driver`); the
  rule's rendering of diagnostics uses `Diagnostic` as every layer does.
- Every stage from `mir` on sees it: `--emit=mir`, `--emit=linker-script`,
  `--emit=llvm-ir` and `erebos build`. An error stops the run with no dump, as
  errors of MIR building do. `--emit=typeck` and earlier stop before it.
- `into_mir` needs a program without errors, so the check runs only on one.
  A program with a type error reports that first, and these on the next run.
- MIR is the same at every optimization level, so the verdict does not depend on
  `-O`. The check runs after the program is fully lowered, so it does not depend
  on the schedule of ADR 0028.

Compile-time evaluation runs bodies this check has not seen, as ADR 0033 foresaw;
there are no interrupts at compile time, so nothing is lost. Whether the analyses
of M5 must run before evaluation stays M5's question.

### Contexts, as MIR shows them

- The **thread context** starts at `Mir::entry`.
- The **handler context** starts at every handler of a maskable vector.
- Each handler of an unmaskable vector starts a **context of its own**.

`mir::Handler` gains `kind: target::VectorKind`: the kind in the target's list for
a variant of `Vector` (ADR 0084), `Maskable` for a variant of the chip's list
(ADR 0089). Default handlers are not in `Mir::handlers`, and so are no contexts
(ADR 0085). `Mir::reset` starts none: `_start` runs before the entry function,
with interrupts masked, and names no `static mut`. A body with `comptime: true`
is reached by no context, since nothing at run time calls it.

### What is a call

A context reaches a body through, in a block reachable from the start block of
the body that holds it:

- `Statement::Call`, to its callee;
- `Terminator::Panic`, to `Mir::panic` (ADR 0078: a check reaches the panic
  function from the context of the check).

Nothing else is an edge:

- **`sym f`** gives an address to assembly, and assembly is outside what the check
  sees (ADR 0078).
- **An `extern fn`** has no body to follow. A function that only C or assembly
  calls, `#[export]` or not, is in no context, nor is any function nothing calls.
- **The calls code generation or LLVM inserts** (`memcpy`, `__udivdi3`,
  `__aeabi_uldivmod`, `__critical_enter`) are not in MIR. ADR 0078 counts them as
  calls, and they would matter only if they reached the panic function or named a
  static. They do neither: a test checks that every `#[export]` function of `core`
  other than `_start`, and `__critical_enter` and `__critical_exit`, reach no check
  and no `static mut`, following calls and `sym` operands, since
  `__aeabi_uldivmod` reaches its loop through `sym`. A later change that gives one
  of them a check fails that test, and must then add the edge.

### What names a static

A **place** is a statement or terminator that holds, in a block reachable from
the start block of its body:

- an operand `Const::Static(s)`, or
- a `sym` operand `AsmSym::Static(s)`, of an `asm` statement or of the `asm`
  terminator of a naked function,

with `s` declared `static mut` or `extern static mut`. Statements with two such
operands for one static are one place. A `static` and a byte string are not
checked (roadmap: only `static mut` and `extern static mut`). A static of size 0
is a static like any other: nothing can race on it, but the rule does not read
types. A place's span is its statement's or terminator's in MIR.

**Compile time does not name.** `const P: *mut u32 = &raw mut X;`, a block
`comptime { &raw mut X }` inside a function, and the initializer of a `static`
holding `&raw mut X` run at compile time, in no context. What a function gets from
them is a raw pointer, and raw pointers are what ADR 0078 leaves unchecked until
M5. `size_of` and `len` of a static's type name nothing either: MIR has no operand
for them.

**This is the way out** for what the rule cannot express. Nothing can protect a
static from an unmaskable handler, and there is no attribute that silences the
check. A program that knows better writes it visibly, with the pointer made at
compile time; `let p: *mut u32 = &raw mut COUNT;` in a function is a place like
any other:

```
static mut COUNT: u32 = 0;
const RUNS: *mut volatile u32 = &raw mut COUNT as *mut volatile u32;
```

What the check no longer sees, the program answers for. An access through a plain
pointer has neither the fence of `critical` nor the guarantees of `volatile`: `while
*P == 0 {}` may read once, as ADR 0078 describes, which is why the example is
`volatile`. A type `volatile` does not apply to (ADR 0023), such as an array, a struct
or a `u64`, is reached through a plain pointer, with `core::arch::compiler_fence` where the order
matters. Neither makes a read-modify-write or a multi-word access atomic.

### Inside or outside `critical`

Within one body, a forward walk over pairs of block and depth starts at the first
block with depth 0. The depth is tracked per statement: `CriticalEnter` adds one and
`CriticalExit` takes one away, and the terminator has the depth after the block's
last statement, which its successors start with. A place, a call or a check at depth
0 on any path is **outside**; otherwise it is **inside**. A call of the panic function
inside a block stays inside: it is no exit (ADR 0078).

`critical` in a handler context changes nothing for rule 3, and is not read there.

Blocks nothing reaches hold no places. Code after a `return` or a diverging call
has no MIR at all; `if false { ... }` has blocks and counts, since MIR building
does not fold it.

### The rule over the call graph

For each context, a breadth-first search over bodies gives what it **reaches**.
A static is reached from a context when a body the context reaches holds a place
naming it. The walk starts from the handlers in the order of `Mir::handlers`, and
visits a body's calls and checks by block, then by statement, so that the path it
keeps is the shortest and the choice among equals is fixed.

**Rule 1.** For a static reached from the thread context and from the handler
context, a second search from `Mir::entry` follows only the calls and checks that
are outside. A body it finds is **open**. Each place naming the static that is
outside, in an open body, is one error, `analysis::unprotected-static`.

This is ADR 0078's "every path passes through a `critical` block" read from the
other end: a place is unprotected exactly when some path reaches it with no block
around it. A recursion entered inside a block is not open, however it calls itself.

**Rule 2.** For a static reached from an unmaskable context and from any other
context, each place naming it in a body that unmaskable context reaches is one error,
`analysis::unmaskable-static`. A place reached from two unmaskable contexts is
reported once, for the first in the order of `Mir::handlers`. Being inside
`critical` changes nothing. A place can break both rules, and is then reported
under both codes.

**Rule 3** adds nothing to check. Two maskable handlers may share a static freely,
and a static reached from one context only is not shared.

### Diagnostics

| Code | Case |
|------|------|
| `analysis::unprotected-static` | a place in the thread context, outside `critical`, naming a static the handler context also reaches (rule 1) |
| `analysis::unmaskable-static` | a place in an unmaskable context naming a static another context also reaches (rule 2) |

Each error is one place, as ADR 0078 words it. It has:

- the **primary label** on the place;
- a **label for each step of the path** from the context to the place, one per call
  or check, on its span: all of them, since a shorter list hides the call to
  protect, and in files of their own when they cross files (ADR 0049). A path
  through `core` shows `core`'s line, which is the check that is there;
- a **label on one place of the other context**: for rule 1 the first place, by
  `BodyId`, block and statement, in the handler context; for rule 2 the first, in the
  same order, of the thread context, else of the handler context, else of the other
  unmaskable contexts in the order of `Mir::handlers`. It names the handler the search
  found the place's body from, and shows no path to it: the path to fix is the
  primary's. When that place is the same place (a helper both contexts call), the
  primary label says both instead of a second label;
- a **help** saying what to do.

Errors are emitted for rule 1, then for rule 2, each by `BodyId`, block, statement
and the static's id. `Diagnostics::into_vec` sorts them by primary label and keeps
that order on a tie, so a place that breaks both rules shows rule 1 first.

```
error[analysis::unprotected-static]: `COUNT` is reached outside `critical` in the thread context, and a handler reaches it too
   ╭─[main.ers:8:5]
 8 │     COUNT += 1;
   ·     ─────┬────
   ·          ╰── reached here, outside `critical`
 ...
12 │     bump();
   ·     ───┬──
   ·        ╰── called here, outside `critical`
 ...
20 │     COUNT = 0;
   ·     ────┬────
   ·         ╰── the handler `tick` reaches it here
   ╰────
  help: put the access, or one call on the path to it, in a `critical` block

error[analysis::unmaskable-static]: `RUNS` is reached from the unmaskable handler `nmi`, and the thread context reaches it too
   ╭─[main.ers:19:5]
19 │     RUNS += 1;
   ·     ────┬────
   ·         ╰── reached here; `critical` does not stop this handler
 ...
30 │         if RUNS >= 1 {
   ·            ──┬─
   ·              ╰── the thread context reaches it here
   ╰────
  help: give the handler a static of its own, or reach this memory through a pointer from a `const`
```

A check on the path reads `this check may call the panic function, outside
`critical``, and a call under rule 2 reads `called here`. Names are shown as paths:
a body's or a static's symbol with `.` written `::` (ADR 0051), so that an
`#[export]` item shows its own name. The texts are snapshots, and may be reworded
without a new ADR; the codes and what each error labels may not.

### Conformance tests

- **ui, rejected** (`// emit: mir`), riscv32 unless named:
  - rule 1 at each way of naming: read, write, compound assignment, `&raw`, `sym`,
    an `extern static mut`; in the entry function and in a callee;
  - a path of two calls; a path across modules in files of their own, showing each
    file; a function called once inside `critical` and once outside, showing the
    outside call; a recursion entered outside; several places in the handler context,
    labelling the first;
  - inside and outside on different paths: `critical` in one branch only, an access
    after a block's `break`, `return` and `?` have left it, and after a block's end;
  - the panic function naming a static, reached by a check outside `critical`, the
    handler reaching it by a check too; on thumbv7m, a check inside `core::arch` on the
    path (the `assert` of `nvic_address`, through `nvic_enable`);
  - on thumbv7m, a handler at a variant of the chip's list, as for `Vector`;
  - rule 2: NMI and the thread, with the thread's access inside `critical`; a fault
    handler and a maskable one; two unmaskable handlers, both reported; one helper
    called by two unmaskable handlers, reported once; a place that breaks both
    rules; a fault handler with a check, and a panic function naming a static the
    thread names;
  - `tests/e2e/critical_race_riscv32.ers` with `critical` removed.
- **ui, accepted** (`// emit: mir`, so that the dump shows the program passed):
  a block around the place; around a call on the path; nested; a recursion entered
  inside; the panic function's access inside its own block; the forms
  `while !critical { DONE } {}` and the wait of ADR 0086; two maskable handlers
  sharing; a handler's own static, the thread's own, an unmaskable handler's own; a
  `static` and a byte string in every context; a panic function naming a static in a
  program with no handler, whose only other callers are default handlers; a raw pointer
  from a `const`, from `comptime` and from a `static`'s initializer, used
  in every context; a pointer taken inside `critical` and used outside; a function
  nothing calls; a function named only by `sym`; an `#[export]` function nothing in
  the program calls.
- **ui, stage:** a program that breaks the rule compiled to `--emit=typeck` has a dump;
  compiled to `--emit=llvm-ir` at both optimization levels, it reports the same error.
- **Unit test:** the functions of "What is a call" reach no check and no `static mut`,
  on both targets, and the walk finds a check in a function that has one, so
  that it can fail. The body of `Mir::reset` itself names no `static mut`, as
  "Contexts, as MIR shows them" relies on. The walk is its own, not `src/analysis`'s,
  since it follows `sym` and the check does not, and a bug in the check should not hide here.
- **`tests/rejected.rs`:** the two codes are exempt (ADR 0069). A snippet cannot
  break either rule alone: rule 1 needs a place the thread context reaches, and a
  snippet's statements land in any function; a handler in a snippet clashes with a
  base that serves its vector, which hides the code, as for `ty::interrupt-return`.
  The ui tests cover them.
- **End to end:** every program with handlers passes through the check, and those of
  the table above are rewritten to obey the rule, with the same expected output:
  `critical` around each read on the rule 1 list, and on the rule 2 list the
  constant pointer above. `critical_race` and `uart_interrupt` on both targets are the
  programs the roadmap asks for, a helper called in `critical` and in the handler
  among them.

## Consequences

- A program that names a static in the thread context and in a handler can no
  longer race on it unnoticed, and an error shows the path to fix.
- Correct programs pay: a static written before interrupts are enabled, or read after
  they stop, needs `critical` anyway, and costs two or three instructions at `-O`. A
  later step may accept more (ADR 0078).
- The rule's way out, a `const` pointer, is one line, and no check sees through it. `*P
  += 1` in two contexts races unnoticed. That is ADR 0078's position on raw pointers until M5.
- A pointer taken inside `critical` is free outside it. The same. But a pointer taken
  at run time outside `critical`, or in an unmaskable handler, is still an error, so
  the help names the `const`.
- A panic function that names a `static mut` meets the rule like any other function.
  It writes `critical` around the access when a handler also has a check; and when a
  fault handler has a check it cannot be protected, so a fault handler that may
  fail a check reaches only what no other context names. ADR 0085's default handlers
  add nothing.
- Adding a check to one of the functions the compiler inserts, or a `static mut` to
  `core`, is now a change the test catches.
- `analysis` is where the analyses of M5 go, and a row of the tables of ADR 0063.
- `src/analysis` reads spans of `core`, so a snapshot of a path through `core` moves
  when that line of `core` moves. Only one test has such a path.

## Alternatives considered

- **Inside `src/mir/`** — no change to the tables, but the layer that defines MIR
  would also judge it, and M5 would reopen the choice.
- **On HIR, during type checking** — `critical` is already a block there and spans
  are finer, but the checks that reach the panic function are made by MIR building,
  which is what ADR 0078 needs counted.
- **After the MIR dump** — a dump of an invalid program helps to debug the check, but
  the stage would then say nothing of whether the program is valid, and rejected ui
  tests would need the last stage.
- **Following constants' relocations** — a body bound to data that points to `X`
  reaches `X`. This catches `const P`, but `static R: *mut u32 = &raw mut X` read at run
  time stays unseen unless statics are followed too, and so does a pointer stored in
  a `static mut` by anyone. Rule 2 would then leave only fixed addresses as a way out.
- **An attribute that silences the check** (`#[allow(...)]`) — a second way out next
  to raw pointers, and Erebos has no lint levels.
- **A warning instead of an error** — ADR 0078 calls it an error; a racy access is
  not defined.
- **One error per static, listing its places** — less text with many accesses, but the
  roadmap asks for the path, and ADR 0078 says "each place".

Defaults assumed while writing this ADR, for correction when accepting it:

- **One path per place, the shortest** — every path would be exponential in the
  worst case; fixing one and rerunning shows the next.
- **Reading `enable_interrupts` to accept accesses before it** — left out: it needs
  order across calls, ADR 0078 lets a later step add it, and `critical` costs little.
- **`SVCall` treated as the thread when the `svc` is its only source** — left out, as
  ADR 0084 says.
- **Names with `::`, not symbols** — diagnostics write paths everywhere else; a
  `path` field on `Body` and `StaticDef` would be exact for an `#[export]` item in a
  module, and costs two fields for one name in one message.
