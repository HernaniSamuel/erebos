# 0089. The chip's interrupts

- Status: Accepted
- Date: 2026-11-06
- Amends: 0084 (the path after `at` may also name a variant of the chip's
  list on a target whose interrupts are vectors; the meaning of
  `resolve::not-a-vector`)

## Context

ADR 0084 lets a handler serve what the core defines, the variants of
`core::arch::Vector`. A chip adds its own interrupts, which differ per chip:
`core` cannot list them. The roadmap agrees that they come from an enum, which
`erebos svd` writes (ADR 0092), and leaves to this step how the compiler knows
which enum is the chip's. ADR 0084 keeps the form: the path after `at` stays a
path to a variant.

The two targets reach the chip's interrupts differently:

- **ARMv7-M** has the NVIC in the architecture: each interrupt line `n` of the
  chip is exception `16 + n`, with a word of its own in the vector table. A
  handler of a line is a handler like SysTick's.
- **RISC-V** in machine mode has one cause for all of them,
  `MachineExternal`. What says which source raised it is a controller outside
  the architecture, at an address of the chip's: the PLIC on QEMU `virt`, and
  other things elsewhere (the ESP32-C3's file lists 62 interrupts, and that
  chip has no PLIC). The roadmap agrees that the program dispatches them.

So the same enum is a list of vectors on one target and a list of numbers on
the other.

## Decision

### `#[interrupts]`

An enum declares itself the chip's list with an attribute:

```
#[interrupts]
pub enum Interrupt: u32 {
    Uart0Rx = 0,
    Uart0Tx = 1,
}
```

- It applies to an enum declared in the program, takes no argument, and is
  given once. On any other item it is `resolve::misplaced-attribute`, with an
  argument `resolve::attribute-argument`, and given twice
  `resolve::duplicate-attribute`, as the attributes of ADR 0051 and 0079.
- **A program has at most one such enum**, in any module, `pub` or not, as it
  has one `memory` block (ADR 0057): each further one is
  `resolve::duplicate-interrupts`, labelling it and the first, in the order of the
  enums' ids, which follows the source. A handler at a variant of a further one
  reports nothing more: its variants resolve as the list's, so that the program gets
  one error for one mistake. `core` declares none. A program with none has no chip's
  list, which is every program until now.
- The enum's integer type is `u32` and it has no payloads, or
  `ty::interrupts-enum`.
- **A variant's value is the number the chip's controller gives the
  interrupt:** the line of the NVIC on thumbv7m, the source of the PLIC on QEMU
  `virt`. A program converts it with `as u32` where it needs the number.
- The attribute does not change the enum otherwise: its variants are used as
  any variant's, and `use`, visibility and `match` apply.

### On thumbv7m: vectors

`src/target.rs` says, per target, whether the chip's interrupts are vectors:
`Target::external_vectors`, on thumbv7m the first word (16) and the most
interrupts the architecture has (496, so exceptions up to 511), on riscv32
none.

On a target where they are vectors:

```
use chip::Interrupt;

interrupt fn transmitted() at Interrupt::Uart0Tx {
    // ...
}
```

- The path after `at` may name a variant of the chip's list. It is resolved as
  ADR 0084 says, and shorthand and integers stay refused. A path that names
  anything but a variant of `core::arch::Vector` or of the chip's list, the
  variant of another enum included, is still `resolve::not-a-vector`, whose help
  now names `#[interrupts]`.
- **A handler at a variant of the list is maskable**, in the handler context of
  ADR 0078, and returns `()` or `!`. `PRIMASK` masks every line, and every line
  resets to priority 0, so the one level of ADR 0078 holds.
- `resolve::duplicate-vector`, `ty::interrupt-params`, `ty::interrupt-return`,
  `resolve::call-interrupt` and the other rules of ADR 0084 apply as to any
  handler. A vector is told by its enum and variant, **never by its name alone**:
  a chip's variant called `HardFault` is a line of the chip, and does not have to
  diverge, and a handler at `Vector::SysTick` and one at a chip's variant called
  `SysTick` serve two vectors. So `--emit=typeck` and `--emit=mir` write a list's
  vector with its enum's path (`at chip::Interrupt::SysTick`), and a `Vector`'s as
  ADR 0084 has it (`at SysTick`).
- **A value of 496 or more is `ty::interrupt-number`**, at the variant: the
  architecture has no such line. The limit is the architecture's and not the
  Cortex-M3's 240, since this target also runs on later cores (ADR 0074). The
  check runs whether or not a handler names the variant.

