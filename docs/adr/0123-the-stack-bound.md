# 0123. The stack bound

- Status: Accepted
- Date: 2026-11-16
- Amends: 0003, 0028 and 0093 (one analysis reads, besides MIR, the frame
  sizes that code generation measured), 0057 (the declared stack size is
  checked against the program's worst case), 0082 (the diagnostic handler
  keeps the warnings that give frame sizes)

## Context

A program declares its stack's size in `memory` (ADR 0057), and has no other
memory that grows: no heap, no OS. A program that runs past that size writes
below the stack's region, and ADR 0057 says so: "a program that needs more
stack than it declares still fails at run time". Since ADR 0120 nothing in run-time
code recurses, so the call graph has no cycle that the compiler sees, and a
worst-case depth exists. This is the argument of the JPL's "Power of Ten"
(G. J. Holzmann, IEEE Computer 39(6), 2006), whose first rule forbids
recursion so that "an upper bound on the use of stack memory can be derived
statically". MISRA C:2012 forbids recursion (Rule 17.2) for the same reason,
and ISO 26262-6 lists "no recursions" among the design principles of software
units (read through secondary sources; the table's number differs between
editions, so none is cited).

The depth cannot be read from MIR:

- A frame's size exists only after register allocation, and differs between
  `-O0` and `-O2`. LLVM measures it in prologue/epilogue insertion. The
  `.stack_sizes` section that reports it is a `TargetOptions` flag that neither
  `inkwell` nor LLVM's C interface reaches. The same pass also reports each
  frame through the context's diagnostic handler when a function carries the
  attribute `"warn-stack-size"`: tried with LLVM 22's `llc` on both targets at
  both levels, `"warn-stack-size"="0"` gives one warning per function with a
  frame, `stack frame size (48) exceeds limit (0) in function 'f'`, whose
  numbers are those of `.stack_sizes`. The compiler installs that handler
  already (ADR 0082).
