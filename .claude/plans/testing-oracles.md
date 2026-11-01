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
- Status: done
- Implements: ADR 0068, section 2 (what it needs)
- Changes: `tests/e2e.rs` (`run`, `compile_c`, `qemu_path`, the
  expectation reader) to `tests/common/`; `tests/e2e.rs` keeps its
  test and its output unchanged.
- Tests: `cargo test --test e2e` as before, with no snapshot or
  expectation changed.
- Decisions:
  - The harness is `tests/common/qemu.rs`, declared in `common/mod.rs`
    with `allow(dead_code)` like the other helpers (not every test runs
    programs). It moved `Expectation`/`expectation`, `compile_c`,
    `qemu_path` (private) and the two timeouts, unchanged in text.
  - `run` is split instead of moved whole: what is specific to `tests/e2e`
    (program naming from its directory, `.opt` file names, the work
    directory, the build) stays in `e2e.rs::run`; the boot became
    `qemu::boot(name, elf, log, wait) -> Run { status, output, events }`
    and the verdict `qemu::check(name, expected, run)`. Step 3 can call
    `boot` with its own wait and compare outputs itself, without header
    comments or the e2e naming.
  - `boot` removes the old log itself, so every caller gets a fresh one.
  - `EXIT_TIMEOUT` and `RUN_FOREVER_CHECK` are `pub` in `qemu` because
    `check` names the first in its message and `e2e` picks the wait.
  - Error texts are byte-for-byte those of before; no expectation or
    snapshot changed, and `cargo test --test e2e` passes.

