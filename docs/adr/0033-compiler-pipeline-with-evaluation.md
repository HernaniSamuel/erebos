# 0033. The compiler pipeline with compile-time evaluation

- Status: Accepted
- Date: 2026-10-15
- Amends: 0003 (the pipeline diagram and its layer rule), 0028 (MIR
  building, not only type checking, asks for values)

## Context

ADR 0003 drew the pipeline before compile-time execution existed. Its
diagram puts "Const evaluation" between type checking and MIR, producing
"addresses, sizes, layouts, offsets". Since ADR 0027 and 0028:

- the evaluator is a MIR interpreter, so it runs after MIR building, not
  before;
- it gives values back to type checking and to MIR building, through the
  on-demand schedule of ADR 0028;
- addresses are never known at compile time; they are relocations filled
  in by the linker (ADR 0027, 0030);
- layouts are computed from types by their own query, not by the
  evaluator.

ADR 0028 says each layer "still consumes only the output of the one
before it", which the old diagram no longer shows, and names one edge
back, from type checking. MIR building also asks for values (the value of a
constant or of a `comptime` expression it lowers). Accepted ADRs
cannot be edited, so the diagram is redrawn here.

## Decision

The pipeline, with the feedback edges ADR 0028 allows:

```
source
  -> SourceMap / Span      file ids, byte ranges; shared by everything
  -> Lexer                 tokens with spans
  -> Parser                AST (syntax only)
  -> Name resolution       HIR, for the whole program at once

  per item, on demand (ADR 0028):
  -> Type checking         typed HIR           <-- values --+
       uses Layouts        size, alignment, field offsets    |
  -> MIR building          MIR                 <-- values --+
  -> Evaluation            values, data with relocations  --+
                           (MIR interpreter, ADR 0027)
  -> MIR analyses          later: definite init, moves, borrows

  -> Codegen               LLVM IR -> object file, from MIR and evaluated data
  -> Link                  ld.lld + generated linker script
```

Rules, replacing rule 1 of ADR 0003:

1. A layer consumes the output of the layer before it. The only edges
   back go to the evaluator: type checking and MIR building ask it for
   values, and get back **values**, never MIR or types.
2. **Layouts** are a function of types, computed on demand by the rules of
   `src/layout.rs` for type checking (`size_of`), MIR building and the
   evaluator. They are not a pipeline stage. MIR types carry their layout,
   and codegen reads it from them, never from the checker's types.
3. **Codegen** consumes MIR and the data the evaluator produced (bytes and
   relocations), and nothing earlier: never the AST, HIR or types of the
   checker.
4. **Addresses of objects in the program** are never computed at compile
   time; data that points to them holds relocations for the linker. A
   pointer made from an integer (ADR 0019) is just that integer.

Evaluation runs MIR that the analyses of M5 have not checked yet; whether
they must run before it is decided with them.

The other rules of ADR 0003 (MIR is the core, `miette` diagnostics with
spans, modules in one crate, growing end to end) are unchanged.

## Consequences

- The diagram matches the code: `src/eval.rs` runs MIR, `src/query.rs`
  schedules the stages and asks for layouts, whose rules are in
  `src/layout.rs`, and `src/codegen` reads only `src/mir` and the
  evaluated data.
- `AGENTS.md` states the layer rule with the feedback edge.

## Alternatives considered

- **Leaving ADR 0003 as it is, with ADR 0028 as a note** — a contributor
  reading the diagram would put the evaluator in the wrong place.