- Some calls exist only after code generation: `memcpy`, `memmove` and `memset`
  for aggregates (and LLVM's own at `-O2`), the 64-bit division of ADR 0076,
  and on thumbv7m the `__aeabi_` memory functions. The same test showed a
  function copying 200 bytes calling `memcpy`, which no MIR shows. ADR 0093
  lists these: the `#[export]` functions of `core`, which LLVM calls by name.
- Assembly calls through `sym` (ADR 0079): `_start` jumps to the entry function,
  thumbv7m's `__aeabi_uldivmod` calls `core`'s division.
- Interrupts and faults push onto the same stack (ADR 0085): 64 bytes of the
  trap entry on riscv32, 32 bytes stacked by the core on thumbv7m.

ADR 0121 makes the panic function run its body at most once, and ADR 0122 lets
the program say what the compiler cannot measure.

## Decision

### Where it runs

The **stack bound** is computed after code generation and before the link, on
every `erebos build`, at the optimization level being built: the frames are
those of the code that is flashed. Nothing turns it off. A new stage,
`--emit=stack`, prints its report (below).

- It is an analysis, `src/analysis/stack.rs`, with one public function that the
  driver calls. It reads the whole program's MIR, before `leave_out`, as the
  other analyses of the whole program (ADR 0093), and the **frames**: one
  number per body, and the largest frame of the default handlers (ADR 0085),
  which code generation makes and MIR does not show. The frames are plain
  numbers; the analysis names no type of `codegen`, so the tables of
  `tests/architecture.rs` do not change. This amends the rule of ADR 0003, 0028
  and 0093 that analyses read only MIR: this one reads what code generation
  measured, which the driver passes it.
- `--emit=llvm-ir` and the stages before it do not run it, nor do they report
  the errors below.

### Frames

For an object (`erebos build`, `--emit=stack`), code generation sets
`"warn-stack-size"="0"` on every function it defines, after the IR is complete;
`--emit=llvm-ir` does not show the attribute. The diagnostic handler (ADR 0082)
keeps the warnings whose text is `stack frame size (N) exceeds limit (0) in
function 'f'`, and drops the other warnings as before.

- A function is matched to its body by the name of the LLVM function the body
  declared, and to the default handlers by theirs. A reported name that is
  neither is a function LLVM made (a specialized copy, an outlined sequence);
  the bound cannot place it, and code generation reports `codegen::llvm`: "LLVM
  made the function `x`, which the stack bound does not know". This is a
  deliberate choice: the bound refuses rather than guesses. Attributing
  `x.suffix` to `x`, as LLVM names its copies, is not sound here, since an
  Erebos symbol holds dots of its own (`uart.init`, ADR 0051), so `uart.init`
  could be a copy of `uart` or a function of its own; and the calls into a
  copy would still be missing from the graph. LLVM makes such copies with
  passes that `-O2` runs only on some inputs (function specialization, partial
  inlining, the machine outliner on functions marked `minsize`, which Erebos
  does not set), so a valid program may one day fail to build with this
  error. It then names the copy, and the fix (an attribute that stops the
  pass, or a mapping that is sound) is a later ADR. The e2e and generated
  programs, at both levels, make none today.
- A body with no warning has a frame of 0: it has none, or LLVM inlined it
  everywhere and removed it. In the second case its callers' frames hold what
  its frame held, and its callees stay callees of the body (below), so nothing
  is lost.
- A body's **cost** is its frame plus the `N` of its `#[stack(N)]` (ADR 0122);
  an `extern fn`'s cost is its `N`.

### The graph

The bound's graph has a node for every body that is not `comptime`. A body's
edges come from the blocks reachable from its start block:

- a `Statement::Call`, to its callee;
- a `Statement::CriticalEnter` or `CriticalExit`, to `core`'s
  `__critical_enter` or `__critical_exit` (ADR 0086);
- a `Terminator::Panic`, to the panic function (as ADR 0093 counts it, unlike
  ADR 0120);
- a `sym` operand that names a body (`AsmSym::Body`), in an `asm` statement or
  a naked function's `asm` terminator: assembly may call what it names, and
  the bound does not read templates;
- the **inserted calls**: to each `#[export]` function of `core` other than
  `_start` (the **inserted functions**), from every body that is not naked, not
  external, and not in the **inserted closure**. MIR lists the inserted
  functions in a new field, `Mir::inserted`, which ADR 0093's test of those
  functions then reads too. They are small, and the bound assumes any other
  function may call any of them.

The **inserted closure** is the inserted functions and every body they reach
through the edges of the first four kinds **without** `Terminator::Panic`
(calls, `critical`, `sym`). It is `core` code that the compiler marks
`no-builtins` (ADR 0020), written to need no inserted call: `__udivdi3`
reaches `core`'s private division loop, which inserted edges would turn into a
cycle. The exclusion is sound only while the closure holds nothing but such
code, so the bound **checks** it: a body of the closure that is not `core`'s,
or that is the panic function, is an internal error of the compiler (a
panic of the bound, which `tests/architecture.rs` counts), since only a change
to `core` can cause it. ADR 0093's test already checks that the inserted
functions reach no check, so the closure does not reach the panic function
through a check either. The oracle of the machine's calls (below) checks
that the closure's code calls nothing the graph does not show. A body that
`main` and the closure both reach (`core::memcpy`, which programs may call)
is in the closure and has no inserted edges, which that oracle covers too.

A method call is a call (ADR 0114); an `extern fn` has no edges.

### Cycles

`analysis::stack-cycle` is looked for on the graph **without** the
`Terminator::Panic` edges: calls, `critical`, `sym` and inserted calls
(`src/analysis/call_graph.rs`'s `Edges::Calls`, extended with the last two
kinds). ADR 0120 has already rejected every cycle of calls alone, so a cycle
found here goes through a `sym` operand or an inserted call. The edges into
the panic function close cycles too (the panic function reads a slice, whose
check calls the panic function), but those are bounded by the guard, and the
depth below cuts them.

### Depth, before and during a panic

The panic function runs its body once (ADR 0121). The depth of a body `b` is
computed in two states, **before a panic** and **while panicking**:

- `D_s(b)` = cost(`b`) + the largest `D_s(t)` among the targets `t` of its
  edges (0 without edges), for `s` either state, `b` not the panic function;
- `D_before(panic)` = cost(panic) + the largest `D_while(t)` among the
  targets of the panic function's edges;
- `D_while(panic)` = the panic function's frame alone, since the guard spins
  before its body runs. It has no edges in this state.

This is well defined. Without its panic edges, the graph has no cycle once
`analysis::stack-cycle` reports none. A panic edge in the state "while
panicking" ends at a value with no edges, and one in the state "before"
leads into the state "while", which never leads back. So the graph of pairs
(body, state) has no cycle, and the depths are computed once per pair,
memoized, in an order that comes from MIR. A check in the panic function,
or in what it calls, costs one more frame of the panic function, and nothing
more.

**The panic function is one function.** Code generation marks it `noinline`,
so that LLVM keeps one copy, with its frame and its guard, which the checks
call; without this, `-O2` could inline it into a caller, duplicating the guard
and moving its frame into that caller, where "one more frame of the panic
function" would name nothing. Its frame is then always measured on its own
body. The cost at run time is nil: it is called only on a failing path.

### Contexts and the total

Code that preempts runs on the same stack. Following ADR 0078's one level, and
ADR 0084's kinds of vector:

- **`E`**, an exception's entry, is a property of the target (`Target`, a new
  field): 64 bytes on riscv32, which the trap entry saves (ADR 0085), and 36 on
  thumbv7m, the 32 bytes the core stacks plus the 4 it may add to align the
  stack to 8 (`CCR.STKALIGN`, ADR 0085). A test ties the riscv32 value to the
  trap entry's `#[stack(64)]`.
