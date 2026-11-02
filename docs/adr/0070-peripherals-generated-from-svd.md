# 0070. Peripherals generated from SVD

- Status: Accepted
- Date: 2026-11-02
- Amends: 0063 (the tables gain the module `svd`, which uses `syntax`
  and which `driver` uses, and the external crate `roxmltree`, used only
  in `svd`)

## Context

ADR 0058 to 0067 give peripheral types what the SVD files of three real
riscv32 chips describe, so that a generator can write them (ADR 0067).
The roadmap leaves M3.4b the generator's form: a tool of its own, or a
file the compiler reads at compile time, which bears on reproducible
builds (ADR 0033 makes a build a function of its sources;
`tests/reproducible.rs` checks it) and on the dependency that reads XML.
It also leaves which real chip's SVD the tests generate and compile,
given its size and license.

An SVD file is XML. The compiler has two dependencies, `inkwell` and
`miette`, each used by one module (ADR 0063). Of the surveyed files
(ADR 0067), the ESP32-C3's is under the Apache License 2.0, the
project's own; the GD32VF103's and the CH32V003's, taken from community
repositories, state no license. The ESP32-C3's, version 18, takes
1.37 MB.

Zig has both forms for C headers: `@cImport` reads a header at compile
time, and `zig translate-c` writes Zig source from it once. The source
written once can be read, reviewed and edited, and a new version of the
header shows as a diff of it.

## Decision

### A subcommand that writes source

```
erebos svd <input.svd> [-o <output.ers>]
```

`erebos svd` reads an SVD file and writes an Erebos source file: a
module the program declares, as `mod esp32c3;`, kept with the program's
sources and built as any other. **The compiler never reads SVD**: a
build depends on its `.ers` files alone, as before.

- `-o` names the output; left out, it is in the input's directory,
  named after the input's file name without its last extension, if it
  has one, made a module's name (ADR 0049) by ADR 0071's steps for a
  module (characters, lower case, a leading digit, keywords), with the
  extension `.ers`: `esp32-c3.svd` gives `esp32_c3.ers`, `32bit.svd`
  `_32bit.ers` and `match.svd` `match_.ers`. An existing
  file is replaced, but an output that is the input is an error, before
  anything is read. Nothing is written when an error is reported.
- **The output is a function of the input's bytes and its file name**:
  no date, no directory, `\n` line endings, and an order fixed by the
  file (ADR 0071, 0072). Generating twice gives the same bytes, and a new
  version of a vendor's file shows as a diff of the generated module.
- It starts with a comment naming the generator (`erebos svd`, without
  a version of the compiler), the input's file name, the device's `name`
  and `version`, and saying that edits are lost when it is generated
  again; then the device's `licenseText`, when it has one, as a comment,
  each line with its white space at the end removed.
