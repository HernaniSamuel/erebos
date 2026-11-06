# 0072. SVD constructs, and what has no declaration

- Status: Accepted; amended on 2026-11-02 (a derived set with values of its own); amended by 0092 (`interrupt` is read)
- Date: 2026-11-02

## Context

ADR 0067 says what each SVD construct the surveyed chips use becomes in
Erebos, and leaves out what none of them uses: the write effects without
a value that has no effect (`modifiedWriteValues` `clear`, `set`,
`modify`), a reset value on a register without fields, arrays of bit
fields and arrays of peripherals. It leaves to M3.4b what `erebos svd`
(ADR 0070) does with what has no declaration, with `access` values that
are no SVD value (the GD32VF103's `write`, on 2 registers and 4 fields,
and the CH32V003's `read/clear`, on one field), and how it finds
peripherals that share bytes without `alternatePeripheral`, which are
`over` (ADR 0066): the ESP32-C3's `RNG` lies inside `APB_CTRL`, and its
file does not say so.

SVD also has elements no surveyed file uses (`writeConstraint`,
`protection`, `writeOnce`) and elements that describe nothing of a
peripheral type (`interrupt`, `cpu`, `addressBlock`). The ESP32-C3's
file states `addressUnitBits` 32 while its offsets count bytes, and it
has 62 interrupts, which are M4's.

## Decision

`erebos svd` writes ADR 0067's mapping, in the forms below. **What has
no declaration is declared as close as the language allows, with a
comment `// svd: ...` above the item saying what was left out, and the
warning `svd::not-declared` at the SVD element.** The whole file is
generated, and what it lost is shown both in the code and when it is
generated.

### Properties

`size`, `access`, `resetValue` and `resetMask` are inherited from the
device, the peripheral and the clusters to their registers, as SVD has
it; a register states its own over theirs. A register without a
`resetValue` at any level has no reset value, and its fields no
default, as `svd2rust` gives none; nothing is lost, so nothing is
warned. A `resetMask` stated nowhere covers every bit of the register,
and the bits of either above the register's size are not read. `size` must be 8, 16 or 32
(ADR 0058): a register of another size is left out, with the comment and
`svd::not-declared`. `addressUnitBits` and `width` are not read: every
offset counts bytes, as in every surveyed file.

### `derivedFrom`

- **Where the name is looked up.** A name without `.` names an element
  of the same kind with the same parent: a sibling peripheral, a
  register or cluster of the same peripheral or cluster, a field of the
  same register; a set of enumerated values is named by its own `name`,
  among the sets of every field of the same register. A name with `.` is
  a path from the device: `PERIPHERAL.REGISTER.FIELD`, through clusters
  by their names, and `PERIPHERAL.REGISTER.FIELD.SET` for a set. A name
  that finds nothing is `svd::unknown-reference`, and elements deriving
  from each other `svd::derived-cycle` (ADR 0070).
- **What is copied.** The derived element is the base element's
  children and the elements the base states itself (its `size`,
  `access`, `resetValue`, `resetMask`, `dim`, effects, bits), then the
  derived element's own over them. What the base inherits from its
  parents is not copied: the derived element inherits from its own
  parents, where it stands.
- **Merging children.** A child of the derived element with the SVD
  `name` of one of the base's replaces it, where the base had it; the
  others are added after the base's, in their order.
- **A derived peripheral shares its base's type** when it states no
  `registers` and none of `size`, `access`, `resetValue` and
  `resetMask` of its own: then the type the base's registers make is
  its type too. Otherwise it has a type of its own, built from what was
  copied and merged, and named after it (ADR 0071).

### Peripherals

- A peripheral is `peripheral NAME: TYPE at baseAddress`.
- **`derivedFrom`** is as above.
- **`dim`** is one peripheral per index, at `baseAddress + i *
  dimIncrement`, all of one type (ADR 0067).
- **`alternatePeripheral`** is `over` that peripheral (ADR 0066), looked
  up among the peripherals as a `derivedFrom` without `.` is.
- **Peripherals sharing bytes without it are found by the generator**,
  which computes each peripheral type's extent as the compiler does
  (ADR 0058, 0064: the end of the last byte its entries occupy,
  saturating above 2³²), and examines the peripherals in the order of
  the file. Let `S` be the earlier peripherals a peripheral `P` shares a
  byte with. When `S` is empty, `P` has no `over`. Otherwise, the
  candidates are the peripherals of the file, other than `P`, such that
  every peripheral of `S` is the candidate or is written `over` it; when
  there is one, `P` is written `over` **the first candidate in the
  order of the file**, with a comment saying that the SVD does not state
  it. A candidate need not share a byte with `P` (ADR 0066 accepts
  that), as when `P` covers `B` and `C`, both `over A`; and when `S` is
  `B` alone, `B` written `over A`, both are candidates, and the first in
  the file is taken.
  When no such `Q` exists, as a peripheral covering two that do not
  overlap each other, one `over` cannot say it: **`P` is written without
  `over`**, the warning `svd::overlap` names the peripherals of `S`, and
  the generated module does not compile: the compiler reports
  `ty::peripheral-overlap` there, as it would for the same peripherals
  written by hand.
- `addressBlock`, `groupName`, `headerStructName`, `prependToName`,
  `appendToName` and `interrupt` are not read: interrupts are M4's, and
  the others describe C headers or no register.

### Registers

- A register is `access NAME: TYPE at addressOffset`. With no field, its
  type is the unsigned integer of its size; with fields, the bit-field
  struct of ADR 0071.
- **Its mode** is its `access`, inherited as above; when no element
  states one, it is taken from its fields (ADR 0067): `read` when every
  field is `read-only`, `write` when every field is `write-only`, `read
  write` otherwise and with no fields. `writeOnce` is `write` and
  `read-writeOnce` is `read write`, with "once" not declared: comment
  and `svd::not-declared`.
- **`dim` with a `[%s]` name** is a register array, `[T; dim]`, with
  `step dimIncrement` written when it is not the register's width. A
  `dimIndex` other than `0` to `dim - 1` makes it a list instead.
- **`dim` with a `%s` name** is one register per index, the indices from
  `dimIndex` (a range `2-3`, `A-C`, or a list `A,B,C`) or `0` to `dim -
  1`, at `addressOffset + i * dimIncrement`.
- **`alternateGroup`** is `in` the group's name in lower case;
  **`alternateRegister`** is `in` the register's own name, a group of
  its own (ADR 0067). A register with both is in the `alternateGroup`.
  The register `alternateRegister` names is not looked up: the group
  does not need it.
- **`readAction`** is the register's read effect: `clear` is `read
  clears`, `set` is `read sets`, `modify` and `modifyExternal` are `read
  modifies` (ADR 0060).
- **`derivedFrom`** is as above.
- **A reset value on a register without fields** that is not 0 is not
  declared (ADR 0067): comment and `svd::not-declared`. So is a
  `modifiedWriteValues` other than `modify` (SVD's default) on a
  register without fields, which has no field to carry it.
- `writeConstraint` and `protection` are not declared: comment and
  `svd::not-declared`. `dataType` is not read.

### Clusters

A `<cluster>` is a cluster (ADR 0064) of a peripheral type of its own,
in the module of ADR 0071, its registers' offsets counted from the
cluster. **`dim` with a `[%s]` name** is a cluster array, its `step`
always written (ADR 0064 requires it); with a `%s` name, one cluster per
index. `alternateCluster` is `in` the cluster's own name; `derivedFrom`
is as above.

### Fields

- A field is `NAME: TYPE at BITS`, its bits from `bitOffset` and
  `bitWidth`, `lsb` and `msb`, or `bitRange`, written `at 3` for one bit
  and `at 0..=7` for more. A field stating more than one form takes the
  first of those three that it states.
- **`derivedFrom`** is as above.
- **Its type** is the enum of its enumerated values (below), when it has
  them; otherwise `bool` for one bit, and for more the smallest of `u8`,
  `u16`, `u32` and `u64` that holds them (ADR 0059).
- **Its mode** is written where it differs from the register's
  (ADR 0065, 0067): a `read-only` field of a `read write` or `write`
  register is `read`, a `write-only` field of a `read write` or `read`
  register `write`; a `read-write` field takes no word, and its
  register's mode applies. `writeOnce` is `write-only` and
  `read-writeOnce` is `read-write`, with "once" not declared: comment
  and `svd::not-declared`.
- **`dim`** is one field per index (ADR 0067), at `bitOffset + i *
  dimIncrement`, named as ADR 0071 says.
- **`modifiedWriteValues`**, its own or its register's, is the field's
  write effect (ADR 0060): `oneToClear` is `write 1 clears`, and so for
  the six. `clear` and `set` are not declared (ADR 0067): the field has
  no effect, with the comment and `svd::not-declared`. `modify` is
  SVD's default, the ordinary write that stores the value written: an
  explicit `modify` is read as an absent one, with no effect, no comment
  and no warning, here and on a register without fields.
- **`readAction`** on a field is its read effect, as for a register.
- **Its default** is the register's `resetValue` at its bits (ADR 0060,
  0067): `true` or `false`, an integer, or the enum's variant with that
  value. A field has none when it has a write effect (ADR 0060), when
  `resetMask` leaves one of its bits unknown, or when its enum has no
  variant with that value, since no integer converts to an enum
  (ADR 0035); the last one is not declared: comment and
  `svd::not-declared`.
- `writeConstraint` is not declared, as for a register.

### Enumerated values

- A field's `enumeratedValues` are an enum on the field's integer type,
  a variant per `enumeratedValue` with a `value`, in the file's order. An
  `isDefault` value, which names the values no variant has, is no
  variant: such a value is defined (ADR 0036).
- **A value with don't-care bits** (`#1x0`) is several values under one
  name: it is left out, with the comment and `svd::not-declared`. **A
  value another variant already has** is left out the same way, since
  two variants with one value are `ty::duplicate-discriminant`
  (ADR 0035). An enum left with no variant is not written, and the field
  is an integer.
- **Two sets on one field**, one for reads and one for writes (`usage`),
  or two of any usage, have no declaration, a field having one type: the
  field is an integer, with the comment and `svd::not-declared`.
- An `enumeratedValue` without a `name`, which SVD allows, is no
  variant; its value is still defined (ADR 0036), and nothing is warned.
  One whose name is left empty by ADR 0071's step 2 is no variant
  either, with the comment and `svd::not-declared`.
- A set with `derivedFrom` is the enum written for the set it names,
  when that set was written as an enum on the same integer type as the
  derived field's. Otherwise the named set's values are copied, and the
  derived field's set is judged by the rules above as any other: an enum
  of its own, or an integer field. A base enum whose variants do not fit
  the derived field's bits is the compiler's `ty::bit-field-width` (see
  Consequences).

  **Note, 2026-11-02:** "the enum written for the set it names" holds
  only when the derived set's values, merged with the base's as
  `derivedFrom` copies and merges them, equal the base set's values: the
  same names and values, in the same order. A derived set that adds,
  replaces or reorders a value is a set of its own, an enum of its own or
  an integer field, with nothing left out and no new warning.

### Values that are no SVD value

**An `access`, `readAction`, `modifiedWriteValues` or `usage` whose
value SVD does not define is read as absent**, with the warning
`svd::invalid-value` at it and a comment at the item: the rules above
then apply as if the element were not there. So the GD32VF103's `write`
on a field leaves the field the register's mode, and on a register,
when no parent states an access, gives it the mode its fields imply.
The generator does not guess what the vendor meant.

### Comments

An element's `description` becomes a `//` comment above its item, its
white space collapsed and the text wrapped at 79 columns. The `// svd:`
comments of what has no declaration come after it.

### What is checked

| Code | Case |
|------|------|
| `svd::not-declared` (warning) | an SVD construct the generated item does not declare (the cases above) |
| `svd::invalid-value` (warning) | a value SVD does not define, read as absent |
| `svd::overlap` (warning) | a peripheral sharing bytes with earlier ones that one `over` cannot name |

## Consequences

- The ESP32-C3's file generates whole, `RNG` `over` `APB_CTRL`; the
  GD32VF103's and the CH32V003's generate with a warning per value that
  is no SVD value.
- A field of a `clear`, `set` or `modify` write effect is declared
  without it, as ADR 0067 foresaw; its comment and the warning say that a
  field assignment to another field of its register triggers it.
- An `over` the generator computes hides an overlap that a mistake in
  the SVD made: the file is taken as the truth of the chip, and the
  comment shows that the SVD did not state it.
- Which of two overlapping peripherals is written `over` the other
  depends on the order of the file, as renames do (ADR 0071): a vendor
  reordering them flips it. In the ESP32-C3's file, `APB_CTRL` comes
  first, so `RNG` is `over APB_CTRL`.
- **The generator does not repeat the compiler's checks**, but for the
  extents `over` needs. A file the compiler would reject, as a field past
  its register's bits, two fields sharing a bit, an enumerated value
  wider than its field, a misaligned offset, a step smaller than its
  register, two registers of one mode at one offset, or an address at or
  past 2³², generates without an error, and its errors are reported when
  the generated module is compiled, at the generated code; the comments
  carrying each element's description help find the element. The
  extents are computed twice, by the generator and the compiler; the
  tests compile every module they generate, which shows a difference.
- Interrupts are not generated; M4 decides how a program names them.

## Alternatives considered

- **An error for what has no declaration** — a chip with one `modify`
  field would generate nothing until an ADR adds it.
- **The comment without the warning** — what was lost would be found
  only by reading the generated file.
- **Access values that are no SVD value read as aliases** (`write` as
  `write-only`, `read/clear` as `read-only` with `read clears`) — a guess
  at the vendor's meaning, and each chip brings its own spellings.
- **An error for them** — the GD32VF103 would not generate until its
  file is corrected.
- **Peripherals `over` others named on the command line**
  (`--over RNG=APB_CTRL`) — a recipe per chip, without which the
  ESP32-C3 does not compile.
- **A peripheral's extent from its `addressBlock`** — it may be larger
  than its registers, and the compiler checks the registers' extent; two
  blocks that overlap where no register is would need an `over` the
  compiler does not.
- **Values with don't-care bits expanded into one variant per value** —
  variants named by a number the SVD does not give.
