# 0092. Interrupts generated from SVD

- Status: Accepted
- Date: 2026-11-06
- Amends: 0070 (`svd::missing-element` also covers an `interrupt`; the program of the
  tests gains two more kept modules), 0071 (the generated file ends with the list;
  a row for its name and variants), 0072 (`interrupt` is read; "Interrupts are not
  generated" no longer holds)

## Context

The roadmap agrees that `erebos svd` writes the chip's interrupts as an enum, from
`<interrupt>`; ADR 0072 reads nothing of them so far. SVD places them under
peripherals, each with a `name`, a `value` and an optional `description`:

```xml
<peripheral>
  <name>AES</name>
  <interrupt>
    <name>AES</name>
    <value>48</value>
  </interrupt>
```

A peripheral may have several, and several peripherals may name one interrupt: vendors
repeat a shared line under each peripheral that uses it. The ESP32-C3's file has 62
interrupts, each with a name and value of its own, the largest value 61. `<cpu>` may
state `deviceNumInterrupts`, not always; the ESP32-C3's file names its core `RV32IMC`.

ADR 0089 says what `#[interrupts]` means on each target.

## Decision

### What is written

The generated file ends, after the peripherals, with the list:

```
#[interrupts]
pub enum Interrupt: u32 {
    // UART 0, receive.
    Uart0Rx = 0,
    // UART 0, transmit.
    Uart0Tx = 1,
}
```

- **The enum is named `Interrupt`.** It is the last item, so if a peripheral has that
  name the enum is the later one, and ADR 0071's rule makes it `Interrupt_`, with
  `svd::renamed`.
- **Its variants are in the order of their values**, the file's order where two
  values are equal (see below). Each is named by ADR 0071's Pascal case (`UART0_TX` is
  `Uart0Tx`), and each of its steps and `svd::renamed` apply. The element's
  `description` is a comment above it, as ADR 0072 says.
- **Values are in decimal**, as bit positions are (ADR 0070): they are ordinals, which
  datasheets number in decimal.
- **`#[interrupts]` is always written**, on every chip. The generator does not read
  `cpu`, so a module means the same whatever core a program builds it for (ADR 0089).
- **A file with no `interrupt` writes no enum**, so every output of a file without
  them stays what it is.

### What is read

- `interrupt` elements of every `peripheral`, in the order of the file, each
  peripheral's **own**, not copied by `derivedFrom`. A derived peripheral without its
  own would only repeat its base's, which gives the same list. A `dim` on the
  peripheral repeats nothing: its `interrupt` is one, and it is an item that stands for
  the whole element, so ADR 0071's step 1 removes any `%s` or `[%s]` from its name.
- `name` and `value` are required: `svd::missing-element`, or `svd::invalid-number` for
  a value SVD's syntax does not allow (ADR 0070). A name with nothing left after its
  characters are replaced is `svd::invalid-name`, as for any item but an enumerated
  value.
- A value too large for `u32` is not the generator's: as ADR 0072 says of the
  compiler's checks, it is reported when the module is compiled. So is a value above
  the target's limit (`ty::interrupt-number`, ADR 0089).
- `cpu`, with `deviceNumInterrupts` and `nvicPrioBits`, stays unread.

### The same line or the same number, twice

Names are compared after ADR 0071's conversion, so `UART_TX` and `UART__TX` are one
name.

| Case | What is written | Warning |
|------|-----------------|---------|
| one converted name and one value, under several peripherals or not | one variant, with the first's description | none: it is what vendors write for a shared line |
| two converted names with one value | the first in the file, since two variants with one value are `ty::duplicate-discriminant` (ADR 0035); a `// svd:` comment above the enum names the others | `svd::not-declared`, at each left out |
| one converted name with two values | both, the later one `_` after it, by ADR 0071 | `svd::renamed` |

### The tests' files

- **The ESP32-C3's** generates with 62 variants, and the executable of ADR 0070 still
  builds for riscv32. The summary its test compares gains a line, `interrupts: 62`. That
  summary is this test's alone: the snapshots of the other files hold the generated
  module, which has no enum when the file has no `interrupt`, so no rule is needed for a
  summary with none.
