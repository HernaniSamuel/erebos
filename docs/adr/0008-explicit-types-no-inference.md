# 0008. Explicit types, no inference

- Status: Accepted; amended by 0024 (`*mut T` to `*const T`)
- Date: 2026-10-12

## Context

Type inference adds compiler complexity (unification, constraint solving) and
hides information that matters on bare metal: widths, signedness, layout.

## Decision

- Every `let`, parameter, return, field and static has an **explicit type**.
- There is no inference and no implicit conversion. Casts use `as`.
- Type checking is **bidirectional**: an expression may take its type from the
  expected type at that position. This is checking, not inference, and it is
  how literals get their types:

  ```
  let a: u8 = 200;   // ok: 200 checked against u8
  let b: u8 = 300;   // error: literal out of range for u8
  let c: u32 = a;    // error: expected u32, found u8 (use `a as u32`)
  ```

- A literal with no expected type is an error.

## Consequences

- The type checker is a single pass over typed HIR with no constraint solving.
- Code is more verbose, by design.
- Generics (later) will require explicit type arguments unless an ADR says
  otherwise.
