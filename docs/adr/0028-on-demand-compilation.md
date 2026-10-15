# 0028. On-demand type checking and evaluation

- Status: Accepted; amended by 0033 (MIR building also asks for values)
- Date: 2026-10-14
- Amends: 0003

## Context

ADR 0003 runs the pipeline layer after layer: every function is type
checked, then every function is lowered to MIR. Compile-time execution
(ADR 0027) breaks that order. Checking `let buf: [u8; SIZE]` needs the
value of `SIZE`; computing it may call functions, which must be type
checked and lowered to MIR first, and their bodies may need other
constants in turn. In M6, a type such as `Ring(u8, 64)` exists only once a
function has run.

## Decision

Type checking, MIR building and evaluation run **on demand**, per item,
instead of over the whole program at once:

- Name resolution (HIR) still runs over the whole program first.
- When type checking needs a constant's value, it asks the evaluator,
  which type checks and lowers to MIR only what that evaluation needs,
  runs it in the MIR interpreter, and returns the value. Results are
  remembered, so each item is checked, lowered and evaluated once.
- An item whose evaluation needs its own value, directly or through
  others, is an error that shows the cycle (`ty::cycle`).
- Afterwards, every item not yet processed is checked and lowered, so
  every **non-generic** item is compiled, used or not. A function with
  `comptime` parameters (M6) is different: it is type checked only in its
  instances, so one that is never instantiated gets name resolution and
  nothing more, and **its errors are not reported** (ADR 0027). Generic
  code is validated by instantiating it, for example in a test.
- Instantiations nest (an instance may instantiate others) up to a fixed
  depth; beyond it, `ty::instantiation-depth` reports the chain of
  instantiations. This catches `fn f(comptime N: usize) { f(N + 1); }`,
  which creates new instances forever without necessarily exhausting the
  step budget of any single evaluation.

The direction of the layers does not change: each still consumes only the
output of the one before it, and code generation still sees only MIR. What
changes is the schedule, plus one narrow feedback edge: type checking may
ask for the value of a constant, and gets back a value, never MIR.

### Diagnostics in source order

The order in which items are processed depends on which was needed first,
so diagnostics are **sorted by the position of their primary label**
(file, then offset) before they are shown, keeping the order of emission
between diagnostics at the same position. Diagnostics without a label,
such as a missing entry function, come last, in order of emission. Output
never depends on the
schedule, so snapshot tests stay stable. Dumps (`--emit`) likewise show
items in source order.

## Consequences

- Constants, array lengths and (later) types can depend on code run at
  compile time, in any order in the source.
- The driver keeps per-item state (not started, in progress, done) instead
  of calling each layer once; in-progress items detect cycles.
- Errors in generic functions that are never instantiated go unreported.
- Sorting diagnostics changes the order of some existing snapshots once,
  where errors were reported in pass order rather than source order.
- The layers remain testable one by one.

### Conformance tests

- ui snapshots for `ty::cycle` (direct, and through several items) and,
  in M6, `ty::instantiation-depth`.
- A program whose items depend on each other against source order
  (a constant used before its declaration, by a function declared before
  the constant), compiled and run on QEMU.
- A snapshot whose diagnostics are emitted in an order different from
  source order, showing them sorted.

## Alternatives considered

- **Evaluate constants before type checking, with a separate evaluator for
  a subset of expressions** — keeps the strict order, but cannot call
  functions, and is thrown away when types become values.
- **Iterate the whole pipeline until nothing changes** — simpler
  bookkeeping, but repeats work and makes cycles hard to report.
