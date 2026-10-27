# 0017. Literal operands of `as` take the target type

- Status: Accepted; amended by 0059 (a literal operand of `as` with a bit-field struct as its target takes the struct's integer type)
- Date: 2026-10-12

## Context

ADR 0008 gives literals the type expected where they appear and rejects a
literal with no expected type. The operand of `as` has no expected type: the
cast changes the type. So `1 as u32` and, worse, the most common bare-metal
idiom, `0x1000_0000 as *mut volatile u8`, were errors.

## Decision

A literal operand of `as` is checked against the cast's target type, or
against `usize` when the target is a pointer:

```
0x1000_0000 as *mut volatile u8   // the literal is a usize
300 as u8                          // error: literal out of range for `u8`
```

Any other operand of `as` is typed on its own, as before.

## Consequences

- Fixed addresses read naturally, with no intermediate variable.
- `as` on a literal behaves like a typed literal, including range checks.
- A literal still never gets a type that is not written somewhere nearby.
