# 0112. Borrows of statics, peripherals and volatile places in MIR

- Status: Accepted
- Date: 2026-11-12
- Amends: 0095 (T5 for a `Ref` of a volatile place; the rule R4), 0107
  (`Ref` carries volatility; R3 gains `Element` and `ConstElement`; the
  borrow analysis roots paths at statics and peripherals, and counts a `sym`
  of a `static mut` as a write), 0109 (a volatile reference to a slice: its
  layout, `SliceAddress` and `SliceLen`), 0093 (the analyses of the whole
  program gain the conflicts across calls; the walk of the call graph is
  shared)

## Context

ADR 0110 lets a program borrow a `static mut`, a peripheral, a cluster and a
volatile place, and adds volatile references. ADR 0111 makes statics and
peripherals roots of the borrow check and adds the conflicts of a call with
what its callee reaches. This ADR says how MIR holds them and how the
analyses compute them.

MIR today (ADR 0095, 0107, 0109):

- A static is reached through its address: `_t = const static S`, a
  temporary of the static's address type, followed by `Deref`
  (`Builder::static_place`). A peripheral is reached the same way, through
  `_t = const peripheral P`. MIR building makes a new temporary for each
  (`temp_place`), written by that statement only.
- A register is `Field(i)` of a place of a peripheral type, and an element of
  a register array or a cluster array is `Element(entry, i)` or
  `ConstElement(entry, i)` (ADR 0064).
- `Ty::Ref` has `mutable` and the referent. A place is volatile when the last
  pointer on its path is volatile, or when it is a register (ADR 0023, 0058);
  "a reference is never volatile" (ADR 0105). MIR building, the validator,
  code generation (`address`) and the evaluator each decide it and must
  agree.
- T5 requires the place of a `Ref`, as of an `AddressOf`, not to be
  volatile. R3 allows `Field`, `Index`, `ConstIndex`, `Deref`, `Offset`,
  `Subslice` and `ConstSubslice` after the root of a `Ref`'s place.
- The borrow analysis (`src/analysis/borrows.rs`) builds a path for a place
  rooted at a local, and gives no path to one through the `Deref` of a raw
  pointer, which is how every static and peripheral is reached: a static has
  no path (roadmap, "M5.4a done").
- The check of shared statics (`src/analysis/shared_statics.rs`) finds the
  places that name a static by their `const static` operand, so a `Ref` of a
  static's place is already one (ADR 0107).

## Decision

### Types and rvalues

- `mir::Ref` gains `volatile: bool`. Dumps print `&volatile T` and
  `&mut volatile T`. Its layout is a thin pointer, or the raw slice of its
  mutability and volatility for a referent `[T]` (ADR 0109).
- **A place is volatile** when the last pointer **or reference** on its path
  is volatile, or when it is a register: a `Deref` of a volatile reference
  makes the place volatile as a `Deref` of a volatile pointer does. MIR
  building (`is_volatile`), the validator, code generation and the evaluator
  read it so.
- `Rvalue::Ref` is unchanged. Its type is the reference to its place's type,
  of the mutability written, **volatile exactly when its place is**.
- `SliceAddress` and `SliceLen` of a volatile reference to a slice are those
  of the volatile raw slice, and its layout is that raw slice's, amending ADR
  0109, which lays out a reference to a slice as the raw slice of its
  mutability only.
- A `Cast` from a volatile reference goes to a raw pointer as from the
  volatile raw pointer of its mutability (ADR 0110).

Code generation lowers a volatile reference as the reference it is, and an
access through one as a volatile access (ADR 0023). The evaluator runs a
`Ref` of a volatile place as `AddressOf`, and an access through a volatile
reference is `eval::volatile-access`, as through a volatile pointer.

### Rules of the validator

ADR 0095 changes:

- **T5**: the type of `Ref { mutable, place }` is `&T`, `&mut T`,
  `&volatile T` or `&mut volatile T`, as `mutable` says and volatile exactly
  when the place is, with `T` the place's type; the place is **not a
  register**: when its last projection applies to a peripheral type, the
  place's type is a peripheral type (a cluster or an element of a cluster
  array). `AddressOf` keeps its rule: its pointer is never volatile (ADR
  0021). A volatile reference fits only the same volatile reference type.