- **`D_def`**, a default handler's depth, is the largest frame of the default
  handlers plus the depth of the panic function before a panic.

The worst case is the sum of four parts:

| Part | Value | Why |
|------|-------|-----|
| thread | the depth of `_start` before a panic | `_start` reaches the entry function through `sym`; the thread starts there |
| maskable | the largest of `E + D(h)` over the maskable handlers `h`, and `E + D_def` if a maskable word of the table has no handler | one level: maskable handlers do not preempt each other, and any one of them can preempt the thread |
| unmaskable | the sum of `E + D(h)` over the unmaskable handlers `h` the program declares, plus `E + D_def` on thumbv7m when NMI has none | each can preempt any code, the others included, once: a handler that raises its own vector again, such as a fault handler that faults, leaves ADR 0078's model, as one that re-enables interrupts does (Consequences) |
| faults | `E + D_def`, plus `E` + the largest default handler's frame + the panic function's frame | a fault reaching a default handler can happen anywhere, once before a panic and once more inside the panic function, where the guard stops (ADR 0121) |

All depths are before a panic. The sum is pessimistic: the deepest points of
the parts rarely meet, and `critical` sections, which keep handlers out, are
not used to lower it. It is sound under ADR 0078's model; priorities (after
M6) would let it be lower.

The bound compares the total with the size of the `stack` placement
(ADR 0057). A program without a `memory` block is already rejected by
`erebos build` (`link::no-memory-map`); `--emit=stack` prints the report
without a comparison.

### Diagnostics

| Code | Case |
|------|------|
| `analysis::stack-overflow` | the worst case exceeds the declared stack |
| `analysis::missing-stack` | the bound reaches a function that needs `#[stack(N)]` and has none (ADR 0122) |
| `analysis::stack-cycle` | a cycle through `sym` operands or inserted calls |

- **`analysis::stack-overflow`**: "the stack may need 2304 bytes, more than the
  2048 declared", at the `stack` placement, labeled "2048 bytes declared here".
  One note, with a line for each part that is not 0 (a `Diagnostic` has a
  single note), each giving the part's bytes and its deepest path with each
  body's cost: "the thread context needs 1460 bytes:
  `_start` (0) → `main` (48) → `fill` (1392) → `core::memcpy` (20, inserted)".
  The paths name bodies as the other analyses do (dots written `::`). A help:
  "declare a stack of at least 2304 bytes, or make the deepest path need less".
