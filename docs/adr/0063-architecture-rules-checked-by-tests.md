# 0063. Architecture rules checked by tests

- Status: Accepted
- Date: 2026-10-28
- Amends: 0003 (the shared vocabulary grows beyond spans and diagnostics,
  to `int`, `ops`, `builtin` and `target`; `miette` is used only in
  `diag`), 0033 (rule 1: the evaluator names HIR items, by their ids
  only, to ask for them; `query` reaches every layer of the front end as
  the scheduler of ADR 0028)

## Context

ADR 0003 and ADR 0033 say which layer may consume which output, ADR 0007
forbids `nsw`/`nuw`, and AGENTS.md says user errors are diagnostics,
never panics. Until now only review enforced these rules: nothing failed
when `codegen` imported a type of the checker, or a new `unwrap()` sat
on a path user input reaches. Code is written more and more by agents,
some on cheaper models, and the review is the last line; a rule that a
test can check should not depend on a reviewer noticing.

The rules are about modules, not just ideas: `src/` has one module per
layer (ADR 0003, rule 4), so "codegen sees only MIR" becomes "the
module `codegen` names no module of the crate but `mir` and the shared
vocabulary", which a test reads from the source.

## Decision

`tests/architecture.rs` checks, as part of `cargo test`, the rules
below. Its tables (`SHARED`, `LAYERS`, `ONLY_IDS`, `EXTERNAL`, `PANICS`)
are the current list; the tables here are the list this ADR starts
with.

### 1. Layers

Every file of `src/` belongs to a module of `LAYERS`, the longest one
that starts its module path (`src/mir/build/known.rs` belongs to
`mir::build`), and names other modules only as its row allows. A module
may always use its own submodules.

The **shared vocabulary** may be used by every module: modules that
carry no layer's output and use no layer, only each other. They are
`span` and `diag` (ADR 0003: shared by everything), and `int`, `ops`,
`builtin` and `target`, which call themselves vocabulary the layers
share. Beyond it:

| Module | May also use | Why |
|---|---|---|
| `syntax` | nothing | lexer and parser read source only |
| `hir` | `syntax` | name resolution consumes the AST |
| `layout` | `hir` | layouts are a function of types (0033, rule 2) |
| `memory` | nothing | an evaluated memory map, data like layouts |
| `ty` | `hir`, `layout`, `query` | checks HIR; asks for layouts and values through `query` (0028) |
| `mir` | `layout`, `memory` | the MIR itself: the data codegen reads (0033, rule 3) |
| `mir::build` | `mir`, `hir`, `ty`, `layout`, `memory`, `query`, `eval` | builds MIR from typed HIR, asks for values (0033, rule 1) |
| `eval` | `hir` (ids only), `mir`, `query` | runs MIR; names HIR items only to ask `query` for them |
| `query` | `hir`, `ty`, `layout`, `memory`, `mir`, `mir::build`, `eval` | the on-demand scheduler (0028) |
| `codegen` | `mir` | MIR and evaluated data only (0033, rule 3) |
| `link` | `mir` | the memory map MIR carries |
| `driver` | `syntax`, `hir`, `ty`, `query`, `mir`, `codegen`, `link` | runs the pipeline |

The MIR and its builder are separate rows so that "codegen sees only
MIR" holds of the data: `mir` names no part of the front end, and an
item `mir` re-exports from `mir::build` (as `build_body`) counts as
`mir::build`. An item a layer re-exports from any other module (as
`mir` re-exports `MemoryMap`, or `hir` the access modes of `syntax`) is
part of that layer's output and counts as its own.