- **R3** gains `Element` and `ConstElement`: `&mut (*_t).ConstElement(2, 1)` is
  a borrow of an element of a cluster array.

And gains:

| Id | Rule |
|----|------|
| R4 | a local assigned `Use(const static S)` or `Use(const peripheral P)` is a temporary (no name); no other statement, call destination or `asm` output writes it, and no `Ref` or `AddressOf` takes its own place (`&_t`, which would let a write through the pointer change it) |

R4 is what lets the analyses read `_t` as the static or peripheral it holds
the address of, wherever `(*_t)` is the base of a place in the body. Where
`_t` is used as a value (`&raw mut P` gives `_t` itself, which a `let` then
copies), it is a raw pointer like any other, and nothing is rooted through
the copy. MIR building already writes each such temporary once
(`static_place`, the peripheral's place, `&raw` of a peripheral), and it is
the only producer of MIR (the driver and the evaluator only read it). R4 is
checked for every body.

### The borrow analysis

ADR 0107's paths change:

- **Roots.** A path's root is a local, a static or a peripheral. A place
  whose base is a temporary of R4 followed by `Deref` is rooted at that
  static or peripheral; the `Deref` of the temporary is the root itself, not
  a step. Any other `Deref` of a raw pointer still gives no path.
- **Steps under a peripheral type.** `Field`, `Element` and `ConstElement`
  applied to a place of a peripheral type are `Elem` steps, so two parts of
  one peripheral always overlap (ADR 0111); this holds below a `Deref` of a
  reference to a peripheral type too.
- **Overlap of roots.** Two paths with different roots overlap only when both
  roots are peripherals whose byte ranges (address, and the type's extent)
  meet; they then overlap whatever their steps. Otherwise ADR 0107's
  comparison applies to paths of one root.
- **Names.** A static or a peripheral is named in labels by its path with `::`
  (ADR 0093, "Diagnostics"), followed by the steps as ADR 0107 names them.
- **`sym`.** A `sym` operand naming a `static mut` or an `extern static mut`,
  in an `asm` statement or terminator, is a write of the whole static, among
  the statement's writes (ADR 0111). Today `Flow::asm` reads only the inputs
  and outputs. A `sym` of a static without `mut`, or of a body, is no access.

Nothing else in the analysis changes: the sites, what each local may hold,
liveness, what is in use, the order of accesses and the errors of
`analysis::borrow-conflict`. The analysis still runs only on a body with at
least one site.

### The conflicts across calls

A new analysis of the whole program, `src/analysis/call_borrows.rs`, run by
`analysis::check` after the check of shared statics, finds
`analysis::borrow-conflict-in-call` (ADR 0111):

1. **Accesses.** For every body, the accesses whose path is rooted at a
   static or a peripheral, with their kind (ADR 0106's table), path and span,
   taken as the borrow analysis takes them (its collection of accesses is
   shared, not copied), `sym` included as above. Only blocks that `bb0`
   reaches count, as in ADR 0093.
2. **Reach.** From each callee, a breadth-first search over `Statement::Call`
   edges, in ADR 0093's order, gives the bodies it reaches and, for each, the
   shortest path of calls. `Terminator::Panic` is **not** an edge here (ADR
   0111). The walk is `shared_statics`'s, moved where both checks use it with
   the set of edges as a parameter (ADR 0093's search keeps following
   `Terminator::Panic`, with its order and ties unchanged), not a second one.
   The result is kept per callee body, so a callee is searched once however
   many calls lend it a borrow.
3. **In use.** For every body holding a site rooted at a static or a
   peripheral, the borrow analysis's fixed points give, at each
   `Statement::Call`, the sites in use during the call (ADR 0107, "In use":
   what its reference arguments may hold, and what the locals live after it
   hold), and for each a later use when one is held after the call. The
   borrow analysis exposes this as a function of `analysis::borrows`; the
   fixed points are computed again for these bodies, not stored in MIR.
4. **Conflicts.** For each such call and site, the first access, in the order
   of the search, of a reached body whose path overlaps the site's and whose
   kind conflicts with the site's mutability by ADR 0106's table is one
   error. Errors are emitted by `BodyId`, block, statement and site.

A body that holds no site rooted at a static or a peripheral costs nothing
but its accesses. A `Terminator::Panic` of the body holding a site is no
call for item 3: nothing is live after it. A `Statement::Call` of the panic
function, written by name in the program, is a call like any other, in item 2
and item 3.

`call_borrows` runs whatever the check of shared statics reported, so that a
program shows the errors of both. Both push to one sink; `Diagnostics::
into_vec` sorts by primary label and keeps the order of pushing on a tie, so
at one span the errors of shared statics come first.

The check of shared statics (ADR 0093) is unchanged: the place of a `Ref`
already names its static by the `const static` operand of its temporary, and
a use of a reference names none.

### The analyses of moves and linearity

Unchanged: a `Ref` of a static's or a peripheral's place is a use of no local
(its base is a temporary of R4 that is read, which the analysis of moves
already allows), and neither gives nor consumes a value (ADR 0107).

### Conformance tests

- **Unit tests of the validator:** T5 (a `Ref` of a volatile place with a
  type that is not volatile, and the other way; a `Ref` of a register; a
  `Ref` of a cluster accepted), R3 (`ConstElement` accepted), R4 (a
  `const static` assigned to a named local, a temporary of R4 written a
  second time, and `&raw const _t` of one).
- **Unit tests of the borrow analysis** over hand-built MIR: a site rooted at
  a static and a write of it while in use; two sites under one peripheral
  type, at different registers, in conflict; the same below a `Deref` of a
  reference to a peripheral type; two peripherals whose bytes meet and two
  whose bytes do not; a `sym` of a `static mut` as a write, and of a
  `static` as nothing.
- **Unit tests of `call_borrows`** over hand-built MIR: a conflict two calls
  down; a `sym` as a write; a call to an external body reaching nothing; a
  `Terminator::Panic` in the callee whose panic function writes the static,
  not followed, and the same in a function reached through a call of a
  function returning `!`; a `Statement::Call` of the panic function by name,
  followed; a callee searched once for two calls.
- **The shared walk:** ADR 0093's search, with panic edges, finds the same
  paths as before the move: its ui snapshots do not change.
- **ui** (`--emit=mir`): the dumps of `&volatile T`, `&mut volatile T` and a
  `Ref` of a static, a peripheral and a cluster element.
- Every program of the tests passes the validator, as for every earlier step.

## Consequences

- MIR needs no new place base: statics and peripherals stay reached through
  their address, and R4 makes that address a root the analyses can trust.
  A later change to how MIR building reaches a static fails the validator
  instead of silencing the borrow check.
- `shared_statics` and `call_borrows` share one walk of the call graph, and
  `call_borrows` and `borrows` one collection of accesses, so the rules of
  "what is a call" and "what is an access" are written once.
- A body with a borrow of a static has its borrow fixed points computed twice:
  once per body before evaluation, once for the whole program.
- Volatility is decided in four places (MIR building, the validator, code
  generation, the evaluator), now with references; a test of each guards
  their agreement, as for pointers.

## Alternatives considered

- **A place base `Static(S)` in MIR, as rustc's** — the analyses would read
  the root directly, but every layer that lowers a place (code generation,
  the evaluator, the validator, four analyses) would gain a base, for what
  R4 states in one rule.
- **The analysis tracking which static a local may hold, as it tracks
  borrows** — no rule in the validator, but a fixed point for what MIR
  building writes once.
- **Storing the borrows in use at each call in MIR, from the per-body
  analysis** — no second computation, but MIR would carry an analysis's
  result, and the validator and the dumps would have to know it.
- **A `volatile` flag on `Rvalue::Ref` instead of on the type** — the type is
  where code generation and the evaluator look when they reach the referent,
  as for pointers.