- **`analysis::missing-stack`**: one error per function, in MIR's order, at its
  span: "the stack of `f` is not known: it is naked" (or "it holds `asm` without
  `nostack`", "it is an `extern fn`"), with a note giving the path that reaches
  it from a part's start, and the help "declare it with `#[stack(N)]`, the
  bytes it uses that the compiler does not measure (ADR 0122)".
- **`analysis::stack-cycle`**: one error per strongly connected component, in
  the form of ADR 0120's `analysis::recursion` (the shortest cycle from the
  component's first body, a label on each call or `sym`, a note with the
  cycle), with the message "`a` and `b` reach each other through assembly, so
  the stack has no bound" and no help about loops.
- With an error of the last two, the worst case is not computed, and
  `analysis::stack-overflow` is not reported.
- **Which path.** Among the targets of a body's edges with the largest depth,
  the first in the order of its edges (statements in order, then the
  terminator; inserted calls last, in the order of `Mir::inserted`). An edge
  whose target has depth 0 is never followed, so a path ends at the last body
  that costs something. Every
  order comes from MIR, none from a hash map, so a program gives the same
  report on every run (ADR 0028).

### The report

`erebos build --emit=stack` prints:

- the worst case, and the declared size when there is a `memory` block;
- each part with its bytes and its deepest path, as in the note above;
- one line per body that is not `comptime`, in MIR's order: its symbol, its
  frame, its `N` if any, its depth before a panic, and the bodies its edges
  reach, in their order, with "inserted calls" standing for the inserted
  functions when it has those edges. A body that no part reaches is marked
  "not reached"; its depth is computed all the same, or shown as "unknown"
  when it reaches a function that needs `#[stack(N)]` and has none. The oracle
  of the machine's calls (below) reads these lines.

When the bound reports an error (any code of "Diagnostics", the overflow
included), `--emit=stack` fails with those diagnostics and prints no report.

**Bodies no part reaches** are not in the total: an `#[export]` function only
C calls, a function only C calls through its root-module symbol (ADR 0051), a
function nothing calls. What runs of them at run time runs on the stack of
the C code that calls them, so it is inside the `N` of the `extern fn`
through which the program enters that C code (ADR 0122). The report's
depth of such a body is the number the program adds to that `N`. Since these
bodies are not counted, `analysis::missing-stack` is not reported for what
only they reach.

The report shows `core`, which the other dumps leave out: the paths go
through it.

### What ADR 0120 left open

- A panic that re-enters the panic function: ADR 0121's guard, counted above.
- A cycle through assembly (`sym`): `analysis::stack-cycle`.
- A cycle through C (an `extern fn` that calls back the Erebos function that
  called it): inside the `extern fn`'s `N`, which counts what it calls back
  (ADR 0122). The bound rests on that number, which is the program's word.
- A recursive function that compile time reaches and that only C or `sym` calls
  at run time: through `sym` it is a cycle of this graph; through C it is in
  the `N` of the `extern fn` that calls it.

### Tests

- **Unit tests of the analysis**, on MIR built from small programs with frames
  given by the test: the two states of the panic function; a panic function
  that reads a slice (a check that calls it back) reports no
  `analysis::stack-cycle`; each part; inserted calls, and none from the
  inserted closure; `sym` edges; the choice of path among equal depths; a
  cycle through `sym`; each case of `analysis::missing-stack`; a body no part
  reaches, in the report and not in the total.
- **Unit tests of code generation**: on both targets at both levels, a function
  with a 40-byte array in its frame reports a frame of at least 40; a function
  without a frame reports none; the warning's text is read as LLVM 22 writes
  it.
- **ui snapshots of `--emit=stack`**: a program with a handler, a check, a call
  that copies an aggregate (an inserted `memcpy`), a naked function and an
  `extern fn`, on both targets.
- **Build tests** (`tests/elf.rs`): on both targets at both levels, a program
  whose stack fits builds, and the same program with a smaller stack is
  rejected with `analysis::stack-overflow`, snapshotted; one test for each of
  `analysis::missing-stack` and `analysis::stack-cycle`.
