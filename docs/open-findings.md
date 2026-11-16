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
- `src/analysis/borrows.rs`, `later_use`: `reach_from` marks every block
  reachable from the fault's successors and does not tell a forward edge from
  a back edge, so a later use in a forward block whose span starts before the
  access is labelled "in the next iteration of the loop" in a body with no
  loop (the bounds check of an index splits `(*r).buf[(*r).head]` into two
  blocks). ADR 0106 and 0107, as amended on 2026-11-11, say only a read
  reached through a back edge gets that mark; the fix is to use the wording
  only when the fault's block is reachable from the use's block. The label is
  the only thing wrong, not what is accepted. (M5.4a, step 4 and the fix to
  `pointer_place`)
- `src/analysis/moves.rs`, `diagnostic` (around line 387): a move whose span
  starts after the start of the use is called "in an earlier iteration of the
  loop" whether or not the body has a loop, so
  `(*r).buf[{ let q: &mut Ring = r; (*q).head }]` says so. The rule is
  textual and dates from the analysis of moves (ADR 0097, 0099). (M5.4a)
- References have no debug type (`&T` named in the pointer type): there is no
  debug-info emission in `src/codegen/` to attach it to. The `comptime`
  parameter and `comptime T: type` positions of ADR 0105 have no check or test,
  since the language has neither. (M5.4a)
- `src/ty.rs` (around lines 959-966): the help "a `&` reference never becomes
  `&mut`..." is also given when the referents differ (`&i32` where `&mut u32`
  is expected), where it misleads; add `want.referent == have.referent` to the
  condition. (M5.4a)
- `src/mir/build.rs`: `argument` falls back to `self.expr` when
  `self.current` is `None`, a path that cannot happen and that no Decision
  records; `is_mut_variable` takes `&mut self` only because `ty_of` does.
  The `Const` guard of step 2 (a use of a constant of reference type has type
  `{error}`) is redundant with the return check; it is kept because
  `read_constant` fails without it. (M5.4a)
- The generated programs of ADR 0068 have only scalar types, so no generated
  program holds a reference. (M5.4a)
- `touch(&mut (*s)[0], &(*s)[1])` on a slice gives two errors (the index check
  reads `s` through `len(s)`, then the elements overlap), one on an array; it
  follows ADR 0108 and is only noisy. In `two(&mut (*s)[..4], &mut (*s)[4..])`
  the message is "cannot use `s`", where "cannot borrow `(*s)[_.._]`" would
  match the other cases. (M5.4b)
- `src/layout.rs`: `layout::of` works on `hir::Ty`, whose `slice_elem` and
  `is_aggregate` stay raw-only, so a HIR `&[T]` would get a thin pointer's
  layout; no input reaches it today, a latent trap if references become
  layout operands. (M5.4b)
- `len(&buf)` leaves a dead `_7 = &_1` in MIR, so `len(&mut buf)` takes a
  `&mut` borrow that can conflict with a live one. `name()` writes
  `ConstSubslice(0, N)` of an `N`-element array as the array, so `&buf[0..N]`
  reads "`buf` is borrowed here" like the converted `&buf`; `&raw const
  (*v)[n..]` through a volatile pointer has a non-volatile type, as `&raw
  const (*v)[n]` does. (M5.4b)
- `src/mir/build.rs`: `slice_index` and `reference_slice_index` evaluate the
  index before reading `len(s)`, where ADR 0109 writes `len(s)` first
  (ranges do it as the ADR says); observable only through a raw-slice
  variable reassigned inside the index. (M5.4b)
- `src/ty/reference.rs`: ADR 0110 asks for a label "the value of a `break`
  out of a `critical` block"; the code puts that wording in the message, as
  the other `ty::reference-position` cases do, and keeps the tail case's
  label. (M5.4c, step 1)
- `src/analysis/borrows.rs`: `Flow::name` indexes `registers[*index]`
  directly where the neighbouring `Struct` arm uses `.get()`, so a hand-built
  test with a wrong index would panic; the doc comment of `later_use` does not
  mention `Point::whole_call`. `src/mir/validate.rs`, `address_temporaries`:
  `addressed.contains` and the filter over `written` are quadratic in a
  body's static references times its writes, harmless at current sizes. The
  `bytes` of a peripheral saturates on overflow, and a `sym` access carries
  `reads: None`; step 2's Decisions do not list these two. (M5.4c, steps 2 and 3)