ADR 0090 says what the list does to the vector table.

### On riscv32: a list for the program

On a target where they are not vectors the compiler reads the list for nothing:
the attribute's rules above hold, except the limit, and the table of ADR 0085 does
not change. The list gives the program the names to dispatch with; ADR 0091 shows
how.

**A handler at a variant of the list is `ty::interrupt-not-a-vector`**, with a
help that says the interrupt arrives at `Vector::MachineExternal`. It is the type
checker's, not resolution's, since lowering to HIR does not know the target.

The attribute is accepted on every target so that a generated module means the
same everywhere, and `erebos svd` need not know the core (ADR 0092).

### Diagnostic codes

| Code | Meaning |
|------|---------|
| `resolve::duplicate-interrupts` | a second enum with `#[interrupts]` in a program |
| `ty::interrupts-enum` | an enum with `#[interrupts]` whose integer type is not `u32`, or with a payload |
| `ty::interrupt-number` | a variant of the chip's list with a value the target's table cannot hold |
| `ty::interrupt-not-a-vector` | a handler at a variant of the chip's list, on a target where they are no vectors |

`resolve::misplaced-attribute`, `resolve::duplicate-attribute` and
`resolve::attribute-argument` also cover `#[interrupts]`, and the help of
`resolve::unknown-attribute` lists it.

### Conformance tests

- **ui snapshots:** each code above; `#[interrupts]` on a struct, a function, a
  static and a constant; given twice; with an argument; two lists in two modules,
  and three for the two further reports; a list of `u8`, and one with a payload; on
  thumbv7m values 495 (accepted) and 496 (`ty::interrupt-number`, also with no
  handler naming the variant); on riscv32 value 496 accepted; a handler at a
  variant in full, through `use`, and of a list in a module; a variant of an enum
  without the attribute (`resolve::not-a-vector`); two handlers at one variant
  (`resolve::duplicate-vector`); a handler at a variant called `HardFault` that
  returns `()`, accepted; a handler at `Vector::SysTick` beside one at a list's
  variant `SysTick`, accepted and told apart in `--emit=mir` and `--emit=typeck`; a
  handler at a variant of a further list, with one error for it; a handler returning
  `u32` (`ty::interrupt-return`); a handler of the list on riscv32
  (`ty::interrupt-not-a-vector`).
- **Unit test:** `Target::external_vectors` on each target.
- **End to end:** see ADR 0090, 0091, whose programs run handlers.

## Consequences

- A chip's module is declared with one line, and nothing else in the program says
  it is the chip's; the attribute is the one place to read.
- A module with `#[interrupts]` compiles on both targets, but a program with
  `at Interrupt::X` builds only for thumbv7m, as any program naming a `Vector`
  builds for one target (ADR 0084).
- A program cannot declare two chips' lists. A board with two interrupt controllers
  writes one list, or one list and plain enums.
- From M4.5, the handlers of a list are in the handler context, and what they reach
  is checked as for SysTick. On riscv32 the rule applies through `MachineExternal`'s
  handler.
- A hand-written enum works as a generated one, so a program needs no SVD.

## Alternatives considered

- **A line of the `memory` block (`interrupts chip::Interrupt;`)** — the uniqueness
  comes for free and no attribute is added, but it mixes interrupts into the memory
  map, and a generated module would not be enough by itself.
- **A fixed path (`chip::Interrupt`)** — no new syntax, but a hidden convention:
  renaming a module would change what the program means, and no diagnostic could
  explain it.
- **A new item (`interrupts Interrupt { ... }`)** — says what it is, but the roadmap
  agrees on an enum, whose values, `as`, `match` and diagnostics already exist.
- **The chip's variants added to `core::arch::Vector`** — `core` is the same for
  every chip (ADR 0052).
- **On riscv32, `at` accepted, with a dispatch the compiler generates** — it would
  have to know the address and the protocol of a controller that is no part of the
  architecture, and the ESP32-C3 has none. The roadmap leaves dispatch to the
  program.
- **An error for `#[interrupts]` on riscv32** — the generator would have to know
  the target of a module that is not compiled yet.

Defaults assumed while writing this ADR, for correction when accepting it:

- **The integer type must be `u32`** — any unsigned type could carry the numbers,
  but `Vector` is `u32`, and the conversion to a number stays one form.
- **The limit is 496, not 240** — a Cortex-M3 has at most 240 lines; a program
  built for one that lists more gets a larger table and no error.
- **The name `#[interrupts]`** — `#[chip_interrupts]` says more, but the attribute
  stands above an enum that shows what it is.
