# 0071. Names generated from SVD

- Status: Accepted; amended by 0092 (the generated file ends with the list)
- Date: 2026-11-02

## Context

`erebos svd` (ADR 0070) writes items named after the elements of an SVD
file. SVD names are C identifiers, usually in capitals: `UART0`,
`CONF0`, `RXFIFO_RST`. A repeated element holds `%s`, replaced by each
index (`CH%s_CONF0`), or `[%s]` for an array (`PRIORITY[%s]`). Erebos
code names peripherals in capitals (`UART0`), types in Pascal case
(`Uart16550`) and registers and fields in lower case (`rbr`, `dlab`);
the compiler does not require it.

Two problems follow from converting names. In the ESP32-C3's file
(ADR 0067), the registers `CONTINUE` of `AES` and `SHA`, and a field of
each, are `continue` in lower case, a keyword; `IN` of `GPIO` is `in`,
which is a word only by position (ADR 0061), and so a name wherever the
generator writes it. And one module holds a peripheral type and the
types of its registers, whose names may meet: the peripheral `RTC_CNTL`
has a register `RTC_CNTL`, and both would be `RtcCntl`.

Registers of different peripherals share names (`CTRL`, `INT_RAW`), so
their bit-field structs cannot all be in one module.

## Decision

### Where items go

- The generated file holds, in the order of the SVD, **a module per
  peripheral type**, then **the peripherals**, all `pub`.
- A peripheral type's module holds the type, then, in the order of its
  registers, the bit-field struct of each register that has fields and
  the enums of its fields, then **a module per cluster type**, which
  holds the cluster's type and its registers' items in the same way.
- A path from one generated module to another is relative, through
  `super`, so that the file works wherever the program declares it.

```
pub mod uart0 {
    pub peripheral Uart0 {
        pub read write conf0: Conf0 at 0x20,
        // ...
    }

    pub struct Conf0: u32 {
        pub parity: bool at 0 = false,
        // ...
    }
}

pub peripheral UART0: uart0::Uart0 at 0x6000_0000;
pub peripheral UART1: uart0::Uart0 at 0x6001_0000;
```

### How names are converted

An item is named from the `name` of one SVD element, in these steps:

1. **The index.** An item that is one element of a repeated SVD element
   (a peripheral, register, cluster or field with `dim`, written one per
   index; ADR 0072) has `%s`, or `[%s]`, replaced by its index as
   `dimIndex` writes it: `CH%s_CONF0` with index 2 is `CH2_CONF0`, and
   `FOO[%s]` made a list by a `dimIndex` of `2,3` (ADR 0072) is `FOO2`.
   Any other item, which stands for the whole element (an array, or a
   type, module or enum shared by every index), has `[%s]` and `%s`
   removed: `CH%s_CONF0` is `CH_CONF0`, `PRIORITY[%s]` is `PRIORITY`.
2. **The characters.** Each character that cannot be in an identifier
   of the lexer (an ASCII letter, digit or `_`) is replaced by `_`; then
   each run of `_` is made one, and `_` at either end removed.
3. **The case**, by the item's kind (table below). **Pascal case**
   splits a name at `_` and writes each part with its first character
   upper (a digit stays a digit) and the rest lower: `RND_DATA` is `RndData`, `UART0`
   `Uart0`. **Lower case** lowers every letter and keeps `_`. A
   peripheral keeps its case.
4. **A leading digit** gets a `_` before it: the enumerated value
   `1MHZ` is `1mhz` in Pascal case (its first character is no letter),
   and the variant `_1mhz`.
5. **Names Erebos already gives a meaning** get `_` after them (below).
6. **Names that meet** get `_` after them (below).

Steps 2 and 4 change what the SVD wrote: when either does, the
warning `svd::renamed` is reported at the element. A name that is empty
after step 2 is the error `svd::invalid-name`, but for an enumerated
value, which is then no variant, as one without a `name` (ADR 0072).

