# 0015. Expression-oriented blocks and implicit unit return

- Status: Accepted
- Date: 2026-10-12

## Context

A guiding rule for Erebos is that code can be understood by reading it, with
no need to remember things the compiler does behind the scenes. That rule is
satisfied when a convention is visible where it applies.

## Decision

- **Blocks are expressions**, as in Rust. Statements end with `;`; a final
  expression without `;` is the value of the block; an empty block, or one
  ending in a statement, has type `()`. `if`/`else` is an expression.
- **`-> ()` may be omitted.** A function signature without `->` returns `()`.
  The convention is visible in the signature itself.
- A block of type `()` needs no trailing `()`.
- When a block ends in `expr;` but a non-`()` type is expected, the
  "mismatched types" diagnostic suggests removing the semicolon.

## Consequences

- A misplaced `;` is always a compile error, never a silent change of
  meaning: if the expected type is not `()`, the block's `()` type does not
  match; if it is `()`, there was no value to lose.
- Signatures stay short for the very common "returns nothing" case.
