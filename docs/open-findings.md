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
