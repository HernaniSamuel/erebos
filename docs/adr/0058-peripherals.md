# 0058. Peripherals

- Status: Accepted; amended by 0060 (register types, fields reached through registers, read effects), 0061 (registers in alternate groups may share bytes), 0064 (register arrays and clusters in a peripheral type, their layout, overlaps and accesses; `&raw` of a cluster; arrays of a peripheral type as a cluster's type only), 0066 (a peripheral `over` another is not checked for overlap with it)
- Date: 2026-10-25
- Amends: 0013 (volatility is no longer only a property of the pointer
  type: accesses through `*mut T` and `*const T` are volatile when `T` is
  a peripheral type, and `volatile` on such a pointer is an error;
  volatile pointers stay for memory that is not a register), 0023
  (volatility flows from a peripheral too, not only from a pointer: a
  register reached from a peripheral is accessed volatile with no pointer
  on its path), 0048 (a register offset, a register type in error and a
  peripheral address are failed values too)

## Context

Programs drive devices through volatile pointers (ADR 0013, 0023),
usually to a struct describing the device's registers:

```
struct Uart { thr: u8 }
const UART: *mut volatile Uart = 0x1000_0000 as *mut volatile Uart;
(*UART).thr = b'h';
```

That gives each access its width and makes it volatile, but the language
knows nothing else about the device. A struct's layout places the
registers, so a gap is a padding field and a register at offset `0x18` is
a count of bytes before it. Nothing says that a register is read-only, and
writing one is accepted. Two registers at one offset, as the 16550 UART's
receive buffer (read) and transmit holding register (write) at offset 0,
cannot both be fields.

ADR 0002 makes peripherals a language construct, and the roadmap gives
M3.2 `peripheral` blocks: registers at offsets, with widths and access
modes, accessed as MMIO, a write to a read-only register a compile error.
Later steps add fields at bit positions, write-1-to-clear fields, reads
with side effects and reset values (M3.3), and register arrays, clusters
and `derivedFrom` (M3.4), so that a tool can generate the declarations
from a chip's SVD file. A chip has several instances of one design, as
`UART0` and `UART1`: SVD describes the second with `derivedFrom`, and
Rust's `svd2rust` gives both a single register block type, so that one
driver serves them.

## Decision

### Peripheral types and peripherals

```
peripheral Uart16550 {
    read rbr: u8 at 0x00,
    write thr: u8 at 0x00,
    read write ier: u8 at 0x01,
    read lsr: u8 at 0x05,
}

peripheral UART0: Uart16550 at 0x1000_0000;

UART0.thr = b'h';
```

Two items share the word `peripheral`:

- A **peripheral type**, `peripheral NAME { registers }`, describes a
  device's registers. It is a type, named like a struct, by a path
  (ADR 0049).
- A **peripheral**, `peripheral NAME: TYPE at ADDRESS;`, is an instance
  of a peripheral type at an address. It is a place, named like a static,
  whose type is that peripheral type. Two instances of one design are two
  peripherals of one type.

Their grammar:

```
peripheral_type = "peripheral" NAME "{" [ register { "," register } [ "," ] ] "}"
register        = [ "pub" ] access NAME ":" type "at" expr
access          = "read" | "write" | "read" "write"
peripheral      = "peripheral" NAME ":" type "at" expr ";"
```

- `at` gives a register's **offset** from the start of its peripheral, and
  a peripheral's **address**. Both are `usize` values computed at compile
  time, like array lengths and the values of the `memory` block
  (ADR 0029, 0057), in the scope of the module holding the item, so
  constants name them.
- A register's **access mode** is written before its name: `read` (it is
  read-only), `write` (write-only) or `read write`. There is no default:
  the mode is what the compiler checks, and SVD files state it.
- A register's **type** is `u8`, `u16` or `u32`: an unsigned integer whose
  width is the register's, at most the target's widest single access
  (ADR 0023). M3.3 extends what the type may be (fields, enums).
- The type of a peripheral must name a peripheral type
  (`resolve::not-a-peripheral-type` when it names another type; a name
  of nothing stays `resolve::unknown-type`).
- Registers are private to the module of their peripheral type unless
  written `pub`, as fields are (ADR 0050): a register is visible in that
  module and its descendants, and a `pub` one wherever its peripheral
  type is reachable, **and only there**. A `pub` peripheral of a private
  peripheral type is reachable, but its `pub` registers are not seen
  through it, as the `pub` fields of a `pub static` of a private struct
  are not; a module publishing a device makes its type `pub` too.
  Peripheral types and peripherals are private items unless `pub`, and
  are brought in with `use` like other items. A driver module can keep
  its registers private and offer functions.
