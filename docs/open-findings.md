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
- The `ty::step-too-small` label in `src/query/hardware.rs` reads "each
  element of `u8` takes 1 bytes"; the plural is wrong for 1. (M3.4a)
- The overlap check of ADR 0064 costs about the shorter length per pair of
  entries, so a user-written `[T; 0x7FFF_FFFF]` stalls the compiler for
  minutes; the ADR accepts it, but `docs/reference.md` does not say so.
  (M3.4a)
- `src/query.rs` (around lines 374-385) and `src/ty/pattern.rs` (lines
  65-67 and 325) use `diags.len()` before and after a call to decide `ok`
  and to count diagnostics reported elsewhere. `len` counts warnings too,
  so the first warning a compiler layer emits would mark a function as
  failed; count errors only when adding it. (M3.4b)
- The SVD reader looks siblings up linearly (`lookup` builds a vector of
  every peripheral id for each `derivedFrom`), so many derived peripherals
  cost quadratic time (20 000 take 11 s in a debug build); a map from name
  to id per parent would fix it. The generator is quadratic in the number
  of peripherals too (each is compared with every earlier one): 65 536
  stacked peripherals in a 300-byte file take minutes, with bounded output
  and memory; a cap on peripheral lists would bound the time. (M3.4b)
- ADR 0071 makes the generator's output quadratic in the number of equal
  names ("`_` as many times as it takes"); there is no cap. (M3.4b)
- `src/svd/names.rs`, `suffix`: the `is_reserved` branch cannot fire and
  `Converted.changed` is never read; `src/svd/fields.rs` (around line 643)
  falls back to an integer or `bool` without a word if a planned enum is
  missing (only input that already gives `svd::invalid-name` reaches it).
  (M3.4b)
- `erebos svd tests/svd/chips/esp32c3.svd` prints about 83 KB of warnings:
  the `svd::renamed` warning of `RTC_CNTL` has labels 1 600 lines apart and
  miette draws every line between them. Options: leave it, put the other
  item's position in the message, or print far-apart labels as separate
  snippets in `render`. (M3.4b)
- The `svd::renamed` and `svd::invalid-name` warnings for an alternate
  group, and `svd::not-declared` and `svd::overlap`, label the whole
  element, since the model has no span for `alternateGroup`. (M3.4b)
- `src/link.rs` (line 16): the comment of `UNLOADED` cites
  `attributes_section`, which does not exist (the match is inside
  `unloaded(arch)`); `src/codegen/mod.rs` (`lower`, line 109) has
  redundant parentheses in `(mir.bodies()).map(...)`. (M3.5)
- `div64_false_links_with_core` (`src/codegen/mod.rs`) checks only that the
  build and link succeed, not the values, and writes to a fixed directory
  (`erebos-codegen-div64`) that two concurrent test processes would share.
  (M3.5)
