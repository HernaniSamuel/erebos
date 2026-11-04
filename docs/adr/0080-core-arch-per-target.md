# 0080. `core::arch`, one file per target

- Status: Accepted; corrected on 2026-11-04 (a program cannot define `__aeabi_uldivmod`, on any target)
- Date: 2026-11-03
- Amends: 0052 (`core` has a module whose file depends on the target),
  0074 and 0076 (`core` defines `__aeabi_uldivmod` and
  `__aeabi_ldivmod`, so C code on thumbv7m divides 64-bit values; the
  consequence of both ADRs that says it cannot no longer holds)

## Context

With inline assembly (ADR 0079), `core` can define what needs an
instruction of the target: fences (ADR 0081), the EABI's 64-bit
division for C code on thumbv7m (ADR 0074, 0076), and in later steps the
startup code (M4.2), masking interrupts and waiting for one (M4.3).
Their templates differ per target, and `core` is compiled with every
program (ADR 0052).

The language has no conditional compilation (ADR 0077): a program built
for two targets has one meaning, and test programs that need one
machine say it in a header. Zig chooses code with `if` on a constant
target that is known at compile time and analyzes only the branch
taken; Rust has `#[cfg]`. Either would let any code differ per target.

## Decision

### The module

`core` declares a public module `arch`, whose file is chosen by the
target of the build (ADR 0073), not by the rule of ADR 0049:

| Target | File of `core::arch` |
|--------|----------------------|
| `riscv32imac-unknown-none-elf` | `rt/arch/riscv32.ers` |
| `thumbv7m-none-eabi` | `rt/arch/armv7m.ers` |

The files are embedded in the compiler with the rest of `core`
(ADR 0052) and diagnostics name them as above. A build compiles and
checks only its target's file; the other is compiled by the builds of
its own target. `core::arch` follows every other rule of `core`:
private items are private, and its exported symbols are reserved as
`core`'s are: a program defining `__aeabi_uldivmod` is rejected, on
every target, because names starting with `__` are reserved
(`resolve::reserved-name`, ADR 0020, 0052). The files have no modules of
their own; a step that needs one says where its file lives.

**Note, 2026-11-04:** the text said the program was
`resolve::duplicate-symbol` on thumbv7m and accepted on riscv32. A name
starting with `__` is reserved before any symbol is compared, so the
program is `resolve::reserved-name` on both targets, whether the
function is `#[export]` or not and whichever module it is in; no program
reaches `duplicate-symbol` with these names.

### Portable and target-specific items

- A **portable item** is declared in every file of `core::arch` with
  the same kind, name, signature (or type) and meaning, so a program that
  uses only portable items builds for every target. Each portable item
  is listed in `docs/reference.md`, and a test builds a program that
  uses all of them on every target.
- Every other item of a file is **specific** to its target. A program
  that names one builds only for that target: on another, the path names
  nothing (`resolve::unknown-path`, or the code of whatever name the
  path has, as for any missing item). That is how a program for one target
  says so, as with its own assembly (ADR 0079).

This step defines the portable `compiler_fence` and `fence` (ADR 0081)
and, specific to thumbv7m, the two EABI functions below. Later steps
add items with their ADRs.

### `__aeabi_uldivmod` and `__aeabi_ldivmod`

`rt/arch/armv7m.ers` defines the EABI's 64-bit division functions as
naked functions (ADR 0079), exported under those names, so that C code
linked into a thumbv7m program divides 64-bit values. Each takes the
dividend in `r0`–`r1` and the divisor in `r2`–`r3`, and returns the
quotient in `r0`–`r1` and the remainder in `r2`–`r3`; the remainder has
the sign of the dividend, as with `%` (ADR 0007). Each calls a new
function of `core::builtins` in Erebos with the C ABI, which returns
the quotient and stores the remainder through a pointer:

```
pub fn __erebos_udivmoddi4(n: u64, d: u64, rem: *mut u64) -> u64
pub fn __erebos_divmoddi4(n: i64, d: i64, rem: *mut i64) -> i64
```

and moves the remainder into `r2`–`r3`. They are `pub` inside the
private module `builtins`, so that `core::arch` can name them in `sym`
operands and programs cannot. Both reuse the loop of the
existing 64-bit division (ADR 0020), once for the quotient and the
remainder together. Code generation does not call them: Erebos code
divides through the functions of ADR 0076, as before. A zero divisor
returns what `core`'s loop returns for it (a quotient of all ones and
the dividend as the remainder, before signs); C leaves it undefined,
and the EABI's call of `__aeabi_ldiv0` is not made.

## Consequences

- `core` has code that differs per target, in one module, where a reader
  sees all of it; programs do not.
- Programs and tests can use fences and, later, interrupt masking
  without knowing the target, through the portable items.
- Naming a specific item is the program's statement that it targets one
  machine; the error is a plain unresolved path, not a message saying
  the item exists on another target.
- A portable item whose signature differs between files breaks only the
  builds of one target; the test that uses every portable item on every
  target catches it.
- C code on thumbv7m can divide 64-bit values; the consequence of ADR
  0074 that said it cannot is gone.

## Alternatives considered

- **`if` on a target constant at compile time, analyzing only the
  branch taken, as Zig** — conditional compilation by another name, and
  code in the branch not taken would not be checked; ADR 0077 rules it
  out for programs, and `core` needs no more than a file per target.
- **One `asm` with a template per target** — chooses the instructions,
  not items that exist on one target only, such as the EABI's functions.
- **Compiler intrinsics instead of `core` code** — each instruction
  would need code generation support, where `asm` lets `core` write it
  in the language.
- **Telling the program "`X` exists only on thumbv7m"** — needs the
  compiler to read the other target's file on every build, for a better
  message; can be added later without changing what builds.
