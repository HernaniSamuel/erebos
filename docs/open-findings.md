# Open findings

Minor findings the reviews left open, worked off between steps. The
`Left open` sections of the plans in `.claude/plans/` keep when and why
each was found; this file lists what is still open, each entry ending
with the step that found it. Remove an entry when it is fixed.

The findings M2 left open were worked off on 2026-10-21, and those of
M3.1 to M3.3 on 2026-10-28; these remain.

## Code

- Values computed at compile time are numbered after `core`'s bodies, so
  their `FnId` in a `hir` dump moves whenever `core` gains a body; no
  snapshot shows one today. `core`'s ids must be known before the
  program is lowered, and the number of values only after, so keeping
  them stable means renumbering after lowering; left until a snapshot
  needs it. (M2.6)

## Tests

- The `bss` check of `tests/e2e/memory_map.ers` and `memory_map_ram.ers`
  cannot fail on QEMU, which starts with RAM zeroed (their comments now
  say so); one that can fail needs the harness to fill RAM before the
  program starts, and the range `_start` zeroes is checked by
  `tests/elf.rs`. (M3.1)
- Seed 1093 of `generated_programs_agree` disagrees only with `-O` (function `f2`):
  reduce it to a `tests/ui` or `tests/e2e` program and fix it. (testing-oracles)
- The rejected and generated tests are not in `.cargo/mutants.toml`;
  the divisor of ADR 0068 section 1 differs from the generator (`(e | 1)`);
  small cleanups in `tests/e2e.rs`, `tests/generated.rs`, `tests/rejected.rs`
  and `CONTRIBUTING.md` are listed in `.claude/plans/testing-oracles.md`,
  `## Left open`. (testing-oracles)
