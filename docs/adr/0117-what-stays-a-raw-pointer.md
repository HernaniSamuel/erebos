# 0117. What stays a raw pointer

- Status: Accepted
- Date: 2026-11-14
- Amends: 0077 (every board offers `print`, and programs print byte strings
  through it)

## Context

`core` and the test programs were written before references existed, so
every address they pass is a raw pointer: a parameter that lends a value
(`fn push(ring: *mut Ring, byte: u8)`), a slice (`fn print(s: *const [u8])`,
called as `print(&raw const b"...")`), a result stored through a pointer
(`__erebos_udivmoddi4(n, d, rem: *mut u64)`). M5.4a to M5.5 added references,
slices behind references, references to statics and peripherals, and methods
(ADR 0105, 0108, 0110, 0114), and left `core` and the tests on raw pointers
for M5.7. ADR 0116 moves `assert` messages and the panic function's
parameter to references.

The roadmap gives M5.7 "`core` and the tests moved to references where they
can; what stays raw, measured", and its pending decisions set the
expectations: parameters by address and slices move to references; what
stays raw is `extern`, `asm` operands, fixed addresses outside peripherals,
reinterpreting memory and the inside of allocators; "if the safe form needs a
written lifetime or region, leave it raw". Three later decisions wait for the
measurement:

- whether to mark the dereference of a raw pointer, since the language has no
  `unsafe` (ADR 0030): "waits until it is clear what remains";
- returning a reference tied to the one reference parameter (deferred ideas):
  "if most of them stay raw because the safe form returns or stores a
  reference, open an ADR for this";
- a static placed in a named region, for DMA buffers, and handles into static
  arrays with range-typed indexes, for linked data (deferred ideas), which
  would each replace some raw pointers.

References are second-class (ADR 0094): parameters and `let` without `mut`,
never in fields, returns, statics, arrays or `Option`. An `asm` operand may
not be a reference (`ty::asm-operand-type`). References are not compared
(ADR 0105). `extern fn` and `#[export]` signatures may take references,
passed as pointers (ADR 0105, 0108), so `extern` alone, which the roadmap
lists, is no longer a reason to stay raw: what keeps a raw pointer at the C
ABI is a C signature no reference states (reason 3 below) or a test of the
ABI's pointers (reason 1). The roadmap's "inside of allocators" has no case:
there is no heap and no allocator in `core` or the tests (ADR 0002).

At the start of M5.7, 109 of the 175 source files under `tests/e2e/` and
`tests/boards/`, and three of the four files of `rt/`, write a raw pointer.
Many of those lines are the same few things repeated: 46 definitions of
`fn print(s: *const [u8])` and 8 of `puts`, about 31 of them the same loop
writing each byte through the board's `putc`, `&raw const b"..."` at their
calls, and 20 panic functions taking `*const core::PanicInfo`.

## Decision

### The rule

In `rt/`, `tests/boards/` and `tests/e2e/`, a raw pointer is written only
where one of the reasons below applies. Everywhere else the address is a
reference: a parameter that lends a value or a slice is `&T`, `&mut T`,
`&[T]` or `&mut [T]`; a `&raw const` or `&raw mut` that only lends a place to
a call is `&` or `&mut`; a thin pointer and a length passed together are one
`&[T]` or `&mut [T]`. A free function stays a free function: turning programs
into methods is not this rule's concern, and M5.5's programs already test
methods.

The rule holds for new code in those directories too. No test enforces it:
it is checked when a change is reviewed, against this ADR. The ui tests,
`tests/layout/`, the snippets of `tests/rejected.rs` and the generated
programs of ADR 0068 are not under it: they test the types they write.

### The reasons a raw pointer stays

Each raw pointer that stays has one reason, the first of this list that
applies:

1. **The test's subject.** A program that exists to test raw pointers keeps
   the ones it tests: indexing through a pointer (`p[i]`), `&raw`, volatile
   pointers, `slice`, casts between pointers and integers, pointers across
   the C ABI (`aapcs_c.ers` and the other `*_c.ers` programs, whose C side
   writes `T *`), the borrow check not following raw pointers. What such a
   program uses only incidentally (its printing, its panic function, a
   helper taking a buffer) moves to references.
