# 0043. At most four witnesses searched

- Status: Accepted
- Date: 2026-10-18
- Amends: 0040 (a non-exhaustive `match` lists three missing values and
  says whether there are more, not how many)

## Context

ADR 0040 lists the values a non-exhaustive `match` misses as patterns,
witnesses, in the order its search finds them: the first three, then
"and N more". Counting N means finding every witness, and their number
can grow exponentially with the patterns. With
`enum P: u8 { A(bool, ..., bool) = 0 }` of `2n` positions and one arm per
`i < n` with `true` at positions `i` and `i + n` and `_` elsewhere, the
M2.4b compiler reports "and 4093 more" for `n = 12`, and each `n + 2`
takes about four times as long. Only three witnesses are shown; the rest
are found to be counted.

The count says little anyway: an integer position gives one example
value, not the values missing there (ADR 0040), so N counts patterns the
search happened to write, not values.

## Decision

The search for witnesses stops at the **fourth**. The first three are
shown, in the order ADR 0040 finds them; when a fourth exists, the list
ends with "and more":

```
match m {
    Msg::Nop => 0,
    Msg::Led(true) => 1,
    Msg::Write(0, 5) => 2,
}
// non-exhaustive `match`: `Msg::Led(false)`, `Msg::Write(0, 0)`,
// `Msg::Write(1, _)` and more not covered
```

- The label is "missing: " and the same list; the help still suggests an
  arm for each witness shown, or a `_` arm.
- With three witnesses or fewer, every one is listed, joined with "and",
  as today.
- This holds for every `match`, also one without arms on an enum of many
  variants, which lists its first three variants "and more".
- An integer scrutinee keeps the message of ADR 0037, which asks for a
  `_` or `let` arm and lists no witness.

What is missing and in which order are unchanged: only the count goes.

### Conformance tests

- **ui (typeck):** the example above; `match m {}` on an enum without
  payloads of five variants and on `Msg` ("and more"); exactly three
  witnesses (all listed, no "and more"); exactly four (three, "and
  more"); the `P` example of "Context" with `n = 32`, reported with three
  witnesses and "and more".

## Consequences

- The cost of reporting a non-exhaustive `match` no longer grows with the
  number of values it misses; finding the witnesses it shows can still be
  exponential in the worst case, as the third consequence says.
- The message no longer says how much is missing; a user who wants the
  rest adds the arms shown and compiles again, or adds a `_` arm.
- Deciding whether a `match` is exhaustive stays exponential in the worst
  case, since with `bool` positions it can express any boolean formula;
  no limit is imposed on it.

## Alternatives considered

- **The exact count** (ADR 0040) — informative for a handful of variants,
  but its cost has no bound.
- **Counting up to a limit**, as "and at least 100 more" past it — keeps
  "and 2 more" for small cases, at the price of one more rule and a number
  that still counts patterns rather than values.
- **Listing more witnesses**, such as ten — longer messages for the same
  information: what to add next.
