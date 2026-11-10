# 0104. Variant values in MIR

- Status: Accepted
- Date: 2026-11-10
- Amends: 0039 ("In the compiler": the MIR type of an enum with payloads also
  stores the value of each variant, when it is known), 0095 (the rule E1)

## Context

The analyses of one body read only MIR (ADR 0093, 0099). ADR 0103 has the
analysis of linearity tell which variant a `switch` edge is for, so that the
payload of `.Some` holds nothing on the edge of `.None`: "for the edge of a
case, every variant whose discriminant is not the case's value; for
`otherwise`, every variant whose discriminant is a case's".

MIR cannot say that. An enum with payloads is a MIR type with its layout and
its payload values (ADR 0039, "In the compiler"); a `switch` holds the
values of its cases, and nothing links a value to a variant. The value of a
variant is in the HIR (`VariantValue`), and `Queries::discriminant` computes
it, which for an explicit value may run code (ADR 0035).

`mir::Mir::enums` lists the value of every variant, for the dumps (and for
the instances of `Option` and `Result`, ADR 0045), but the analysis cannot use
it: `check_body` runs on each body as `query` builds it (ADR 0099), before
`into_mir` assembles that list.

No ADR says that the MIR type must not hold the values, but the compiler is
built so, and `tests/ui/eval_enum_payload_values.ers` pins it: `mir_enum`
never asks for a value, because computing one can run a function that builds
a variant of the same enum, whose MIR needs the type, and ADR 0038 makes that
program valid (functions that run while an enum's values are computed may use
the enum when they need no value). A type that waited for its values would
make it a `ty::cycle`.

## Decision

- `mir::Variant` gains `value: OnceLock<u64>`: the tag that names the variant,
  as the bits of the enum's integer type, the value the `switch` of a `match`
  or of `?` compares. `EnumDef::set_value(variant, value)` stores it. The
  first value stays: a variant has one value, and a second store must be the
  same (a debug assertion).
- The value is stored when it is known, never waited for:
  - `Queries::mir_enum` stores, after the variants are set, those known
    without evaluating anything: a literal or implicit value, those of
    `Option` and `Result`, and any already computed;
  - `Queries::discriminant` stores a value when it finishes computing it, if
    the MIR type exists; a type made later finds it already computed.
  So a value may be missing while it is computed, and for good when it
  failed, which was reported.
- `Mir::enums` and `Variant.value` agree wherever both exist: `into_mir`
  asserts, in debug builds, that the type of every enum with payloads stores
  the value it lists, which every program without errors has.
- The validator gains a rule (ADR 0095):

  | Id | Rule |
  |----|------|
  | E1 | each case of a `switch` on the tag of an enum with payloads (`copy p.tag`, `p` a place) is the stored value of exactly one variant of that enum |

  It judges the compiler, as the other rules do: MIR building asks for the
  value of every case it writes, and the `switch` of `?` is on a builtin enum,
  whose values are known. A program in which two variants have one value, or a
  value failed, is rejected by `ty::duplicate-discriminant` or by the error
  of the evaluation, and the build stops before a body that matches on the
  enum is used (ADR 0035, 0038, 0040); E1 relies on that and does not check
  it again.
- The analysis of linearity reads the values from the MIR enum, and relies on
  E1: every case of a `switch` on a payload enum's tag is the stored value of
  some variant. On the edge of a case, only that variant is possible; on
  `otherwise`, no variant that has a case is. A variant whose value is not
  stored is none of the cases (two variants never have one value), so it is
  dead on the edge of a case and alive on `otherwise`. A case with no stored
  variant, which E1 rejects, refines nothing: that is only a fallback so that
  the analysis never decides from a missing value. The result on a case edge
  depends only on the values of the case's variants, not on the order the
  bodies were built in or on which other values happen to be stored by then
  (ADR 0028), and a value stored late changes no verdict.
- Nothing else reads the values. Code generation and the evaluator keep using
  the constants that building a variant writes.

## Conformance tests

- **ui (MIR):** `tests/ui/linear_enum_values.ers`: a payload enum whose
  values are out of order and one of which is computed by a function (already
  computed when the MIR type is made, since type checking the `match` asked
  for it), with a `match` that consumes each payload and one that forgets one.
  `tests/ui/enum_values_late.ers`: the value is computed by a function that
  builds a variant of the same enum, so the type exists while the value is
  being computed and takes it when it is done; the `match` on the tag passes
  E1, and `into_mir` asserts the list and the type agree.
- **ui (typeck):** `tests/ui/eval_enum_payload_values.ers`, unchanged in
  behavior: the program is accepted and the values are in the dump.
- **unit:** the tests of `src/analysis/linear.rs` give the enum its values,
  out of order and not stored, and check that the variant of an edge is found
  by value, not by position; the test of E1 in `src/mir/validate.rs` accepts
  cases that are values of one variant and rejects a case that no variant has,
  two variants with one value, and a variant with no stored value.

## Consequences

- An analysis tells the variant of a `switch` edge from MIR alone, as ADR 0093
  requires.
- The MIR type of an enum is still made without waiting for any value, so
  ADR 0038's guarantee and its test hold.
- `mir::Variant` has a field more; tests that build enums by hand give it
  (`OnceLock::from(n)`).
- A value stored late changes no verdict: an analysis needs only the values of
  the cases of the `switch`es it meets, which are stored before it runs.
- The validator has a rule more, which stops a change to MIR building that
  writes a case without asking for its value.

## Alternatives considered

- **A function from outside MIR, given to the analysis** (`&dyn Fn(EnumRef,
  u32) -> Option<u64>`, answered by `query`) — no change to MIR, but data
  reaches an analysis from outside MIR, against ADR 0093; and a value that is
  not yet known must be treated as "none of the cases", which is sound only
  by the argument above, kept out of sight in the caller.
- **Store the value in the type when it is made** (`value: u64`) — the
  simplest, and no value is ever missing, but it makes the type wait for the
  values: the program of `tests/ui/eval_enum_payload_values.ers`, valid by
  ADR 0038, becomes a `ty::cycle`. A placeholder (0) for a value in progress
  would be stored for good and be wrong.
- **Use `Mir::enums`** — the values are there, but only after `into_mir`, so
  the analysis of one body, which runs as the body is built, could not read
  them.
- **Link a case to its variant in the `switch`** (a variant index beside each
  value) — the analysis would need no table, but every `switch` and every
  consumer of it changes, for one analysis.
- **Refine nothing; treat `Option(Transfer)` like any enum** — a `.None` arm
  would owe the transfer, which ADR 0102 rejects.
