# 0120. No recursion in code that runs on the target

- Status: Accepted
- Date: 2026-11-16

## Context

Erebos has no heap and no OS, so a program's stack is all its memory. Its size is
declared in the `memory` block (ADR 0057), and a program that runs past it writes
over whatever is next. M5.11 will compute the worst-case stack depth of a program
and check it against that size. A worst case exists only if no path of calls can
grow without bound, and in Erebos today recursion is the only way one can: there
are no function pointers, no closures and no dynamic dispatch. Every call in MIR
names its callee (`Statement::Call`), so the call graph is closed and finite.

Recursion is allowed today. A text search (not the compiler) found it in the e2e
programs almost only as the idiom that prints a number digit by digit
(`print_u32` and `print_u64`: about 70 definitions in more than 55 programs
of `tests/e2e/`). Two programs
recurse for their own sake: `fib` in `functions.ers`, `walk` in
`returned_references.ers`. A few ui tests recurse. `rt/` and `tests/boards/` do
not. LLVM turns tail recursion into a loop at `-O`, not at `-O0`, so the
optimizer does not bound it either.

Compile time is another matter. The evaluator runs MIR bodies (ADR 0027, 0033),
and it limits the depth of nested calls, so recursion there stops with an
error and has no stack on the target to overflow. A function that only
compile time calls is a MIR body like any other: its `comptime` flag is
false, and only the bodies of constants and `comptime` blocks are marked
`comptime` (ADR 0029).

Two ways to bound recursion were weighed with the user. One forbids it. The
other allows it under an annotation that declares its depth
(`#[recursion(depth = N)]`). The compiler cannot check a depth that depends on
data, so the annotation would be the program's word for it, and the stack
bound would rest on that word. Forbidding is also the decision that can change
later: a rule can be relaxed without breaking a program, but not tightened.

## Decision

### The rule

**No cycle in the run-time call graph.** If a function can reach itself through
calls in code that runs on the target, that is an error,
`analysis::recursion`. This covers direct recursion (`f` calls `f`) and mutual
recursion (`f` calls `g`, which calls `f`) of any length.

### The call graph

The rule reads the call graph off MIR, the same graph ADR 0093 ("What is a
call") defines for the shared statics, with one difference:

- **An edge** is a `Statement::Call` in a block reachable from the start block
  of the body that holds it, from that body to its callee. A method call
  (ADR 0114) is a `Statement::Call` like any other. A call inside `critical` is
  an edge like one outside it.
- **A check that calls the panic function (`Terminator::Panic`) is no edge
  here**, though ADR 0093 counts it for the contexts. With it, a panic function
  that prints its message through a function that reads a slice would be a
  cycle: the read has a bounds check, and the check calls the panic function.
  Even `print(info.message)` would then be rejected. A check failing inside
  the panic function does re-enter it, though. That re-entry, and its bound,
  are left to M5.11 (see Consequences).
- **Nothing else is an edge**, as in ADR 0093. A `sym` operand gives an address
  to assembly, which the check does not see. An `extern fn` has no body. The
  calls that code generation or LLVM inserts (`memcpy`, `__aeabi_uldivmod`,
  `__critical_enter`) are not in MIR.

### What runs on the target

Not every function is checked. A function that only compile time calls may
recurse, since its recursion is bounded by the evaluator's depth limit. The
same function is rejected once code that runs on the target calls it too.
Precisely:

1. **Compile-time bodies** are the bodies marked `comptime`: a constant's value,
   or a `comptime` block. Neither is ever part of the program.
2. **Compile-time only** are the bodies that the compile-time bodies reach
   through edges.
3. **Run-time roots** are every body that is not external, not `comptime`, and
   not compile-time only. `core`'s bodies are included.
4. **Run-time code** is everything the run-time roots reach through edges.
   A cycle among run-time bodies is an error. A cycle that no run-time root
   reaches is not.

A body can be both: a constant calls `f` and `entry` calls `f` too. `f` is
then no root (point 3), but `entry` is, and reaches it, so `f` is run-time
code and its cycle is an error.

This rule makes every function compiled for the target a root unless
compile time reaches it. Three cases follow, each for a function that
compile time does not reach:

- a recursive function that nothing calls is rejected;
- so is one that only C calls by its symbol, `#[export]` or not, since a
  function of the root module has its own name as its symbol (ADR 0051);
- so is one that only assembly calls through `sym`.

A recursive function that compile time reaches, and that only C or `sym`
calls at run time, is accepted: neither is an edge, so no run-time root
reaches it. That is one of the openings left to M5.11 (Consequences).

### The diagnostic

Each strongly connected component of the run-time call graph that holds a
cycle gets one error (a body that calls itself is a component of one with a
cycle). The error shows one cycle of the component:

- **Which cycle.** The component's body that comes first in MIR's order of
  bodies is where the cycle starts. The cycle is the shortest path from that
  body back to itself within the component, found breadth first, with each
  body's calls taken in their order in the body. A cycle can run through
  `core`, though no test makes one (see Consequences).
- **The message** names the functions, in the cycle's order, by path, with
  dots written `::` as the other analyses show them: "`fib` calls itself",
  "`even` and `odd` call each other", and for three or more, "`a`, `b` and
  `c` call each other in a cycle".
- **One label on each call of the cycle**, at the span MIR keeps for the
  call's statement, which is the call expression (for a method call, the
  whole `x.f(...)`), in the cycle's order:
  "`even` calls `odd` here", or "`fib` calls itself here". A body that calls
  the next one more than once gets a label only on its first call.
