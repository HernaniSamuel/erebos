# 0103. Linear values in MIR, and their analysis

- Status: Accepted
- Date: 2026-11-10
- Amends: 0095 (the rules L1 to L4), 0097 ("Uses of a moved place": `end`
  may use a partly moved place), 0098 ("Kinds in MIR": a struct's definition
  also says whether it is declared `#[linear]`), 0099 ("The analysis of
  moves": `end` is an event of it)

## Context

ADR 0101 says which types are linear and where a linear value may not be
held; ADR 0102 says when a linear value is consumed, the rules on returns and
overwrites, `end`, and the diagnostics. The analyses that look at one body
are in `src/analysis/`, run by `analysis::check_body` on each body when
`query` has built it, before compile-time evaluation can run it (ADR 0094,
0099). The analysis of moves follows only variables and parameters, and ADR
0100 notes that the analysis of linearity will have to follow temporaries,
such as the result of a discarded call.

MIR today (ADR 0095, 0098):

- `StructDef` and `EnumDef` carry whether the type is copy; `mir::Ty::is_copy`
  reads it.
- A `match` on a place of a non-copy type tests the place itself: its
  `switch` reads `copy p.tag`, and a non-copy binding is
  `_b = move (p as k).i` at the end of its alternative. A non-copy scrutinee
  is an enum with payloads, so every enum holding a linear value is tested in
  place. `?` tests its operand the same way.
- A discarded call keeps its result in a temporary (`_4 = call make();`). A
  literal or a variant is written into a temporary by parts, then moved
  (`_7.tag = const 0; _2 = move _7`). A literal of a struct with no fields
  writes nothing at all: `fn a() -> Tok { Tok {} }` is `_0 = move _1` with
  `_1` never written, and a discarded `Tok {};` leaves no trace in MIR.
- Calls are statements; a call that does not return is followed by an
  `unreachable` terminator; checks that fail end in `panic`.

MIR does not say where a value is built, which the analysis of moves never
needed (ADR 0100, "Alternatives considered"). The analysis of linearity
needs it: a `#[linear]` struct with no fields, the natural token, would
otherwise never be seen to hold a value.

## Decision

### Kinds, `new` and `end` in MIR

- `StructDef` gains `linear: bool`, set by MIR building from the HIR
  declaration (`#[linear]`), as `copy` is. `mir::Ty` gains `is_linear`
  (ADR 0101's rule, computed from the parts, an array or the unsized slice
  type `[T]` of a linear type included, never a pointer) and a way to list
  the linear parts of a type: the paths of `Field` and `Payload`
  projections, from the type, that reach a struct marked linear, the empty
  path included.
- `Statement::New(Place, Span)` says that a value of a `#[linear]` struct is
  complete in the place, with the span of the literal. MIR building emits it
  where it builds a struct literal of a type marked linear, after the write
  of its last field and before the value is moved anywhere; for a literal
  with no fields, where the literal is. Dumps print `new _1;`. Placed after
  the fields, it never sits before a field whose value can return (`?`):
  `Transfer { a: f()?, b: g()? }` returns before the value exists, and owes
  nothing for it.
- `Statement::End(Place, Span)` is `end(e)` (ADR 0102), with the span of the
  expression `e`. Dumps print `end _1;`. A temporary operand is first
  written to a temporary, as for any temporary that is moved.
- Compile-time evaluation and code generation do nothing for `new` and
  `end`; the check of shared statics (ADR 0093) ignores them.

### Rules of the validator

ADR 0095 gains, for every body and for the program:

| Id | Rule |
|----|------|
| L1 | the place of `new` and of `end` is a local not bound to read-only data, followed only by `Field` and `Payload` projections, of a struct type marked linear |
| L2 | no struct is marked both copy and linear |
| L3 | no local's type and no static's type reaches, through arrays, slices, fields, payload values and the pointees of pointers, an array or a slice whose element type is linear; no static's type is linear |
| L4 | no assignment, call destination or `asm` output whose place goes through a pointer (`Deref`, `Offset`) has a linear type |

L3 is checked each definition once, as M4 is. A flag set wrong by MIR
building is caught by L2 or, through the kinds, by M1, M2 and M4 (ADR 0098).
Type checking rejects every program that would break L3 and L4 (ADR 0101),
so they judge the compiler, as every rule of the validator does.

### `end` in the analysis of moves

ADR 0097 makes reading, moving or taking the address of a place an error
when a part of it is moved. `end` is the exception: it ends what is left of
a struct whose linear fields were moved out, which is how such a struct is
taken apart (ADR 0102). So, in the analysis of moves (ADR 0099):

- `end` of a place is a use that is a fault only when a site whose path is a
  prefix of the place reaches it (the place, or a place containing it, is
  moved); sites of its parts are no fault;
- it then kills the sites related to the place and generates a site moving
  the place whole, with the span of `e`, so that a later use is
  `analysis::use-after-move` with "moved here" at `end`.

`new` writes nothing the analysis of moves follows: it names a temporary.

### The analysis of linearity

`src/analysis/linear.rs` checks ADR 0102's rules over one body's MIR.
`check_body` runs it after the analysis of moves, and only on a body that
analysis accepted: a value used after a move is reported once, by moves, and
not again as a value that was never consumed.

- A **linear path** is a local other than `_0` followed by `Field` and
  `Payload` projections, whose type is a struct marked linear: one for each
  linear part of each local's type. Every local is followed, temporaries
  included; `_0` is the value the function gives its caller.
- An **origin** is where a value comes into the function: a parameter (the
  body's span, the message naming the parameter), a `new` (the literal's
  span), or a call whose destination has linear paths (the call's span).
  A body has finitely many. A **fact** is a pair of a linear path and an
  origin: the path holds the value that came in there.
- The analysis is a forward analysis over the blocks reachable from `bb0`,
  of the set of facts that may hold at each point, joined by union and
  iterated to a fixed point, in bit sets over the pairs of a linear path and
  an origin whose value can be of that path's type. A linear path is
  **held** where a fact of it holds. Every transfer below kills the facts of
  some paths and adds facts computed from the state, a function that only
  grows with its input; over a finite set, the iteration ends. Blocks
  nothing reaches are not visited, as in the analysis of moves (ADR 0099).
- Facts come in:
  - at the start of the body, for every linear path of a parameter, with
    that parameter as origin;
  - at `new` of a place, for that place's own path, with the `new` as
    origin;
  - at a call, for every linear path its destination is a prefix of, with
    the call as origin.
- Within a block, each statement, then the terminator, is applied in order:
  1. its **operands**, left to right: a `move` of a place kills the facts of
     every linear path the place is a prefix of (the place and its parts);
  2. `end` of a place: a linear path that the place is a proper prefix of
     and that is held is a **fault** (not consumed at `end`); then the facts
     of every linear path the place is a prefix of are killed;
  3. its **write**:
     - `new` of a place: its own path, if held, is a fault (overwritten);
       its fields were written by the statements before it and are not
       looked at. Then the place's own fact comes in, as above;
     - a call's destination: every linear path the destination is a prefix
       of that is held is a fault (overwritten), and its facts are killed;
       then the call's facts come in;
     - an assignment `Use(move q)`: every linear path the destination is a
       prefix of that is held is a fault (overwritten), and its facts are
       killed; then, for each fact (`q` followed by `r`, `o`) that held
       before this statement, the fact (destination followed by `r`, `o`):
       what was held moves with the value, and keeps its origin. So a
       variant written without a payload gives nothing (`_7.tag = const 0;
       _2 = move _7`), and a literal's fields give what their values held;
     - no other write gives or faults: an `asm` output is never linear, and
       writing a copy field of a held `#[linear]` struct changes the value
       (`t.channel = 5`).
- On an edge out of a `switch` whose operand is `copy p.tag`, `p` a local
  followed by `Field` and `Payload` projections, the facts of the linear
  paths under `p`'s payloads of every variant that cannot be the value on
  that edge are killed: for the edge of a case, every variant whose
  discriminant is not the case's value; for `otherwise`, every variant whose
  discriminant is a case's. This is the only refinement: a tag copied to
  another local and tested there refines nothing. It relies on ADR 0098's
  rule that a `match` on a non-copy place tests the place itself, and on
  `?` doing the same; the MIR snapshot of a `match` and of a `?` on an
  `Option(Transfer)` (`tests/ui`) pins that shape.
- A `return` terminator is a fault for every held linear path (not consumed
  at the return). `unreachable`, `panic` and an `asm` with `noreturn` are
  none (ADR 0102, "Paths that do not return").

### Errors

Faults are found after the fixed point, in one pass over the reachable
blocks, and grouped into ADR 0102's diagnostics, **one error per value**,
that is per origin.

- A held fact (`p`, `o`) at a `return` is a fault of the value `o`, not
  consumed. So is a write over a held fact (`p`, `o`) whose incoming value
  has the same origin `o` (the write is that `new` or that call, or a move of
  a fact with origin `o`): the value came round a loop without being
  consumed, as `start(1);` (`_4 = call start(1)` over `_4`) or
  `let t: Transfer = start(1);` (`t = move _4` over `t`) in a loop. Each is
  `analysis::linear-not-consumed`, at the origin, naming the place `p`
  where it is held, with a label at each such return and write (for the
  `let`, the write `t = move _4`, whose span is the `let`'s).
- A write over a held fact (`p`, `o`) whose incoming value has another
  origin is a fault of the value `o`, overwritten:
  `analysis::linear-overwritten`, at that write, labeling the origin. A
  value reported as not consumed is not reported again as overwritten.
- A field held at `end` is `analysis::linear-not-consumed`, at `end`,
  labeling the field's origin.
- A place is named by its variable followed by its fields and payloads:
  `t`, `s.h`, and for a payload value the variant, as `o` (in `.Some`) or
  `s.h` (in `.Some`, value 2) when the variant has several; a temporary is
  "this value", at its origin. When the value was moved to the place where it
  is held, a label says where it was moved there. When a value is held by
  different places at different faults, each is named in its label.
- One origin can give several linear paths at once: a call returning a
  `Pair { a: Transfer, b: Transfer }`, or a `#[linear]` `Wrapper` with a
  `Transfer` field. The error of that origin names, in its labels, every
  path of it held at each fault, and only those: a part already consumed
  (`p.a` passed on, `p.b` forgotten) is left out, so the error names `p.b`.
- Spans and errors are ordered as ADR 0099 orders them, and an error
  repeated at one span with the same labels is reported once.

The cost is the number of blocks times the number of facts, in bit sets,
per iteration; a body's facts are few, its linear paths times the origins of
their types.

## Consequences

- `check_body` runs two analyses; whatever `query` does with a body that
  fails one is unchanged (ADR 0099).
- A token with no fields, `#[linear] struct Tok {}`, is checked as any other
  linear value: its literal is a `new`.
- A value known to be `.None` is precise when it is written in the function
  and reaches its variable by moves; from a call or a parameter, the
  analysis knows its variant only after a test of its tag.
- A change to MIR building that tests a tag through a copy would make valid
  programs fail; the snapshots above catch it.
- The dumps of MIR change only where a program builds a `#[linear]` struct
  or has `end`.
- `analysis` still names only `mir` (ADR 0063).

## Alternatives considered

- **`new` before the first field** — the literal holds a value while its
  later fields are evaluated, so a `?` among them would return with an
  incomplete literal held, and `Transfer { a: f()?, b: g()? }` would be an
  error.
- **No marker; a value given by the first write of a field** — no new
  statement, but a struct with no fields writes nothing, so a token would
  never be seen, and a discarded `Tok {};` leaves no trace in MIR.
- **`#[linear]` only on structs with at least one field** — a field of type
  `()` is written (`_1.0 = const ()`), so it would be seen without a marker;
  but that is a rule of the language made to suit MIR.
- **Every linear path given by a move out of a temporary** — no marker and
  no transfer of what is held, but a `.None` written in the function would
  need a `match`, and `Tok {};` still leaves no trace.
- **Lowering `end` to a `move` into a temporary nothing reads** — no new
  statement, but the analysis of moves would see a use of a partly moved
  place, which `end` allows, and the analysis of linearity would have to
  tell that temporary from a discarded call.
- **Following only variables and parameters, as moves does** — simpler, but
  a discarded call of a linear type, `make();`, would go unreported.
- **Tracking held paths instead of facts with origins** — smaller sets,
  but the errors could not say where the value came in, nor tell a value
  that came round a loop from one that overwrites another.
- **A new origin at each move, at the move's span** — simpler, but
  `let a: Transfer = start(1); let b: Transfer = a;` forgotten would be
  reported at `let b`, not where the transfer started.
- **Both `linear-not-consumed` and `linear-overwritten` for a value that
  reaches a return and its own write again** — one fault, two errors, which
  ADR 0097 and 0100 avoid.
- **Running both analyses on every body** — reports a use after a move a
  second time, as a value never consumed.
