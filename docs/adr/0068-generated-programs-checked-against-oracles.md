# 0068. Generated programs checked against independent oracles

- Status: Accepted
- Date: 2026-10-29

## Context

The tests of the compiler know the answer in advance, because a person
or an agent wrote it down: snapshots, `tests/e2e` with its
`expect-output`, `tests/layout` against clang. That finds the bugs
somebody thought of. The two tests that look further do not check what
the program computes:

- `tests/fuzz.rs` mutates programs and demands only that the compiler
  does not panic, up to LLVM IR. A program compiled to the wrong code
  passes.
- `tests/e2e/comptime_differential.ers` does check the result: every
  operation runs once in the compiler (`comptime`, ADR 0027, 0029) and
  once on the target, with inputs C hides from LLVM. But the operations
  and the values in it are the ones its author listed.

Code is written more and more by agents on cheaper models (ADR 0063),
and the review that catches the odd case is the costliest part. The
compiler already holds two implementations of the language that share
no code below the MIR: the evaluator (`eval`) and LLVM through
`codegen`. Where they disagree on a program, one of them is wrong, and
no one has to know which answer is right to know that. Integer
arithmetic wraps and `MIN / -1` is defined (ADR 0007, reference), so
there is no undefined behavior for a generator to dodge.

## Decision

`tests/generated.rs` generates programs and runs each against two
oracles. Both exist today; neither is a hand-written expectation.

### 1. The programs

A generator in `tests/common/` writes, from a seed, programs made of
functions over the integer types (`u8` to `u64`, `i8` to `i64`, `usize`,
`isize`) and `bool`: operators `+ - * / % << >> & | ^`, unary `-` and
`!`, comparisons, `as` casts between integer types, `let` and `let
mut`, `if`/`else`, `match` on integers, and `while` and `loop` with a
trip count fixed by construction. The first version has no memory, no
pointers, no aggregates, no enums and no `extern`s but the opaque
inputs of rule 2; each may be added by a later ADR or by a step that
says so, since each widens what the oracles must agree on.

A generated program is valid: it type-checks without a diagnostic.
Where the language checks a value known at compile time (ADR 0032), the
generator keeps the operand unknown to the checker, as a function
argument is, instead of avoiding the operation: a divisor is a
parameter, so a zero is a run-time panic that the generator rules out
by its own choice of inputs, not by the shape of the program. A test of
the generator compiles a few hundred seeds to MIR and demands no
diagnostic, so that a generator that drifts into invalid programs fails
by itself and not as a false find of the compiler.

The generator is seeded by xorshift64, as `tests/fuzz.rs` is, and
takes no dependency.

### 2. The oracles

Each program has functions `f0`, `f1`, ... that take integers and
return a hash of what they compute, and a `main` that calls each twice:

- at run time, with arguments from C functions in a file the program
  links, as `comptime_differential.c` does, so LLVM cannot fold the call;
- in a `const` initialized by `comptime`, with the same literal
  arguments, so the evaluator computes it.

`main` prints `generated ok` and stops QEMU with exit code 0 when every
pair agrees, and otherwise the index of the first function that
disagrees. The test builds each program unoptimized and with `-O` and
runs both on QEMU. So three results must be equal: the evaluator's, the
unoptimized code's, and the optimized code's. A miscompilation by
`codegen`, a wrong fold by LLVM and a wrong step of the evaluator each
break the equality.

### 3. Running them

`cargo test` runs a small, fixed number of programs from a fixed seed,
so that a failure reproduces. `EREBOS_GENERATED_PROGRAMS` and
`EREBOS_GENERATED_SEED` raise the number and move the seed, for runs
that take longer than a test should (overnight, on a machine at rest).
A failure prints the seed and keeps the program's source in the test's
temporary directory, and the message says how to run that seed again.

A failing program is fixed by adding a test of its own (a `tests/ui` or
`tests/e2e` program), not by keeping the seed in the generated test.

## Consequences

- Bugs in integer arithmetic, casts, shifts and control flow that no
  one thought to list show up without anyone reading code: a reviewer
  no longer has to find them.
- Both oracles are the compiler's own. If the evaluator and `codegen`
  share a wrong reading of the language, the test cannot see it; the
  C compiler, which is independent, would (Alternatives).
- A test that runs QEMU costs time in every `cargo test`: the default
  count is chosen so that it adds seconds, not minutes.
- The generator is test code that has to be kept valid as the language
  changes, and grows with it.
- Reducing a failing program to a small one is not part of this ADR; a
  failure is a program of some tens of lines, not a one-line case.

## Alternatives considered

- **Compare with the same program in C, built by clang** — the oracle
  is independent of the compiler. It needs a translator from the
  generated form to C that matches Erebos's wrapping, shifts and
  division exactly (signed overflow is undefined in C, so every
  operation goes through unsigned types), and C is a third dialect to
  keep. Left for a later ADR, once this one shows whether the compiler's
  own oracles find enough.
- **Check only that `-O` and unoptimized output agree** — needs no
  evaluator, but a wrong result that both levels share passes. The
  evaluator is the cheaper second oracle, as it exists.
- **Property-based testing with `proptest`** — a dependency for what
  the seeded generator does, and its shrinking works on values, not on
  programs.
- **Coverage-guided fuzzing (`cargo-fuzz`)** — finds crashes and
  hangs in the lexer and parser, not wrong code, and needs a nightly
  toolchain and sanitizers that the MSVC setup of ADR 0011 does not
  have.