- `src/syntax/parser.rs` (`report_unexpected`, `report_unexpected_help`): a
  string literal met inside an `asm` where the operand takes none
  (`in(reg) "str"`, `sym "x"`) is `parser::string-outside-asm` ("string
  literals exist only inside `asm`"), which misleads there; the step 1
  Decisions say `asm` uses its own `asm_expected`, true only for its direct
  arguments. Also the lexer's "unterminated string literal" message and its
  `asm` help are pinned by no snapshot (a unit test checks the code only);
  `quoted` picks the help with `open.starts_with('b')`. (M4.1)
- `src/mir/mod.rs` (`Body::for_each_asm`): clones and `Box::leak`s the
  `InlineAsm` of every `noreturn` terminator on each call, and `leave_out`
  and the callers of `for_each_operand` call it more than once per body; it
  could leak only when `f` changes something. `Terminator::InlineAsm`
  holds a leaked `&'static`, a per-compile leak in long test processes. (M4.1)
- `const` operands reuse `comptime` evaluation, so an evaluation error
  inside one says "this `comptime` expression" (one origin text in
  `query`); `in("")` prints "`` is no register". `ty::asm-register` and
  `eval` could carry their own wording. (M4.1)
- `src/codegen/asm.rs`: a `sym` naming a function that is also a register
  name (`x10`) is pasted into the template unquoted. (M4.1)
- `rt/builtins.ers` (the header of the 64-bit division section): "A zero
  divisor never reaches them, since division checks happen before the call
  (ADR 0007)" is true only for Erebos code now that C code reaches
  `__erebos_udivmoddi4` through `__aeabi_*`; add "from Erebos code".
  `__divdi3` and `__moddi3` now repeat the sign logic of
  `__erebos_divmoddi4`. (M4.1)
- A template that makes LLVM call `report_fatal_error` still ends the
  compiler: the diagnostic handler of ADR 0082 does not see it, and a
  process-wide fatal error handler would need an ADR. (M4.1)
- `check_start` (`src/hir/lower.rs`, around line 1033) asks whether an
  item is in the program with `module.0 < core_root.0`, and
  `check_symbols` asks the same through its `in_core` helper (`>=`); one
  helper would say it once. (M4.2)
- `ast::SymTarget::Entry(Span)` keeps a span that nothing reads: it is
  there for symmetry with `Path`, and a lint on it would mean dropping
  the span. (M4.2)
- `extern interrupt fn external() at Vector::SysTick;` gives a second,
  misleading parser error ("expected `{`, found `;`") after the right one
  ("expected `fn`, found `interrupt`"): `recover_to_item` stops at
  `interrupt fn` and the rest is parsed as a handler with a body. One
  error would be enough; `extern entry fn` recovers cleanly. (M4.3)
- The doc comment of `Hir::dump` in `src/hir/mod.rs` runs past 100 columns
  after `chip_interrupts` was added to its list; `rustfmt` does not reflow
  comments. (M4.4)
- `Interrupt::span` in `src/svd/mod.rs` is filled in by the parser and read
  by nothing (`name_span` is used everywhere); clippy does not warn since
  the field is `pub`. (M4.4)
- `interrupt_block` in `src/svd/interrupts.rs` builds the left-out lines with
  `flat_map(|text| comment(0, "// svd: ", text))`; `svd_lines(0, &left_out)`
  in `src/svd/write.rs` does the same. (M4.4)
- The `svd::renamed` warning for `Interrupt_` labels the first interrupt's
  `name` (the enum has no SVD element of its own), so it reads as if that
  interrupt were renamed; a label text saying "the enum, written at the end"
  would help. (M4.4)
- `chip_table_words` in `src/query/hardware.rs` ends in a `max()?` for a list
  with no variant, which cannot occur (`enum I: u32 {}` is
  `resolve::empty-enum`); `docs/reference.md` ("The table with the chip's
  interrupts") says such a list leaves the table at 16 words. Drop the
  sentence, or say that an empty enum is rejected first. (M4.4)
- `--emit=hir` prints `FnKind::Interrupt(HandlerVector { enum_id, variant,
  span })` through `Debug`, so the new `enum_id` shows in the HIR dump of any
  handler. No snapshot has one today; the day one does, it churns with the
  ids of enums. (M4.4)
- In `unprotected` in `src/analysis/shared_statics.rs`, the parameter
  `handlers` (the maskable roots, used only for `is_empty`) sits next to
  `handler: &Context`; a name like `maskable_roots` would not be confused
  with it. (M4.5)
- In `src/mir/validate.rs`, `volatile_after` has a `_ => from.volatile` arm
  (as `Builder::is_volatile` does), so a new projection that crosses a
  pointer compiles without setting volatility, while the module header says
  the `PlaceElem` matches have no `_` arm. Reword the header or name the
  projections. (M5.1)
- In `src/mir/validate.rs`, `binary` has two arms with the same result
  (`Unit | Never | Array | Slice | Struct` and `Enum | Peripheral`); they
  could be one. (M5.1)
- `Builder::cast` in `src/mir/build.rs` builds a `SliceAddress` plus a
  `Cast` for `*mut [u8]` to `*const u8`, which `fits` would accept without
  the `Cast` (ADR 0024); using `fits` there keeps the earlier MIR for that
  case. Harmless as it is. (M5.1)
- `report` in `src/mir/validate.rs` formats with a string holding a raw
  newline and two spaces instead of `format!("\n  {violation}")`; the
  output is the same, but the form is harder to read and no `Decisions`
  entry records it. (M5.1)
- The `Display` arm of `Location::Program` in `src/mir/validate.rs` is never
  reached, since `Violation::fmt` returns early for it; drop its text or say
  that it is a placeholder. (M5.1)
- `src/mir/build/asm.rs` and `src/mir/build/bit_struct.rs` still test
  `matches!(.., Operand::Copy(_))`; they only see integers and pointers, so it
  is harmless, but the plan says to use `Operand::place()`. (M5.2)
- `diagnostic` in `src/analysis/moves.rs` names the first site by span when a
  fault is reached by sites of different depth (`a.f.g` and `a` moved, `a.f`
  used), which may not be the place that holds the use; `describe` can name a
  place `m.One.0`, which no source can write. (M5.2)
- A type error in a file without `entry` and `panic fn` shows
  `resolve::missing-entry` and `resolve::missing-panic` instead of the type
  error; nobody looked into why. (M5.2)
- The enum arm of `lacks_layout` in `src/query.rs` clones the payload `Vec`
  only to call `copy_declaration_fails`, which could take a slice. (M5.2)

- A value of one origin with several linear paths (`let p: Pair = pair();
  let mut x = p.a; x = p.b;`) written over by another of its paths is
  `analysis::linear-not-consumed`, not `linear-overwritten`: ADR 0103 takes
  "same origin" as "came round a loop". Precise origins per linear part need
  an ADR amending 0103; pinned by `same_origin` in
  `tests/ui/linear_not_consumed.ers`. (M5.3)
- `is_linear` in `src/ty/linear.rs` keeps no memo of finished types, so a
  non-linear acyclic nest (A holds two B, B two C, ...) is walked 2^depth
  times. (M5.3)
- `report_made_arrays` in `src/ty/linear.rs` walks the whole type of every
  expression of a program with a `#[linear]` struct; quadratic in the worst
  case. (M5.3)
- The L1 violation for a bad projection in `linear_place`
  (`src/mir/validate.rs`) prints the `Debug` form of `PlaceElem`, where other
  violations print the MIR dump form. (M5.3)
- `hop_labels` in `src/analysis/linear.rs` compares the description with
  `THIS` to tell "moved here" from "moved into `x` here"; the local's
  `name.is_none()` would not depend on `describe`. `type Key` there repeats
  what `bits::key` returns; export the alias from `bits.rs`. (M5.3)
- `is_reserved` in `src/svd/names.rs` renames no built-in function name
  (`assert`, `len`, `slice`, `end`), so an SVD item named `end` would give
  `resolve::reserved-name` in the generated program; not confirmed with an
  SVD. (M5.3)

## Tests

- The `bss` check of `tests/e2e/memory_map.ers` and `memory_map_ram.ers`
  cannot fail on QEMU, which starts with RAM zeroed (their comments now
  say so); one that can fail needs the harness to fill RAM before the
  program starts, and the range `_start` zeroes is checked by
  `tests/elf.rs`. (M3.1)
- Seeds 723, 891, 1093 and 2485 of `generated_programs_agree` disagree
  only with `-O`. It is an LLVM bug, not Erebos's: the RISC-V backend
  (RV32 with M, `-O1` or higher; LLVM 22.1 and 23.1.3, and plain C with
  clang) drops the high word of an `i64` product when two or more
  multiplications by a constant like `-(2^k - 1)`, `k` from 34 to 60, are
  in one basic block. The emitted IR is valid (no `nsw`, `nuw`,
  `inbounds`). Confirmed for 723, 1093 and 2485; 891 (`mul i64 x,
  -9223372036854775709`) is probably the same, its reduction was
  inconclusive. The default run (20 seeds) does not reach them. A
  reproducer in IR:
  `define i64 @f(i64 %b, i64 %b2) { %m = mul i64 %b, -4503599627370495;
  %m2 = mul i64 %b2, -4503599627370495; %r = xor i64 %m, %m2; ret i64 %r }`
  (one instruction per line); compile it for `riscv32 -march=rv32imac -O2`
  and `a1` is never written. Documented in `docs/reference.md`, "Integer
  semantics"; not worked around (the other options were a split `mul` in
  codegen or other optimization flags). Remove this entry when a fixed LLVM
  is in use. (testing-oracles)
- The rejected and generated tests are not in `.cargo/mutants.toml`;
  the divisor of ADR 0068 section 1 differs from the generator (`(e | 1)`);
  small cleanups in `tests/e2e.rs`, `tests/generated.rs`, `tests/rejected.rs`
  and `CONTRIBUTING.md` are listed in `.claude/plans/testing-oracles.md`,
  `## Left open`. (testing-oracles)
- The header of `tests/e2e/plic_virtio/devices.ers` says `PLIC_M0` is served
  by the same driver as `PLIC.ctx[i]`; `main.ers` reads it directly.
  (M3.4a)
- `tests/e2e/register_index_out_of_bounds.ers` covers a read past a
  register array only; a write has no run-time test. (M3.4a)
- `src/diag.rs`, the test `a_warning_renders_with_the_warning_glyph`: an
  `assert!(out.contains("something is odd"))` repeats the stricter
  `"⚠ something is odd"` assertion two lines above. (M3.4b)
- `tests/e2e/svd_virt/virt.svd`: the Decisions of step 6 name the access of
  each register except `DLL`, `DLM` and `IER`, which inherit the device's
  `read-write`. (M3.4b)
- `tests/e2e.rs`: two string literals of the harness tests hold a real line
  break instead of `\n`; `tests/e2e/panic_location/main.ers` has `use
  board::exit;` out of order among its imports. (M3.5)
- `tests/ui/target_thumbv7m_aggregate_abi.ers` pins only three call forms
  of the caller at `-O0` and none at `-O`; `byval align 8`, the copied
  over-aligned `byval`, `i8`/`i16` returns and `[N x i64]` of other sizes
  are checked only by `aapcs_c` on QEMU. (M3.5)
- `tests/e2e/aapcs_c.ers` and `.c` (about 1300 and 950 lines) were written
  by a script that is not kept; a change to ADR 0075's table means editing
  53 regular blocks by hand. Keep the generator as a documented script.
  The step 7 Decisions of the plan also say `x72_1` skips `r1` (only
  `x72_0` does) and that the script is both kept out and the author of the
  comments. (M3.5)
- `tests/elf.rs` (`Symbol` parsing): `size.parse().unwrap_or(0)` turns an
  unparsable size into 0, so the `compiler_fence` check (`len() <= 4`)
  would pass without checking; `ARCH_FILES` in `tests/e2e.rs` repeats the
  file names `core_arch_file` in `src/driver.rs` holds. (M4.1)
- `tests/e2e/fence_handoff.ers` cannot fail if `fence()` or
  `compiler_fence()` emitted nothing (QEMU does not reorder; ADR 0081 says
  so); only `tests/elf.rs` checks the instructions. Do not cite it as
  evidence of ordering. (M4.1)
- The Tests line of plan step 7 names a "harness test that a failing exit
  code fails the run"; none has that name (`generated.rs` line 172 and the
  `expect-exit: 1` programs cover it). `tests/boards/thumbv7m-none-eabi.ers`
  has a comment line of 82 columns and an `asm(...)` line of 96. (M4.1)