- Peripheral types and peripherals share their module's one namespace
  with every other item (ADR 0049): a peripheral type and a struct, or a
  peripheral and its own type, named alike in one module are
  `resolve::duplicate-item`, and a built-in type's name is
  `resolve::duplicate-type`, as for any item.
- An empty peripheral type, with no register, is allowed; it occupies no
  address.
- `peripheral` starts an item only where an item starts and a name
  follows it; `read`, `write` and `at` are recognized only inside the
  items, and the register's name is the identifier just before `:`, so
  `read write: u8 at 0` is a read-only register named `write`. Elsewhere
  all four are ordinary names, as `read` and `write` often are for
  functions. Inside a function body, `peripheral` followed by a name is
  `parser::local-peripheral`, as other items there are `parser::local-*`;
  no expression starts with two names.
- Attributes do not apply to either item. Neither has a symbol or
  storage: nothing is emitted for a peripheral, whose address appears in
  the code that uses it.

### Layout

A peripheral type's **extent** is the end of its last register,
`max(offset + width)` over its registers, and 0 for an empty one,
computed exactly (a `u32` at `0xFFFF_FFFC` gives 2³², which is no
`usize`); its
**alignment** is the width of its widest register, and 1 for an empty
one. A peripheral covers `[address, address + extent)`.

- A register's offset must be a multiple of its width, and a peripheral's
  address a multiple of its type's alignment, so that every register
  access is aligned: ADR 0023 emits one instruction and assumes the
  address aligned (ADR 0019).
- Two registers of one type may not share a byte, except when one is
  `read` and the other `write`: then a read can only mean one of them and
  a write the other, as the 16550's receive buffer and transmit holding
  register at offset 0.
- Two peripherals may not share a byte.
- A peripheral must end at 2³² or below. Unlike a region (ADR 0057), it
  needs no address one past its end, and chips place peripherals at the
  top of the address space.

### Accesses

A register is reached from a peripheral, `UART0.thr`, or through a
pointer to a peripheral type, `(*u).thr`. Either is a place of the
register's type:

- **Each read is one volatile load of the register's width at its
  address, and each write one volatile store.** ADR 0023's guarantee
  holds for them as for any volatile access: never split, merged,
  widened, narrowed, duplicated, removed or reordered relative to other
  volatile accesses, those through volatile pointers included; not
  ordered against ordinary accesses; no fences.
- Reading a `write` register is an error, and so is writing a `read`
  register. A compound assignment, `UART0.ier |= 1`, reads and writes, so
  it needs `read write`; it is one load then one store, not atomic
  (ADR 0023).
- Writing through a `*const` pointer to a peripheral type is
  `ty::write-through-const`, as for any pointer. Writing a `read`
  register through one is both errors: each is a fault of its own, and
  fixing one leaves the other.
- Indexing a pointer to a peripheral type, `u[i]` (ADR 0021), is
  `ty::peripheral-index`: a peripheral type has no size, so no stride.
  Register arrays and clusters are M3.4's to design.

A peripheral type is **not a type a value can have**. Like `[T]`
(ADR 0031), it exists behind a pointer, and as the type of a peripheral;
the type of a variable, parameter or return value, a field, an array or
slice element, a payload or a type argument of `Option` or `Result`, the
type of a static, `extern static`, constant or `comptime` value, and the
operand of a layout query (`size_of`, `align_of`, `offset_of`) are errors.
A peripheral or `*u` read or written as a whole is an error too: a device
is reached one register at a time. `&raw mut *u` and `&raw const *u` are
neither, and give the pointer back.

`&raw const UART0` and `&raw mut UART0` give a `*const Uart16550` and a
`*mut Uart16550`, through which a driver serves any instance:

```
fn putc(u: *mut Uart16550, byte: u8) {
    while (*u).lsr & 0x20 == 0 {}
    (*u).thr = byte;
}

putc(&raw mut UART0, b'h');
```

- A peripheral is not `mut`, and `&raw mut` of one is allowed: what may
  be written is the registers' business.
- Accesses through a pointer to a peripheral type are volatile because
  they reach registers, so the pointer is written without `volatile`:
  `*mut volatile Uart16550` is an error, which keeps one spelling for one
  type.