- **`tests/e2e/uart_interrupt_riscv32/virt.svd`** describes what the milestone uses on
  `virt`: the 16550 with its `interrupt` (source 10), the PLIC with its enable words,
  the test finisher. **`tests/e2e/uart_interrupt_armv7m/an385.svd`** describes the
  CMSDK UART0 of `mps2-an385` with its lines (RX 0, TX 1), in hand-written files from
  QEMU's sources. Each has its generated module kept beside it, and `tests/svd.rs`
  requires the same bytes, as for `tests/e2e/svd_virt/` (ADR 0070).
- **A new `tests/svd/interrupts.svd`** has what those lack: each row of the table
  above, holes, an out-of-order file, a name with characters to replace, a peripheral
  named `Interrupt`, a derived peripheral with and without its own; and
  `tests/svd/errors_interrupt_*.svd` each error.

### The milestone

**thumbv7m:** an entry function queues a message in a `static mut` ring buffer inside
`critical`, starts the transmitter with the first byte, and waits with the form of ADR
0086 until the queue is empty. The handler `at Interrupt::Uart0Tx` clears `INTSTATUS`
and writes the next byte. The program exits with the output checked, at both levels.

**riscv32:** the same, with the handler at `Vector::MachineExternal`, dispatching the
PLIC's source 10 through `Interrupt::Uart0`, clearing `IER.THRI` when the queue is
empty.

**riscv32 also,** one file with a hand-written `#[interrupts]` enum, and no SVD: the
16550's loopback gives a received byte (`IIR` 4) and a transmit interrupt (`IIR` 2) of
one source. The CMSDK UART has no loopback, and a byte through QEMU's standard input
did not reach it on Windows in the probe of ADR 0091, so reception has no end-to-end
test on thumbv7m.

All use `// takes-exceptions: yes` (ADR 0088), and a panic function whose exit code is
not the expected one.

### Conformance tests

- **Snapshots** (`tests/svd.rs`): `interrupts.svd`, generated and compiled by the
  harness of that file on riscv32 and on thumbv7m; each error file.
- **Bytes:** each kept module is what the generator writes.
- **Summary:** the ESP32-C3's, with `interrupts: 62`.
- **End to end:** the programs above.

## Consequences

- A generated module gives a program the chip's interrupts by name, with one `use`.
- Names come from ADR 0071: `Interrupt::Uart0Tx`, where the peripheral stays `UART0`
  and a datasheet says `UART0_TX`; a search ignores case, as ADR 0071 says.
- A renumbering by the vendor shows as a diff of the module. A variant renamed with `_`
  depends on the order of the file, as elsewhere.
- A peripheral `interrupt` no longer goes unread, so a file with an `interrupt` lacking
  a `value`, which used to generate, now fails. That is an invalid file by SVD's schema.
- Reception on thumbv7m is untested end to end: its line number in the SVD comes from
  QEMU's sources, and only a `nvic_pend` reaches its handler.
- `core` is unchanged by this ADR, and the compiler never reads SVD (ADR 0070).

## Alternatives considered

- **Variants as the SVD writes them (`UART0_TX`)** — found in a datasheet as written,
  and they would keep the case of peripherals, but ADR 0071 gives variants Pascal case,
  and the exceptions should be few.
- **The enum first in the file** — the list is seen on opening the file, but a peripheral
  named `Interrupt` would then be the one renamed, for an item it does not know of.
- **One variant per peripheral and interrupt (`Uart0Uart0Tx`)** — no merging needed, but
  long names, and two variants of one value are an error.
- **An error for two names with one value** — the whole chip would fail for one shared
  line, where ADR 0072 declares as close as it can and warns.
- **`#[interrupts]` only when `cpu` names an Arm core** — a program on riscv32 would have
  to add nothing, but `cpu` is optional, `other` is a value of it, and ADR 0089 keeps the
  attribute's meaning on every target.
- **A test of reception through a TCP chardev on thumbv7m** — it would cover reception,
  with a larger harness that would have to be proved reliable on Windows; left for when a
  program needs it.

Defaults assumed while writing this ADR, for correction when accepting it:

- **The name `Interrupt`** — `Irq` is shorter, but `Interrupt::Uart0` reads as what it
  is, and `core::arch::Vector` is a whole word.
- **The milestone's reception left out on thumbv7m** — see above.