- `tests/architecture.rs` (the handler file check of ADR 0082) is textual:
  a glob import such as `use inkwell::llvm_sys::core::*` followed by a
  function the allow-list does not name would not be caught. The ADR
  accepts the limit; an `extern`/`use` parse instead of a text search
  would close it. (M4.1)
- ADR 0083's conformance list names the LLVM IR of `sym entry` in the root
  module and with the entry function in a module; the ui snapshots
  (`asm_sym_entry*.ers`) cover only the entry function in `mod app`. The
  root case runs on QEMU in every e2e program, but has no snapshot. (M4.2)
- `tests/e2e/critical_race_riscv32.ers` has `PERIOD = 50` (5 microseconds
  at the 10 MHz `mtime`), chosen because a longer period made the test
  prove too little. On a very loaded host a handler that outlasts one
  period leaves the timer pending again at `mret`, and the 200000
  increments could overrun the harness timeout. Not reproduced (12
  parallel QEMU processes passed); lower the count or raise the period if
  it ever flakes. (M4.3)
- `the_takes_exceptions_header_is_read` (`tests/e2e.rs`) does not test a
  repeated `// takes-exceptions: yes`, which ADR 0088 says changes
  nothing; the code handles it. (M4.3)
- `tests/rejected.rs` with `EREBOS_REJECTED_PROGRAMS=2000` (seed 0): one
  injection of 65203 is accepted, the existing `ty::index-out-of-bounds`
  snippet at `tests/e2e/layout.ers:37`. Not a snippet of M4.3 and not
  checked against the tree before M4.3; the default run of 40 passes.
  (M4.3)