- `src/analysis/call_graph.rs`: `Reach::search` and `path` take `C:
  AsRef<[Call]>` so that `Scan` and a plain `Vec<Vec<Call>>` both serve;
  CONTRIBUTING prefers plain data before generics. (M5.4c, step 3)
- `src/ty/method.rs`, `method_call` (about lines 83 to 98): with `fn g(s: &&S)
  -> u32 { s.f() }` the compiler reports `ty::reference-position` and then
  `ty::unknown-method` ("`&S` has no method `f`"), a cascade of the first
  error. A `Ty::Ref` referent could be taken as `Ty::Error` there; `s.n` on
  such a base may do the same and was not checked. (M5.5)
- `src/ty/place.rs`, the doc comment of `dereferenced` (around line 641) says
  the base "is read as the operand of `*` is", but the function reads
  nothing: it records the base in `Tables::derefed` and builds the place.
  (M5.5, step 2)
- `src/hir/lower/resolve.rs`, `no_function`: the help "its functions are ..."
  also lists reserved or already-reported names, such as `__bad` in
  `Q::missing()`. `use self;` in a module gives `resolve::use-cycle`
  ("brings in `m::self` through itself") and `use self::a;` gives
  `resolve::unknown-path`; the plan says such a path "is judged when
  resolved", so both follow it, but the first message is confusing.
  (M5.5, step 1)
- `src/analysis/shared_statics.rs`: `unprotected_peripherals` and
  `unmaskable_peripherals` repeat the lines that compute `itself`, `reaches`
  and the wording of the title; a helper would remove the copy. `Named::key`
  converts the index with `index as u64`, where `u64::try_from` would say it
  is checked. (M5.6, step 3)
- `src/driver.rs`, the comment before `Found`: an edit left a short wrapped
  line ("... and no peripheral (ADR 0115)" / "and have no check. The walk");
  reflow the paragraph. (M5.6, final review)
- `src/ty.rs`, the `assert` message help (`value_tails`): `loop { break p; }`
  and `comptime { <raw slice> }` as a message get `ty::mismatched-types`
  without the `&*msg` help, because the mismatch is on the `break` value or
  the `comptime` operand; the reference's "a block, an `if` or a `match`" is
  accurate, and step 1's decision does not name the two. `rt/arch/armv7m.ers`
  (around line 291): the comment line added for the `__aeabi_` divisions is 91
  characters where the paragraph wraps near 78. (M5.7, final review)
- `src/analysis/borrows.rs`, `Flow::refusal`: `&CONST` returned from a
  function is told as "a constant, which has no address of its own", with the
  help to declare a `static`, only when the constant is an aggregate (MIR
  building copies its data into a temporary with `LocalDecl::constant` set);
  for a scalar constant (`const LIMIT: u32 = 8; fn f() -> &u32 { &LIMIT }`) the
  error is "a temporary value of this function" with the local's help. Both
  are `analysis::returned-borrow`, so the program is rejected either way; ADR
  0118 asks for the constant's help in both. (M5.8, step 11)
- `src/analysis/borrows.rs`: an `analysis::borrow-conflict` whose sites are
  held through two calls and read by one use (`let b: &u8 = if c { first(&buf)
  } else { other(&buf) }; buf[0] = 1; *b`) repeats the label "the borrow is
  used later here" once per site, at the same `*b`; the older snapshots with
  several sites do the same, so it was left. (M5.8, step 6)
- `src/mir/validate.rs`, R5: the message for two or more reference parameters
  reads "returns the shared reference `T`, but has 2 reference parameters,
  `_1` (...), `_2` (...), not at most one"; "has 2 reference parameters (...),
  but at most one is allowed" reads better. The tests match substrings only.
  (M5.8, step 1)
- `src/analysis/borrows.rs`, `call_def_of`: it takes `args.first()` and drops
  any other reference argument without saying so. It is sound only because R5
  and P3 leave at most one for a callee that returns a reference; a
  `debug_assert!` or a pointer to R5 at the call site would say it. (M5.8,
  step 2)
- `src/analysis/borrows.rs`, `returned_borrows`: the `seen` deduplication
  cannot fire for one site reaching one `Return` by two paths, because the
  held state at a block start is already merged. It matters only for two
  `Return` blocks with the same borrow span and the same "returned here" span,
  and it is redundant for the borrow's span, which is already in the labels.
  (M5.8, step 2)