A path names a module when it starts with `crate::`, with `super::`
(resolved against the file's module), or with the name of a module the
file imported (`use crate::mir;` or `use crate::mir::{self, Body};`,
then `mir::build_body`). Each is read as a `use` tree over as many
lines as it takes. A `super` above the crate root fails, and so does a
glob import of any module outside the file's own layer, since what it
brings in is then used without a path.

`lib.rs` holds only `pub mod` declarations, and `main.rs` uses the
compiler only through `erebos::driver`.

### 2. External crates

`inkwell` is used only in `codegen` (ADR 0005) and `miette` only in
`diag`. No module names `llvm_sys`, the raw LLVM API that `inkwell`
re-exports, which would bypass the checks below.

### 3. No wrap flags

- Codegen names no builder method or flag of `nsw` or `nuw`
  (`build_int_nsw_add`, `set_no_signed_wrap_flag`, `LLVMBuildNSWAdd`):
  with `llvm_sys` out of reach, this covers every path of codegen, also
  those no test reaches and the code of `core`.
- Every program of `tests/ui` and `tests/e2e` that compiles is compiled
  to unoptimized LLVM IR, and no `add`, `sub`, `mul`, `shl` or `trunc`
  in it carries `nsw` or `nuw`; each of the five must appear at least
  once, so that the check cannot pass unchecked. Unoptimized IR runs
  `mem2reg` alone, which adds no flags; optimized IR is not checked,
  since LLVM adds the flags where it has proven them safe. The dumps
  leave `core` out, which the first check covers.

### 4. Panics

The test counts, in each file of `src/` outside its test module:
`.unwrap()`, `.expect(` but the parser's own `self.expect(` in
`src/syntax/parser.rs` (which reports a diagnostic), `::unwrap` and `::expect` through a path
(`.map(Option::unwrap)`), and `panic!`, `unreachable!`,
`unimplemented!`, `todo!`, `assert!`, `assert_eq!` and `assert_ne!`. It
does not count `debug_assert!`, indexing, slicing, or arithmetic that
overflows in a debug build. 246 are counted today.

The counts are compared with `PANICS`. A count that grows fails: the
report lists the files that grew, without a table to paste, and the
count is raised by hand once the step's `Decisions` say why the new
panic is an internal invariant no input reaches. A count that drops
also fails, until it is lowered, so the slack cannot build up and hide a
later panic.

### How the checks read the source

The checks blank out comments and the contents of string and character
literals, keeping line breaks, and cut each file at its test module: a
`#[cfg(test)]` at the start of a line followed by `mod name {`, which
must be the last item of its file. A test module in a file of its own,
or one followed by other code, fails rather than cut the wrong code. The
checks do not parse Rust; `the_checks_see_what_they_check` pins what
they recognize and what they reject.

### Lints

`Cargo.toml` turns on the clippy lints `dbg_macro`, `todo`,
`unimplemented`, `print_stdout`, `print_stderr` (only `main.rs` prints,
with an `expect` that says why) and `allow_attributes_without_reason`,
which the `-D warnings` gate makes errors.

### Changing the tables

An implementer does not change `LAYERS` or `ONLY_IDS` on its own: it
stops and asks. An edge the rules of ADR 0003, 0028 and 0033 allow is
added with an entry in the step's `Decisions`, approved by the user; an
edge they do not allow needs a new ADR. A new module of `src/` takes its
row the same way. `PANICS` follows its own rule above.

## Consequences

- Crossing a layer, using `inkwell` outside codegen, emitting a wrap flag
  or adding a panic fails `cargo test`, whoever wrote the code.
- The test sees the paths a file writes, not what reaches it through
  values: `mir::build` reads the checker's tables through `query`
  without naming `ty`, and `ty` could ask `query` for a MIR body without
  naming `mir`. Transitive access through `query`, and "values, never
  MIR" (ADR 0028, 0033), stay with review.
- Whether a raised `PANICS` count is justified is checked only in
  review; the test makes the raise visible in the diff.
- The panic table changes with many steps, also when code moves between
  files, since it counts per file.
- Being textual, the checks can still be fooled by code written to fool
  them, such as a path built by a macro. They are meant to catch
  mistakes, not to replace review.

## Alternatives considered

- **Split the layers into crates** (ADR 0003, rule 4) — Cargo would
  enforce the edges for free, but the boundaries are not stable yet and
  the split is a large change; the test can be dropped when it happens.
- **Parse the source with `syn`** — exact, but a new dependency for a
  check whose textual version is enough today.
- **Forbid `unwrap` with clippy (`unwrap_used`, `expect_used`)** — each
  of the existing uses would need an attribute of its own; the counting
  table keeps them visible without that churn.
- **Keep the module table only in this ADR** — accepted ADRs do not
  change, so the first new module would make it stale; the test holds
  the current table, and this ADR the rules for changing it.
- **Rename the parser's `expect`** — touches 39 call sites to make the
  count simpler; excluding `self.expect(` costs one condition.
