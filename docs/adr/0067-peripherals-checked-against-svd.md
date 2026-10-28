# 0067. Peripherals checked against SVD

- Status: Accepted
- Date: 2026-10-28

## Context

The roadmap asks M3.4 to check the design of peripherals against the
SVD file of a real riscv32 chip, so that a generator, `svd2erebos`, can
write declarations from it, and leaves to that check several questions
of ADR 0060 and 0061: access modes of fields, SVD's write effects without
a value that has no effect (`modifiedWriteValues` `clear`, `set` and
`modify`), the reset value of a register without fields, and peripherals
sharing addresses (`alternatePeripheral`).

Three SVD files were surveyed on 2026-10-28, each parsed and counted
whole:

- **ESP32-C3**, `esp32c3.svd` from `github.com/espressif/svd`, version 18,
  under the Apache License 2.0;
- **GD32VF103**, `GD32VF103.svd` from `github.com/riscv-rust/gd32vf103-pac`;
- **CH32V003**, `CH32V003xx.svd` from `github.com/ch32-rs/ch32-rs`.

| SVD construct | ESP32-C3 | GD32VF103 | CH32V003 |
|---|---|---|---|
| peripherals, registers, fields | 37, 1150, 3479 | 46, 841, 4556 | 21, 274, 950 |
| register widths | 32 | 8, 16, 32 | 8, 16, 32 |
| `dim` with a `[%s]` name (an array) | 19 | 0 | 0 |
| `dim` with a `%s` name and indices (a list) | 37 | 0 | 0 |
| `dimIncrement` larger than the register | 23 | 0 | 0 |
| `dim` on fields | 28 | 0 | 0 |
| `<cluster>` | 0 | 0 | 0 |
| `derivedFrom` | 3, on peripherals | 15, on peripherals | 2, on peripherals |
| registers stating no `access` (nor their peripheral or device) | 1145 | 40 | 20 |
| registers whose fields are all `read-only` / all `write-only` | 281 / 71 | 48 / 12 | 27 / 34 |
| registers mixing field modes, and the `read-only` / `write-only` fields in them | 61: 54 / 57 | 33: 121 / 40 | 11: 38 / 6 |
| `alternateRegister` | 0 | 7 | 4 |
| peripherals sharing bytes | 1 (`RNG` and `APB_CTRL`) | 0 | 0 |
| `modifiedWriteValues`, `readAction`, `enumeratedValues` | 0 | 0 | 0 |
| registers without fields whose reset value is not 0 | 0 | 0 | 0 |
| names that, lowercased, are words of Erebos (`CONTINUE`, `IN`) | 3 registers, 2 fields | 0 | 0 |

A field without `access` takes its register's, as SVD has it. Counts of
fields count each field once, with that access. `alternateRegister`
marks, in both chips, a timer's channel control register declared twice
at one offset, once for input capture and once for output compare, with
different fields. Two access values are no SVD value: `write`, on 2
registers and 4 fields of the GD32VF103, and `read/clear`, on one field
of the CH32V003.

## Decision

The design of ADR 0058 to 0066 expresses these files. What each SVD
construct becomes:

- **`derivedFrom` on a peripheral** is a second peripheral of the same
  peripheral type (ADR 0058); no construct is added. A derived
  peripheral that redefines registers has a type of its own.
- **`dim` with a `[%s]` name** is a register array, and `dimIncrement`
  its `step` (ADR 0064). **`<cluster>`** is a cluster, with `dim` a
  cluster array (ADR 0064).
- **`dim` with a `%s` name and a list of indices** is one register per
  index, each with its own name: the list may skip indices or start past
  0 (the ESP32-C3's `CH%s_RX_CONF0` for channels 2 and 3), and its
  registers need not be one array.
- **`dim` on a field** is one field per index. Arrays of bit fields are
  not added: the surveyed fields repeat at most a few times, and a field
  reached by a run-time index would be a new kind of place.
- **`dim` on a peripheral** is one peripheral per index; arrays of
  peripherals are not added.
- **A register's `access`** is its mode (ADR 0058). **A register stating
  none**, nor its peripheral or device, takes its mode from its fields,
  as `svd2rust` does: `read` when every field is `read-only`, `write`
  when every field is `write-only`, `read write` otherwise, and with no
  fields, as `svd2rust` falls back to. SVD has no default: it makes
  `access` mandatory on a register when no parent states it, so such
  files leave out a required element. So a status register whose fields are all
  read-only is `read`, and a whole write to it is an error, which field
  modes alone would not catch (ADR 0065 checks field assignments).
- **Field `access`** is the field's mode (ADR 0065), written only where
  it differs from the register's.
- **`alternateRegister` and `alternateGroup`** are alternate groups
  (ADR 0061); a register naming only the register it overlaps goes in a
  group of its own.
- **`alternatePeripheral`**, and peripherals sharing bytes without it,
  are `over` (ADR 0066).
- **`enumeratedValues`** is an enum without payloads as the field's type
  (ADR 0059); none of the surveyed files has one.
- **A register's `resetValue`** gives its fields' defaults (ADR 0060).

**Not added**, since none of the surveyed files uses them:

- SVD's write effects without a value that has no effect (`clear`,
  `set`, `modify`: any write clears, sets or changes the field). ADR
  0060's six effects stay all there is.
- A reset value on a register without fields: every surveyed one is 0
  or absent, and nothing in the language would read it.

When a later chip needs one of these, an ADR adds it with that chip's
file as its case. How `svd2erebos` treats what has no declaration
(warning, comment or error), access values that are no SVD value, how
it names items and words of Erebos,
whether it is a tool of its own, and which SVD files the tests carry, is
M3.4b's.

## Consequences

- The roadmap's pending decisions on field modes, effects without a
  neutral value, reset values of registers without fields and
  peripherals sharing addresses are settled.
- Register arrays, clusters, field modes and `over` are added for what
  the surveyed chips need, or what SVD defines and the PLIC of QEMU
  `virt` has (clusters), not for every SVD construct.
- A generator writing a field with SVD's `clear`, `set` or `modify`
  write effect cannot declare it; until an ADR adds them, such a field
  is declared without its effect, and a field assignment to another
  field of its register triggers it.
- The survey describes three files at one date; a vendor's next version
  may use more of SVD.

## Alternatives considered

- **A register without `access` as `read write`**, `svd2rust`'s fallback
  for a register without fields — the
  ESP32-C3's 281 status registers would accept whole writes, which the
  hardware ignores.
- **Every SVD construct in the language** — features no surveyed chip
  uses, each with its rules and diagnostics, before a case shows what
  they should mean.
- **Register lists (`%s`) as arrays** — the indices may not start at 0
  or be contiguous, and the registers of one index often belong to one
  channel, which a cluster would describe better than an array per
  register.
- **Arrays of bit fields** (`tx_end: [bool; 2] at 0 step 1`) — a new kind
  of place for few repetitions, each of which a field of its own names.
- **Arrays of peripherals** — none of the surveyed chips has them; several
  instances of one type, with constants or a table of pointers, serve a
  loop.