- **Its format**: four spaces of indentation per module; items and
  entries in the syntax of ADR 0058 to 0066, every optional part in the
  grammar's order (`step`, read effect, `in`; for a field its mode, bits,
  read effect, write effect, default); offsets, addresses and steps in
  hexadecimal, `0x` then upper-case digits, in groups of four from the
  right separated by `_` when longer than four (`0x6000_0000`, `0x20`);
  bit positions in decimal; integer defaults in decimal below 10 and as
  offsets are from 10. The format is the generator's, not the language's:
  it may change from one version of the compiler to the next, and a kept
  module is then generated again (the tests' own included).
- What each SVD construct becomes, what the generator does with what
  has no declaration, and how it finds peripherals `over` others, is
  ADR 0072; how it names items, ADR 0071.

### Reading XML

The XML is read with **`roxmltree`** (0.21.2): a read-only tree, with no
dependencies of its own, under MIT or Apache-2.0, which gives the byte
range of every node, so that diagnostics point into the SVD file.

### Where it lives

The generator is the module `svd` (`src/svd/`). It is no layer of the
pipeline: no layer uses it, and it uses the shared vocabulary (`span`,
`diag`) and `syntax`, for the lexer's table of keywords alone
(ADR 0071). The command line reaches it through `driver`, as it reaches
the compiler (ADR 0063). The tables of `tests/architecture.rs` gain:

| Table | Entry |
|---|---|
| `LAYERS` | `svd`, which may use `syntax` |
| `LAYERS` | `driver` may also use `svd` |
| `EXTERNAL` | `roxmltree`, used only in `svd` |

### Diagnostics

The generator reports through `diag`, with spans into the SVD file,
which is a file of the source map like a module's. A **`Diagnostic`
gains a severity**, error or warning: an error stops the generation, a
warning is shown and the output is written. The compiler reports no
warnings yet; a warning is not an error in `Diagnostics::has_errors`,
while `len` and `is_empty` count both.
The errors:

| Code | Case |
|------|------|
| `svd::xml` | a file that is not well-formed XML, or not UTF-8 (without a span, then) |
| `svd::not-svd` | a root element other than `device`, or no `peripherals` |
| `svd::missing-element` | an element SVD requires and no parent supplies: a peripheral's `name` or `baseAddress`, a register's `name`, `addressOffset` or `size`, a field's bits |
| `svd::invalid-number` | a number SVD's syntax does not allow (decimal, `0x`, `#` for binary) |
| `svd::invalid-dim` | a `dim` without `dimIncrement`, a `dimIndex` that is no list or range, or of another length than `dim`, a `[%s]` or `%s` missing from a repeated name |
| `svd::unknown-reference` | a `derivedFrom` naming no element of its kind, or an `alternatePeripheral` naming no peripheral (ADR 0072 says where a name is looked up) |
| `svd::derived-cycle` | elements deriving from each other |

ADR 0071 and 0072 add the codes of names and of what has no declaration.

### The files the tests carry

- **The ESP32-C3's SVD**, version 18, from `github.com/espressif/svd`,
  whole, in `tests/svd/chips/`. Its `licenseText` holds its notice. A
  test generates it and **compiles** it: with a small program that
  reaches a few of its registers (an array element, a list element, a
  field, `RNG` over `APB_CTRL`, `continue_`), it is built into a riscv32
  executable at both optimization levels, so that every offset, address
  and default is computed, every peripheral type and peripheral checked
  (ADR 0058), and accesses of each shape built into MIR and code. The
  executable is not run: the official QEMU has no ESP32-C3 machine. The
  test compares a summary with a snapshot: how many peripheral types,
  peripherals, registers and fields were written, and every warning.
  The generated module is not kept: thousands of lines a snapshot would
  carry, which the summary and the build stand for.
- **A hand-written SVD of the devices of QEMU `virt`** the program on
  QEMU uses (the 16550 UART, the test finisher, the PLIC, the
  virtio-mmio transports), in `tests/e2e/svd_virt/`, beside the module
  generated from it and the program. A test generates the module again
  and requires the same bytes as the one kept, so that the generated code
  is reviewed as it changes, and the e2e harness builds it unchanged.
- The GD32VF103's and the CH32V003's are not carried, their license
  being unclear. What they have that the ESP32-C3 lacks (access values
  that are no SVD value, `alternateRegister`, widths of 8 and 16 bits)
  is in small hand-written files of `tests/svd/`, one per construct.

## Consequences

- Builds stay a function of the `.ers` files; the compiler does not
  depend on XML, and a chip's peripherals can be read, reviewed and
  edited as Erebos.
- A program carries the generated module, which must be generated again
  when the SVD changes, or when the generator's format changes; nothing
  checks that it was, outside the tests' own files.
- The crate gains `roxmltree`, and its binary the generator, which the
  compiler does not use.
- `diag` knows warnings before the compiler emits any; the warnings of
  the deferred ideas (roadmap) will use them.
- The repository grows by 1.37 MB for the ESP32-C3's file.

## Alternatives considered

- **SVD read at compile time** (a `peripheral` item naming a file) —
  the compiler would parse a vendor's XML format, its errors would point
  into it, and no Erebos text of the peripherals would exist to read or
  correct.
- **A separate binary in a Cargo workspace** — keeps `roxmltree` out of
  the compiler's crate, at the cost of a workspace (`cargo test` at the
  root then needs `default-members`) and of the keyword table, which the
  tool would copy or reach through the compiler's crate anyway.
- **`svd-parser`**, the model `svd2rust` uses — less to write, but it
  refuses values outside SVD, as the GD32VF103's `write`, which ADR 0072
  reads as absent, resolves `derivedFrom` its own way, and brings more
  dependencies.
- **An XML reader written by hand** — no dependency, but a few hundred
  lines of XML (comments, entities, CDATA) to test, for a format that is
  not the language (ADR 0004 is about Erebos's own syntax).
- **An excerpt of the ESP32-C3's file** — lighter, but it would not show
  that a whole chip generates and compiles, which the roadmap asks.
- **Downloading the file in the test** — a test that needs the network,
  or that passes without having run.
- **The module of QEMU `virt` generated by the e2e harness** — nothing
  generated would be reviewed, and the harness would gain a step.