| Item | Case | Example |
|---|---|---|
| peripheral | as in the SVD | `UART0`, `TIMG0` |
| peripheral type | Pascal case of the peripheral it is declared for | `Uart0` |
| its module | lower case of that peripheral | `uart0` |
| register, cluster, field | lower case | `rxfifo_rst`, `ch2_conf0` |
| register array, cluster array | lower case | `priority` |
| cluster type, and its module | Pascal case, lower case | `Ch`, `ch` |
| a register's bit-field struct | Pascal case of the register | `Conf0`, `ChConf0` |
| a field's enum | the struct's name, then Pascal case of the field | `LcrWls` |
| an enum's variant | Pascal case of the enumerated value | `Eight` |
| an alternate group (ADR 0061) | lower case | `in dlab` |

A peripheral type is shared by the peripherals derived from the one it
is named for (ADR 0072): `UART1` is a `uart0::Uart0`. One SVD element
repeated by a list or an array is one register type, and one field
element repeated by `dim` one enum: the registers `ch0_conf0` to
`ch5_conf0` share `ChConf0`, and the fields `tx0` and `tx1` of a field
`TX%s` of `LCR` share `LcrTx`. The letters of an index are cased with
the rest of the name: `CH%s_CONF0` with index `A` is `cha_conf0`.

### Names Erebos already gives a meaning

Any converted name, a peripheral's and a group's included, that is a
**keyword of the lexer** (`continue`,
`match`, `loop`, ...), `core`, or the name of a **built-in type**
(`bool`, `u8` to `u64`, `i8` to `i64`, `usize`, `isize`, `Option`,
`Result`) gets a `_` after it: `continue_`. The keywords are the
lexer's table itself (`syntax`), so a keyword added later is escaped
without a change to the generator. Words recognized by position only
(`read`, `write`, `at`, `step`, `in`, `over`, `clears`, `sets`,
`toggles`, `modifies`) are names wherever the generator writes a name
(ADR 0058, 0060, 0061, 0064, 0066), and stay as they are: `GPIO`'s
`IN` is the register `in`.

### Two items with one name

Two items of one module, two entries of one peripheral type, two fields
of one bit-field struct, or two variants of one enum may end up with
one name after conversion. **The later one, in the order the file is
written, gets `_` after it**, as many times as it takes, and the
warning `svd::renamed` is reported at its SVD element, labeling the
other. So the struct of `RTC_CNTL`'s register `RTC_CNTL`, written after
the peripheral type `RtcCntl`, is `RtcCntl_`.

Groups are labels of their peripheral type, in no namespace
(ADR 0061): two groups never meet anything.

| Code | Case |
|------|------|
| `svd::renamed` (warning) | an item renamed because another of its scope has its name, or because its SVD name has characters no identifier has or starts with a digit |
| `svd::invalid-name` | a name with nothing left after its characters are replaced (step 2) |

## Consequences

- Generated code reads like Erebos written by hand: `UART0.conf0.parity`,
  `fn reset(u: *mut uart0::Uart0)`.
- A name is found in the datasheet by its letters, not its case; a
  search for `RXFIFO_RST` needs to ignore case.
- Every register type of a peripheral is in its module, so `uart0::Conf0`
  and `i2c0::Ctrl` never meet; a driver brings in what it needs with
  `use`.
- A name renamed with `_` depends on the order of the file: a register
  added before another in a vendor's next version may move the `_` to
  the other. The warning shows each.

## Alternatives considered

- **SVD names as they are** (`CONF0`, `RXFIFO_RST`) — found in the
  datasheet as written and never keywords, since every keyword is in
  lower case; but the generated code would not look like the rest, and
  the peripheral type would still need a suffix to differ from its
  peripheral (`UART0_Regs`).
- **A raw identifier** (`r#continue`) — a feature of the language for a
  case of the generator.
- **An error for a name that meets another** — the ESP32-C3 would not
  generate, for one register named as its peripheral.
- **An error for a name with characters no identifier has**, or one
  starting with a digit, as a variant `1MHZ` — the whole file would fail
  for one name, where ADR 0072 declares as close as it can and warns.
- **All register types of the file in one module, prefixed by their
  peripheral** (`Uart0Conf0`) — long names, which a module per
  peripheral type gives without the prefix.
- **A fixed name for every peripheral type**, as `svd2rust`'s
  `RegisterBlock` — two of them brought in with `use` collide, and a
  pointer type does not say which device it is.
