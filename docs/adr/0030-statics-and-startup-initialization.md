# 0030. Statics and startup initialization

- Status: Accepted; amended by 0034 (no statics of type `!`), 0051 (the symbols of statics)
- Date: 2026-10-15
- Amends: 0012 (`_start` initializes `.data` and `.bss`), 0022 (the
  alignment of `.data` and `.bss`), 0027 (comparing pointers at compile
  time), 0029 (a result may hold pointers to statics; the meanings of
  `eval::pointer-escapes` and `eval::pointer-compare`)

## Context

Constants (ADR 0029) are values: they have no address and cannot change.
Bare-metal programs also need data with an address: tables passed by
pointer, counters updated by the program, buffers handed to DMA, globals
shared with C. ADR 0027 already says their initial values are computed at
compile time; this ADR fixes the rest.

Memory that holds initial values must also be set up before the entry
function runs. ADR 0012 keeps `_start` to two instructions because M0 had
no statics; that no longer holds.

## Decision

### `static` and `static mut`

```
static CRC_TABLE: [u32; 256] = crc_table(0xEDB8_8320);
static mut TICKS: u32 = 0;
static mut CONFIG: Config = Config { baud: 115_200, parity: false };

#[align(64)]
static mut DMA_BUF: [u8; 512] = [0; 512];
```

- A static is an item, at the top level, with an explicit type
  (ADR 0008) and an initializer, which runs at compile time with the rules
  of ADR 0027 and 0029. Its type is any type a constant may have. Statics
  share the single namespace of items (ADR 0029). A `static` inside a
  function body is an error (`parser::local-static`).
- A static is a **place**: one object in memory, with an address
  (`&raw const CRC_TABLE`). Using it reads that memory; it is not copied
  into each use, as a constant is.
- A `static` cannot be assigned (`ty::assign-immutable`) nor have its
  address taken with `&raw mut` (`ty::address-of-immutable`).
- A `static mut` is read and written like a `mut` variable: `TICKS += 1`,
  `&raw mut DMA_BUF`. Accesses are ordinary loads and stores. Sharing one
  with an interrupt handler needs more than that; interrupts arrive in M4,
  whose ADR decides how (volatile, atomics or critical sections).
- **A static is read where the read is written**, like a read through a
  pointer (ADR 0018), never deferred as a local's may be: any call can
  change a `static mut`. In `TICKS + bump()`, where `bump` increments
  `TICKS`, `TICKS` is read before the call.
- Every static is evaluated and emitted, used or not, under its own name
  as its symbol, like functions (ADR 0026): C code can declare it
  `extern uint32_t TICKS;`. Modules (M2.6) will revisit the names.

**Where they live.** A `static` goes to `.rodata`. A `static mut` goes to
`.data`, or to `.bss` when its initial value is all zero bytes. Padding in
initial values is zero (ADR 0027).

**Read-only memory.** The compiler assumes a `static` never changes, and
may use its value instead of reading it. Writing to it through a pointer
made by a cast is not defined: the write may seem to take effect on QEMU,
where `.rodata` is in RAM, while reads the compiler already resolved still
see the old value; on a board with flash it may fault or be ignored. At
compile time it is an error (`eval::read-only-write`).

### `#[align(N)]` on statics

`#[align(N)]` raises the alignment of a static, with `N` computed at
compile time as for structs (ADR 0022, 0029). Unlike on a struct, it does
not change the size: `DMA_BUF` above is 512 bytes at an address that is a
multiple of 64. `N` follows the struct rules (`ty::invalid-align`).
`#[packed]` does not apply to statics (`resolve::misplaced-attribute`).

### Statics at compile time

Any evaluation (ADR 0029) may use statics:

- Reading a `static` gives its value, evaluated on demand; a static whose
  value needs itself is a `ty::cycle`. Its padding reads as zero, as in
  the object file.
- Reading or writing a `static mut` is an error (`eval::static-mut`): its
  value is only known at run time.
