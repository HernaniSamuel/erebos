# 0022. Data layout and alignment control

- Status: Accepted; amended by 0030 (the alignment of `.data` and `.bss`), 0031 (unsized types and the layout of slices)
- Date: 2026-10-14

## Context

On bare metal, layout is part of a type's meaning: a struct may mirror a
register block, a packet header or a hardware descriptor, and a buffer may
need a specific alignment for DMA, caches or page tables. Hardware also
restricts accesses: a misaligned load or store traps on many cores (RISC-V
may trap or emulate it slowly; Cortex-M0 faults), and a device register must
be accessed in one bus transaction of its width (ADR 0023).

Rust reorders struct fields by default and needs `#[repr(C)]` for a
predictable layout. C fixes the order, and GCC offers
`__attribute__((packed, aligned(N)))`, `sizeof`, `_Alignof` and `offsetof`.

## Decision

### Default layout: C rules, no reordering

Every type has a **size** and an **alignment** (a power of two); every
object of the type is placed at an address that is a multiple of its
alignment.

| Type | Size | Alignment |
|------|------|-----------|
| integers, `bool`, pointers | as in the target's C ABI | as in the target's C ABI |
| `[T; N]` | `N * size(T)` | `align(T)` |
| `()` | 0 | 1 |

On riscv32 (ADR 0010), `u8`/`bool` are 1/1, `u16` 2/2, `u32`/`usize`/pointers
4/4 and `u64` 8/8.

A **struct** lays out its fields in declaration order. Each field starts at
the next offset that is a multiple of its alignment (padding is inserted).
The struct's alignment is the largest of its fields' (1 if it has none), and
its size is rounded up to a multiple of its alignment:

```
struct S { a: u8, b: u32, c: u16 }
// a at 0, 3 bytes of padding, b at 4, c at 8, 2 bytes of padding
// size 12, alignment 4
```

Fields are **never** reordered: offsets are predictable from the source.
Programs that want less padding order their fields themselves. Enum layout
is decided with enums (M2.4).

**Padding bytes have unspecified values.** Copying a struct may carry
whatever was in memory (for example, old stack contents) into its padding.
Data that leaves the CPU (DMA descriptors, packets, anything compared byte
by byte) should declare its gaps as explicit fields and set them.

### `bool` in memory

A `bool` occupies one byte. Storing writes 0 or 1. **Loading treats any
nonzero byte as `true`**, so every byte value is a valid `bool` in memory
and reading one is never undefined, wherever the byte came from (a device
register, a pointer cast, memory written as `u8`). The cost is a compare on
each load from memory.

This replaces M1's code generation, which stored `bool` as LLVM `i1`:
reading a byte other than 0 or 1 through a `*const bool` was undefined
behavior there.

### Compatibility with C

For the types C also has (integers, pointers, structs with at least one
field and arrays with at least one element), the layout is the one the
target's C ABI gives the equivalent C type, so such a struct matches a C
header. `#[packed]` and `#[align(N)]` match GCC and Clang's
`__attribute__((packed))` and `__attribute__((aligned(N)))` on structs,
which are compiler extensions rather than standard C. Zero-sized types have
no portable C equivalent (below), and enums are not covered until M2.4.

### Zero-sized types

`()`, structs without fields (or whose fields are all zero-sized) and
arrays of length 0 (or of a zero-sized element) have **size 0**. Their
alignment follows the rules above: 1 for `()` and empty structs (or `N`
with `#[align(N)]`), `align(T)` for `[T; 0]`.

- **No memory:** reading or writing a zero-sized value performs no memory
  access, and a zero-sized local occupies no stack space.
- **Addresses without identity:** `&raw const` of a zero-sized place yields
  a pointer aligned for the type, but its address is otherwise unspecified.
  Two zero-sized objects, or a zero-sized object and the next field or
  variable, may share an address, so comparing such pointers says nothing
  about which object they name.
- **Inside structs:** a zero-sized field still raises the struct's
  alignment to its own, and is placed at the next offset aligned for it,
  which may coincide with the following field's offset.
- **Interoperability:** C has no zero-sized types (an empty struct is
  invalid in standard C, size 0 as a GCC extension, size 1 in C++). M2.2
  decides how they cross function calls; they are not part of any
  C-compatible layout.

### Querying layout

```
size_of(T)          // usize, in bytes
align_of(T)         // usize, in bytes
offset_of(T, field) // usize, in bytes
```

They are compile-time constants of type `usize` (usable in constant
expressions once `const` exists, M2.3). `size_of`, `align_of` and
`offset_of` become keywords; their argument is a type, not an expression.
This makes them unavailable as names; no existing program uses them.

### Attributes

Layout and placement details use attributes, written before an item:

```
#[packed]
struct Header { kind: u8, len: u32 }   // size 5, alignment 1

#[align(64)]
struct DmaDescriptor { addr: u32, len: u32 }   // size 64, alignment 64
```

The rule of thumb: a **keyword** changes what something is (`entry fn`,
`panic fn`, `volatile`); an **attribute** changes how it is laid out or
placed in memory. An unknown attribute, or a known one in the wrong place,
is an error.

