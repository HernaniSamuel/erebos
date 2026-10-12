# 0004. Hand-written lexer and parser

- Status: Accepted
- Date: 2026-10-12

## Context

Good diagnostics are a primary goal. `pest` was considered: it makes the first
grammar fast to write, but its errors are generic ("expected X, Y or Z"), it
has no error recovery (stops at the first error), and its parse tree still has
to be converted into an AST by hand, so every grammar change touches two places.

## Decision

The lexer and parser are written by hand:

- A hand-written lexer producing tokens with spans.
- A recursive-descent parser, with **Pratt parsing** for expressions
  (precedence and associativity in one table).
- Error recovery at statement/item boundaries, so one run reports many errors.
- The parser produces the AST directly; there is no intermediate parse tree.

## Consequences

- Full control over error messages and recovery.
- More code up front (on the order of 1–2k lines for a Rust-like syntax).
- The grammar lives in code; `docs/` should keep an informal grammar reference
  in sync.

## Alternatives considered

- **pest** — see context.
- **`logos` for the lexer** — viable if the hand-written lexer becomes a burden;
  not needed initially.
- **Parser combinators (chumsky, nom)** — better errors than pest, but adds a
  heavy abstraction for little gain over plain recursive descent.
