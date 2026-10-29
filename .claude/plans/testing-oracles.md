# Plan: testing-oracles Generated programs and injected errors

ADRs: 0068, 0069
Base: 909e717
Program on QEMU: generated programs (ADR 0068), each built unoptimized and
with `-O`; the compiler is not changed.
Not a roadmap step: it adds tests, not language. It does not wait for
M3.4a and may be built between roadmap steps; it touches no file of
`src/`.
Out of scope:
- a reducer for failing programs, the C oracle (clang) of ADR 0068's
  Alternatives, and generated memory, pointers, aggregates and enums:
  each wants its own ADR or a plan once these two show what they find;
- coverage-guided fuzzing (`cargo-fuzz`), mutation testing
  (`cargo-mutants`, configured in `.cargo/mutants.toml`) and a check
  that the files that guard the gate are not changed: separate work;
- moving `tests/fuzz.rs` or `comptime_differential` onto the new
  generator.

Order: the generator and its own test (1) come before anything runs
them; the harness that builds and runs a program on QEMU, now private to
`tests/e2e.rs`, moves to `tests/common` (2) so that the generated test
(3) uses it; the injections (4, 5) are independent of 1 to 3 and may be
built first.

## 1. test(generated): generate valid integer programs
- Status: todo
- Implements: ADR 0068, section 1
- Changes: `tests/common/generator.rs` (seeded xorshift64, the programs
  of section 1, one `.c` file with the opaque inputs), `tests/generated.rs`
  with the test of the generator only.
- Tests: `generator_makes_valid_programs`: a few hundred seeds compiled
  to MIR, no diagnostic; the same seed gives the same text; every
  operator, integer type and construct of section 1 appears in at
  least one program of the run, so that the test cannot pass without
  using them.

## 2. refactor(test): share the harness that runs a program on QEMU
- Status: todo
- Implements: ADR 0068, section 2 (what it needs)
- Changes: `tests/e2e.rs` (`run`, `compile_c`, `qemu_path`, the
  expectation reader) to `tests/common/`; `tests/e2e.rs` keeps its
  test and its output unchanged.
- Tests: `cargo test --test e2e` as before, with no snapshot or
  expectation changed.

## 3. test(generated): check generated programs against both oracles
- Status: todo
- Implements: ADR 0068, sections 2 and 3
- Changes: `tests/common/generator.rs` (`main` with the run-time and
  `comptime` calls), `tests/generated.rs` (build unoptimized and `-O`,
  run, compare), `EREBOS_GENERATED_PROGRAMS` and
  `EREBOS_GENERATED_SEED`, the failure message that prints the seed and
  keeps the source in `CARGO_TARGET_TMPDIR`.
- Tests: `generated_programs_agree`, with a default count measured so
  that the test adds seconds (the measure goes in `Decisions`); a test
  that a program whose `comptime` result is changed by hand is reported
  as a disagreement, so that the oracle is shown to be able to fail.

## 4. test(rejected): inject errors into valid programs
- Status: todo
- Implements: ADR 0069, sections 1 and 2
- Changes: `tests/rejected.rs`; the catalog starts with the codes of
  the table "Checks on values known at compile time" of
  `docs/reference.md`, and grows in step 5.
- Tests: `injected_errors_are_rejected`; a test that a snippet inserted
  into a program the compiler is known to accept without checking
  (a deliberately broken catalog entry in the test) is reported, so that
  the test is shown to be able to fail.

## 5. test(rejected): every code has a snippet or a reason
- Status: todo
- Implements: ADR 0069, section 3
- Changes: `tests/rejected.rs` reads the codes of the check tables of
  `docs/reference.md`; the catalog gets the snippets for the other
  codes it can have, and the exempt list the reasons for the rest.
- Tests: `every_code_has_a_rule`; the test fails for a code added to the
  reference's table and to neither list.

## 6. docs: document the generated and rejected tests
- Status: todo
- Changes: `CONTRIBUTING.md` (Tests), `AGENTS.md` (project layout),
  `docs/adr/README.md` and the two ADRs set to `Accepted` by the user,
  not by this step.

## Decisions made while planning

- ADR numbers 0068 and 0069: the next free numbers; two ADRs, as the two
  tests answer different questions and either may be accepted or
  dropped without the other.
- The plan is named `testing-oracles`, not after a roadmap step, since
  it is not one.
- The first generator has only integers and control flow, so that every
  disagreement is arithmetic or a cast, and the oracles are the ones the
  compiler already has.
- The e2e harness moves to `tests/common` (step 2) instead of being
  copied, so that a change to how a program runs on QEMU is made once.
