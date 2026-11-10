# 0099. The analyses of one body

- Status: Accepted; amended by 0100 (the analysis of moves follows only
  variables and parameters), 0103 (`end` is an event of the analysis of
  moves)
- Date: 2026-11-09
- Amends: 0063 (the module `query` uses `analysis`)

## Context

ADR 0094 puts the analyses that look at one body (moves, linearity, borrows)
in `src/analysis/` (ADR 0093), running on each body right after it is built
and validated, before compile-time evaluation can run it and before it joins
the program's MIR; a body they reject is a body that failed to build. It
leaves to M5.2, which adds the first of them, the edge from `query` to
`analysis` in the tables of ADR 0063.

Today:

- `Queries::mir_body` builds a body with `mir::build_body` and caches it;
  `Builder::finish` validates it (ADR 0095) and returns `None` for a body that
  failed, which `mir_body` stores, so that whatever needs it stops (a
  constant's value, a `comptime` call). `Queries::comptime_value` builds the
  body of a `comptime` expression with `mir::build_expr_body` and evaluates it
  at once; it is never stored.
- `analysis::check(&Mir, ...)`, the check of shared statics, runs in the
  driver on the assembled program.
- The row of `query` in ADR 0063's `LAYERS` table does not name `analysis`.

## Decision

### Where it runs

`analysis::check_body(&Body, &mut Diagnostics) -> bool` runs every analysis of
one body, reports what it finds, and says whether the body passed. `query`
calls it:

- in `Queries::mir_body`, on the body `build_body` returned, before storing
  it: a body that does not pass is stored as one that failed to build;
- in `Queries::comptime_value`, on the body `build_expr_body` returned, before
  evaluating it: one that does not pass is not evaluated, and the expression
  has no value, as when its body fails to build.

Every body of the program goes through `mir_body` (functions, `comptime`
functions, the initializers of constants and statics, `core`), and every
`comptime` expression through `comptime_value`, so the evaluator runs no body
the analyses rejected. A body is analysed once: `mir_body` caches the body or
its failure, and `comptime_value` caches the expression's value or its
failure, so an expression used twice reports its errors once. A body whose
building failed is not analysed: its errors are those already reported.
Errors are reported where they are found, so nothing else is reported for a
value that depended on the body.

The analyses of the whole program stay in `analysis::check`, run by the driver
on the assembled program (ADR 0093).

### The tables of ADR 0063

The row of `query` gains `analysis`: the scheduler runs the analyses of one
body as it runs MIR building and evaluation (ADR 0028). The row of `analysis`
is unchanged: it names only `mir` and the shared vocabulary.

### The analysis of moves

`src/analysis/moves.rs` checks ADR 0097's rules over one body's MIR:

- A **move path** is a local followed by `Field` and `Payload` projections:
  the places a `move` can name (ADR 0098, M3). Each `move` operand of the body
  is a **move site**, with its path and span.
- A site is **related** to a place when its path is a prefix of the place (the
  place was moved, or a place containing it) or the place is a prefix of its
  path (a part of the place was moved). The tag of an enum is no part of its
  payload values, so a site moving a payload value is not related to the tag.
- A forward analysis over the blocks reachable from `bb0` computes, at the
  start of each, the set of sites that may reach it: the union of what its
  reachable predecessors give, iterated to a fixed point. Blocks nothing
  reaches, which MIR building may leave (ADR 0095), are not visited and feed
  no join. The cost is the number of blocks times the number of sites, in bit
  sets, for each iteration.
- Within a block, statements and then the terminator are applied in order,
  and within each, in this order:
  1. its **uses**, left to right as they are written in it: the operands, in
     order (the arguments of a call, the inputs of an `asm`), and the places
     of `&raw`, `SliceAddress` and `SliceLen`. The index locals of `Index`,
     `Offset` and `Element` are no uses: they are `usize`, which is copy and
     never moved. A use is reached by a fault
     when a related site reaches it. Each use kills every site related to its
     place (none, when it is valid); a use that is a `move` then generates its
     own site;
  2. its **writes**: the destination of an assignment, a call or an `asm`
     output. A destination is reached by a fault when a site whose path is a
     proper prefix of it reaches it: an assignment to part of a place moved
     whole. Sites of its own parts are no fault, since writing a part moved
     out gives it a value again. The destination kills every site whose path
     is a prefix of it, or that it is a prefix of: the place is valid after
     the write, every part of it included.
  So `p = set(p, true)` moves `p` in the call's arguments, then writes `p`,
  which kills the site the call just generated, and `p` is valid after it;
  and `f(a, a)` moves `a` at the first argument and uses a moved `a` at the
  second.
- Uses kill related sites whatever happens, so the analysis is a plain
  analysis of generated and killed sites, and its fixed point does not depend
  on the order blocks are visited in. Errors are found afterwards, in one
  pass over the reachable blocks from their sets at the fixed point: a use or
  a destination reached by a fault is `analysis::use-after-move` (ADR 0097).
  Killing at the use is what makes one fault one error: after an error on
  `a.f` where `a` was moved, the site moving `a` is killed on that path, so
  a later use of `a.g` is not reported either.
- The error is at the span of the `move` that is the use, or else of the
  statement or terminator, with a label for every site that makes the fault.
  A label's site is "in an earlier iteration of the loop" when its span
  starts after the use's, or when it is the use's own `move` reaching itself,
  as `consume(h)` in `loop { consume(h); }`: in both cases the move reaches
  the use only through a back edge.
- Spans are ordered by their start, then their end; locations by block, then
  statement index, the terminator last. Labels are in the order of their
  spans, then of their sites' locations. A body's errors are reported in the
  order of their uses' spans, then of their locations, and two errors at the
  same use span with the same labels, as MIR that repeats code for one source
  expression would give, are reported once.
- The help suggesting `#[copy]` (ADR 0097) reads the type of the moved place
  in MIR: the name of its `StructDef` or `EnumDef`, and whether each field or
  payload value is copy. `analysis` names nothing beyond `mir`.
- Locals bound to read-only data, statics and places through pointers are not
  move paths and are never moved (ADR 0098, M3), so the analysis ignores
  them.

## Consequences

- Compile-time evaluation runs only bodies the analyses of one body accepted,
  as ADR 0094 requires; ADR 0033's pipeline gains that stage.
- M5.3 and M5.4 add their analyses to `check_body`, with no new edge.
- `tests/architecture.rs` gains the edge from `query` to `analysis`.
- The analysis relies on the validator's rules (ADR 0095, 0098): it reads only
  MIR that passed them, and matches every operand and statement without
  checking them again.

## Alternatives considered

- **Running the analyses inside MIR building** (`Builder::finish`, after the
  validator) — no new edge, but `mir::build` would name `analysis`, and the
  validator, which judges the compiler, would sit beside checks that judge the
  program (ADR 0095, "Alternatives considered").
- **Running them in the driver, on the assembled program, as the check of
  shared statics** — no new edge, but evaluation would run bodies first,
  which ADR 0094 rules out.
- **Tracking moved paths instead of move sites** — smaller sets, but the
  error could not name the moves that reach the use.
