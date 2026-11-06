# 0090. The thumbv7m table with the chip's interrupts

- Status: Accepted
- Date: 2026-11-06
- Amends: 0057 (the origin of the code's region must also suit the table of the
  program's list), 0074 (`code_align` stays 128 as a minimum; a program with a list
  may need more), 0083 and 0085 (the table has more than 16 words with a list, so
  `_start` is further from the origin; default handlers for lines, and for words
  with no variant, on thumbv7m)

## Context

ADR 0085 gives the thumbv7m table 16 words, the system exceptions, and leaves to
this step its length with the chip's interrupts and what follows for `code_align`.
Today a line enabled in the NVIC makes the core read a word of `_start`'s code.

ADR 0074 warns of the alignment: ARMv7-M requires a table to be aligned to a power
of two at least as large as the number of exceptions the chip supports times 4, and
at least 128 bytes, since `VTOR.TBLOFF` starts at bit 7. `code_align` is 128 today,
and the reset entry, which is that table, is at the origin of the code's region.

ADR 0085 also says that with a panic function that takes information, each vector
without a handler has a default handler of its own, naming the vector. A chip's
list multiplies that: about 50 bytes per interrupt (the 28 of `PanicInfo`, the name,
and the small function).

A list may have holes: the numbers of a chip's lines are rarely contiguous.

## Decision

### Length

| Program | Words of `.vector_table` |
|---------|--------------------------|
| without a chip's list (ADR 0089) | 16, as ADR 0085 says |
| with a list whose largest value is `m` | `16 + m + 1` |

The list alone decides, whether or not a handler names a variant, and whichever
module holds it. `_start` follows the table, at the origin plus 4 bytes per word;
the linker script does not change. riscv32's table does not change either: 33 words,
the chip's interrupts arriving at the word of `MachineExternal`.

### The words past the system exceptions

| Word | Holds |
|------|-------|
| `16 + n`, where a variant has the value `n` | the handler at that variant, or its default handler |
| `16 + n`, where no variant has it | the default handler of a word with no variant |

Words 0 to 15 are as ADR 0085 says.

**A word with no variant holds a default handler, not 0.** The architecture never
reads the reserved words 7 to 10 and 13, which hold 0 and stay so. A line in a hole
can be read: a list may be incomplete, and `nvic_pend` (ADR 0091) takes any number.
A word of 0 would make the core jump to address 0 and report a fault with no name;
this reaches the panic function and says `unknown`, as riscv32 does for a cause
`Vector` lacks.

**Past the table's end nothing is checked.** A line above the list's largest value
makes the core read a word of `_start`'s code, as ADR 0085 says of any line today,
and so does any line in a program with no list. The list is the program's statement
of what the chip has, and the compiler cannot see a number that a program computes.

### Default handlers

- **A panic function without a parameter:** the one shared function,
  `__erebos_unhandled`, for these words too. No information is built, as ADR 0053
  asks.
- **A panic function with `*const core::PanicInfo`:** as ADR 0085, one function per
  variant without a handler, each passing read-only information of its own:
  - the symbol is `__erebos_unhandled_irq_<variant>`. The prefix `irq_` keeps it
    from meeting `__erebos_unhandled_<variant>` of a `Vector` (a chip may call a
    line `SysTick`) or `__erebos_unhandled_other`;
  - `reason` is `PanicReason::UnhandledVector`, `message` the variant's name,
    `file` empty, `line` and `column` 0, as ADR 0085 says. A chip's line called
    `SysTick` and an unhandled `Vector::SysTick` print the same message: the panic
    function tells them apart through `IPSR`, and a prefix on every line's name would
    cost bytes for a case that needs a chip to reuse a core's name;
  - the words with no variant share `__erebos_unhandled_other`, the name and the
    `unknown` message riscv32 already has. It exists on thumbv7m only for a program
    whose list has a hole.

Default handlers stay **no contexts** of ADR 0078 (ADR 0085).

A panic that names the line is what is missing when a handler was forgotten or
named the wrong line, so the rule of ADR 0085 stays, and the cost follows: about 3
KB for 62 interrupts and 12 KB for 240, in a program whose panic function takes the
information, however few lines it uses. A program that needs those bytes declares
its panic function without the parameter, and pays one function.

### The origin's alignment

`Target::code_align` stays 128 on thumbv7m, as the minimum. A program with a list
needs its code's origin to be a multiple of

