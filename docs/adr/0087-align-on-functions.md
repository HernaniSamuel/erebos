# 0087. `#[align(N)]` on functions

- Status: Accepted
- Date: 2026-11-05
- Amends: 0022 (`#[align(N)]` also applies to functions with a body)

## Context

`mtvec` holds multiples of 4 (ADR 0085), but `riscv32imac` has compressed
instructions, so LLVM may align a function to 2. The riscv32 trap entry is a
naked function, whose template cannot move its own symbol: an `.p2align` in
the template puts padding after the symbol. ADR 0083 already foresaw the
same need on thumbv7m: if `_start` stops landing where the vector table
expects, "the fix is then an explicit alignment on the function, which code
generation sets". The roadmap's M4.4 will need a table's alignment, which is
a section's, and is no function; nothing else needs this today.

`#[align(N)]` exists for structs and statics (ADR 0022).

## Decision

`#[align(N)]` applies to any function **with a body** of the language: a
plain function, a naked function, the entry function, the panic function and
a handler. On an `extern fn`, it is `resolve::misplaced-attribute`, since the
function is defined elsewhere.

- It says that the function's first instruction, when the function is in the
  object, is at a multiple of `N`. LLVM may align it further; the attribute
  only raises. A function that LLVM inlines and drops has no address, and an
  `#[export]` or `sym` keeps it. On a function run only at compile time
  (ADR 0029), it has no effect and no diagnostic.
- `N` is computed at compile time as it is for structs and statics, a power
  of two up to the limit they have, or `ty::invalid-align`. A function has no
  natural alignment in the language, so the rule "not below the natural
  alignment" asks nothing more here: `#[align(1)]` is valid and changes
  nothing.
- It may be given once (`resolve::duplicate-attribute`). `#[packed]` stays
  misplaced on a function.
- Code generation sets the alignment of the LLVM function. A body's alignment
  is part of MIR, shown in `--emit=mir`, and visible in `--emit=llvm-ir`.

### Conformance tests

- **ui snapshots:** `#[align]` on a function, a naked function, the entry
  function, the panic function, each in LLVM IR, with a constant expression as
  its argument; on an `extern fn` (`resolve::misplaced-attribute`), with the
  help naming functions; twice; not a power of two, above the limit;
  `#[packed]` on a function.
- **ELF:** on both targets at both levels, an `#[export]` function with
  `#[align(64)]` and a naked function with `#[align(64)]` have addresses that
  are multiples of 64. On riscv32, an `#[align(4)]` naked function holding a
  single compressed return ends at 2 mod 4. A naked function right after it
  is at 2 mod 4 without the attribute (the control, which also pins the order
  of the two) and at 0 mod 4 with it.
- **End to end:** a program with an aligned function runs on both targets.

## Consequences

- A function's alignment is written in the source, for a reader to see. The
  trap entry needs no compiler knowledge of its name.
- The help of `resolve::misplaced-attribute` for `#[align]` now lists functions.
- A large alignment costs padding bytes in the code's region, which the program
  asked for.

## Alternatives considered

- **Rounding up in `_start`** (`.p2align 2` first in the template, and `(trap +
  3) & -4` written to `mtvec`) — no change to the language, but a trick that needs
  a comment, and the symbol stands before the code it names.
- **The compiler naming `core::arch::__erebos_trap` by path and aligning it** — no
  change to the language either, but one more item the compiler knows, on one
  target, for what an attribute says in the source.
- **The trap entry inside `_start`**, after its last jump, with `.p2align 2` and
  a local label — no new symbol, but a reader of a backtrace sees `_start` for
  every trap, and `.text.start` holds more than the reset entry.
