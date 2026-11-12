# 0094. Ownership: copies, moves and references

- Status: Accepted; amended by 0110 (no reference leaves a `critical` block by `break`)
- Date: 2026-11-09
- Amends: 0033 (the analyses that look at one body run before that body is
  evaluated)

## Context

M5 brings ownership and borrow checking (ADR 0009) and methods. It is split
into steps (roadmap, "M5 steps"), and like ADR 0078 for M4, this ADR sets the
model they share before the first of them starts, so that no step decides
something a later one must break. Each step's own ADRs settle the details this
ADR leaves to them; the roadmap lists them under "Pending decisions".

Where the language stands:

- Every value is copied: `let`, assignment, arguments and reading a place copy
  the whole value (ADR 0021). ADR 0021 calls this provisional, expects Rust's
  direction (scalars and pointers copied, a struct or enum only if declared
  copyable), and notes that a move is the same machine operation as a copy.
- There are no references: `&raw const` and `&raw mut` make raw pointers, and
  `&` without `raw` is kept free for references (ADR 0021). Raw pointers are
  unchecked (ADR 0013, 0019), and so are slices, `*const [T]` (ADR 0031).
- There are no destructors (ADR 0078) and no heap (ADR 0002): no code runs
  at the end of a scope.
- MIR reads every value with `copy` (`Operand::Copy`); it was designed so that
  explicit moves and borrows could be added (ADR 0009). The first MIR analysis,
  the check of shared statics, runs over the whole program after MIR is built
  (ADR 0093), and compile-time evaluation runs bodies that no analysis has seen
  (ADR 0033).
- ADR 0078 leaves to M5 the ownership of peripherals and other values that
  must not be copied, and the sharing of references with handlers, saying that
  M5 adds checks and changes no meaning of that ADR. The roadmap asks the M5
  ADRs to say how ownership and references extend to priorities and to several
  cores.

What other languages do:

- **Rust**: types are affine (a value is used at most once, and may be dropped,
  running `Drop` implicitly); references are first-class, stored in structs
  and returned, with lifetimes written where elision does not apply, and a
  borrow checker over the whole function (NLL, Polonius).
- **Linear types** (Haskell's `LinearTypes`, Austral): a value must be used
  exactly once, so forgetting it is an error. That catches a hardware token
  that must be given back, which Rust cannot, since forgetting a value is safe
  there.
- **Second-class references** (Hylo/Val, Swift's `inout`, Ada's parameter
  modes): a reference can be a parameter but cannot be stored or returned, so
  no lifetime is ever written and the check stays inside one function.

## Decision

### Three kinds of type

Every type is one of:

- **Copy**: using a value copies it, and the source stays usable. These are
  the integers, `bool`, `()`, bit-field structs (ADR 0059), enums without
  payloads (ADR 0035), raw pointers and slices, `&T` (below), an array of a
  copy type, `Option(T)` and `Result(T, E)` whose arguments are copy, and a
  struct or an enum with payloads declared `#[copy]`.
- **Affine**, every other type: using a value **moves** it, and the source
  cannot be used again until it is assigned a new value. A value may go out of
  scope without being used.
- **Linear**, an affine type marked so (M5.3): a value must be moved before
  every point where the function holding it returns, and is never overwritten.

`#[copy]` on a struct or an enum with payloads requires every field and
payload value to be of a copy type, else it is an error at the attribute. It
does not change the layout or the ABI. A type containing an affine value is
affine, and one containing a linear value is linear, whatever it declares.
Peripheral types and `!` have no values (ADR 0058, 0034), and so no kind.

Copy is **opt-in**. Forgetting `#[copy]` on plain data gives an error at the
second use, whose help suggests the attribute; forgetting to mark a handle
under the opposite default would let it be duplicated in silence.

### Moves

- A move is the same machine operation as a copy (ADR 0021); what changes is
  that the compiler rejects every later use of the source: a read, a move, a
  borrow, or a field of it, until a new value is assigned to it.
- The check is per function and follows control flow: a place moved on some
  path into a point is moved at that point.
- Nothing runs when a value is moved from, goes out of scope or is
  overwritten. There are no destructors.
- A value **computed at compile time** cannot be of a non-copy type: a `const`
  or a `comptime` expression would make a new value at each use, forging the
  handle the type stands for. A static's initializer is not one: it makes the
  one object the static is.
- A **static** of a non-copy type is never moved out of: it may be assigned
  whole (a `static mut`), a field of a copy type may be read, and from M5.4 it
  may be borrowed. How a context comes to own what a static holds is M5.6's.
- Which other places a value may be moved out of (a field, an array element, a
  payload bound by `match`, a place through a raw pointer), and the
  diagnostics, are M5.2's.

### References

`&T` and `&mut T` are references, made by `&place` and `&mut place`, with
`*r` the place they refer to. They are **second-class**:

- A reference type may be the type of a **parameter** and of a **`let`
  without `mut`**, and of the expressions that give them their values (an
  argument, as `f(&x)`, and an initializer), and nothing else: not a field, an
  array or slice element, a payload, an argument of `Option` or `Result`, a
  return type, the type of a static, constant or `comptime` value, or the
  pointee of a pointer.
- A `let` of a reference type is bound once, by its initializer.
- No lifetime is ever written, and the check stays inside one function.
- While a `&mut` borrow of a place is in use, the place is used only through
  it; while a `&` borrow is in use, the place is not written, moved or
  borrowed `&mut`. Two references passed to one call do not overlap if one of
  them is `&mut`.
- `&T` is copy; `&mut T` is not, and passing one to a call lends it for the
  call without moving it.
- A reference becomes a raw pointer only with `as` (ADR 0008). Raw pointers
  stay the unchecked way out for what references cannot express: structures
  that hold a view, accessors that return one.
- The value of a `critical` block is never a reference.

The rest is M5.4's: how long a borrow held by a `let` lasts; when two places
overlap (fields of one struct, elements of one array); a `&mut` passed for a
static the callee also names; references to volatile places and to registers;
a reference made from a raw pointer; sub-slices; indexing and fields through a
reference; and references in the signatures of `extern fn`, `#[export]`,
naked functions, the entry function and handlers, with their ABI (ADR 0006,
0025, 0075). Returning a reference tied to the one reference
parameter, as Rust's elision does, is a later addition that would break no
program.

### Methods

`impl` blocks, receivers `self`, `&self` and `&mut self`, and `x.f()` are
M5.5's. `x.f()` borrowing `x` as the receiver asks is the direction (the
reason methods waited for references, roadmap), and M5.5's ADR decides it as
an amendment of ADR 0008.