- Taking the address of any static is allowed, and needs only its
  address, not its value. The result is a pointer whose address the linker
  fills in (a relocation). It can be offset, stored in memory, and be the
  result of the evaluation: data the compiler emits holds it as a
  relocation. So statics may point at each other:

  ```
  struct Node { next: *const Node, value: u32 }
  static A: Node = Node { next: &raw const B, value: 1 };
  static B: Node = Node { next: &raw const A, value: 2 };
  ```

- A pointer in the result must point into its object or one past its
  end: its offset is between 0 and the object's size, inclusive, so an
  `END` pointer is fine. A slice in the result (ADR 0031) must lie wholly
  inside its object. Anything else is `eval::pointer-out-of-bounds`. This
  holds for statics and byte strings, whose sizes the compiler knows. It
  does not hold for an `extern static`, whose declared size need not be
  its real one (ADR 0031 declares a symbol of unknown size as a `u8`): a
  pointer or slice into one may have any offset and length, and the
  linker adds the offset.
- Such a pointer still cannot become an integer (`eval::pointer-to-int`),
  since its address exists only after linking.
- A pointer into memory that belongs to the evaluation itself (its
  variables) still cannot escape (`eval::pointer-escapes`).

### Comparing pointers at compile time

This replaces what ADR 0027 says on comparisons, for every kind of memory
the compiler holds: statics, `extern static`s, byte strings (ADR 0031) and
the evaluation's own variables. Each rule gives the run-time answer, or an
error where the run-time answer is not known.

- Pointers into **the same object** compare by offset with `==` and `!=`.
  The orderings (`<`, `<=`, `>`, `>=`) are answered only when both offsets
  are between 0 and the object's size, inclusive; outside that range the
  address arithmetic may have wrapped, and they are an error.
- Pointers into **different objects** are known to be different only when
  both point strictly inside their objects (an offset below the size), and
  neither object is a byte string or an `extern static`. Then `==` is
  false and `!=` true.
- Every other comparison between pointers into different objects is an
  error: ordering, and `==` or `!=` when either pointer is one past the end
  of its object, either object has size zero, or either is a byte string
  or an `extern static`. At run time those may be equal, since one object
  can start where another ends (as C11 6.5.9p6 allows), byte strings may
  share memory, and two `extern static`s may name the same object.
- Comparing a pointer into compiler memory with one made from an integer
  stays an error (ADR 0027).

These errors are `eval::pointer-compare`.

**Dangling pointers.** The variables of a running call never share
memory with each other: codegen gives each its own stack slot for the
whole call, so pointers to two of them are different, as the rules above
assume. Lifetime markers that let LLVM reuse a slot within a call would
need this rule revisited. The variables of a call stop existing when it
returns; at run time their stack slots are reused, so a pointer to one may
equal a pointer to a later call's variable and read its value. At compile
time such a pointer is **dangling**: it can be copied, but comparing it or
accessing memory through it is an error (`eval::dangling-pointer`).

The interpreter of M2.3a treats a pointer one past the end of an object as
different from a pointer into another object, orders offsets that have
wrapped, and keeps the variables of returned calls alive; all three are
corrected with this ADR.

### External statics

```
extern static mut rx_count: u32;
extern static crc_lookup: [u32; 256];
```

An `extern static` is defined outside the program (C, assembly) and found
by the linker, like an `extern fn` (ADR 0026). It has no initializer, and
nothing checks that its definition matches. Without `mut`, it promises
that its value never changes, and the compiler may rely on that; a global
that other code changes must be declared `extern static mut`. At compile
time its address can be taken, but reading or writing it is an error
(`eval::extern-static`). `#[align(N)]` does not apply
(`resolve::misplaced-attribute`): the definition decides its alignment.
An `extern static` inside a function body is a `parser::local-static`.

### Startup initializes `.data` and `.bss`

`_start` (ADR 0012) now does, in order:

1. set the stack pointer;
2. fill `.bss` with zeros;
3. copy the initial values of `.data` from their load address to their run
   address;
4. jump to the entry function.

The loops work a word at a time, between symbols the linker script
defines: `__bss_start`, `__bss_end`, `__data_start`, `__data_end` and
`__data_load`. Each loop tests its bounds before the first word, so an
empty `.data` or `.bss` writes nothing. Names starting with `__` are
already reserved (ADR 0020).

