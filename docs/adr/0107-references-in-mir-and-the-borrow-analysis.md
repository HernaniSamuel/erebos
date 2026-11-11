# 0107. References in MIR, and the borrow analysis

- Status: Accepted
- Date: 2026-11-11
- Amends: 0095 (T1 and T5 for references; the rules R1 to R3), 0098 ("Kinds
  in MIR": the kind of a reference; "Where MIR building moves": a `&mut`
  argument is lent by a reborrow, and a borrow is never a simple operand),
  0099 ("The analysis of moves": the place of a borrow is a use; `check_body`
  runs the borrow analysis)

## Context

ADR 0105 is the language of references in M5.4a, and ADR 0106 the borrow
check as the program sees it. The analyses of one body read MIR in
`src/analysis/`, after the validator (ADR 0095) and before evaluation, from
`analysis::check_body`, which runs the analysis of moves and, on a body that
passed it, the analysis of linearity (ADR 0099, 0103).

MIR today (ADR 0095, 0098, 0103):

- `mir::Ty::Ptr` is a raw pointer, thin or to a slice; `PlaceElem::Deref`
  applies to one. `Rvalue::AddressOf { mutable, place }` is `&raw`.
- Locals live for the whole call of the function; MIR has no storage
  markers. MIR building reuses some temporaries (literals, variants, the tags
  of `[.None; N]`).
- MIR building reads a place operand into a temporary before a later operand
  that is not simple (`Builder::operands`, `is_simple`), so the operands of a
  statement keep the order of the source (ADR 0098).
- The analysis of moves follows named locals and their `Field` and `Payload`
  paths; the analysis of linearity follows every local; both are forward
  analyses over the blocks `bb0` reaches, in bit sets (`analysis::bits`).

## Decision

### Types, rvalues and places

- `mir::Ty` gains `Ref(Ref)`, with `mutable: bool` and the referent, leaked
  like pointees. It is a type of values with the layout of a thin pointer.
  `Ty::is_copy` is true for a shared reference and false for a mutable one;
  no reference is linear. Dumps print `&T` and `&mut T`.
- `Rvalue::Ref { mutable, place }` borrows a place; dumps print `&_1` and
  `&mut (*_2).0`. Its type is the reference to the place's type.
- `PlaceElem::Deref` applies to a reference as to a pointer, giving the
  referent. `Offset` does not apply to a reference (it is pointer
  arithmetic).
- `Rvalue::Cast` converts a reference to a raw pointer, by the rules of ADR
  0105, and never anything to a reference.

Compile-time evaluation runs `Ref` as `AddressOf` and dereferences a
reference as a pointer. Code generation lowers `Ty::Ref` as a thin pointer
(LLVM `ptr`), passes it as one on every target (ADR 0105), runs `Ref` as
`AddressOf`, and gives it no attribute a pointer lacks. A local whose place a
`Ref` names lives in memory, as one that `&raw` names does: the scan that
decides which locals get a stack slot (`FnCodegen::new`) reads both. Its debug
type is a pointer type named `&T` or `&mut T`.

**A result returned through memory** (`sret`, ADR 0025, 0075) is written by
the callee through a pointer to the caller's destination. When a call has an
argument of a reference type, code generation gives it a temporary slot for
its result and copies the result to the destination after the call, so that
`x = f(&x)`, which the borrow check accepts (ADR 0106: the call's result is
written after the callee returns), never hands the callee one address as both
its result and its argument. A call with no reference argument is lowered as
today; one whose raw pointer argument reaches its destination is the
program's word, as before.

The check of shared statics (ADR 0093) treats the place of a `Ref` as it
treats the place of `&raw`: a static it names is named there. This changes
nothing ADR 0093 decides in M5.4a, since only statics that are never written
can be borrowed (ADR 0105) and the rule concerns `static mut`; M5.4c amends
ADR 0093 when it allows borrows of a `static mut`. The analysis of linearity
treats a `Ref` as it treats `&raw`: it neither gives nor consumes a value.

### What MIR building writes