- `src/ty/unsafety.rs` (around lines 95-103): `Operation::label` repeats the
  verb table of `Access::gerund` for `ExternStatic`; one small helper on
  `Access` would remove it. (M5.9)
- `tests/snapshots/ui__ui@naked_unsafe_block.ers.snap`, case
  `block_after_the_asm`: the `unsafe` block is labelled "not allowed here",
  while the other two cases say "an `unsafe` block, not needed on a naked
  function"; consistent with step 3's decision, so a note. (M5.9)
- The two boards hold the same `print_u32` and `print_u64`, copied by hand;
  nothing keeps the copies equal. (M5.10, step 1)
- `src/diag.rs`: the unit test of `note` writes its source with a literal
  line break where the neighbouring tests use `"fn main() {}\n"`. The working
  copy of `src/analysis/mod.rs` and of two `tests/ui/recursion_*.ers` files
  has CRLF line endings; Git normalizes them on commit
  (`.gitattributes`), so only the editor is at fault. (M5.10, step 3)

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
- The `later_use` fix in `src/analysis/borrows.rs` (a use outside a loop is
  "used later") has no test on an array, `touch(&mut a[i], &a[j])`.
  The comment in `tests/e2e/slice_driver.ers` says the checksum wraps in 32
  bits, but no payload wraps; drop the word or add a payload that does.
  (M5.4b)
- `tests/e2e/static_buffer_riscv32.ers` and `static_buffer_armv7m.ers` exit
  with 4 when `clear` did not zero the buffer, a code no header comment
  names; the programs declare their own 16550 and CMSDK peripheral types
  where ADR 0110 says the board's. The 10 new snapshots of step 4 carry an
  `assertion_line` header, which 23 of the tracked snapshots now have; strip it with the next snapshot update. (M5.4c,
  steps 4 and 7)
- `tests/rejected.rs`, `never_functions`: `.chain(["exit"])` hard-codes the
  name of the board's terminator, so a board that names it otherwise has the
  same blind spot that step 4 fixed for `exit`; reading the signature of the
  board's function would be sturdier. (M5.5, step 4)
- `tests/ui/parse_impl.ers`: `#[align(N)]` and `#[naked]` on a function of a
  type (ADR 0113, "What does not change") have only a parser snapshot, and
  nothing tests that they reach codegen (`#[align(64)]` gives `define void
  @S.f() #1 align 64` by hand). One line in `codegen_impl_symbols.ers` would
  pin it. (M5.5, final review)
- `tests/ui/reference_places.ers`: `&mut r[i]` through a `&mut [T]` has no
  direct case (`&mut t[..2]`, `t[1] = x` and `&mut m.buf[0]` are there); the
  code path is shared. (M5.5, step 2)
- `tests/e2e/uart_interrupt_riscv32/main.ers` (header, lines 17-19) says the
  generated `Finisher` is still written by `svd_virt` and `uart_fields`; only
  `svd_virt` writes the generated `virt::test::Finisher`, and `uart_fields`
  writes a hand-written `FinisherStatus` (a bit-field struct of the same
  kind). Say "a bit-field finisher struct". (M5.6, step 1)
- `tests/e2e/uart_interrupt_armv7m/main.ers`: the comment on the `critical`
  block says why the block exists but not why `nvic_enable` and
  `enable_interrupts` stay outside it (they name no peripheral), as step 1's
  Decisions say it does. (M5.6, step 1)
- `tests/e2e/uart_owned_armv7m/main.ers` and `tests/e2e/uart_owned_riscv32/
  main.ers`: header lines longer than the wrap (a 122-character line at line
  16 of the first; 81 characters at line 2 of the first and lines 14, 15 and
  140 of the second), and the paragraph at lines 17-18 of the second breaks
  after "which". (M5.6, step 4)
- `tests/e2e/assert_message.ers`, line 3: the header still says the message is
  dropped "until M2.6", which is old; `docs/roadmap.md` (the M2.6 row) shows
  `info: *const PanicInfo` as history. (M5.7, final review)
- `src/mir/validate.rs`: no test shows that a volatile result with one
  reference parameter that is not volatile is accepted by R5, as the step's
  Decisions say; type checking ties the two. (M5.8, step 1)
- `src/analysis/borrows.rs`, the test
  `a_borrow_held_without_a_call_gets_no_call_label`: its `apart` case does not
  test what its comment says (the walk starts from a local defined by the
  `Ref` itself, so the call on another local is never visited); and no test
  makes the call label come from the next-iteration branch of `holding_call`
  (`round: true`). (M5.8, step 3)