## 3. test(generated): check generated programs against both oracles
- Status: done
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
- Decisions:
  - Default count `DEFAULT_PROGRAMS = 20`, seed 0. Measured: about 0.35 s
    per program (two builds, two QEMU boots), so `generated_programs_agree`
    with its two tests takes about 7 s in total (40 programs took 14 s;
    300 programs from seed 1000 took 104 s). Twenty keeps it to seconds;
    the long runs use the two variables.
  - `main`, `on_panic` and the `memory` block are all written by
    `Gen::main` in the generator (the plan said the `memory` block is added
    "where the program is built"); one place gives the whole text, and the
    block is harmless to the `--emit=mir` test of step 1. The UART and
    the test finisher are `peripheral`s as in `tests/e2e/hello.ers`.
  - The `comptime` side is a module-level `const CT_<f>_<k>: u64 = comptime
    f<f>(...);` (ADR 0068 says a `const`), the run-time side is
    `f<f>(opaque_<ty>(lit), ...)` inside `main`. A `bool` argument is
    `true`/`false` at compile time and `opaque_u8(0|1) == 1` at run time.
  - Each function is called with two sets of literals (`k` = 0, 1). A
    literal is, in eight, 0, 1, the type's maximum, minimum, -1 (maximum -1
    for unsigned), a small value in -10..10 or a random value of the width
    (twice). Edge values come first because that is where integer
    operations differ.
  - `main` stops at the first disagreement: `if bad == 0 { bad = check(n,
    ct, rt); }`. `check` prints `#n <comptime> <run time>` and returns `n`
    (the function's number from 1, since exit code 0 is success); the exit
    code is `n`, through the finisher's `(n << 16) | 0x3333`. `on_panic`
    exits 99, so a generated program that panics fails with `exit code
    Some(99)`.
  - The test compares through `qemu::check` with `Expectation {
    "generated ok\n", exit 0 }`, so a disagreement is reported with the
    output and exit code, plus any CPU exception.
  - `run_both` builds in `target/.../tmp/generated/<name>/` and deletes
    the directory on success; on failure the `.ers`, both ELFs and the
    QEMU logs stay, and the message gives the directory and the command
    that runs the seed again.
  - The C object of `OPAQUE_C` is compiled once per level in a
    `OnceLock`, shared by both tests, which run in parallel: a per-program
    object would have raced on the same file name.
  - The program waits `WAIT` = 10 s (the value of `qemu::EXIT_TIMEOUT`, but
    named here since it is this test's choice).
  - `a_changed_comptime_result_is_a_disagreement` takes seed 1, rewrites
    the initializer of `CT_0_0` to `(<it>) ^ 1`, which always differs, and
    demands a failure at both levels: exit code 1 (the first function) and
    the `#1` line of the output.
  - A bad number in `EREBOS_GENERATED_PROGRAMS` or `EREBOS_GENERATED_SEED`
    panics with its name instead of falling back to the default.
  - Found by running it: seed 1093 (`EREBOS_GENERATED_SEED=1093
    EREBOS_GENERATED_PROGRAMS=1`) disagrees only with `-O` (function `f2`,
    `#3`: comptime and unoptimized agree, the `-O` build gives a different
    value). Out of this step's scope (the compiler is not changed); it is
    a find for the planner, to turn into its own `tests/ui` or `tests/e2e`
    program once reduced. One in 300 programs from seed 1000 failed; the
    seeds 0 to 39 and 0 to 299 of the generator test pass.

## 4. test(rejected): inject errors into valid programs
- Status: done
- Implements: ADR 0069, sections 1 and 2
- Changes: `tests/rejected.rs`; the catalog starts with the codes of
  the table "Checks on values known at compile time" of
  `docs/reference.md`, and grows in step 5.
- Tests: `injected_errors_are_rejected`; a test that a snippet inserted
  into a program the compiler is known to accept without checking
  (a deliberately broken catalog entry in the test) is reported, so that
  the test is shown to be able to fail.
- Decisions:
  - Besides the two tests the plan names, `statement_boundaries_are_inside_functions`
    checks the place finder on a small text (strings, characters and
    comments with braces, dead code, a constant condition), since a wrong
    place would show as a false alarm of the main test.
  - The catalog is `CATALOG: &[Snippet { code, stage, place, text }]`; the
    ADR's "statements or declaration" is `Place::Statements` or
    `Place::Items`. The four codes of the reference table: shift (`1 << 32`
    into a `u32`), division (`7 / 0`), index (an array of 2 indexed by 2),
    and the field one, which is `Items`: a `struct InjReg: u8` with a 4-bit
    field, and a function that writes 16 in a literal. All use names
    `injected_*` / `Inj*`, which no base program has, and stage `Mir`
    (where these checks run).
  - Bases are every `tests/ui/*.ers`, `tests/ui/*/main.ers` and the same of
    `tests/e2e`, in sorted order; a directory program is injected only in
    its `main.ers`, compiled from its own path so its modules resolve.
    A base is used for a snippet only if it compiles to the snippet's stage
    without a diagnostic (compiled once per stage, per base).
  - From the seed (the `Rng` of the generator, `tests/common`), the bases are
    shuffled and the first `programs` that took an injection are used;
    each gets every snippet, at a place picked by the `Rng`. The variables
    are `EREBOS_REJECTED_PROGRAMS` (default 150) and `EREBOS_REJECTED_SEED`
    (default 0). Of the 478 bases about 210 compile to MIR, so a run of
    1000 takes all of them (840 injections, 5 s); the default takes 3 s.
    Bad numbers panic with the variable's name, as in `tests/generated.rs`.
  - Places are found by text, with no parser (the test may not use `src/`
    parsing): the line after the `{` that opens a function body, and after
    each line ending in `;` inside one. Dead code and unchecked code are
    avoided, since the reference says the checks do not look at them and
    the first version gave 7 false alarms in 228 injections (after
    `return`/`break`, after `let x: ! = loop {}`, after a call to a `-> !`
    function, inside `while BITS < 32`). The rule: a block is dead after a
    line of it with `return`, `break`, `continue`, `loop`, `: !` or a call to
    a function of the program declared `-> !`; a block whose header names
    `true`, `false` or a name in capitals is skipped with all it holds.
    Conservative on purpose: it drops some valid places (a block with a
    `return` in one line is dead for the rest of that block) rather than
    report a false alarm. With it, seeds 0 to 5 over all bases gave no
    false alarm. A remaining case would be an `if` whose two multi-line
    branches both return, followed by a statement; none exists in the bases.
  - A failure prints `file:line` of the inserted snippet, the code and the
    snippet, and for a wrong code the diagnostics; it gives the two
    variables to run it again. No file is kept (the injected text is the
    base plus the snippet at that line).
  - `a_broken_catalog_entry_is_reported` runs the same `run` function on
    `tests/e2e/hello.ers` with two broken entries: valid text under the code
    of an error (reported as "was accepted"), and an error under another
    code (reported as "was not reported"). Failing both ways shows the
    test can fail for a skipped rule and for a wrong one.
  - `injected_errors_are_rejected` also demands at least
    `min(programs, catalog length)` injections, so that a wrong base path or
    an empty set of bases cannot pass.
  - Not added to `additional_cargo_test_args` of `.cargo/mutants.toml`: it
    would be the one change outside the step's files; the test is fast and
    catches mutants of the checks, so the planner may want it there.

## 5. test(rejected): every code has a snippet or a reason
- Status: done
- Implements: ADR 0069, section 3
- Changes: `tests/rejected.rs` reads the codes of the check tables of
  `docs/reference.md`; the catalog gets the snippets for the other
  codes it can have, and the exempt list the reasons for the rest.
- Tests: `every_code_has_a_rule`; the test fails for a code added to the
  reference's table and to neither list.
- Decisions:
  - "The check tables" is the one table of the section "Diagnostics" of
    `docs/reference.md`, which lists every code (the other tables repeat
    codes of it). `reference_codes` reads the rows that start with
    ``| ` `` between `## Diagnostics` and the next `## `, and takes each
    backticked name of the first cell, so a row of two codes
    (`resolve::missing-entry`, `resolve::multiple-entries`) gives two.
    The test also demands more than 100 codes, so that a changed heading
    cannot make it pass on an empty list.
  - The rule is a function `rule_problems(codes, catalog, exempt)` so that
    the test of the test (`a_code_without_a_rule_is_reported`) can run it on
    small lists: it reports a code in neither list, a code in both, a
    snippet or an exempt name that is not a code of the reference (a typo,
    or a code removed), and an exempt group with no reason.
  - `EXEMPT` is `&[Exempt { codes, reason }]`, a reason per group, since
    most reasons are shared (the program's one `memory` block, `entry fn`,
    the `eval_*` programs). Reasons name the `tests/ui` program that
    covers the code when there is one.
  - The catalog went from 4 to about 140 snippets, one per code, written
    with a macro `s!(code, Stage, Place, text)`; rustfmt expands it, so the
    file is long. Stage is the first one that reports the code: `Ast` for
    lexer and parser codes, `Hir` for `resolve::`, `Typeck` for `ty::` and
    `Mir` for `eval::` (and the four of step 4). A base is skipped for a
    snippet only when it does not compile to that stage, as in step 4.
  - Statement snippets use only literals and their own `injected_*` names.
    A snippet whose error depends on its surroundings is an item: the
    errors of `break`, `?`, a peripheral, a pointer or a bit field need a
    function or a type of their own, so they are `Items` with a function
    (`fn injected_f`) that is never called. `parser::unclosed-block` is the
    item `fn injected_f() {`, since a `{` in the middle of a function is
    closed by the function's own `}`.
  - `resolve::multiple-entries`, `multiple-panics`, `duplicate-memory` and
    the other `memory` rules are injected as items with a second `entry
    fn`, `panic fn` or `memory` block at the end: it is wrong for every
    base, whether or not it has one. The memory snippets use addresses
    (`0x9000_0000`) no base uses, and names `INJ*`.
  - The `eval::` snippets are constants initialized by a `comptime` block
    (items), except `eval::assertion-failed`, a statement. `eval::step-limit`
    has no snippet: 10,000,000 steps in each of the injections would take
    seconds in a debug build.
  - Exempt (with reasons in the test): `resolve::missing-entry` and
    `missing-panic` (an absence, which text cannot add), `resolve::call-entry`
    (the entry's name is the program's), `ty::special-fn-params`,
    `panic-param`, `entry-returns`, `panic-returns` (they are about the
    program's one entry or panic function; a second one stops at
    `multiple-*`), the five `ty::` rules of the `memory` block (a second
    block stops at `duplicate-memory` in a base that has one),
    `eval::step-limit`, `eval::pointer-compare`, `dangling-pointer`,
    `uninit-read`, `pointer-out-of-bounds` (a whole program each),
    `codegen::llvm` (environment) and `link::no-memory-map` and
    `link::region-overflow` (found when building, and depending on the
    whole program).
  - `DEFAULT_PROGRAMS` goes from 150 to 40: with the full catalog 150
    programs took 31 s and 40 take 11 s. Every program still receives every
    snippet; only the number of places and bases drawn falls. 1000 programs
    (every base that compiles) took 130 s and gave no failure, so the
    snippets are right in every position the finder allows.
  - The second test of step 4's decisions, "not added to
    `additional_cargo_test_args`", still holds: the planner may want the
    test in the mutants run, now that it costs 11 s.

## 6. docs: document the generated and rejected tests
- Status: done
- Changes: `CONTRIBUTING.md` (Tests), `AGENTS.md` (project layout),
  `docs/adr/README.md` and the two ADRs set to `Accepted` by the user,
  not by this step.
- Tests: none; the step changes only documentation.
- Decisions:
  - `docs/adr/README.md` already lists 0068 and 0069 as `Accepted`
    (commit e79b979), so it and the ADRs are not touched.
  - The two tests get one bullet each in the Tests list of
    `CONTRIBUTING.md`, after the architecture bullet and before the bug
    fix rule. Each says what it finds, whether it needs QEMU, its two
    environment variables with defaults (20 programs and 40 base
    programs, seed 0), and how to reproduce a failure. The rejected bullet
    also states the rule that a new diagnostic code needs a snippet or an
    exempt reason, since `every_code_has_a_rule` will otherwise fail it
    for a contributor who has not read the test.
  - `AGENTS.md` project layout gets `tests/common/`, `tests/generated.rs`
    and `tests/rejected.rs` as one line each, in the style of the others.
  - `docs/reference.md` is not changed: it describes the language, not
    the tests.

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

## Left open

Minor findings that the reviews left open, found on 2026-11-01.

- Seed 1093 of `generated_programs_agree` disagrees only with `-O`
  (function `f2`, reported as `#3`; `comptime` and the unoptimized
  build agree). The compiler is out of scope here; it needs a reduced
  program as a `tests/ui` or `tests/e2e` test and a fix. Found in
  steps 3 and 6 (final review).
- `injected_errors_are_rejected` (about 11 s) and the generated test are
  not in the `additional_cargo_test_args` of `.cargo/mutants.toml`, so
  mutation runs do not use them. Found in steps 4 and 5.
- ADR 0068 section 1 says a divisor is a parameter; the generator uses
  `(e | 1)` (step 1 Decisions). The ADR text and the code differ; a note
  in the ADR's Consequences would close it. Found in the final review.
- `tests/e2e.rs`: the `build failed` format string has a literal line
  break instead of `\n`. Found in step 2.
- `tests/generated.rs`: `program.source[start..].find(";\n").unwrap()`
  has no `expect` message; `WAIT` copies `qemu::EXIT_TIMEOUT` and can
  drift from it. Found in step 3.
- `tests/rejected.rs`: `read_dir(...).unwrap()` in the base finder
  panics without saying why when `tests/ui` is missing; two multi-line
  string literals stand for `"\n## Diagnostics\n"`. Found in steps 4
  and 5.
- `CONTRIBUTING.md`: "the catalog of that file" in the rejected-programs
  bullet is ambiguous; say "the `CATALOG` of `tests/rejected.rs`".
  Found in step 6.