- Two `///` lines run past the width of their neighbours, which `rustfmt`
  does not wrap: the doc comment of `call_of` in `tests/e2e.rs` and the
  one of `function_alignment_moves_the_symbol` in `tests/elf.rs`. (M4.3)
- The `dsb`/`isb` of `nvic_disable` cannot be observed on QEMU, and
  `irq_disabled_armv7m.ers` only disables a line before pending it; the
  plan's `Decisions` do not say the barriers go untested (ADR 0091 cites
  CMSIS for them). (M4.4)
- A generated list value past `u32` is written as is for the compiler to
  reject (`ty::literal-out-of-range`); only the reader's unit test in
  `src/svd/read.rs` covers it, no snapshot. (M4.4)
- The header comment of `tests/ui/shared_static_unmaskable.ers` could say
  that `EVERY` is reported under rule 2 for `crash` only (one body, one
  owner), and that the maskable handler `tick` appears only in rule 1's
  label. (M4.5)
- In `tests/ui/shared_static_flow.ers`, the `return` and `?` cases cannot
  tell a walk over depth from a reading of the text (the access after the
  block is outside either way), and `continue` is not tested apart; only
  `break` and the nested `critical` discriminate. A gap against the literal
  wording of ADR 0093, not a bug. (M4.5)
- `the_inserted_roots_are_there` in `src/driver.rs` asserts `memcpy`, the
  `critical` pair and `__aeabi_uldivmod` only, so the rename of another root
  (`__udivdi3`, `memmove`...) drops it silently; the step 4 `Decisions` list
  more roots than any test asserts, and the root filter (`runtime` and no
  `.` in the symbol) relies on how body names are written. (M4.5)
