# 0055. Repeat counts and dead arms under failed values

- Status: Accepted
- Date: 2026-10-21
- Amends: 0048

## Context

ADR 0048 makes a type that reaches a failed array length or alignment
erroneous, and lists what it silences. Two cases the compiler handles
are not in its text:

- **Repeat counts.** ADR 0048 names array lengths and `#[align(N)]`
  arguments. The count of `[x; N]` is also a value required at compile
  time (ADR 0029), and it can fail the same ways. Today a failed count
  behaves partly like a failed length: `let a: [u8; 4] = [0; 1 / 0];`
  silences the body, while `&raw const [0; 1 / 0]` still reports
  `ty::untyped-literal` and `ty::not-a-place`. Whether that follows a
  rule or happens by accident is undecided.
- **Dead arms.** ADR 0048 says a `match` on an enum that reaches a
  failed value is not checked for coverage. The compiler also skips the
  check for dead arms (ADR 0037) on it, and `docs/reference.md`
  documents what the compiler does, so the ADR and the reference
  disagree.

## Decision

- **A repeat count is an array length.** `[x; N]` has type `[T; N]`, so
  its count is the length of that type and, when it fails, a failed
  value in the sense of ADR 0048. The expression's type reaches it, and
  every rule of ADR 0048 applies. In particular, type errors of the
  expression that hold whatever the count are still reported: in
  `&raw const [0; 1 / 0]`, `0` has no expected type
  (`ty::untyped-literal`) and a repeat expression is no place
  (`ty::not-a-place`), whatever `N` is. This is what the compiler does
  today.
- **Dead arms are among the checks silenced.** A `match` whose scrutinee
  reaches a failed value is checked neither for coverage nor for dead
  arms. Both come from the same analysis over the type's variants and
  payloads (ADR 0040), whose payload types are erroneous, so a dead arm
  found there could be wrong. This is what the compiler does today.

## Consequences

- No change to the compiler: this records two of its behaviors as
  decisions, and the reference already describes them.
- A real dead arm in a `match` on such an enum is reported only once
  the failed value is fixed, as the other silenced errors are.

## Alternatives considered

- **Treat a failed repeat count as a value error only, not a failed
  length** — then `let a: [u8; 4] = [0; 1 / 0];` would report a
  mismatch between `[u8; 4]` and a length computed as 0, the cascade
  ADR 0048 exists to prevent.
- **Check dead arms on such a `match` anyway** — the check reads the
  same erroneous payload types as the coverage check, so it would
  report arms dead or alive from a wrong picture of the type.

## Conformance tests

- **ui (MIR):** `mir_failed_repeat_count.ers`: `let a: [u8; 4] =
  [0; 1 / 0];` reporting only the division, a `7 / 0` after it not
  reported; `&raw const [0; 2 / 0]` reporting the division,
  `ty::untyped-literal` and `ty::not-a-place`; a `match` on an enum with
  a `*const Broken` payload with a repeated arm, reporting nothing.