The script guarantees, for the loops to be right:

- `.data` and `.bss` start at an address aligned to the largest
  alignment they contain and to at least 4, and so does the load image of
  `.data`. lld aligns a
  load address only to its section's own alignment, so the script sets it
  on the output section (`.data : ALIGN(4) { ... }`), not on the symbols.
- `__data_start` is the start of `.data`, `__data_load` its load address
  (`LOADADDR(.data)`), and `__bss_start` the start of `.bss`.
- `__data_end` and `__bss_end` are the ends of their sections, after
  padding to a multiple of 4 placed **inside** the sections
  (`. = ALIGN(4);` as their last statement). The ranges therefore cover
  their sections, all five symbols are multiples of 4, and the padding
  belongs to no other section.
- The ranges `[__data_start, __data_end)`, `[__bss_start, __bss_end)` and
  the load image `[__data_load, __data_load + __data_end - __data_start)`
  are pairwise disjoint.

This amends ADR 0022, whose script aligned the start and end of `.data`
and `.bss` to the largest alignment they contain. Their starts are now
also aligned to at least 4; their ends are padded to a multiple of 4
inside the sections, and no more: nothing needs an end aligned further.

**Memory layout until M3.** The generated linker script lays the image out
as on a board whose program is in flash. It splits QEMU's 128 MiB of RAM
into two regions: `IMAGE`, the first 64 MiB, in the role of flash, holds
`.text`, `.rodata` and the load image of `.data`; `RAM`, the last 64 MiB,
holds `.data`, `.bss` and the stack (`> RAM AT> IMAGE`). The copy
therefore runs on every boot, also on QEMU, which loads each segment at
its load address. In M3, memory maps declare the regions instead.

**Sections.** The linker script also gathers the small-data sections that
GCC emits on RISC-V by default, and clang with `-msmall-data-limit=8`:
`.srodata*` with `.rodata`, `.sdata*` with `.data`, `.sbss*` with `.bss`.
Linking uses `--orphan-handling=error`, so a section the script does not
place is a link error, not data left outside what `_start` initializes.
The sections that are not loaded (`.symtab`, `.strtab`, `.shstrtab`,
`.comment`, `.riscv.attributes`, debug information) are placed
explicitly, not discarded: the symbol table is what tools and tests read.
The script defines no `__global_pointer$`, so the linker never rewrites
accesses relative to `gp`, which `_start` does not set.

This amends ADR 0012, whose rule "it does nothing else" now allows exactly
these two loops. `_start` stays visible in `--emit=llvm-ir`.

### Diagnostic codes

| Code | Meaning |
|------|---------|
| `eval::static-mut` | reading or writing a `static mut` at compile time (ADR 0029) |
| `eval::extern-static` | reading or writing an `extern static` at compile time |
| `eval::read-only-write` | writing to a `static` or a byte string at compile time |
| `eval::dangling-pointer` | comparing or accessing through a pointer to a variable of a returned call |
| `eval::pointer-out-of-bounds` | a result pointing outside its object, or a slice not inside it |
| `parser::local-static` | `static` or `extern static` inside a function body |

Two codes of ADR 0029 change meaning: `eval::pointer-escapes` is now only
a result pointing into the evaluation's own memory, since pointers to
statics and byte strings may leave; `eval::pointer-compare` now also
covers `==` and `!=`, by the rules above.

A `static` without an initializer, or an `extern static` with one, is a
`parser::unexpected-token`, as for functions. `static` becomes a keyword.

### Conformance tests

- **ui snapshots** for each code above; assignment to and `&raw mut` of a
  `static`; `#[align(N)]` with an invalid `N` and `#[packed]` on a static;
  a cycle between statics; the two statics pointing at each other above,
  which is not a cycle; each case of pointer comparison above, including
  one past the end of an object, offsets that wrapped, and `extern
  static`s; a dangling pointer compared and read; results pointing one
  past the end of a static (accepted) and beyond it, and a slice longer
  than its static (`eval::pointer-out-of-bounds`).
