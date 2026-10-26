# Open findings

Minor findings the reviews left open, worked off between steps. The
`Left open` sections of the plans in `.claude/plans/` keep when and why
each was found; this file lists what is still open, each entry ending
with the step that found it. Remove an entry when it is fixed.

The findings M2 left open were worked off on 2026-10-21; these remain.

## Code

- Values computed at compile time are numbered after `core`'s bodies, so
  their `FnId` in a `hir` dump moves whenever `core` gains a body; no
  snapshot shows one today. `core`'s ids must be known before the
  program is lowered, and the number of values only after, so keeping
  them stable means renumbering after lowering; left until a snapshot
  needs it. (M2.6)
- The `reachability` doc comment in `src/hir/lower.rs` has one line of
  about 98 columns. (M3.2)
- The doc comment of `Lowerer::ty` (`src/hir/lower.rs`) names only `[T]`
  where it has no size; it also turns a peripheral type outside a
  pointee or a register into `{error}`, records it, and drops `volatile`
  from a pointer to a peripheral type. (M3.2)
- `Mir::peripherals` and the masks `LeftOut::peripheral_types` and
  `LeftOut::peripherals` (`src/mir/mod.rs`) have no doc comments of
  their own. (M3.2)

## Diagnostics

- `recover_to_item` stops at a `memory {` at the depth of the items, and
  counts only braces: after an error in a constant's initializer, a
  `memory { region: 1 }` struct literal later in it, outside braces, is
  parsed as a block and gives one more error. (M3.1 work-off)
- A register with its name left out, `read: u8 at 0`, reports "expected
  `read` or `write`, found identifier" labeling the word `read`, which
  reads as contradictory (`src/syntax/parser.rs`, `register`); no test
  shows it. (M3.2)
- The `typeck` dump leaves out register visibility. (M3.2)

## Docs

- The `ty::no-fields` row of the diagnostics table in
  `docs/reference.md` says "a value that is not a struct"; a peripheral
  type is not one either, and its registers are accessed as fields.
  (M3.2)
- "Building" in `docs/reference.md` says the `typeck` dump leaves out
  peripheral types and peripherals reaching a failed value, which cannot
  be seen: with errors no dump is printed. (M3.2)
- `docs/reference.md`, about line 1447: "`&raw mut UART0` is one" is
  unclear; "is such a pointer" is meant. (M3.2)
- `docs/reference.md`: lines about 2079 and 2549 are too long, and 16
  and 2461 stop short, after editing. (M3.2)

## Tests

- The `bss` check of `tests/e2e/memory_map.ers` and `memory_map_ram.ers`
  cannot fail on QEMU, which starts with RAM zeroed (their comments now
  say so); one that can fail needs the harness to fill RAM before the
  program starts, and the range `_start` zeroes is checked by
  `tests/elf.rs`. (M3.1)
- The end-of-file fallback for a missing access mode (`peripheral X {`
  at the end of the file) has no test, as for `memory_decl`. (M3.2)
- Two of the three helps of `ty::unknown-register` have no snapshot:
  "none of its registers is visible here" and "`T` has no registers".
  (M3.2)
- No snapshot shows a static's initializer holding a peripheral's
  address in the LLVM IR; `tests/e2e/peripherals/` covers it at run
  time. (M3.2)
- `tests/snapshots/ui__ui@typeck_peripheral_slice.ers.snap` has an
  `assertion_line` header, which almost no other snapshot has. (M3.2)
