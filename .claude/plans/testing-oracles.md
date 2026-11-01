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
- Status: done
- Implements: ADR 0068, section 1
- Changes: `tests/common/generator.rs` (seeded xorshift64, the programs
  of section 1, one `.c` file with the opaque inputs), `tests/generated.rs`
  with the test of the generator only.
- Tests: `generator_makes_valid_programs`: a few hundred seeds compiled
  to MIR, no diagnostic; the same seed gives the same text; every
  operator, integer type and construct of section 1 appears in at
  least one program of the run, so that the test cannot pass without
  using them.
- Decisions:
  - `Program::uses` is a bit set of a `Feature` enum, built from the
    features that travel with each expression (`Ex::uses`) and recorded
    only for text that ends up in the program, since the generator drops
    expressions it made. The coverage assertion reads that set instead of
    searching the text, where ` + ` or `loop` could match by chance; a
    check that `Feature::ALL` has `Feature::COUNT` entries keeps a new
    variant from escaping the demand.
  - The shift-amount, cast and observation `as u64` forms count for the
    features they exercise (`ShiftByLiteral`, `ShiftByValue`, `Cast`,
    `CastBool`); the cast that folds a value into the hash `h` counts for
    none, since every program has it.
  - A divisor is `(e | 1)`, never zero, instead of a parameter whose
    inputs are chosen not to be zero (ADR 0068, section 1): the
    inputs are chosen by `main`, in step 3, and a divisor computed from
    them can be zero for inputs nobody picked. The `| 1` is not counted as
    a use of `|`. For signed `e` it is `-1` for some inputs, so `MIN / -1`
    and `MIN % -1` are reached.
  - A shift amount is a literal in `0..bits` or an unknown value of any
    integer type, signed ones included, which the target masks (reference,
    "Integer semantics"); a negative or large amount is where the two
    oracles could differ, so it is generated.
  - Where no type is expected (operand of `as`, left operand of a
    comparison, `match` scrutinee, shift amount, the value folded into
    `h`), an expression that reads a variable is asked for, with a typed
    leftmost position (the left of a binary operator, the first branch of
    an `if`, the first arm of a `match`): `(1 + x) as u8` and
    `(if c { 1 } else { x }) as u8` are `ty::untyped-literal`, and so is
    `(5 + 6) as u8`. Found by running the first version, which gave the
    latter two.
  - Bool operators are `!`, `==` and `!=` (and comparisons); `&&`, `||`
    and `& | ^` on `bool` are not generated, since ADR 0068 lists
    neither.
  - Not generated: `continue`, `break` with a value, `match` on `bool`
    and a call from one function to another; ADR 0068, section 1, does
    not list them.
  - Every expression is parenthesized (`if` and `match` too), so the text
    does not depend on operator precedence and the generator does not
    track it.
  - A program has 2 to 4 functions of 2 to 4 parameters, the first of
    them an integer, a `bool` one in one case out of six; each starts
    with `let mut h: u64 = 0` and ends with `h`, with `h = h * 1_000_003
    + (e as u64)` for each value it observes. The parameters and the
    top-level variables are all folded in at the end, so that no
    computation is dead.
  - Loops: a counter `u8` bound 1 to 4, in a `while c < N` or in
    `loop { if c >= N { break; } ... }`, nested at most twice (`BLOCK_DEPTH`),
    so a function runs at most 16 iterations of a body.
  - The program ends with a skeleton `entry fn main() -> ! { loop {} }`
    and `panic fn on_panic()`, since the compiler demands them
    (`resolve::missing-entry`); step 3 replaces `main` with the real one.
    It has no `memory` block: `--emit=mir` does not need one, and step 3
    adds it where the program is built.
  - The opaque inputs are `OPAQUE_C`, a constant with the identity
    functions for the ten integer types (`usize` as `uint32_t`, `isize`
    as `int32_t`, as in `comptime_differential.c`); the program declares
    the `extern`s. `bool` arguments are made in step 3 from
    `opaque_u8(v) == 1`, as the differential test does.
  - `Rng` is the xorshift64 of `tests/fuzz.rs`, seeded through a step of
    splitmix64, so that seeds 0, 1, 2, ... give unrelated programs
    (xorshift's first outputs of nearby seeds are alike).
  - `generator` is declared in `tests/common/mod.rs` with `allow(dead_code)`
    like the other helpers there, since every test that has `mod common`
    compiles it and only `tests/generated.rs` uses it.
  - 300 seeds, as the plan says "a few hundred": it takes about 2 s; a run
    of 5000 seeds, made by hand once, found no invalid program. The test
    also demands that the seeds give mostly different programs (more than
    nine in ten), so that a seed that is ignored fails.

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