- **`#[packed]`** gives the struct alignment 1 and no padding. Its fields,
  and everything inside them, may be misaligned. A place is **possibly
  misaligned** when the path that reaches it goes through a field of a
  packed struct at any depth: `h.len`, `h.inner.x` and `h.arr[i]` all are.
  - Reading and writing a possibly misaligned place is compiled as a
    misaligned access (on riscv32, LLVM splits it into byte accesses).
  - Taking the address of a possibly misaligned place whose type has
    alignment above 1 (`&raw const h.len`, `&raw const h.inner`) is an error
    (`ty::packed-field-address`): a pointer such as `*const u32` promises
    alignment, and dereferencing a misaligned one may trap. This can be
    relaxed later, together with explicit unaligned accesses.
  - Through a volatile pointer, a possibly misaligned scalar with alignment
    above 1 cannot be accessed at all (`ty::volatile-packed-field`,
    ADR 0023): the split would turn one register access into several.
    Scalars of alignment 1 (`u8`, `i8`, `bool`) stay accessible. Register
    blocks should use the default layout, with explicit padding fields for
    gaps.
  - A type with `#[align(N)]`, or containing one, cannot be a field of a
    packed struct (`ty::aligned-in-packed`): the alignment it asks for
    could not be kept.
- **`#[align(N)]`** raises the struct's alignment to `N`, a power of two not
  below its natural alignment, and rounds its size up to a multiple of `N`.
  Lowering alignment is what `#[packed]` is for.
- `#[packed]` and `#[align(N)]` cannot be combined.
- `#[align(N)]` will also apply to statics (M2.3), for buffers that need it
  without a wrapper struct.
- There is no per-field alignment: use explicit padding fields, or a field
  whose type is an aligned struct.

### Alignment of every object

The alignment of a type is a promise about every object of that type the
program creates, **except inside a `#[packed]` struct**, where objects are
possibly misaligned and the compiler treats them as such (above). Each part
of the toolchain keeps the promise:

- **Fields and elements** are aligned by the layout rules above.
- **Locals** get stack slots with the type's alignment. `_start` (ADR 0012)
  keeps the stack pointer 16-byte aligned, as the riscv32 C ABI requires.
  For a larger alignment, the function realigns its own frame; LLVM does
  this when the stack slot asks for it.
- **Statics and byte strings** (M2.3) are emitted with their alignment, which
  becomes the alignment of their section in the object file. The generated
  linker script keeps input section alignment and aligns the start and end
  of `.data` and `.bss` to the largest alignment they contain, so the copy
  and zeroing loops in `_start` stay valid.
- **Memory regions** declared in the language (M3) will be checked against
  the alignment of what is placed in them.

The guarantee does **not** extend to pointers made from integers (`as`,
ADR 0019), from unchecked pointer indexing (ADR 0021) or from external
code: the compiler assumes they are aligned and cannot check it.

**Known limitation until M4:** aligning a DMA buffer or descriptor is only
half of using it. Handing it to a device needs the writes to RAM to happen
before the volatile write that starts the device, and the compiler does not
order ordinary accesses against volatile ones (ADR 0023).

### Conformance tests

M2.1 ships with tests on riscv32 for:

- **Size, alignment and offsets:** an e2e program that checks `size_of`,
  `align_of` and `offset_of` for scalars, padded structs, nested structs,
  arrays, zero-sized types, `#[packed]` and `#[align(N)]`, against values
  computed by hand and cross-checked with the same declarations compiled by
  clang for riscv32.
- **Object alignment:** an e2e program that checks the address of an
  over-aligned local (`#[align(64)]`) is a multiple of 64, at both
  optimization levels.
- **Packed code generation:** LLVM IR snapshots showing accesses with
  alignment 1 for packed fields and for nested places under them, and ui
  snapshots for `ty::packed-field-address`, `ty::volatile-packed-field`
  (including a nested path) and `ty::aligned-in-packed`.
- **`bool` loads:** an e2e program reading bytes 0, 1 and 2 through a
  `*const bool`, at both optimization levels.
- **Volatile code generation:** see ADR 0023.

## Consequences

- Layout is predictable from the source and matches the target's C ABI for
  the types C has; there is no `#[repr(C)]` to forget.
- Programs pay for padding unless they order fields well; the compiler does
  not optimize layout behind their back.
- `#[packed]` produces no unexpected misaligned accesses: possibly
  misaligned places are only reached through accesses the compiler knows
  are misaligned, and never through volatile ones or aligned pointers.
- Padding is not zeroed; data copied out of the CPU should use explicit
  padding fields.
- Every byte is a valid `bool` in memory, at the cost of a compare per load.
- The attribute syntax is ready for later placement attributes (sections,
  interrupt vectors) without new keywords.
- Over-aligned locals cost a frame realignment in the functions that have
  them.

## Alternatives considered

- **Rust's default (reordering) plus `#[repr(C)]`** — saves space, but
  layout stops matching the source, and on bare metal predictability is
  worth more.
- **Keyword modifiers (`packed struct`, `align(64) struct`)** — consistent
  with `entry fn`, but every layout or placement knob would need a new
  keyword.
- **`size_of::<T>()` as in Rust** — needs generic-call syntax, and
  `offset_of` cannot be a function anyway.
- **Allowing pointers to packed fields (C, GCC)** — a frequent source of
  traps on cores without misaligned access.
- **Zero-sized types of size 1, as in C++** — gives each object a distinct
  address, but wastes memory and breaks `[T; 0]` and empty markers that
  should cost nothing.