- `src/analysis/borrows.rs`, the test
  `a_temporary_copied_from_a_constant_is_the_constant_whatever_borrows_it`:
  its comment says "A part of it, `&CONST[1]`", but the case borrows the whole
  temporary through a reborrow in a `let`. (M5.8, step 4)
- `src/ty/reference.rs`, `report_made_references`: a `return` inside a
  `comptime` expression inside a `critical` block is recorded by the walk;
  that expression is its own `Fn`, so it is judged unreachable, and no test
  shows it. (M5.8, step 5)
- `tests/ui/returned_reference_borrows_accepted.ers`: the twins `lent`
  (`poke(slot(r, 0))`) and `no_parameter` (`table()`) are accepted because
  they leave nothing for a conflict to attach to; their snapshots are a MIR
  dump and would not show a result that held too much. The unit tests of steps
  2 and 3 cover the mechanism. (M5.8, step 6)
- The LLVM snapshots of step 7 have no volatile result and no `&mut` result
  through an `extern fn`, and the C program of step 8 runs only a `&[u8]`
  result across the ABI; the ABI reads the signature, so kind and linkage add
  nothing, but no test says so. (M5.8, steps 7 and 8)
- `tests/ui/`: the compile-time bodies other than the four of ADR 0119's
  conformance list (a repeat count, `#[align]`, a bit-field default, a `memory`
  region value, a register offset, a peripheral address) have no `unsafe`
  test; they work, because every `Fn` with a body is a root, but nothing pins
  it. (M5.9)
- `tests/rejected.rs`, the `unsafe` removal test: it scans the root file of
  each e2e program, so the blocks of submodule files
  (`panic_location/parse/digits.ers`, 3 blocks) are not removed one by one; its
  tokenizer ends an unterminated character literal at the end of the line,
  where the lexer reports an error. (M5.9, step 7)
- `tests/e2e/unsafe_boundary.ers`: the comment "Called once: two rings would
  share the bytes" documents a contract the safe `pub fn` does not enforce; not
  a soundness hole, two rings would only print odd output. (M5.9, step 8)
- `docs/reference.md` says that `&raw const p[i]` emits no `inbounds`; no
  review looked at the LLVM IR for it. (M5.9, step 10)
- The snapshot of `call_borrows_accepted.ers` lost its `assertion_line:`
  header (insta metadata) when it was updated by hand in step 2; the next
  `cargo insta` run will put it back. (M5.10, step 2)

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
- `docs/reference.md` (around line 2564), the borrow check's "The error": one
  line of the paragraph about ties is not wrapped like its neighbours. The
  `.claude/plans/M5.4a.md` Decisions of steps 3 and 6 have a badly wrapped
  line (step 3, first bullet) and a `\n` split across two lines inside
  backticks, and the Decisions of step 6 say "as the interrupted run left it",
  which a later reader cannot place; the `Changes` list of step 2 still names
  the `comptime` positions that the Decisions say do not exist. (M5.4a)
- `docs/reference.md`: a reference to a slice "casts as the raw slice of its
  mutability does" (around line 2559) reads as if `&[T] as *mut [T]` were
  rejected, and the compiler accepts it; a few lines of the M5.4b text are
  broken or overlong (around lines 166, 287, 1832, 2279, 2349, 2881, 2893) and
  "the exception that is not one" (around line 2657) is cryptic. The
  `.claude/plans/M5.4b.md` Decisions of step 4 describe `number` as used "once
  for the length and once for the checksum" and the ui file's `copy` as taking
  a `&[u8]` (it takes a `&mut [u8]`); the plan's test bullet for
  `slice_borrows_conversion.ers` expects "converted to a slice here" for
  `held`, where the snapshot has "`buf` is borrowed here". (M5.4b)
- `docs/reference.md`, the `ty::move-out-of-place` row of the check tables
  (around line 7812) names `(*p).f`, `(*s)[i]` and `(*r).f` as places through
  a pointer or reference; the prose and the Decisions of M5.5 step 2 say that
  `r.f`, `r[i]` and a method call with a `self` receiver give the same error,
  and the row should list them. (M5.5, final review)