- No test says that a kept body with a `critical_enter` statement, while
  `Mir::critical_enter` is `None`, is not a P2 (a reading of ADR 0095 that
  the step 4 `Decisions` record); one line in `a_valid_program_reports_nothing`
  in `src/mir/validate.rs`. (M5.1)
- No test covers a binding both inside a nested or-pattern and moved out of a
  static (both errors are reported); none for `match make().items[i]` with a
  non-copy binding (only the `let` form); no MIR snapshot of a `match` in
  place on a `Deref`, `Static` or `Index` scrutinee (the e2e ids 121 to 125 of
  `comptime_differential` run them). (M5.2)
- No ui test pins several `consume(a)` after one move (two errors);
  `use_after_move_once.ers` uses reads on purpose. (M5.2)

- `linear_array_made.ers` has no array literal inside an `if`, `match` or
  `loop` (they work, checked by hand). (M5.3)
- `end.ers` lost the shape `end(if c { t } else { make(2) })` with a
  parameter moved inside the `if`; `EnumDef::set_value` and
  `Queries::known_discriminant` have no unit test of their own (covered by ui
  tests); the `debug_assert_eq!` in `into_mir` that the list of enum values
  agrees with the MIR type mostly checks the wiring of `set_value`. (M5.3)

## Docs

