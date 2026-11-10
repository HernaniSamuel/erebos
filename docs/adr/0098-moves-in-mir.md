# 0098. Moves in MIR

- Status: Accepted; amended by 0103 (a struct's definition says whether it
  is `#[linear]`)
- Date: 2026-11-09
- Amends: 0095 (the operand `move`, and the rules M1 to M4 it must follow),
  0042 (a `match` on a place of a non-copy type tests the place itself, not
  a copy of it; a binding of a non-copy type is moved where its alternative
  has matched, not where its position is reached)

## Context

ADR 0097 says what moves; the analysis that rejects a use after a move reads
MIR (ADR 0094, 0099). MIR today (ADR 0009, 0095):

- reads every value with `copy` (`Operand::Copy`); nothing in MIR tells a
  copy type from a non-copy one;
- has a span per statement and per terminator, none per operand. Most reads
  are statements of their own, with the span of their expression (a field, an
  element, `*p`, a static: `_t = copy _a.f` under the span of `a.f`). Reading a
  local is not: the operand `copy _a` is handed to whatever consumes it, so
  `let b: S = a;` is `_b = copy _a` under the span of the enclosing block or
  function, and both arguments of `f(a, b)` carry the span of the call. A
  diagnostic pointing at "moved here" needs better;
- copies the scrutinee of a `match` into a temporary when it is a named local
  or has a projection, and tests that copy (`Builder::match_`), which would
  move a non-copy scrutinee whole (ADR 0097 says it does not);
- copies what a pattern binds where its position is reached, before the
  tests after it (ADR 0042, "Where bindings are copied"): an alternative that
  binds a value and then fails has already copied it, and the next
  alternative of the chain copies it again. As copies, that is harmless; as
  moves, the second would be a use after a move that the program never does.

## Decision

### The operand

`Operand::Move(Place, Span)` reads the value in the place, as `copy` does, and
says that the place is moved. The span is that of the expression whose value
is moved: the local's name in `let b: S = a;`, an argument in `f(a, b)`, the
field expression in `s.f`. Dumps print `move _1`.

Compile-time evaluation and code generation read a `move` exactly as a `copy`:
the machine operation is the same (ADR 0021), and nothing is written to the
place moved from. The check of shared statics (ADR 0093) treats it as a read.

### Kinds in MIR

`StructDef` and `EnumDef` carry whether the type is copy: for a struct or an
enum with payloads, whether it is declared `#[copy]`; for `Option(T)` and
`Result(T, E)`, whether their arguments are copy (ADR 0096). MIR building sets
the flag from type checking's answer for the HIR type it lowers, so the kind
is decided in one place. `mir::Ty` gains `is_copy`, which reads the flag of a
struct or an enum with payloads, `Option` and `Result` included (they are
enums with payloads in MIR, with no path of their own), takes an array's from
its element, and follows ADR 0096's list for the other types (integers,
`bool`, `()`, pointers), which have no flag. So the
analyses and the validator know the kind of a type without the front end. A
flag set wrong is caught by M4 (a copy type with a non-copy part) or by M1 and
M2 (a read whose operand disagrees with its type), which the validator checks
on every program of the tests.

### Where MIR building moves

Every operand that reads a whole place of a non-copy type is a `move`; one of
a copy type stays a `copy`. This covers the temporaries MIR building makes
(the value of a literal, the result of a call, the operand of `?`, an
argument copied before a later one runs), whose span is that of the
expression that made them. Type checking has rejected a move out of any place
but a local followed by fields (ADR 0097), so the place of every `move` is
one.

Moves appear in the order the program evaluates them (ADR 0018): the operands
of one statement or terminator are evaluated left to right, and whatever is
evaluated between two of them is in statements before it. MIR building keeps
this today because it reads a local into a temporary before a later operand
that is not simple runs (`Builder::operands`), so `f(a, g(a))` is
`_t = move _a`, then the call of `g` with `move _a`, then the call of `f`;
this ADR makes that a rule MIR building must keep, on which the analysis of
ADR 0099 relies.