### Where the analyses run

- The analyses that look at **one body** (moves, linearity, borrows) are in
  `src/analysis/` (ADR 0093). They run on each body right after it is built
  and validated (ADR 0095), before compile-time evaluation can run it, and
  before it joins the program's MIR. A body they reject is a body that failed
  to build: whatever needs it stops, as for a type error in it. So the
  evaluator never runs code the analyses reject.
- The analyses over the **whole program** (shared statics, ADR 0093) run after
  the program's MIR is assembled, as now.
- `query` schedules both (ADR 0028, 0063). The tables of ADR 0063 do not let
  `query` name `analysis` today; the ADR of the step that adds the first
  analysis of one body (M5.2) amends ADR 0063 to add that edge, as ADR 0093
  did for `driver`. This ADR does not change the tables.
- The analyses are the compiler's own (ADR 0009): no `polonius-engine` and no
  other dependency. Second-class references make a check inside one function
  enough.

This amends ADR 0033, whose diagram puts all MIR analyses after evaluation and
leaves the question to M5.

### Contexts (ADR 0078)

Nothing here changes the meaning of ADR 0078; it adds checks.

- **Moves do not cross contexts** in M5.2 to M5.5: handlers take no
  parameters and return nothing (ADR 0084), and moving out of a static is an
  error. Giving a context ownership of a value, a peripheral's handle above all,
  is M5.6's.
- **References do not cross contexts.** A reference lives in the parameters
  and locals of one chain of calls. Borrowing a `static mut` names it, at the
  place of the borrow, as `&raw` does (ADR 0078, 0093). A reference cannot
  outlive the `critical` block it was taken in: the block's value is never a
  reference, and a `let` holding one is scoped and bound once. So every use of
  a reference to a static is covered by the protection of the place that
  borrowed it, and ADR 0078's rule needs nothing more.
- **Priorities** (not scheduled) make `critical` mask up to a ceiling. The
  argument above holds per ceiling: a reference taken inside a block is used
  only inside it.
- **Several cores**: a critical section protects nothing from another core
  (ADR 0078), and neither does a reference taken inside one. A static shared
  between cores needs what that step brings (atomics, a memory model); since
  references and moves never cross contexts here, nothing in this ADR hands
  memory to another core implicitly. Moving a value to another core is a
  construct that step would add.

## Consequences

- Duplicating a handle to hardware becomes an error once a type is not
  `#[copy]` (M5.2), and forgetting one that must be given back becomes an
  error once it is linear (M5.3).
- Programs that use a struct after copying it need `#[copy]`, as ADR 0021
  announced; the help says so, and M5.2 migrates the tests and `core`.
- Accessors that return a reference, structs that hold one, and chaining on
  `&mut self` are not expressible; nor, since a `let` of a reference is bound
  once, is a loop that walks a buffer by moving a reference forward (it keeps
  an index instead, or a raw pointer). Raw pointers are what such code uses.
  The cost is accepted so that no lifetime is ever written.
- Whether a type of M6 made by a `comptime` function is copy, depending on
  its arguments as `Option(T)` does, is M6's to decide.
- A `const` cannot hold a handle; the handles of a program come from functions
  and statics (M5.6).
- Evaluation runs only MIR the analyses of one body accepted, and the pipeline
  of ADR 0033 gains a stage before evaluation.
- No dependency is added for borrow checking.

## Alternatives considered

- **Affine only, as Rust** — simpler, but a transfer whose handle is dropped
  without waiting for it, or a token that must be given back, would go
  unnoticed. Linear is opt-in, so data that is merely not copyable pays
  nothing.
- **Every non-copy type linear** — the strictest, but plain data that is not
  copyable would have to be taken apart or passed on at every return, and the
  language would need a way to end a value outside the module that defines it.
- **Copy by default, with an opt-out** — no program changes, but a handle that
  forgets the mark is duplicated in silence: the default fails on the unsafe
  side.
- **References as parameters only** — a borrow would last exactly one call,
  the simplest check; but `let r: &mut Buf = &mut bufs[i];` and its uses would
  have to be written as a call each time.
- **References returned when tied to one parameter** — gives accessors and
  chaining with no lifetime written, and can be added later without breaking
  programs; left out of M5 to keep the first checker small.
- **First-class references with lifetimes, as Rust** — the most expressive, at
  the cost of written lifetimes and a checker over regions; ADR 0009 named
  Polonius, built around rustc's region model.
- **Analyses after evaluation, as now** — moves are copies in the machine, so
  the evaluator would not misbehave; but the compiler would run, at compile
  time, code it then rejects, and a value computed from it could shape the
  program (an array length, a layout check) before the error is known.
- **No statics of non-copy types until M5.6** — simpler now, but a `static mut`
  holding a driver's state would have to be `#[copy]`, the opposite of what the
  type means.