- **A note** gives the whole cycle in order, ending where it started:
  "the cycle: `even` → `odd` → `even`".
- **A help:** "a function that runs on the target may not reach itself, so
  that its stack has a bound; write the recursion as a loop, keeping what it
  needs in a fixed-size array if it needs a stack".

The errors come in the order of their components' first bodies. Every order
here comes from MIR's vector of bodies and the order of each body's
statements, and none from the iteration of a hash map. A program therefore
gives the same errors, in the same order, on every run (ADR 0028). The rule is
one of the whole-program analyses of `src/analysis/` (ADR 0093, 0099). It
runs after the check of shared statics and the check of conflicts across
calls, reads only MIR, and reports through the diagnostics sink.

### Programs migrated

The e2e programs stop repeating `print_u32` and `print_u64`. Each board module
in `tests/boards/` (`mod board;` of every e2e program, ADR 0077) gets one
copy of each, written as a loop, and a program that printed numbers in
decimal its own way calls the board's. The output of each program stays the
same. The other recursions in the e2e programs and ui tests are rewritten as
loops, and a ui test whose point was a recursive call keeps that point in
another form. Once the check exists, the count of what it rejects in the
tree before the migration is redone by the compiler and recorded in the
roadmap, as was done for `unsafe` (ADR 0119).

## Consequences

- With no recursion in run-time code, the run-time call graph has no
  cycles, and M5.11 can compute a worst-case stack depth over it, except for
  the openings listed below.
- Programs that recurse must be written as loops. For the digit-printing idiom
  this means a small buffer. For a walk of a tree, which Erebos cannot
  allocate anyway, it means an explicit stack in a fixed-size array.
- A recursive function that is never called is now an error too. Dead
  code is still compiled for the target.
- `core` is compiled into every program and its bodies are roots, so a
  recursive helper added to `rt/`, called or not, breaks every program. That
  is intended: `core` runs on the target like the rest. No test of its own
  puts a cycle through `core`. `core` is part of every program the tests
  compile, so the whole suite fails if one appears, and making one would mean
  changing `core` for a test.
- No accepted ADR is changed. The passages of ADR 0078 and 0093 about a path
  that recurses, or a recursion entered inside `critical`, stay true:
  - the analyses still run on a program that this rule rejects, and must
    still end on a cycle;
  - a cycle the rule does not see (below) is still a cycle to them.

  `docs/reference.md`, which said that functions may recurse, is rewritten.
- A recursive function that only compile time calls stays legal. Its run-time
  version is still emitted, but nothing on the target calls it, unless C or
  assembly does (the fourth opening below).
- **Left to M5.11.** The worst-case stack depth must still bound four things
  this rule does not:
  - a check that fails inside the panic function, or in what it calls,
    re-enters the panic function, and can again, as long as the check keeps
    failing. One candidate is a guard in `core` that stops at a second panic,
    as Rust aborts on a panic while panicking. That would amend ADR 0014 and
    0053, which have each check call the panic function directly;
  - a cycle through C: an `extern fn` that calls back into an Erebos function
    that calls it;
  - a cycle through assembly that calls a function named by `sym`;
  - a recursive function that compile time reaches and that C or `sym`
    calls at run time, which this rule accepts ("What runs on the target").

  All four are added to the roadmap's questions for M5.11.
- **Function pointers.** When they come (deferred ideas), a call through one is
  an edge to every function of a compatible signature whose address is taken
  (ADR 0078's model), so the graph stays closed and finite, and those edges
  count for this rule. M6's `comptime` function parameters are direct calls
  after monomorphization and need nothing here.
- The rule adds one analysis of the whole program, linear in the number of
  bodies and calls, to every compilation that reaches MIR.
- `tests/rejected.rs` gains snippets for `analysis::recursion`. A recursive
  function in a snippet is uncalled, which the rule rejects (point 3 above),
  so a snippet of items alone is enough.

## Alternatives considered

- **`#[recursion(depth = N)]`.** The program declares the depth, and the stack
  bound multiplies the cycle's frames by it. Not chosen: the compiler cannot
  check the depth for recursion driven by data, so the bound would rest on an
  unchecked claim; forbidding can be relaxed into this later, the reverse cannot.
- **Count checks that call the panic function as edges**, like ADR 0093. This
  is the strict answer to a panic re-entering the panic function. Not chosen:
  the panic function could then reach no check at all. Reading a slice has a
  bounds check, so a panic function could not print its message safely, and
  the e2e panic functions that print through `board::print` would be rejected
  (13 of them also print numbers). M5.11 bounds the re-entry instead.
- **Stop a second panic in `core` now.** Checks would call a guard in `core`
  instead of the panic function. Not chosen for this step: it changes code
  generation and amends ADR 0014 and 0053, for a question that only the stack
  bound needs answered. M5.11 decides.
- **Roots as the roadmap first listed them**: `entry`, the handlers, the panic
  function and the `#[export]` functions. Not chosen:
  - MIR does not mark `#[export]`, so the rule would need a new field;
  - it misses functions that C calls through a root-module symbol without
    `#[export]`, and functions that only `sym` names, such as the loop of
    `__aeabi_uldivmod`;
  - it lets an uncalled recursive function through.

  Taking every run-time body as a root covers all of these with no change
  to MIR.
- **Check every body that is not `comptime`**, compile-time-only ones included.
  Not chosen: it forbids recursion at compile time, which the evaluator already
  bounds, and which a table computed by a recursive function needs.
- **One error per function of a cycle.** Not chosen: a cycle of `n` functions
  would give `n` errors about one mistake. One error with a label on each call
  shows the same thing once.