- **Oracles**, on both targets at both levels:
  - *the machine's calls*: for every e2e program, `llvm-objdump -d` of the
    executable; every call, and every jump to another function's symbol, is an
    edge of the bound's graph (inserted ones included), or comes from a function
    whose `#[stack(N)]` covers it. This checks the assumption about inserted
    calls, and that `Mir::inserted` is complete;
  - *the measured stack*: a program on QEMU fills its stack's unused part with a
    pattern at entry, runs its deepest path while a timer's handler preempts
    it, and ends that path with a failed check; the panic function counts how
    much of the pattern was overwritten, before it prints anything, then prints
    the count and exits. The stack its printing takes is in the bound but not
    in the count, which makes the check slightly slack, never wrong. The test
    checks that the number is at most the bound that `--emit=stack` reports,
    and at least the size of a 512-byte array local on the path, so that a
    broken count (0) fails. A second program runs the same path, with the same
    local, and reaches the panic function through a fault and a default
    handler instead of a check (the "faults" part), on both targets, with the
    same two checks. This oracle checks that the bound is not too small for what
    the compiler measures; a wrong `N` is the program's word, which no oracle
    of the program can see.
- **The program on QEMU** (roadmap): the e2e program whose stack fits, on both
  targets at both levels, and the rejected one above.

## Consequences

- A program whose stack fits on every path the compiler sees builds; one that
  may not is rejected before the link, with the path that needs the most.
  ADR 0057's consequence "a program that needs more stack than it declares
  still fails at run time" now holds only where the program's word
  (`#[stack(N)]`) is wrong, or outside ADR 0078's model.
- **Outside the bound**, as outside ADR 0078's model: a declared handler that
  raises its own vector again. The likeliest case is a declared fault handler
  that faults (a `LoadFault` handler on riscv32 that loads from a bad address):
  the trap entry runs again on the same stack, without end, and the guard of
  ADR 0121 does not stop it, since it is in the panic function and the fault
  never reaches it: the fault enters the declared handler again, not a
  default one. A fault handler should therefore do nothing that can fault;
  even one that calls the panic function first is entered again by a fault
  inside the panic function, once more than the bound counts, before the
  guard stops it. A guard for declared handlers would be a later ADR.
- A program may fit at `-O2` and not at `-O0`, whose frames are larger; each
  build is checked at its own level, so such a program builds at one and not
  the other.
- The bound depends on LLVM's register allocation: an LLVM upgrade may move it.
  The report and the snapshots show by how much.
- The bound is pessimistic (the sum of the parts, inserted calls from every
  function, a callee inlined at `-O2` counted again). Priorities after M6, and
  reading the machine's calls, could make it tighter.
- Every build compiles the object once, as before; the frames come with it.
  `--emit=stack` compiles an object in memory.
- `docs/reference.md` gains a section on the stack bound, and AGENTS.md's line
  "Analyses ... read only MIR" names the exception.
- **Function pointers** (deferred ideas): a call through one is an edge to every
  function of a compatible signature whose address is taken (ADR 0120's
  model), and the bound takes the deepest.
- **Priorities** (after M6): the maskable part becomes a sum over the levels of
  priority, as RTIC's analysis does.

## Alternatives considered

- **Read the `.stack_sizes` section.** LLVM's own output for this, but the
  option that emits it (`TargetOptions::EmitStackSizeSection`) is reachable
  neither from `inkwell` nor from LLVM's C interface; it would need a C++ shim,
  and an ELF reader.
- **LLVM's remarks (`-pass-remarks-analysis=prologepilog`).** They give the same
  numbers, but are enabled by a command-line option that is global to the
  process, and the tests compile in parallel.
- **Read the frames from the machine code.** Decoding each prologue needs a
  disassembler per target, for what LLVM already says.
- **The larger of `-O0` and `-O2`, on every build.** A program that fits at one
  fits at both, but every build compiles twice, and the code that runs is one
  of the two.
- **An opt-in flag.** Nothing to migrate, but the guarantee would be optional,
  and the stack an unchecked number again.
- **Thread plus the deepest handler.** Tighter, but wrong once an NMI or a fault
  preempts a handler.
- **Read the calls from the object's relocations** instead of assuming the
  inserted calls. Exact, but a hand-written ELF reader and a second source of
  the call graph inside the compiler; the oracle above does the same reading
  in the tests, where an external tool is allowed.
- **Run the bound at `--emit=mir`, before code generation.** The frames do not
  exist there.