- **LLVM IR snapshots** showing a `static` as a constant global, a
  `static mut` with an initial value and one all zero, `#[align(64)]` on a
  global, a pointer to a static inside another static's initial value, and
  the new `_start`.
- **Sections:** the test harness reads the section of each symbol in a
  built ELF and checks that a `static` lands in `.rodata`, a `static mut`
  with an initial value in `.data` and an all-zero one in `.bss` (or their
  small-data variants, gathered with them).
- **MIR snapshot and end to end:** `TICKS + bump()` reads `TICKS` before
  the call.
- **End to end:** reading and writing statics of each kind; the address of
  an aligned static checked at run time; a C file that reads and writes an
  Erebos static, and one that defines an `extern static`.
- **Initialization:**
  - A program whose `.data` holds several values checks them on entry,
    which fails if the copy is wrong, since only `_start` ever writes the
    run addresses.
  - Another program dirties a `.bss` static and jumps back to `_start`
    through a C helper; on the second run, it checks that the static reads
    zero again, using a marker at a fixed address in the `IMAGE` region
    that `_start` never touches.
  - The test harness reads the symbols and program headers of a built ELF
    in which read-only data of odd length (a byte string) comes right
    before `.data`, and `.data` holds only data of alignment 1 and odd
    size. It checks the guarantees above: the five symbols are multiples
    of 4, `__data_load` equals the physical address of the segment holding
    `.data`, each range covers its section, and the three ranges are
    disjoint. A script that aligned only the symbols fails this test.
  - Programs with no `.data` or `.bss` (most existing ones) still run, so
    the loops do nothing when a range is empty.
  - A C object compiled with `-msmall-data-limit=8` links, and its small
    initialized globals hold their values on entry.
- **Differential tests** gain functions that read statics and follow
  pointers between them, at compile time and at run time.
- **Reproducible builds** (`tests/reproducible.rs`) gain a program with
  statics holding padding and pointers to other statics, as ADR 0027
  promised for statics with padding.

## Consequences

- Programs have global state, tables they can pass by pointer, and buffers
  aligned for DMA, all with values computed by the compiler. There is no
  code that runs before the entry function other than `_start`, and no
  order of initialization to get wrong.
- Every boot pays for zeroing `.bss` and copying `.data`, as in C.
- Until M3, programs and their `.data` image must fit in the first 64 MiB,
  and `.data`, `.bss` and the stack in the last 64 MiB. Test programs that
  use free RAM at fixed addresses use the `IMAGE` region above the program.
- `static mut` accesses are plain until M4 decides how interrupts share
  data; code written before then may need to change.
- Writing to read-only memory through a cast pointer is not defined, as
  are a non-`mut` `extern static` that other code changes and `extern`
  declarations that do not match their definitions (ADR 0026).
- Some pointer comparisons that M2.3a answered at compile time now fail,
  since the run-time answer may differ, and pointers to the variables of
  returned calls can no longer be used.

## Alternatives considered

- **Initializers that run at startup, as C++ constructors** — hidden code
  before the entry function (ADR 0002) and an order of initialization
  across statics; compile-time execution makes them unnecessary.
- **Requiring `unsafe` for `static mut`, as Rust** — Erebos has no `unsafe`,
  and the problem it guards against, sharing with interrupts, does not
  exist until M4.
- **Uninitialized statics** (`static mut BUF: [u8; 4096];`) — `.bss` already
  costs nothing in the image; a way to skip zeroing can come with memory
  maps, if a program needs it.
- **Copying `.data` only when its load and run addresses differ** — the
  copy would never run on QEMU until M3, and could be broken unnoticed.
- **Placing the load image right after `.rodata` in one region** — `.rodata`
  may end at an odd address, and the image then touches `.data`; the word
  copy would read misaligned and overlapping memory. Two regions separate
  them, as M3 will, and aligning the output sections aligns them.
- **Leaving `_start` unchanged and trusting the loader** — QEMU's loader
  happens to zero `.bss`, but a reset or a jump back to `_start` does not,
  and boards with flash do neither.
- **`static` inside functions, as Rust** — useful for state private to a
  function, but modules (M2.6) give a place for private statics; it can be
  allowed later without breaking programs.