- `docs/reference.md`: two paragraphs of the M3.4a text are wrapped wider
  than 75 columns ("A name of nothing after `over`..." and the line
  "against the length first (see ..."), and the bullet "A `step` is the
  word after the offset only when no `:` follows it, and so is `in`"
  needs an example. (M3.4a)
- ADR 0071 (lines about 84 and 129) says `svd::renamed` is reported "at its
  SVD element"; it is now reported at the element's `<name>`. Proposed fix:
  "at the `<name>` of its SVD element (at the register, for an alternate
  group), labeling the other". An accepted ADR, so the user's call.
  (M3.4b)
- ADR 0072 says a field with two sets and one bit is "an integer"; the code
  writes a `bool`, as its field-type rule ("otherwise `bool` for one bit")
  supports; the sentence should say so. (M3.4b)
- `docs/reference.md`, "Peripherals from SVD": the excerpts of the example
  omit some blank lines that `virt.ers` has, and two hand-wrapped
  paragraphs (the end of the register-mode paragraph and the `derivedFrom`
  paragraph) run past the file's width. (M3.4b)
- ADR 0075 does not say that a C callee compiled by clang does not realign
  an over-aligned `byval` parameter (the C twin of the 80-byte
  `#[align(16)]` row of `aapcs_c` checks only bytes). One sentence; an
  accepted ADR, so the user's call. (M3.5)
- `docs/reference.md` (line 2721): "Two places depend on the target" lists
  three; line 2745 says the reset entry "sits alone" before the thumbv7m
  parenthesis. `docs/roadmap.md` (lines 67 to 69 and 203 to 216) still
  speaks of M3.5 in the future. `CONTRIBUTING.md` says the ELF tests'
  checks run for every target; three of them stay on riscv32. (M3.5)
- `docs/reference.md`, the `ty::asm-operand-type` row of the diagnostics
  table: it omits the `const` that is not an integer (`asm("{}", const
  true)`), which ADR 0079 and the compiler give this code; the "Operands"
  bullet says "any integer type" without naming the error. (M4.1)
- `.claude/plans/M4.1.md`, step 1 `Decisions`: the `asm_names.ers` entry
  says the test shows every contextual word as a local, a function, a
  parameter and a field; it spreads the words across those roles (fields:
  `options`, `reg`, `sym`, `pure`, `clobber_abi`; functions: `inout`,
  `lateout`; locals and parameters: the rest). (M4.1)
- `docs/reference.md`, "Symbols": the bullet on `_start` has one line
  wider than its neighbours ("`extern static`, because `core::arch`
  defines it (ADR 0083): the program gets the one"). (M4.2)
- `.claude/plans/M4.2.md`, step 1 `Changes` says the HIR dump prints `sym
  entry`; the dump is derived `Debug` and shows `Sym(Entry)`, as its
  `Decisions` say. (M4.2)
- `docs/reference.md`, "Panic information", near the end: "(28 bytes, the
  name and the small function)" dangles after its bullet list and reads
  as if 28 bytes held the name and the function; the `PanicInfo` is 28
  bytes, with the name and the function on top. A few lines edited for
  M4.3 are not wrapped at the file's width. (M4.3)
- ADR 0088, "Consequences", still says the harness tests of the header
  "run on thumbv7m only" and that step 6's first riscv32 program is that
  target's test; step 6 made both tests loop over both targets. One
  sentence; an accepted ADR, so the user's call. (M4.3)
