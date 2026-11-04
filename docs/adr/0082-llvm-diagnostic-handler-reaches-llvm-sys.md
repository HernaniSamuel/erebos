# 0082. The LLVM diagnostic handler reaches `llvm_sys`, in one file

- Status: Accepted
- Date: 2026-11-04
- Amends: 0063 (rule 2: `llvm_sys` is named in no module but
  `src/codegen/diagnostics.rs`, which holds only the diagnostic handler;
  rule 3: the wrap-flag check still covers all of codegen, and new checks
  keep the file to the handler)

## Context

ADR 0079 ("Errors from the assembler") makes an instruction the assembler
rejects `codegen::asm`, at the span of the `asm` expression, and says
that "LLVM's default reaction to such an error ends the process, so code
generation installs its own handler". LLVM reports the error to the
diagnostic handler of the context (`LLVMContextSetDiagnosticHandler`),
and when none is set it prints the message and calls `exit`. A handler
reads the diagnostic with `LLVMGetDiagInfoDescription` and
`LLVMGetDiagInfoSeverity`.

`inkwell` 0.10.0 has `Context::set_diagnostic_handler` as `pub(crate)`,
so the safe API cannot install one. The three functions are in
`llvm_sys`, which ADR 0063 (rule 2) forbids in every module, and
`tests/architecture.rs` enforces (`FORBIDDEN`). The reason is the size of
the raw API: it has every `LLVMBuild*` function and flag setter, a much
larger surface than the safe methods, and the check of rule 3 on wrap-flag
names (`build_int_nsw_add`, `LLVMBuildNSWAdd`) was not designed around it.
With `llvm_sys` out of reach, that check covers every path of codegen.
The handler needs none of that API, but the rule has no exception.

## Decision

`llvm_sys` stays forbidden in every module except one file,
`src/codegen/diagnostics.rs`, the module `codegen::diagnostics`. It is
part of `codegen`: `inkwell` is allowed in it as in the rest of the
layer, and it needs no other module of the crate.

### What the file holds

Only the handler: the callback LLVM calls, its installation on a
context, and the reading of a diagnostic's text and severity, handed to
the rest of codegen as plain Rust data (a `String` and an enum of its
own with the four severities). It builds nothing: no builder, no call
that creates or changes a module, a function, a value or an instruction,
and no name of a wrap flag. It names these items of `llvm_sys` and no
others:

- `LLVMContextRef`, `LLVMContextSetDiagnosticHandler`;
- `LLVMDiagnosticInfoRef`, `LLVMGetDiagInfoDescription`,
  `LLVMGetDiagInfoSeverity`;
- `LLVMDiagnosticSeverity` and its variants `LLVMDSError`,
  `LLVMDSWarning`, `LLVMDSRemark`, `LLVMDSNote`;
- `LLVMDisposeMessage`, which frees the description.

Reaching any other part of the raw API needs a new ADR. The context
pointer to install comes from the safe `Context::raw()`.

### How it is sound

- The pointer LLVM hands back to the callback points to a collector: a
  value of the file that holds the diagnostics seen so far behind a
  `RefCell`. The callback takes only a shared reference to it.
- The collector belongs to the caller of the emission and is declared
  before the `Context`, so it is dropped after it and outlives every call
  LLVM can make to the handler.
- The callback never panics, since a panic in an `extern "C"` function
  aborts the process: it holds no `unwrap`, `expect`, indexing or
  assertion, treats a null description as empty and decodes text that is
  not UTF-8 lossily. The file has no row in `PANICS`.
- The state is per context, never a `static`: tests compile in parallel
  in one process, and a build is a function of its sources (ADR 0028,
  0033).
- Every `Context` that codegen creates gets the handler: the one of
  `emit_llvm_ir`, the one of `emit_object` and those of the scratch
  modules that find which `asm` the assembler rejects (ADR 0079). A
  context left without it would end the process on an assembler error.

### What the severities become

An `Error` is collected; a build that collected one fails and its object
is discarded, even when LLVM went on to produce one. The rest of codegen
turns it into `codegen::asm` or `codegen::llvm` (ADR 0079). A `Warning`,
a `Remark` and a `Note` are dropped: LLVM no longer prints them, and the
compiler reports none of them.

### What it does not cover

`report_fatal_error` and `abort` in LLVM do not go through the diagnostic
handler and still end the process. This ADR does not install a handler
for them (`LLVMInstallFatalErrorHandler` is process-wide, and would be a
new ADR). So the sentence of ADR 0079 that no template ends the compiler
holds for the errors the assembler reports through the handler, not as a
guarantee about every failure of LLVM.

### What the tests check

`tests/architecture.rs` changes in three places:

- `FORBIDDEN` keeps `llvm_sys` and gains an allow-list of one file:
  `external_crates_stay_in_their_module` skips the name in
  `src/codegen/diagnostics.rs` and reports it everywhere else, as today.
- A new test, `the_diagnostic_handler_stays_a_handler`, reads that file
  and fails when it names an identifier that starts with `LLVM` and is
  not in the list above, an identifier that starts with `build_`, the
  type `Builder`, or a wrap flag (`names_wrap_flag`). It also fails when
  the file is missing, so that the allow-list cannot outlive it.
- A new test fails when a file of `src/` holds an `extern` block or
  `link_name`, which would reach LLVM without the word `llvm_sys`, and
  when `extern "C" fn` is defined in a file other than `diagnostics.rs`.

`codegen_names_no_wrap_flag` is unchanged: the file is in `codegen`, so
the existing check on wrap-flag names covers it, as it covers the rest of
codegen. For every file but this one, `llvm_sys` is out of reach; for
this one, the tests keep the names it uses to the list. They check
names, not what the code does with them (see Consequences).

## Consequences

- An assembler error in inline assembly becomes a diagnostic with a span
  and the compiler exits with its own status, as ADR 0079 requires; no
  new dependency, since `llvm_sys` comes with `inkwell`.
- The one use of the raw API is in one small file that a reviewer reads
  whole; the tests keep it from growing into a way around rules 2 and 3.
  The file is `unsafe` code, as are the two uses of `const_gep` and
  `build_gep` in `src/codegen/mod.rs`; `unsafe` is not what the rule
  limits.
- The rule stays textual (ADR 0063): code written to fool it, such as a
  name built by a macro, is not seen, and a name in the list could be
  called with arguments that change what it does. The tests catch
  mistakes; review still covers the file.
- The list of names is part of `tests/architecture.rs`; a handler that
  needs one more function changes it with a new ADR, not on its own
  (ADR 0063, "Changing the tables").
- If `inkwell` exposes the handler, the file goes, the exception with it,
  and `FORBIDDEN` has no allow-list again. That is a new ADR that
  supersedes this one.
- Warnings and remarks of LLVM are no longer visible; if one proves
  useful, reporting it is a change to the ADR.

## Alternatives considered

- **Declare the three C functions by hand in an `extern "C"` block**,
  which avoids the word `llvm_sys` — it passes the name check and bypasses
  its intent: the same raw API is reached, in any module, with nothing to
  keep it to the handler. The extern check above closes this path.
- **Wait for `inkwell` to expose the handler** — blocks M4.1 on another
  project's release; the exception can be removed when it does.
- **Do not use LLVM's handler** — LLVM then prints the message and ends
  the process on an assembler error, which ADR 0079 rules out. Assembling
  each template alone in a child process would catch the exit, at the
  cost of a process per `asm` and a message the compiler cannot read.
- **Allow `llvm_sys` in all of `codegen`** — the rule against wrap flags
  would again depend on review for the whole layer.
