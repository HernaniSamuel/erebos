# 0091. Enabling and dispatching external interrupts

- Status: Accepted
- Date: 2026-11-06

## Context

ADR 0086 says that each source of interrupts has its own enable, and leaves
external interrupts to this step. Two controllers are in question, and they differ
in what matters here:

- **The NVIC is part of ARMv7-M:** its registers are at the same addresses on every
  chip (`ISER` at `0xE000_E100`, `ICER` at `0xE000_E180`, `ISPR` at `0xE000_E200`,
  `ICPR` at `0xE000_E280`, 16 words each, one bit per line). They are write-1
  registers: setting a bit touches only that line, so no read-modify-write races
  between contexts. Many SVD files, like the STM32s', declare a peripheral `NVIC`
  there; others say nothing of it.
- **The PLIC is not part of RISC-V:** its address is the chip's, and some chips, like
  the ESP32-C3, have none. On QEMU `virt` it is at `0x0C00_0000`. A source needs a
  priority above 0 and its bit in the enable word of the context, with `mie.MEIE` set
  in the core; a handler reads `claim`, which names a source and takes it, and writes
  the number back to complete it.

A probe on QEMU 11.1, written in Erebos while planning, found:

- `mps2-an385` has 32 lines (`ICTR` 0). The CMSDK UART0's transmit interrupt arrives
  at line 1, with `IPSR` 17, `INTSTATUS` bit 0 set, and a table in RAM reached through
  `VTOR`;
- on `virt` the 16550 is source 10. Setting `IER.THRI` raises it at once with `IIR` 2;
  with `MCR.LOOP`, a byte written comes back as a received one, with `IIR` 4.

ADR 0078 has one priority level. ADR 0080 makes `core::arch` the place of what a
target's instructions and architecture define, and allows items specific to one
target.

## Decision

### thumbv7m: functions of `core::arch`

`rt/arch/armv7m.ers` gains five public functions, specific to this target, not in
its `// portable:` list:

```
pub fn nvic_enable(irq: u32)
pub fn nvic_disable(irq: u32)
pub fn nvic_pend(irq: u32)
pub fn nvic_unpend(irq: u32)
pub fn nvic_is_pending(irq: u32) -> bool
```

| Function | What it does |
|----------|--------------|
| `nvic_enable` | a write of the line's bit to `ISER` |
| `nvic_disable` | a write to `ICER`, then `dsb` and `isb`, so that no handler of the line starts once it returns |
| `nvic_pend` | a write to `ISPR` |
| `nvic_unpend` | a write to `ICPR` |
| `nvic_is_pending` | a read of `ISPR`, testing the bit |

- **They take the number**, the variant's value converted with `as u32` (ADR 0089):
  `core` cannot name the chip's enum, and a conversion is written (ADR 0008).

  ```
  core::arch::nvic_enable(Interrupt::Uart0Tx as u32);
  core::arch::enable_interrupts();
  ```

- **A number of 496 or more fails an `assert` in `core`**, reaching the panic
  function: no word holds it, and the address would be another register of the
  System Control Space. The word's address is computed by one private function, which
  `comptime` assertions in the same file check at lines 0, 31, 32 and 495, since the
  machine of the tests has lines in the first word only.
- **The four that write are compiler fences, before the write and after it**, as
  `enable_interrupts` is one (ADR 0086). A volatile access does not order ordinary ones
  (ADR 0023), and each of them decides whether a handler runs at that point: statics
  written before the call must be in memory, and statics read after it must not have
  been read before. It costs no instruction.
- `nvic_disable`'s two barriers are for the hardware: without them the core may still
  take the line for a few instructions. Arm's CMSIS does the same.
- **They are callable from any context:** the registers are write-1, and the rule of
  ADR 0078 concerns statics.
- **No priorities, no `IABR`, no `STIR`.** Every line stays at priority 0, the one
  level of ADR 0078; a program that writes `IPR` leaves that model, as it does by
  writing any other priority. `nvic_is_pending` serves a program that polls a masked
  line and the tests.
- **A chip's own `NVIC` peripheral and these functions coexist:** the functions write
  through raw `*mut volatile u32` pointers, which no overlap check concerns (ADR 0058),
  so a module generated from a file that declares the peripheral still compiles, and a
  program may use either.

### riscv32: the program's

Nothing is added to `core::arch`. The PLIC's registers are in the chip's module, as
any peripheral's (ADR 0058, 0070), and the program writes them:

```
const UART_SOURCE: u32 = Interrupt::Uart0 as u32;

entry fn main() -> ! {
    PLIC.priority[UART_SOURCE as usize] = 1;
    PLIC.enable[0].sources[0] = 1 << UART_SOURCE;
    PLIC.ctx[0].threshold = 0;
    asm("csrs mie, {}", in(reg) 0x800 as u32, options(nostack, preserves_flags));
    core::arch::enable_interrupts();
    // ...
}
```

`mie.MEIE` stays one line of `asm`, as ADR 0086 says of `mie` and the CLINT's
programs do.

**One level:** the program gives every source the same priority, above 0, and leaves
the threshold 0. A trap clears `MIE`, so the handler is not preempted (ADR 0078).

### Dispatch on riscv32

The handler is the one of `Vector::MachineExternal`, and it is the program's:

```
interrupt fn external() at Vector::MachineExternal {
    loop {
        let source: u32 = PLIC.ctx[0].claim;
        if source == 0 {
            break;
        }
        match source {
            UART_SOURCE => uart_interrupt(),
            _ => board::exit(3),
        }
        PLIC.ctx[0].claim = source;
    }
}
```

The loop takes the sources that became pending while one was served, and ends on the
0 that means none. Constants of an integer type are patterns (ADR 0037), and an
integer does not convert to an enum (ADR 0035), so the number from `claim` is compared
with the list's values as integers, through constants as above, or with `==`.

The compiler is not involved: the function is a handler like any other, in the handler
context, and a program without one reaches the panic function with the name
`MachineExternal` (ADR 0085).

### Conformance tests

- **Unit, in the file:** the `comptime` assertions on the words' addresses.
- **ui snapshot:** `core::arch::nvic_enable` named on riscv32 is the code a path that
  names nothing has, as ADR 0080 says of an item of another target.
- **End to end,** thumbv7m, both levels, `// takes-exceptions: yes`:
  - a line enabled, pended with `nvic_pend` while masked, seen by `nvic_is_pending`,
    and run once on `enable_interrupts`, its handler finding `IPSR` `16 + n`;
  - `nvic_unpend` leaves it not pending, and no handler runs;
  - `nvic_disable` keeps a pended line from running until `nvic_enable`;
  - `nvic_enable(496)` reaches the panic function, as a failed `assert`;
  - the milestone of ADR 0092.
- **End to end,** riscv32, both levels: the milestone of ADR 0092, and two causes of
  one source through the 16550's loopback (`IIR` 2 and 4).
- `tests/e2e.rs`'s check of portable items keeps passing: these are not in its list.

## Consequences

- Enabling a line on thumbv7m is one call in any program, with or without an SVD.
- The functions are in every thumbv7m program, as the rest of `core` (ADR 0020): an
  estimated 200 bytes, with the `assert`'s `PanicInfo` and message when the panic
  function takes information.
- A program calling `nvic_enable` with no list, or above its largest value, compiles,
  and the core reads a word of `_start`'s code when the line fires (ADR 0090). The
  number is a value at run time, and a list cannot be demanded of a function called by
  hand-written drivers.
- Taking a number gives a program the same power as a raw write, and no check that
  the number is the chip's. M5's ownership of peripherals may offer more; these
  functions stay.
- riscv32's dispatch is written out in each program. A module in `core` cannot do it,
  the PLIC being no part of the architecture; a board's module can.
- Line numbers above 31 cannot run on the tests' machine, so the word's address is
  checked at compile time only.

## Alternatives considered

- **Every register in the program, NVIC included** — no new item in `core`, but every
  ARM program repeats a declaration and bit arithmetic for what the architecture fixes.
- **A peripheral `NVIC` in `core::arch`** — reads like any peripheral, but overlaps the
  one many chips' SVD files generate, and M5's ownership would have to make an
  exception for it.
- **A portable `core::arch::enable_irq(n)`** — not possible: riscv32's file cannot know
  the address of a controller the architecture does not have.
- **`mie.MEIE` in a function of `core::arch` on riscv32** — symmetric, but one line of
  `asm` that ADR 0086 leaves to the program, and `mie`'s other bits would ask for more.
- **Functions that take an enum** — no generics yet (M6), and `core` cannot name the
  chip's list.
- **No check on the number** — one comparison less, but a mistake would write another
  register of the System Control Space with no sign of it.

Defaults assumed while writing this ADR, for correction when accepting it:

- **`nvic_pend`, `nvic_unpend`, `nvic_is_pending` in `core`** — the milestone does not
  need them, but the tests do, and each is two lines over the same word; a program
  debugging a line needs them too.
- **The names `nvic_*`** — the name says the target-specific thing it is; ADR 0080
  allows them as specific to one target.