2. **The safe form is rejected by an analysis.** The reference was written
   and an analysis rejected the program: the borrow check
   (`analysis::borrow-conflict`, `analysis::borrow-conflict-in-call`), the
   check of moves, the check of linearity, or the rule of shared statics and
   peripherals (`analysis::unprotected-static`,
   `analysis::unprotected-peripheral` and their unmaskable forms), and no
   rewriting that keeps what the program tests is accepted. Each case is recorded
   with the code that rejected it: these are what the language lacks, and
   the measurement exists to find them.
3. **A signature fixed by another caller.** A function called by LLVM or by
   C code the program does not write, with a signature that no reference
   type states: `memcpy`, `memmove`, `memset` and the `__aeabi_` memory
   functions of `core` take a thin pointer and a separate length, may be
   given overlapping areas (`memmove`), and compare addresses. A function
   called from assembly takes a reference when the assembly passes what a
   reference needs (a valid, aligned, unaliased address), as the `__aeabi_`
   64-bit divisions pass a slot of their own frame to
   `__erebos_udivmoddi4`.
4. **An `asm` operand.** `ty::asm-operand-type` refuses references; the
   address passed to `asm`, as the semihosting block of the thumbv7m board's
   `exit`, stays a raw pointer.
5. **A fixed address outside a peripheral.** An address that is a number,
   not a place the program declares: the boards' UART and test finisher, the
   NVIC's words that `core::arch` computes from a line's number,
   `board::SCRATCH`. Declaring them as peripherals would put them under the
   rule of ADR 0115 as whole devices: the boards' console is written from
   every context, and the NVIC's registers are write-1 so that no context
   races another, which the rule cannot tell. Whether they become
   peripherals is a question for a later change of that rule, not for M5.7.
6. **Memory a device reads, at an address the program computes.** A DMA
   buffer or a descriptor ring placed at a computed address; the deferred
   idea of a static placed in a named region would remove these.
7. **Linked data by address.** A structure whose elements point to each
   other; the deferred idea of handles into static arrays with range-typed
   indexes would remove these.
8. **A reference returned or stored.** An address kept in a field (the
   `file` and `message` of `core::PanicInfo`, ADR 0116), an array or a slice
   of slices, an `Option`, a static, or returned by a function (a slice into
   an argument), or two halves of one `&mut [T]` in use together (ADR 0108).
   ADR 0094 leaves all of these out of M5.
9. **Reinterpreting memory.** Reading a value as bytes or as another type,
   or an address as a number to do arithmetic on it.

### `board::print`

Every board module (ADR 0077) also offers

```
pub fn print(s: &[u8])          // the bytes of `s` to the UART, through `putc`
```

A program's printing function is removed, and its calls go to
`board::print`, when its body is exactly the loop that writes each byte of
its argument, in order, through the board's `putc` (called directly, or
through a local `putc` that does nothing but call it), with no other effect.
Every other printing function stays in its program, with a reference
parameter where the rule asks for one: one that appends a newline, writes a
device directly (`*UART = ...`) or a device of the program's own (a
`devices::putc`, a `CONSOLE` peripheral), counts what it writes, or takes a
thin pointer as part of what the program tests (reason 1). A `pub fn print`
of a submodule of a program, which calls `crate::board::putc`, follows the
same test. The functions that print numbers stay in the programs: they take
no pointer, and moving them is not this step's concern. ADR 0077's "its
printing helpers stay in the program" changes for this one function.

### What the changes may move

Removing definitions and rewriting signatures moves the lines of the
programs, and with them the panic locations (`file:line:column`) in the
expectations of the programs that print one, as ADR 0077 accepted when the
boards were introduced; nothing else in an expectation changes.

### The measurement

Two numbers are taken, at the start of M5.7 and at its end:

- **Lines:** each line of a `.ers` file under `rt/`, `tests/boards/` and
  `tests/e2e/` (the generated modules of `erebos svd` included) that, with
  its `//` comment removed, writes `*const`, `*mut` or `&raw`. The command
  that counts them is recorded with the counts, so that both counts are
  taken the same way.
- **Sites:** each declaration whose type is or holds a raw pointer (a
  parameter, a return type, a field, a static, a `const`, a local) and each
  `&raw` expression not stored in one of them. One site has one reason; its
  uses are counted with it.

Each line and each site that stays is given one reason of the list above.
The counts, by reason and by directory, are recorded in the plan of M5.7 and
in the roadmap's status, not here: they change as the tests grow.

At the end of M5.7:

- **Returning a reference.** The candidates are the sites where a reference
  was what the program wanted: reasons 2, 6, 7 and 8. Of these, the sites of
  reason 8 are counted apart from the two fields of `core::PanicInfo`, which
  could be nothing else. With `share` the sites of reason 8 (without those
  fields) over the candidate sites:
  - more than 60 percent: the deferred idea of returning a reference tied to
    the one reference parameter becomes a pending decision of the roadmap
    with the counts, to be opened as its own `Proposed` ADR; it is not built
    in M5.7;
  - from 40 to 60 percent, or fewer than 5 candidate sites in all (none
    included): the roadmap records the counts and the user decides;
  - less than 40 percent: the roadmap records the counts in the deferred
    idea, which stays not scheduled.
  The cases of reason 2 are listed with the counts in every case, since some
  may want the same feature.
- **Marking the dereference of a raw pointer** (ADR 0030) moves, whatever the
  counts, from M5 to the pending decisions after M5, with them: the question
  is whether what remains is rare and local enough to be marked.
- **The two deferred ideas** of reasons 6 and 7 get their counts in their
  entries of the roadmap.

## Consequences

- `core` and the test programs read as programs written for the language as
  it is: no `&raw const b"..."`, no `(*s)[i]` on a slice that a reference
  could hold, no pointer parameter where a reference states what it lends.
- The borrow check, the checks of moves and the rule of shared statics see
  what the tests lend, where they followed no raw pointer before; a test that
  shared a buffer in a way they reject is rewritten, or kept raw under reason
  2 with the code that rejected it, in its comments and in the plan.
- The tests of raw pointers stay, under reason 1; the coverage of raw pointers
  does not shrink.
- Every e2e program changes, most by the same few lines; the expected outputs
  change only in the panic locations of the programs that print one.
- The rule is enforced by review only; a later raw pointer that falls under no
  reason is a review finding, not a test failure.
- The three decisions that waited for the measurement get numbers.

## Alternatives considered

- **A ratchet test** counting raw pointers in `rt/` and `tests/boards/`, as
  `tests/architecture.rs` counts panics — `rt/` and the boards would keep
  fewer than ten such lines, all under reasons 3 to 5, which change only with
  `core`'s or the boards' design; review covers them, and the e2e programs,
  where most pointers are, cannot be counted against a fixed number since
  testing raw pointers takes raw pointers.
- **Migrating the ui tests too** — they test diagnostics, and many test them
  on raw pointers on purpose; moving the incidental ones changes hundreds of
  snapshots for spans alone, and tells nothing about what the language lacks.
- **Making the boards' UART and the NVIC peripherals** — they would fall under
  the rule of ADR 0115 as whole devices, and every program printing from both
  contexts, or enabling a line from the thread, would need `critical` where
  nothing races. Left to a change of that rule (registers as separate units,
  already a pending decision).
- **Keeping a `print` in every program** — about 31 copies of the same loop,
  each changed the same way; one in the board is the change made once.
- **Counting lines only** — most lines are a few patterns repeated, and one
  pointer parameter is used on many lines; the decision on returning a
  reference is taken on sites, where each case counts once.
- **Building the named-region statics or the handles in M5.7** — each is a
  feature with its own ADR, larger than this step; the measurement says how
  much each would remove, which is what scheduling them needs.
- **Deciding now whether to mark the dereference of raw pointers** — the
  measurement is what the roadmap waited for; the decision is the user's, with
  the numbers, after M5.