An expression statement whose expression is a discarded place (ADR 0097)
computes the place and reads nothing: no operand is made for it.

`[v; N]` of a non-copy type, which ADR 0096 allows only for a variant written
without payload values, is not built as `v` evaluated once into a temporary
and copied into each element (`Builder::fill`), which would move the temporary
N times. Each element is written instead by storing the variant's tag into it
(`a[i].tag = const k`, in the loop or the stores `fill` makes), as building
the variant writes it (ADR 0039, the other bytes left as they are); no
operand reads a non-copy value, and the destinations are no move paths.

### `match` on a non-copy value

- A `match` whose scrutinee is a place of a non-copy type tests that place,
  and its arms bind from it, without a temporary; the tags and the values
  tested are copy, so the tests read the place without moving it. Such a
  scrutinee is an enum with payloads, `Option` and `Result` included: a
  `match` on a struct or an array is `ty::match-type` (ADR 0037). A scrutinee
  of a copy type, or one that is not a place, is read as today.
  Type checking checks a scrutinee as a read of its whole place, so a non-copy
  scrutinee through a volatile pointer is rejected as a whole aggregate read
  through a volatile place (ADR 0023), as today.
  No code of the function runs between the tests, so testing the place gives
  the values a copy would hold. A handler may run between two tests, but a
  static it shares with this code is read only inside `critical` (ADR 0078,
  0093), and memory reached through a raw pointer is unchecked, as every
  access through one is. A non-copy type is an aggregate, never read through a
  volatile place (ADR 0023), so no read that must happen once is duplicated.
- A binding of a non-copy type is assigned, by `move`, at the end of its
  alternative: after the last test of that alternative passes, before the
  jump to the arm. Type checking has rejected one inside a nested or-pattern
  (ADR 0097), so its position is fixed within its alternative. A binding of a
  copy type is copied where its position is reached, as ADR 0042 says.
- A `let` arm on a non-copy scrutinee moves it into its variable where the arm
  is entered, as it copies it today.

### Rules of the validator

ADR 0095 gains, for every body:

| Id | Rule |
|----|------|
| M1 | a `copy` operand reads a place of a copy type |
| M2 | a `move` operand reads a place of a non-copy type |
| M3 | the place of a `move` is a local not bound to read-only data, followed only by `Field` and `Payload` projections |
| M4 | a struct or an enum with payloads marked copy has fields and payload values of copy types only; checked for the type of every local, through arrays, fields and payload values, each definition once |

A place whose projections T1 rejects is not checked against M1 to M3, as
ADR 0095 says of every rule ("No cascade"). M1 and M2 together make every
read of a non-copy value visible as a `move`, which the analysis of ADR 0099
relies on.

## Consequences

- The MIR dumps of the tests change wherever a non-copy value is read, and
  wherever a `match` on one binds.
- Every pass that matches on operands handles `Move`; the validator, the
  evaluator, code generation and the check of shared statics read it as a
  `copy`.
- A diagnostic about a move points at the expression that moved, not at the
  statement around it.
- The non-copy bindings of an alternative are moved in one place each, so an
  alternative that fails moves nothing.

## Alternatives considered

- **The analysis infers moves from types, MIR keeping `copy`** — less to
  change now, but dumps would not show where values move, the validator could
  not tie a read to its kind, and the spans would stay those of statements.
- **A statement per local read, to give it a span** — precise spans without a
  new operand, but more temporaries and copies at `-O0`.
- **Spans of operands in a side table of the body** — keeps `Operand`
  unchanged, but a table indexed by location must be kept in step with every
  change to the body.
- **Every binding moved or copied at the end of its alternative** — one rule
  for all types, but it changes the MIR of every `match` that binds, for no
  gain with copy types.
