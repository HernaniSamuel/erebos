# 0057. Memory maps

- Status: Accepted
- Date: 2026-10-25
- Amends: 0012 (`_start` sits at the origin of the code's region instead
  of first in RAM, and takes the stack's top from the linker script; the
  stack has a declared size and place), 0030 (the program's `memory`
  block replaces the fixed split of QEMU's RAM; when data shares the
  code's region, the load image of `.data` is its run range)

## Context

Where code, data and the stack live is a property of the board, not of the
architecture: the same RV32IMAC core sits behind 64 KiB of flash at
`0x0800_0000` on one chip and behind RAM at `0x8000_0000` on QEMU `virt`.
Until now the compiler fixed it: the target description (`src/target.rs`)
holds QEMU's RAM, the generated linker script splits it into an `IMAGE`
region playing the part of flash and a `RAM` region (ADR 0030), and
`_start` puts the stack pointer at the end of RAM with an immediate
(ADR 0012). The stack has no size: it grows down from the top of RAM until
it runs silently over `.bss` and `.data`.

ADR 0002 makes memory maps a language construct, and the roadmap gives
them to M3: a `memory { ... }` block generates the linker script. C
programs get the same from a hand-written linker script, a language of its
own with no checks before link time; Rust's embedded crates from a
`memory.x` fragment merged into a script the crate ships.

## Decision

### The `memory` block

```
const KIB: usize = 1024;

memory {
    region FLASH { origin: 0x0800_0000, size: 64 * KIB }
    region RAM { origin: 0x2000_0000, size: 16 * KIB }
    region SRAM2 { origin: 0x2000_4000, size: 16 * KIB }

    code in FLASH;
    data in RAM;
    stack in RAM { size: 2 * KIB }
}
```

A `memory` block is an item. It declares **regions**, each a range of
addresses with a name, and **placements**, which say what each region
holds. A program has exactly one `memory` block to be built into an
executable. It may sit in any module, so that a module describing a board
can hold it, but a second one anywhere in the program is an error.

Its grammar, where regions and placements may come in any order:

```
memory_block = "memory" "{" { region | placement } "}"
region       = "region" NAME "{" "origin" ":" expr "," "size" ":" expr [","] "}"
placement    = "code" "in" NAME ";"
             | "data" "in" NAME ";"
             | "stack" "in" NAME "{" "size" ":" expr [","] "}"
```

- **Regions.** `origin` and `size` are `usize` expressions computed at
  compile time, like array lengths and `#[align(N)]` arguments
  (ADR 0029), resolved in the scope of the module holding the block, so
  constants name sizes and addresses. A region covers
  `[origin, origin + size)`, and must end below 2³², so that the address
  one past its end, which the linker's end symbols hold, exists.
  Region names belong to the block: they are not items, so a constant
  of the same name is no conflict, a name in an expression of the block
  is looked up among items as anywhere else, and code cannot name a
  region.
- **Placements.** Each of `code`, `data` and `stack` is placed exactly
  once:
  - `code in R;` puts in `R`, in this order, the target's reset entry
    (on riscv32, `_start`), the code (`.text`), the read-only data
    (`.rodata`) and the initial values of `.data`. The reset entry is at
    `R`'s origin, which must therefore be a multiple of the target's
    code alignment (4 on riscv32), and must be the board's reset
    address; the compiler cannot check the latter.
  - `data in R;` puts in `R` the run addresses of `.data`, then `.bss`.
    `R` may be the code's region, for a board that runs from RAM: the
    initial values of `.data` are then at its run addresses, and
    `_start` copies `.data` onto itself, which is harmless (ADR 0030
    keeps the copy unconditional). This amends ADR 0030, whose three
    startup ranges are pairwise disjoint: in that case the load image
    and the range of `.data` are one; otherwise the rule holds as
    written.
  - `stack in R { size: E }` reserves `E` bytes for the stack at the
    **start** of `R`, below anything else `R` holds. The stack grows
    down, so a stack that overflows leaves the region below its origin,
    where many chips have no memory or memory that faults, instead of
    running over `.data` and `.bss` unnoticed. Rust's `flip-link` follows
    the same idea, though it gives the stack all the RAM below the
    statics, while here the stack has a declared size and the free RAM
    is left above `.bss`. The stack cannot share the code's region,
    whose origin the reset entry needs. Its start is the region's origin
    rounded up to the target's stack alignment (16 bytes on riscv32), and
    its size must be a multiple of that alignment.
- A region nothing is placed in is allowed. It documents memory the
  program uses by address.
- The words inside the block (`region`, `origin`, `size`, `code`, `data`,
  `stack`, `in`) are keywords only there, and `memory` only where an item
  starts and `{` follows, since all of them are common names elsewhere.
  Attributes and `pub` do not apply to a `memory` block. A `memory`
  block inside a function body, recognized by `memory {` followed by
  `region` and a name, or by `code`, `data` or `stack` and `in`, is
  `parser::local-memory`, as other items there are `parser::local-*`.
  Anything else after `memory {` there is a struct literal, as
  `memory { data: 1 }` for a struct named `memory`.

### The linker script

The compiler generates the linker script from the block:

- `MEMORY` lists every region under its own name, so that the script and
  the linker's messages read like the source. A name the linker script
  language cannot hold there, `INCLUDE`, is a `resolve::reserved-name`.
- `SECTIONS` places the reset entry in an output section of its own,
  first in the code's region, then `.text`, `.rodata` and the initial
  values of `.data`. The linker aligns an output section to the largest
  alignment of its inputs, so with `_start` inside `.text`, a function
  aligned to 64 in an object given with `--link` would move `_start` off
  the origin without a word; in a section of its own, only its own
  alignment counts, which the origin's check covers.
- In the regions of the stack and the data, it places a `.stack` section
  without contents, then `.data` and `.bss`. Everything else
  in the script stays as ADR 0030 has it: `.data` and `.bss` aligned to 4
  and padded inside, the five symbols of the startup loops, the
  small-data sections, the unloaded sections, `--orphan-handling=error`.
- It defines `__stack_start` and `__stack_end`, the bounds of the stack.

`erebos build --emit=linker-script` prints the script instead of linking.

`_start` (ADR 0012, 0030) sets the stack pointer to `__stack_end` instead
of an address fixed by the compiler. Code generation therefore does not
depend on the memory map: the LLVM IR of a program is the same whatever
its board.

### What is checked

When names are resolved:

| Code | Case |
|------|------|
| `resolve::duplicate-memory` | a second `memory` block in the program |
| `resolve::duplicate-region` | two regions with one name |
| `resolve::unknown-region` | a placement names no region of the block |
| `resolve::duplicate-placement` | `code`, `data` or `stack` placed twice |
| `resolve::missing-placement` | `code`, `data` or `stack` not placed |
| `resolve::stack-with-code` | the stack placed in the code's region |
| `resolve::reserved-name` | a region named `INCLUDE` |

"Second" means later in the order of files, which are numbered depth
first (ADR 0049), then of positions in a file; the error points at the
second block, or name, or placement, and labels the first.

Once the block's values are computed:

| Code | Case |
|------|------|
| `ty::empty-region` | a region of size 0 |
| `ty::region-out-of-range` | a region ending at 2³² or past it |
| `ty::region-overlap` | two regions sharing an address; reported once per pair, at the later region, labeling the earlier |
| `ty::misaligned-code` | the code's region starting at an address that is not a multiple of the target's code alignment |
| `ty::stack-size` | a stack of size 0, or not a multiple of the stack alignment, or one that does not fit its region once its start is rounded up (`align_up(origin) + size > origin + region size`) |

A stack size that is both not a multiple of the alignment and too large
is reported as not a multiple. A check that needs a value whose
computation failed (already reported) is skipped, as for failed lengths
and alignments (ADR 0048, 0055).

Syntax errors in the block are `parser::unexpected-token`; attributes on
it are `resolve::misplaced-attribute` and `pub` is `parser::misplaced-pub`,
as for other items. Errors in computing a value are those of any
compile-time evaluation.

When the compiler builds an executable, or prints its script:

| Code | Case |
|------|------|
| `link::no-memory-map` | the program has no `memory` block |

When it links:

| Code | Case |
|------|------|
| `link::region-overflow` | what is placed in a region does not fit in it |

Only the linker knows how large the code is. A region overflow is the
linker's error, read back from its messages. `ld.lld` reports each
section that ends past the region, with the overflow counted up to that
section's end; the compiler reports **one** diagnostic per region, at
the region in the source, naming the first section that does not fit and
the largest overflow, which is how many bytes the region lacks. The
initial values of `.data` are named as such, not as `.data`, whose run
addresses are in another region. The linker's other errors are reported
as today.

## Consequences

- Programs say where they live, and one compiler builds for any board of
  a supported architecture. The target description loses QEMU's RAM.
- The stack has a size, and an overflow leaves its region instead of
  corrupting statics. A program that needs more stack than it declares
  still fails at run time: detecting that, as on a chip that faults below
  RAM, needs trap handlers (M4). On QEMU `virt` every region is carved
  out of one RAM, so an overflow there writes into the region below, if
  any.
- Every program built into an executable must declare a `memory` block,
  including every test program. A board module shared by programs avoids
  repeating it, but only among programs whose module trees can reach it
  (ADR 0049).
- The board's reset address, which `code`'s region must start at, is not
  checked.
- A program on one region of RAM, as QEMU's, splits it in two, since the
  stack cannot share the code's region.
- The last byte of the address space cannot belong to a region.
- Overflow messages depend on the wording of `ld.lld`'s errors; a change
  there falls back to the linker's own message, which still names the
  region.
- Deferred: placing a static in a named region (DMA buffers, statics left
  uninitialized across a reset), which needs `_start` to fill more than
  one range; and reading a region's values from code. Both are recorded
  in the roadmap, which leaves open how a static names its region.

## Alternatives considered

- **A default memory map for QEMU `virt` when the block is missing** —
  fewer lines in test programs, but the board would hide in the compiler
  again, and the first real board would need the block anyway.
- **Roles written inside each region (`holds: code`)** — compact, but the
  list of roles and the list of fields share a syntax; separate placements
  read as what they are.
- **The stack at the end of its region, as C startup files put it** — an
  overflow then runs over `.bss` and `.data` with no sign.
- **The stack's place in the region written by the program** — more
  syntax and more cases to check, for little use: the start is the safe
  place and the only one needed.
- **Region access flags (`rx`, `rw`) as in GNU `MEMORY`** — the linker
  uses them only to place orphan sections, and there are none
  (`--orphan-handling=error`); the placements already say what each region
  holds.
- **Checking region overflow before linking** — the size of the code is
  known only after LLVM, and the linker already checks.
- **A region may end exactly at 2³²** — its end symbols would wrap to 0,
  and the startup loops of ADR 0030, which compare with `bgeu`, would
  skip `.bss` and `.data` without a word. Comparing with `bne` instead
  would allow it, for one byte of address space no board places memory
  in.
- **Rounding a misaligned code origin up, as for the stack** — the reset
  entry would no longer be at the address the board jumps to.
- **`memory`, `region` and the placement words as global keywords, as
  `static` and `entry` are** — `data`, `code`, `size` and `stack` are
  common variable names, and nothing else starts an item with a name, so
  the parser can tell them apart by position.
