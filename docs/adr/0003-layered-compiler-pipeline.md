# 0003. Layered compiler pipeline

- Status: Accepted
- Date: 2026-10-12

## Context

The compiler must stay easy to maintain and to extend with hardware-oriented
features without rework. Coupling between phases (e.g. codegen reading the
AST, type checking reading tokens) is the main source of rework in compilers.

## Decision

The compiler is a strict pipeline. Each layer consumes only the output of the
layer before it:

```
source
  -> SourceMap / Span       (file ids, byte ranges; shared by everything)
  -> Lexer                  -> tokens with spans
  -> Parser                 -> AST (syntax only, no meaning)
  -> Name resolution        -> HIR (ids instead of names, resolved paths)
  -> Type checking          -> typed HIR (checking only, see ADR 0008)
  -> Const evaluation       -> addresses, sizes, layouts, offsets
  -> Lowering               -> MIR (CFG of basic blocks over explicit places)
  -> MIR analyses           -> definite init, moves, later borrow check
  -> Codegen                -> LLVM IR -> object file (ADR 0005)
  -> Link                   -> ld.lld + generated linker script
```

Rules:

1. A layer never reaches past its immediate input.
2. **MIR is the core.** New language features should lower to existing MIR
   constructs. Adding a MIR concept requires justification (and usually an ADR).
3. Diagnostics use `miette`, and every node carries a `Span` from day one.
4. Start as **modules inside one crate** (`syntax`, `hir`, `ty`, `mir`,
   `codegen`, `driver`, ...). Split into workspace crates only once a boundary
   has proven stable or compile times demand it.
5. Grow the pipeline **end to end**: every milestone runs through every layer,
   even if each layer is thin (see `docs/roadmap.md`).

## Consequences

- Each layer can be tested in isolation (snapshot tests per layer).
- Some boilerplate: AST -> HIR -> MIR means similar types defined more than once.
  This is accepted; the separation pays for itself.

## Alternatives considered

- **Single tree through all phases with annotations** — less code early, but
  every phase ends up depending on every other.
- **Many crates from the start** — premature; boundaries are not known yet.