- `.claude/plans/M4.3.md`: step 2 `Tests` says "`static mut` counting runs"
  (the restart programs count in the board's scratch RAM, as its
  `Decisions` say); step 4 `Changes` says "`src/query.rs`: handlers are
  roots" (nothing was added for roots; `into_mir` fills `Mir::handlers`);
  step 5 `Decisions` call ADR 0088 `Proposed` and say the index lists it
  so (it is Accepted since `03a30c0`). (M4.3)
- The `Decisions` of the M4.4 plan: step 1 does not say that only the first
  list is type-checked (a second list gets `resolve::duplicate-interrupts`
  and nothing else); step 3 says a list with no variant leaves the table at
  16 words, which cannot occur (see the `chip_table_words` entry under
  Code); step 4's bullet "From the final review" says "(5 for `Spi`, 4 for
  the hole, 2 for the no-parameter one)" right after "exits with 4", which
  reads as the exit code, and the header comments of the three
  `irq_unhandled_*` tests do not say that the panic function also checks
  `IPSR`. (M4.4)
- `docs/reference.md`: the paragraphs on the table's length and alignment
  ("multiple of 128 because of the table (of", "`_start` is at the origin
  plus 64 (without a list)...") and the one on `code_align` are wrapped
  unevenly, and "as every program did before M4.4" puts a milestone name in
  the reference ("as every program does without one"). (M4.4)
- `docs/reference.md`, "Shared statics": the two examples build only on
  thumbv7m (`Vector::SysTick` and `Vector::NMI` do not exist on riscv32) and
  only the first names the target; "NMI, SVCall and the faults" reads
  narrowly, since every exception of riscv32 is unmaskable too ("the
  exceptions" covers both targets); the printed source line of the sample
  error lacks the `// error: ...` comment that line 6 of the example
  program carries. (M4.5)
- `AGENTS.md`: the sentence added to "Respect layer boundaries" leaves a
  short, ragged line before `tests/architecture.rs checks the modules each`.
  (M4.5)
- `docs/roadmap.md`, "M5.1 done": the gloss of T1 to T9 says "no value of an
  unsized type", where T3 says `!`, `[T]` and peripheral types; and "the
  ADR's rules stand as written" hides two readings that step 4 records (P2
  also covers `VectorWord::Default(i)`, and a `critical_enter` statement with
  `Mir::critical_enter == None` is not a P2); "no test's MIR broke a rule
  beyond the cast" would be exact. (M5.1)
- The step 4 `Decisions` of `.claude/plans/M5.1.md` do not list the
  re-exports that step adds to `src/mir/mod.rs` (`check_program`,
  `assert_valid_program`, `programs_checked`); only step 1 lists the ones it
  added. (M5.1)
- The step 3 `Decisions` of `.claude/plans/M5.2.md` say the nested or-pattern
  error is reported "once, at the binding"; it is once for each binding
  pattern (`.A(let b) | .B(let b)` gives two). They also do not mention that
  `is_place` in `src/ty/place.rs` includes `Peripheral` and `ByteString` (both
  copy, so no behavior changes). (M5.2)
- ADR 0069, section 1, says the stage of a snippet is "the layer whose output
  is needed to see the error"; the catalog now runs every `ty::` snippet to `Mir`
  on purpose, so that the bodies ADR 0032 lowers are checked too. One line in the
  ADR, or a note in its catalog. The `register-*` and `step-*` `injected_use`
  functions name their parameter `p`, not `injected_p`; and the check
  `injections >= programs.min(CATALOG.len())` is global, so a `Mir` snippet
  skipped on most bases goes unnoticed (nobody measured how many bases compile
  to `Mir`). (M5.2)
- `docs/reference.md`, "In MIR": the L3 summary leaves out "through fields,
  payloads and pointees" (ADR 0103), and the L1 phrasing "a local, followed
  by fields and payloads, of a `#[linear]` struct" is ambiguous. A few lines
  of the "M5.3 done" entry in `docs/roadmap.md` run past the usual wrap.
  (M5.3)