- `&place` and `&mut place` are `_t = &place` or `_t = &mut place`, with the
  place as type checking resolved it.
- **A value that is not a place** (ADR 0105) is written to a new local, which
  no other expression of the body uses, then borrowed: `_v = <value>; _t =
  &_v`. A reused temporary would be written again while the borrow is in use,
  and the analysis would report the compiler's own write. A part of such a
  value (`&f().x`) is borrowed from the new local holding the whole value:
  `_v = <value>; _t = &_v.x`.
- **Lending** (ADR 0106): an argument that is a variable or a parameter `r` of
  type `&mut T` is passed as `_t = &mut *r` and `move _t`.
- **The conversion** of a `&mut T` to a `&T` (ADR 0105) is `_t = &*q`, with
  `q` the place holding the `&mut`: the variable or parameter, or the
  temporary holding the value converted.
- **A borrow is not simple**: `is_simple` is false for `&e` and `&mut e`, and
  for an argument that lends a `&mut`, so a place argument before one is read
  into a temporary first, as ADR 0098 requires of operands before anything that
  could write. `f(x.a, &mut x)` reads `x.a`, then borrows `x`.

### Rules of the validator

ADR 0095 changes:

- **T1**: `Deref` applies to a reference, giving its referent; `Offset` does
  not apply to one.
- **T5**: the type of `Ref { mutable, place }` is `&T` or `&mut T`, as
  `mutable` says, with `T` the place's type, and its place is not volatile,
  as for `AddressOf`. A reference fits only the same reference type: a `&mut
  T` does not fit a `&T` place in a `Use`, so the conversion of ADR 0105 is
  always a `Ref`. A `Cast` from a reference goes to a raw pointer, as ADR 0105
  allows, and no `Cast` goes to a reference.
- T9 is unchanged: an `asm` operand is an integer or a thin pointer, not a
  reference.

And gains, for every body and every static:

| Id | Rule |
|----|------|
| R1 | a reference type is only the type of a local, never reached by another type: not a field, a payload value, an array or slice element, a pointee or a referent; nor the type of a static, nor a body's return type |
| R2 | no place that is written (an assignment, a call's destination, an `asm` output), borrowed `&mut` or given to `&raw mut` goes through the `Deref` of a shared reference |
| R3 | the place of a `Ref` is a local or a static followed by `Field`, `Index`, `ConstIndex`, `Deref` and `Offset` projections only |

R1 is checked through the type of every local and every static, through
arrays, slices, fields, payload values, pointees and referents, each
definition once, as M4 and L3 are. By R1, a place of a reference type is a
whole local: no field, element or pointee has one.

### The borrow analysis

`src/analysis/borrows.rs` checks ADR 0106 over one body's MIR.

**Borrow sites and paths.** Each `Ref` statement in a block `bb0` reaches is
a **borrow site**, with its mutability, its span and its **path**: the root
(a local or a static) followed by `Field(i)`, `Elem` (for `Index` and
`ConstIndex`, whatever the index) and `Deref` of a reference. A place that
goes through the `Deref` or the `Offset` of a raw pointer has no path: a
`Ref` of one is no site, and an access to one is none. Two paths
**overlap** when, compared element by element from the root, they meet two
different `Field`s nowhere before one of them ends (ADR 0106).

**What each local may hold.** A forward analysis computes, at each point, for
every local of a reference type, the set of sites whose reference it may hold:

- `_t = &place` sets `_t` to its own site, plus, when the place's path starts
  with a local `r` of a reference type followed by `Deref`, everything `r`
  may hold (a reborrow keeps what it borrows through in use);
- `_t = copy r` or `_t = move r`, with `r` a reference, sets `_t` to what `r`
  may hold;
- any other write of a whole local of a reference type sets it to nothing (a
  cast gives a raw pointer, and no call returns a reference);
- at a join, the sets are united.

**Liveness.** A backward analysis computes the locals **live** at each point:
those that some path from there reads before writing them whole. A local is
read by an operand naming it, by a place whose base it is and which is not
the destination written whole (writing `*r` or `x.f` reads `r` and does not
end `x`), and as an index local. A whole write ends its liveness.

**In use.** The sites in use at an access are what the locals live after the
statement may hold, and, inside the statement:

- for an access by a statement other than a call, what the locals read by its
  later operands may hold (`_3 = Add(copy *_r, copy _x)` with `_r` holding
  `&mut x` and dead after it reads `x` with nothing in use);
- for a call, what every argument of reference type may hold, during every
  argument's evaluation, but not at the write of its result, which happens
  after the callee returns (`x = f(&x)` is accepted);
- never the site the access itself makes.

**Accesses and conflicts.** The statements of a reachable block, then its
terminator, are taken in order, and within each, its accesses in the order of
ADR 0099 (uses left to right, then writes): reads (`copy`, the places of
`&raw const`, `SliceAddress`, `SliceLen`, a `switch` on a tag), shared
borrows, writes (destinations, `asm` outputs, `&raw mut`), moves and `end`,
mutable borrows. An access with a path conflicts with a site in use whose
path overlaps it, by ADR 0106's table; one whose place goes through the
`Deref` of a local holding the site is an access through the borrow, not a
conflict, since its path is rooted at that local, not at the borrowed place.

The analysis runs only on a body with at least one site, and on a body the
analysis of moves accepted. Its cost is the number of blocks times the number
of locals of a reference type and of sites, in bit sets, per iteration of
each fixed point.

**Errors.** After the fixed points, one pass over the reachable blocks finds
every access in conflict, and reports `analysis::borrow-conflict` as ADR 0106
says: the span of the access, a label at each site in conflict, and for each
one later use. That use is chosen among the reads of a local holding the site,
at a point where the site is in use, that a forward search from the access
reaches: the one whose span starts first at or after the access's span start;
if every one starts before it (reached only round a loop), the one whose span
starts first, marked "in the next iteration of the loop". Spans that start
together are ordered by their end, then by location (block, statement index,
the terminator last), as the analysis of moves orders them. The names of places follow `analysis::moves` (a local's
name, `.f`, `[_]`, `*`), with "this value" for a temporary; a site that MIR
building made to lend an argument or for a conversion is described as such.
Errors are ordered and deduplicated as the analysis of moves orders them.

### `check_body`

`check_body` runs the analysis of moves; on a body that passed it, the
borrow analysis and the analysis of linearity, both, so that a body reports
the errors of each. The body passes when all three pass.

### The analysis of moves

The place of a `Ref` is a use, as the place of `&raw` is (ADR 0099):
borrowing a moved place is `analysis::use-after-move`. A `move` of a local of
type `&mut T` is a move like any other; the lending of ADR 0106 is a `Ref`,
so it moves nothing.

## Consequences

- MIR says where every borrow is and what kind it is, so later analyses (the
  shared statics of M5.4c, slices of M5.4b) read them as they read moves.
- A value borrowed with `&f()` takes a local that is never reused.
- The validator gains three rules and two changed ones; every program of the
  tests passes it, as for every earlier step.
- No dependency is added; the analysis shares `analysis::bits` with the two
  others.

## Alternatives considered

- **`Ty::Ptr` with a flag for references** — fewer matches to extend, but
  every place that handles pointers would accept references silently,
  including where ADR 0105 rejects them (an `asm` operand, a static's type);
  a variant of its own makes each match decide.
- **`AddressOf` with a kind** — one rvalue for both, but the shared statics
  check and the analyses would have to tell them apart at every use; the
  borrow analysis reads only `Ref`.
- **Regions or lifetimes in MIR, as rustc's NLL** — the general solution, for
  references stored and returned; second-class references need only which
  locals may hold a borrow and which are live.
- **A borrow lasting to the end of the `let`'s scope** — MIR has no scopes; it
  would need storage markers MIR does not have (ADR 0106).
- **Moving a `&mut` argument and letting the analysis of moves allow it** —
  the analysis of moves would need an exception for one type; a reborrow is
  how the borrow check already sees borrows through a reference.
