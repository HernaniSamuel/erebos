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
- `array_len` keeps the comment explaining a `usize` needed at compile
  time, though `comptime_usize` now holds the logic
  (`src/hir/lower.rs`). (M3.1)
- `memory_decl` keeps the placements in fixed slots 0 to 2 of `placed`,
  and the missing-placement loop lists the words `code`, `data`,
  `stack` again in the same order; one list would remove the coupling.
  (M3.1)
- The `typeck` dump prints a `mir::MemoryMap` (`src/ty.rs`), formatting
  the type checker's dump from a type of the next layer; a neutral type
  shared by both dumps would remove the dependency. (M3.1)

## Diagnostics

- `recover_to_item` stops at any `memory {`, so after a syntax error in
  a body a later `memory { region: 1 }` struct literal gives one more
  error. (M3.1)
- A region without `size` reads "expected `,`, found `}`". (M3.1)
- When an overflowing region sits next to another region that holds
  something, lld's overlap errors follow the `link::region-overflow`
  diagnostic as `error: linking failed:`. (M3.1)

## Tests

- `resolve::stack-with-code` is tested with the stack before the code
  only. (M3.1)
- No test shows that errors in the values of a repeated region, a
  repeated placement or an extra `memory` block are reported. (M3.1)
- Untested: an empty region holding the stack gets both
  `ty::empty-region` and `ty::stack-size`; a region out of range still
  takes part in the overlap checks. (M3.1)
- `elf__region_overflow_code.snap` holds a byte count that changes with
  the code size; an insta filter could redact it. (M3.1)
- "The initial values of `.data` do not fit" and two regions
  overflowing at once have no rendered snapshot, only unit tests.
  (M3.1)
- The comment above the block in the e2e programs and on `VIRT_MAP`
  says "as ADR 0030 had it", but the stack is now sized at the start of
  `RAM`. (M3.1)
- The header of `tests/ui/link_no_memory.ers` claims what only the
  `codegen_*` tests show (other stages need no block). (M3.1)
- Three lines of `tests/e2e/memory_map.ers` and `memory_map_ram.ers`
  pass 100 columns; their `bss` check cannot fail on QEMU, which starts
  with zeroed RAM. (M3.1)

## Docs

- The ELF checks bullet of `CONTRIBUTING.md` leaves out the two
  `link::region-overflow` tests. (M3.1)