- Pointer casts to and from pointers to peripheral types are allowed, as
  any pointer cast (ADR 0019): `0x1000_0000 as *mut Uart16550` makes one
  without a peripheral, and `&raw mut UART0 as *mut u8` reaches the
  device's memory without the registers' widths, access modes or
  volatility. The cast says so where it is written; the language does
  not stop explicit casts.
- `&raw` of a register is an error: the pointer would reach the register
  without its access mode, with nothing written at the use to show it.

At compile time, a register access is `eval::volatile-access`, as any
volatile access is. `&raw` of a peripheral in a constant or a static's
initializer is a pointer made from an integer, its address (ADR 0030).
An address or offset whose computation needs itself, as
`peripheral A: T at &raw const A as usize;`, is `ty::cycle`, as any
value computed at compile time (ADR 0028).

### Failed values

A register's offset, a register's type that is in error, and a
peripheral's address are **failed values** in the sense of ADR 0048 when
they are reported in error: an evaluation failure, a type error, or for
a type, `ty::register-type` or an erroneous type (`!`, `[T]`, a type
reaching a failed value). This amends ADR 0048, whose failed values are
array lengths and `#[align(N)]` arguments. Then, as ADR 0048 has it:

- A peripheral type with a failed offset or register type **reaches a
  failed value**, and so does a type containing a pointer to it;
  a peripheral whose type reaches one, or whose address failed, is
  erroneous. The failed value's own error is the only one reported for
  it, and the checks of "Layout" that need it are skipped: a register
  whose type failed takes part in no overlap check, and a peripheral of
  such a type in none either.
- A function, constant, static or `comptime` block that names an
  erroneous peripheral, or whose locals or expressions have a type
  reaching a failed value, is not built into MIR, as ADR 0048 says.
- A register type that is already in error is not also
  `ty::register-type`: `!` there is `ty::never-in-memory` (ADR 0034) and
  `[T]` is `ty::unsized` (ADR 0031), a register's type joining the
  positions where those are reported; a type reaching a failed value
  stays silent.
- An offset or an address computed from an erroneous peripheral, as
  `peripheral B: T at (&raw const A as usize) + 0x100;` with `A`'s
  address failed, fails in turn without an error of its own, as a length
  computed from a failed length does (ADR 0048), and the checks that
  need it are skipped.

### What is checked

When names are resolved:

| Code | Case |
|------|------|
| `parser::local-peripheral` | a peripheral type or a peripheral inside a function body |
| `resolve::duplicate-register` | two registers with one name in a peripheral type |
| `resolve::not-a-peripheral-type` | the type of a peripheral names no peripheral type |
| `resolve::misplaced-attribute` | an attribute on a peripheral type or a peripheral |

Names shared with other items are `resolve::duplicate-item`, in the one
namespace of a module (ADR 0049).

Syntax errors in either item are `parser::unexpected-token`, as for other
items. Errors in computing an offset or an address are those of any
compile-time evaluation.

Once the values are computed, with a check that needs a failed value
(already reported) skipped, as in ADR 0048, 0055 and 0057 and under
"Failed values" above:

| Code | Case |
|------|------|
| `ty::register-type` | a register whose type is not `u8`, `u16` or `u32` |
| `ty::misaligned-register` | a register's offset that is not a multiple of its width |
| `ty::register-overlap` | two registers sharing a byte, unless one is `read` and the other `write`; once per pair, at the later register, labeling the earlier |
| `ty::misaligned-peripheral` | a peripheral's address that is not a multiple of its type's alignment |
| `ty::peripheral-out-of-range` | a peripheral ending past 2³² |
| `ty::peripheral-overlap` | two peripherals sharing a byte; once per pair, at the later, labeling the earlier |

"Later" is as in ADR 0057: in the order of files, numbered depth first
(ADR 0049), then of positions in a file.

When code is type checked:

| Code | Case |
|------|------|
| `ty::peripheral-by-value` | a peripheral type where a value would have it, or a peripheral or `*u` read or written as a whole |
| `ty::volatile-peripheral` | `volatile` on a pointer to a peripheral type |
| `ty::peripheral-index` | indexing a pointer to a peripheral type |
| `ty::unknown-register` | a name that is no register of the peripheral type |
| `ty::private-register` | a register not visible where it is named |
| `ty::write-to-read-only` | a write, or a compound assignment, to a `read` register |
| `ty::read-of-write-only` | a read, or a compound assignment, of a `write` register |
| `ty::register-address` | `&raw` of a register |