```
max(code_align, the smallest power of two >= 4 * words)
```

| Largest value | Words | Bytes | Origin a multiple of |
|---------------|-------|-------|----------------------|
| no list | 16 | 64 | 128 |
| 15 | 32 | 128 | 128 |
| 16 | 33 | 132 | 256 |
| 61 | 78 | 312 | 512 |
| 239 | 256 | 1024 | 1024 |
| 495 | 512 | 2048 | 2048 |

`ty::misaligned-code` reports it, and its help gives the number of words when the
list is why. Where a value of the list failed, or is above the limit (ADR 0089), an
error is already reported and this check is skipped. It applies whether or not the
program ever writes `VTOR`: as ADR 0074 says, the reset entry is the table a program
would point `VTOR` at, and a table at that origin could not be reached otherwise. On
riscv32 nothing changes: `code_align` is 4.

### Dumps

The words are in `--emit=llvm-ir` under the rule of ADR 0085: the table and its
default handlers appear when the program declares a handler, and a program without
handlers has no `module asm`, whatever its list.

### Conformance tests

- **ui snapshots:** the LLVM IR of a program with a list, a handler at a variant and
  a hole, with a panic function with and without the parameter: the words, the
  `irq_` defaults, `__erebos_unhandled_other`; a variant called `SysTick` and one
  called `other` beside the core's, with distinct symbols; a program with a list and
  no handler has no `module asm`; `ty::misaligned-code` at each boundary of the table
  above (largest value 15 at origin 128, accepted; 16 at origin 128, rejected, and at
  256, accepted; 495 at 1024, rejected, and at 2048, accepted).
- **ELF:** at both levels, `.vector_table` has `16 + m + 1` words at the origin;
  `_start` follows, with bit 0 set; each word holds its handler's or default's
  symbol with bit 0 set, and words 7 to 10 and 13 hold 0; a program with no list is
  as `thumbv7m_vector_table` already checks, 16 words.
- **End to end,** thumbv7m, both levels, with lines pended in the NVIC (ADR 0091):
  a variant without a handler reaches the panic function with its name, a hole
  reaches it as `unknown`, and the panic function reads `IPSR` as `16 + n`; a panic
  function without the parameter reached the same way.

## Consequences

- A program with a list can take any line the list names, and a forgotten handler
  panics by name instead of running code at random.
- A table grows with the highest number, not with what the program uses. On chips
  with many lines that is 1 KB of table that a program pays once it declares the list,
  and more for the defaults; a program that must not pay writes a shorter list by
  hand, knowing that the words past its end are not protected.
- The largest value is what the program knows of the chip, and ARMv7-M's rule speaks
  of what the chip supports. A list shorter than the chip's lines may pass the check
  on an origin that would not suit the whole table.
- The alignment depends on the program, so a change to a chip's module can reject an
  origin that used to pass. Real chips put code at addresses aligned far beyond that
  (`0x0`, `0x0800_0000`), as ADR 0074 notes.
- `__erebos_unhandled_irq_` is one more prefix under the names `__erebos_` reserves
  (ADR 0020).

## Alternatives considered

- **A fixed table of 256 or 512 words in every program** — any line is safe, with no
  list needed, but every program pays up to 2 KB, and a table is no use without
  naming the handlers.
- **A word of 0 in a hole, as the reserved ones** — shows "nothing here" in a dump,
  but those words are ones the architecture never reads, and a line can be read.
- **`code_align` raised to 2048 for everyone** — no dependence on the program, but it
  rejects origins that a small chip's table suits. The check is a few lines.
- **One shared default for every line, with the message `interrupt`** — a fixed cost,
  and the panic function can read `IPSR`, but ADR 0085 refused that form for
  `Vector` because the roadmap agrees the vector is passed, and it is the case where
  a name helps most.
- **The same symbol as a `Vector`'s (`__erebos_unhandled_<variant>`)** — no new
  prefix, but a line named after a system exception would collide, which the
  generator cannot rule out.
- **Reading `deviceNumInterrupts` for the length** — the compiler never reads SVD
  (ADR 0070), and an enum cannot say more than its variants without a second
  construct. The ESP32-C3's file does not state it.

Defaults assumed while writing this ADR, for correction when accepting it:

- **The message of a line is its bare variant name** — a prefix (`irq Uart0Tx`) would
  tell a line from a core's vector of the same name, for the cost of its bytes in
  every default handler.