- `docs/reference.md`, "Shared statics": the sentence "The errors of
  peripherals follow those of statics for the same rule." is glued to a
  147-character line and would sit better next to the sentence on the other
  context's places it qualifies; "...and names no `static mut` and no
  peripheral), nor is a default handler..." overflows the wrap; the text
  before the `main.ers` example of "Peripherals in the rule" names the
  `external` handler, which the shortened block does not show. (M5.6, step 5)
- `docs/reference.md` (around line 5490, "Tests on both targets"): a line of 102
  characters in the paragraph that now names `board::print`; `CONTRIBUTING.md`
  (lines 88-89): the same edit left a short wrapped line. The
  `.claude/plans/M5.7.md` Decisions of step 4 give the counts of programs and
  definitions in one hard-to-follow sentence ("41 programs ... 43 definitions
  ... 55 definition lines removed, 13 rewritten as kept, 1 file deleted");
  split it into plain counts. ADR 0095's status line does not name P5, though
  the validator now rejects a raw pointer as the panic function's parameter;
  the change is the user's (see the report of M5.7). (M5.7, final review)
- `docs/roadmap.md` (the "After M5" entry, around lines 317-322): the sentence
  "A reference that is stored is what still holds them" is not true of the 3
  sites of `erebos_skip`: nothing is stored there, and it stays raw because C
  calls it and `slices.ers` tests the cast. The recount keeps M5.7's reading
  (reason 8: 19 sites, 45 lines in `tests/e2e/`) so the counts compare; step 9
  reads it as reason 1 (16 sites, 43 lines). Both are in the plan and the
  roadmap; which one the roadmap commits to is the user's. (M5.8, step 10)
- `.claude/plans/M5.8.md`: step 6's header says "no compiler code" though a
  `fix(analysis)` precedes it; step 9 calls `x` of `byte_strings.ers` a
  `-> *const [u8]` while it returns `*const [u8; 1]`; the 45 lines of step
  10's recount include `parse.ers:13`, a cast line with no site, so a reader
  who recounts lines by kind finds 44. (M5.8, final review)
- `docs/reference.md`: several lines of M5.9's text run past the 80 columns of
  the rest ("External functions" around line 327, "Statements and blocks",
  "What may be borrowed", "Slices behind references", the `sym COUNT` example,
  `bkpt 0xab`, `csrs mie`), as does the M5.9 status entry of
  `docs/roadmap.md`; the `ty::unsafe-required` row of "Diagnostics" reads as
  if the help about an `unsafe fn` body appeared at compile time too (it
  appears only in an `unsafe fn` body). (M5.9, step 10)
- `docs/roadmap.md` says "117 programs hold the 1004 operations" next to "118
  of 177 files": the difference is `panic_location/parse/digits.ers`, counted
  under its root; a parenthesis would say so. The plan of M5.9 (step 9)
  describes the count of `tests/e2e/` in prose; a short script would make it
  cheaper to repeat. (M5.9, step 9)
- ADR 0030 ("Erebos has no `unsafe`") and ADR 0117 (marking the dereference
  "deferred until it is clear what remains") are stale: neither is marked
  amended by 0119 and 0119's `Amends` does not list them. Whether to add
  "amended by 0119" to their status lines and the index is the user's.
  `.claude/agents/code-reviewer.md` has no item for ADR 0119's two review
  rules (now in `CONTRIBUTING.md`); that change is the user's too. (M5.9,
  final review)
- `docs/reference.md`, "No recursion", "What is checked": it says a recursive
  function that only C or assembly calls is an error, but not the exception
  of ADR 0120 ("What runs on the target"): if compile time also reaches it,
  it is accepted, one of the openings left to M5.11. (M5.10, step 5)
- `docs/roadmap.md`, the M5.10 status bullet: "62 e2e programs use them" is
  not what the count measures; step 1 migrated 62 programs, 21 of which had
  a digit loop that did not recurse. The recount of step 4 (43 programs with
  a recursive digit printer) and the migrated 41 recursive ones do not
  reconcile in the plan or the roadmap, and step 1's Decisions count
  `structs`, `modules/uart` and `panic_location/fmt` unevenly. (M5.10, steps
  1, 4 and 5)
- ADR 0120, Context, keeps the text-search estimate ("about 70 definitions in
  more than 55 programs"); a pointer to the roadmap's recount would stop the
  two numbers clashing. ADR 0077's list of what the boards offer lacks
  `print_u32` and `print_u64`. Both are the user's to change (accepted ADRs).
  (M5.10, final review)