### Volatile pointers

ADR 0013 made volatile pointers a stopgap until `peripheral` blocks, to be
revisited then. They stay: device registers are declared as peripherals,
and volatile pointers remain for memory shared with a device that is not
a register (a DMA buffer, a frame buffer, memory another core writes),
and for addresses known only at run time. Existing programs keep
compiling. What changes in ADR 0013 and 0023 is where volatility comes
from: until now only from the pointer type, so that `*mut T` was ordinary
memory; now also from a peripheral type, so that accesses through
`*mut T` to a peripheral type `T`, and to a register reached from a
peripheral with no pointer at all, are volatile, and `volatile` on a
pointer to a peripheral type is an error.

## Consequences

- A device's registers are declared once, at their datasheet offsets,
  with their widths and access modes, and the compiler rejects reads of
  write-only and writes of read-only registers, misaligned registers, and
  overlapping registers and devices.
- One driver serves every instance of a design through a pointer to its
  peripheral type; M3.4's clusters and `derivedFrom` have a type to build
  on.
- The generated code is that of volatile accesses through a constant
  pointer: no symbol, no storage, no indirection.
- Until M5, any code that can name a peripheral reaches it, as with a
  `static mut` (ADR 0030); ownership of peripherals is M5's (ADR 0021 and
  the M5 pending decisions).
- A peripheral overlapping a region of the `memory` block is not detected;
  writing its registers would write that memory.
- Registers that share an address in one mode, and devices that share
  addresses, cannot be declared. The 16550 itself has such registers:
  with the DLAB bit of LCR set, offsets 0 and 1 are the divisor latch
  DLL and DLM, both read-write, over RBR/THR and IER. SVD describes such
  cases with `alternateRegister` and `alternatePeripheral`. Until a step
  relaxes `ty::register-overlap` and `ty::peripheral-overlap`, a second
  view is reached through a pointer made with `as`, as
  `0x1000_0000 as *mut Uart16550Dlab`, which no overlap check sees. The
  roadmap leaves the question to M3.3, whose program configures the UART
  through its fields, and M3.4, which checks against SVD.
- Fields, enums in registers, write-1-to-clear fields, reads with side
  effects and reset values wait for M3.3; register arrays and clusters
  for M3.4.
- Two new contextual words inside the items (`read`, `write`, `at`) and
  one at item level (`peripheral`); no new global keyword.

## Alternatives considered

- **One block per instance, with its address** (`peripheral UART0 at
  0x1000_0000 { ... }`) — simpler, but a second instance repeats the
  description, and a driver for several instances needs generics (M6).
- **Structs behind volatile pointers, as today** — no access modes, no
  two registers at one offset, and offsets only through padding fields.
- **Access modes as `ro`, `wo`, `rw`** — shorter, as datasheets abbreviate
  them, but read less plainly.
- **The offset first, as datasheet tables list it** (`0x00 rbr: u8
  read`) — like no other declaration in the language.
- **Read-write by default, as in SVD** — an access mode left out would be
  the one the compiler checks least; writing it costs a word.
- **Every volatile scalar as a register type** (signed integers, `bool`,
  enums, pointers) — M3.3 decides what a register holds beyond its bits.
- **`*mut volatile T` for pointers to peripheral types**, required or
  allowed — required, the word adds nothing; allowed, `*mut T` and
  `*mut volatile T` would be two spellings of one meaning, yet two types,
  converted only with `as` (ADR 0008, 0013).
- **`&raw` of a register, giving a volatile pointer** — writing through
  `*mut volatile u8` would bypass the register's access mode.
- **Peripherals inside the `memory` block** — the block describes the
  board's memory, and its names are not items (ADR 0057), while code
  names peripherals.
- **Checking peripherals against the regions of the memory map** — the
  map is needed only to build an executable, and a region nothing is
  placed in may document memory used by address (ADR 0057); left until a
  real board shows the check is wanted.
- **A peripheral may end only below 2³², as a region** — a region needs
  the address one past its end for the linker's symbols; a peripheral
  does not.
- **Removing volatile pointers (ADR 0013)** — memory shared with a device
  that is not a register still needs volatile accesses.
- **`peripheral` as a global keyword** — it is a reasonable variable name,
  and nothing else starts an item with a name, so position tells them
  apart, as for `memory` (ADR 0057).
