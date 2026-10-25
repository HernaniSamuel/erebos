# Erebos language reference (M3.1)

This describes the language as the compiler implements it today. The
reasoning behind each rule lives in the ADRs (`docs/adr/`), cited as
"ADR NNNN". When this document and the compiler disagree, that is a bug in
one of them.

## Programs

A program is a **root file** (`.ers`), the one given to `erebos build`,
plus the files of the modules it declares (ADR 0049), plus the module
`core`, shipped inside the compiler (ADR 0052). Each file is a list of
items, in any order: functions, external functions, structs, enums,
constants, statics, external statics, `comptime` blocks, modules (`mod`),
`use` declarations and the `memory` block. The root file is the **root
module**, and the modules it declares, inline or in their own files, form
a tree under it; each module has its own namespace (see "Modules and
paths"). Items may refer to each other whatever their order and their
files (ADR 0028). The program must contain exactly one **entry function**
and one **panic function**, in any of its modules, and, to be built into
an executable, exactly one **`memory` block**, which gives the board's
memory and what each part of it holds (ADR 0057, see "Memory maps"):

```
const KIB: usize = 1024;

memory {
    region FLASH { origin: 0x8000_0000, size: 64 * KIB }
    region RAM { origin: 0x8001_0000, size: 16 * KIB }
    code in FLASH;
    data in RAM;
    stack in RAM { size: 2 * KIB }
}

entry fn main() -> ! {
    loop {}
}

panic fn on_panic() -> ! {
    loop {}
}
```

- `entry fn` is where execution starts, after `_start` has set up the
  stack, `.bss` and `.data` (see "Startup and memory" below). It takes no
  parameters, must return `!`, and cannot be called.
- `panic fn` runs when an operation cannot produce a result, such as
  division by zero, an index out of bounds, a failed `assert`, or an enum
  value that is no variant, met by a `match` that no arm matches or by `?`
  (ADR 0014, 0036, 0040, 0046). It must return `!`, and may be called
  explicitly. It takes **no parameter, or one of type
  `*const core::PanicInfo`**, through which it learns why and where the
  program panicked (ADR 0053, see "Panic information"):

  ```
  panic fn on_panic(info: *const core::PanicInfo) -> ! { ... }
  ```

The target is RV32IMAC on QEMU `virt` (ADR 0010). The board's memory is
not part of the target: each program declares it in its `memory` block.

## Lexical structure

- **Comments:** `// to end of line` and `/* block */`; block comments nest.
- **Identifiers:** `[A-Za-z_][A-Za-z0-9_]*`, except `_` alone. No item,
  module or `use` of the program may take a name starting with `__`, which
  the runtime support functions of `core` use, nor `_start`, nor `core`, in
  any module (`resolve::reserved-name`, ADR 0020, 0052); a module of the
  root module may not be named `llvm` (see "Symbols") or `rt` (see
  "Modules and paths").
- **Keywords:** `align_of as break comptime const continue crate else entry
  enum extern false fn if let loop match mod mut offset_of panic pub return
  size_of static struct super true use volatile while`. `raw` is a keyword
  only right after `&`. `memory`, and the words inside a `memory` block
  (`region`, `origin`, `size`, `code`, `data`, `stack`, `in`), are no
  keywords: they are recognized only in their place (see "Memory maps"),
  and are ordinary names elsewhere.
- **`_`** is a token of its own, the wildcard pattern of `match`, and
  nothing else (ADR 0037): no variable, parameter, item, field or variant
  can be named `_` (`let _: u32` and `const _: u32` are
  `parser::unexpected-token`), and `_` is not an expression. Names that
  only start with `_`, such as `_x`, are identifiers.
- **`::`** separates the segments of a path: a module from its items
  (`uart::init`) and an enum from its variant (`Mode::Run`), see "Modules
  and paths". **`=>`** separates a pattern from its arm's expression.
- **`.`** before a name, where an operand or a pattern starts, names a
  variant of the expected enum (`.Run`, see "Enums"); after an
  expression, it reaches a field (`p.x`).
- **`?`** is the postfix operator of "The `?` operator".
- **Built-in functions:** `assert`, `len` and `slice` are reserved names:
  no item, module or `use` may have one of them, in any module. A call to
  one of these names is always the built-in; variables may still be named
  so (ADR 0031).
- **Integer literals** (ADR 0016): `42`, `0x2A`, `0o52`, `0b10_1010`; `_`
  separates digits. A decimal literal with a leading zero (`017`) is an
  error. Values must fit in 64 bits. There are no suffixes (`1u8`).
- **Byte literals:** `b'a'` has type `u8`. Printable ASCII is written
  directly; escapes are `\n \r \t \\ \' \" \0` and `\xNN` (two hex digits).
- **Byte strings:** `b"..."` holds the same characters and escapes as byte
  literals (`'` may be written directly, `"` needs `\"`). It ends on its
  line: a line break inside it, or a missing `"`, is an error; write `\n`.
  See "Byte strings" below.
- There are no `char`, text string or floating-point literals; a byte
  string is bytes, not Unicode text (ADR 0016).

## Types

| Type | Meaning |
|------|---------|
| `u8 u16 u32 u64 usize` | unsigned integers; `usize` is pointer-sized (32 bits) |
| `i8 i16 i32 i64 isize` | two's-complement signed integers |
| `bool` | `true` or `false` |
| `()` | the unit type: no value to speak of |
| `!` | never: the type of an expression that does not finish, never of memory (ADR 0034) |
| `*const T`, `*mut T` | raw pointers |
| `*const volatile T`, `*mut volatile T` | raw pointers whose accesses are volatile (ADR 0013, 0023) |
| `[T; N]` | an array of `N` values of type `T`; `N` is a `usize` computed at compile time (ADR 0021, 0029), so a literal too large for `usize` is `ty::literal-out-of-range`, as it is in `#[align(N)]` |
| `*const [T]`, `*mut [T]` | slices: the address of `T` values and how many there are (ADR 0031); `volatile` as for any pointer |
| `Name`, `uart::Name` | a struct declared with `struct Name { ... }`, named by a path (ADR 0021, 0049) |
| `Name`, `uart::Name` | an enum declared with `enum Name: u8 { ... }`: without payloads, stored as its integer type (ADR 0035); with payloads, a tag and the payload values of its variant (ADR 0039) |
| `Option(T)`, `Result(T, E)` | built-in enums with payloads: a `T` or nothing, and a `T` or an error `E` (ADR 0045, see "`Option` and `Result`") |

Every `let`, parameter and return type is written explicitly; there is no
inference (ADR 0008). `-> ()` may be omitted (ADR 0015). Arrays, structs
and enums with payloads are **aggregates**, and so are slices where a rule
depends on it.

The names of the built-in types, `bool`, the integer types, `Option` and
`Result`, are no item's: a struct, enum, function, constant, static,
module or `use` named so, in any module, is `resolve::duplicate-type`
(ADR 0029, 0045, 0049). They are seen from every module, as a single name:
`uart::u32` is no type.

A pointer to a sized type is a **thin pointer**: a plain address. `[T]` on
its own is **unsized**: it exists only behind a pointer, and a variable,
parameter, return value, field, array element or payload value of type
`[T]`, a `static`, `extern static` or `const` of that type, a layout
query (`size_of`, `align_of`, `offset_of`) of it, and a slice of `[T]`
are errors (`ty::unsized`). `[[u8; 4]]` is a slice of arrays, and fine.

**`!` is not a type of memory** (ADR 0034, 0039). No value of type `!`
exists, so it is an error (`ty::never-in-memory`) as the pointee of a
pointer (`*const !`), the element of an array or slice type (`[!; 0]`,
`*const [!]`), a field type, a payload type, the type of a `static`,
`extern static`, `const` or `comptime` value, an operand of `size_of`,
`align_of` or `offset_of`, and under `&raw` (`&raw const x` with
`x: !`). Each `!` written there is reported once, also where nothing uses
it, and the type containing it causes no further error. `!` stays valid
as the type of an expression, as a return type (`-> !`) and in a `let`
whose initializer never finishes (`let x: ! = loop {};`). An array literal
or repeat with an element of type `!` (`[loop {}; 3]`) has type `!`; its
other elements are checked against the expected element type, or the
first element whose type is not `!`. Use `()` for a type without data.

## Functions

```
fn add(a: u32, mut b: u32) -> u32 {
    b += a;
    b
}
```

- Functions may be called before their declaration, and may recurse.
- Arguments are checked against parameter types with no conversion other
  than the pointer conversions of "Typing".
- Parameters cannot have type `()` or `!`.
- There are no methods: functions take their receiver explicitly, such as
  `fn area(r: *const Rect) -> u32` (methods come with references, M5).
- All functions use the C ABI of the target (ADR 0006). A function of the
  root module has its own name as its symbol, and so has one marked
  `#[export]`, so C code can call it by that name; a function of another
  module is named by its path, `uart.init` (ADR 0051, see "Symbols").

### Aggregates by value (ADR 0025, 0039)

Structs, arrays and enums with payloads are parameters and return values
like any other type, passed as the riscv32 C ABI says:

| Aggregate | Argument | Return value |
|-----------|----------|--------------|
| size 0 | not passed | nothing |
| up to 4 bytes | one register | `a0` |
| up to 8 bytes | a register pair | `a0`, `a1` |
| larger | a pointer to a copy the caller makes | memory the caller provides, through a hidden first argument |

A parameter is always the callee's own copy: changing it never changes the
caller's value. Arrays pass like a struct of the same layout; C, which has
no arrays by value, wraps them in a struct. A slice passes like the C
struct `struct { const T *ptr; size_t len; }`: in a register pair, and
returned in `a0`, `a1` (ADR 0031). An enum with payloads passes by its
size and alignment, as its C twin, a union, does (see "Enums"); it is
never of size 0, since its tag has a size. Enums without payloads are not
aggregates: they pass as their integer type.

### External functions (ADR 0026)

```
extern fn c_checksum(data: *const u8, n: usize) -> u32;
```

declares a function defined outside the program, in C, assembly or a
vendor library, which the linker finds; `erebos build --link file.o` adds
the object file. It is called like any function. Its parameters cannot be
`mut`, and nothing checks the declaration against the definition: as in C,
a mismatch is undefined. Its symbol is its own name, in any module; a C
function is declared in one module and brought into the others with `use`
(see "Symbols").

## Statements and blocks

```
let x: u32 = 1;          // immutable
let mut y: u32 = 2;      // mutable
x + y;                   // expression statement
```

A block `{ ... }` is an expression (ADR 0015). Its value is its final
expression without `;`; with no such expression it has type `()`, or `!` if
it cannot reach its end. A block-like expression (`if`, `match`, `while`,
`loop`, `{ }`) may stand as a statement without `;`, but must then have
type `()`. A statement that starts with one ends with it: `if c { } - 1`
is not a subtraction.

Names are scoped to their block; a `let` may shadow an earlier name.

## Structs and arrays (ADR 0021)

```
struct Point { x: i32, y: i32 }

let mut p: Point = Point { y: 2, x: 1 };
p.x = p.y + 1;

let mut buf: [u8; 4] = [1, 2, 3, 4];
let zeros: [u32; 16] = [0; 16];
buf[i] = buf[0];
```

- A struct literal gives every field exactly once, in any order; the
  values run in the order written.
- A field is private to the struct's module unless declared `pub`
  (`pub x: i32`): outside, it cannot be named, and the struct cannot be
  built with a literal (ADR 0050, see "Visibility").
- `[a, b, c]` lists every element. `[v; N]` evaluates `v` exactly once,
  even for `N` of 0, and copies it into every element.
- `s.field` and `a[i]` are places when `s` and `a` are: reading or writing
  one reaches only that field or element. `(*p).field` and `(*p)[i]` reach
  one part through a pointer without copying the whole; there is no
  automatic dereference (`p.field` is an error).
- **Array indexing is checked.** The index has type `usize`; if it is not
  below the length, the program calls the panic function. An index known
  at compile time that is out of bounds is a compile-time error (see
  "Checks on values known at compile time").
- **Pointer indexing is not.** With `p: *const T` or `*mut T`, `p[i]` is the
  place at address `p + i * size_of(T)`, computed with wrapping arithmetic,
  as in C. `(*p)[i]` with `p: *mut [T; N]` is the checked form, and so is
  `(*s)[i]` with a slice `s` (see "Slices").
- `let`, assignment and reading a place copy aggregates whole. `==` and
  `!=` do not apply to them; compare their parts.
- A struct may contain itself only behind a pointer (`next: *mut Node`).
- In the condition of `if` and `while`, a `{` opens the body, so a struct
  literal there needs parentheses: `if (Point { x: 0, y: 0 }).x == 0 { }`.

### Addresses: `&raw`

`&raw const place` and `&raw mut place` give a `*const T` or `*mut T` to a
place: a variable, a static, a byte string, `*pointer`, `a[i]`, `p[i]`,
`(*s)[i]` or a field. `&raw mut` needs a mutable place: not a variable
without `mut`, a `static`, an `extern static` without `mut`, a byte string,
part of a temporary, nor anything reached through a `*const` pointer
(`ty::address-of-immutable`). The result is never
volatile. A plain `&` is reserved for references (M5).

A constant has no address, and neither does any element or field of it:
`&raw const T`, `&raw const T[1]` and `&raw const S.f` are
`ty::not-a-place` (ADR 0029). Through a pointer held in a constant, places
are as usual: `&raw mut (*UART).ctrl` and `&raw mut BASE[1]` are fine.

An element or field of a value that is not a place, such as `(f())[1]`,
`({ x })[0]` or `(comptime t())[1]`, is part of a temporary: `&raw const`
of it points into a temporary holding the value where the expression runs,
never into a variable the value came from, nor into the data of a
constant. Such a part cannot be assigned, nor have a `*mut` address
taken.

`&raw const p[i]` is how a pointer is offset by `i` elements.

## Statics (ADR 0030)

```
static CRC_TABLE: [u32; 256] = crc_table(0xEDB8_8320);
static mut TICKS: u32 = 0;

#[align(64)]
static mut DMA_BUF: [u8; 512] = [0; 512];

extern static mut rx_count: u32;
extern static crc_lookup: [u32; 256];
```

- A static is an item of a module with an explicit type and an initializer,
  which runs at compile time (see "Compile-time execution"). Its type is
  any type a constant may have: sized, and not `!`. A static without an
  initializer is a `parser::unexpected-token`; one inside a function body
  is a `parser::local-static`.
- A static is a **place**: one object in memory, with an address
  (`&raw const CRC_TABLE`). Using it reads that memory; it is not copied
  into each use, as a constant is.
- A `static` cannot be assigned (`ty::assign-immutable`) nor have its
  address taken with `&raw mut` (`ty::address-of-immutable`). A
  `static mut` is read and written like a `mut` variable (`TICKS += 1`,
  `&raw mut DMA_BUF`), with ordinary loads and stores. Sharing one with
  interrupt handlers comes with interrupts (M4).
- **A static is read where the read is written**, like a read through a
  pointer, never deferred: in `TICKS + bump()`, where `bump` changes
  `TICKS`, `TICKS` is read before the call.
- Every static is evaluated and emitted, used or not. A static of the root
  module, or one marked `#[export]`, has its own name as its symbol, so C
  code can declare `extern uint32_t TICKS;`; one of another module is named
  by its path, `uart.regs.BASE` (ADR 0051, see "Symbols").
- **Where they live.** A `static` goes to `.rodata`. A `static mut` goes to
  `.data`, or to `.bss` when its initial value is all zero bytes. Padding
  in initial values is zero, and so are the bytes of an enum with payloads
  outside its variant's tag and payload (ADR 0039): a `static mut` holding
  a variant without payload whose value is 0 goes to `.bss`.
- **Read-only memory.** The compiler assumes a `static` never changes and
  may use its value instead of reading it. Writing to one through a pointer
  made by a cast is not defined; at compile time it is an
  `eval::read-only-write`.
- `#[align(N)]` before a static raises its alignment, with `N` following
  the struct rules (`ty::invalid-align`). Unlike on a struct, it does not
  change the size: `DMA_BUF` is 512 bytes at an address that is a multiple
  of 64. An `N` in error is reported once, and the static keeps its type's
  alignment: an alignment that is part of no type makes nothing erroneous,
  and code using the static is built and checked as usual (ADR 0048, see
  "Failed lengths and alignments"). `#[packed]` does not apply to statics
  (`resolve::misplaced-attribute`).

**External statics.** An `extern static` is defined outside the program,
in C, assembly or a vendor library, and found by the linker, like an
`extern fn`; `erebos build --link file.o` adds the object file. It has no
initializer (`parser::unexpected-token` if given one), and nothing checks
that its definition matches. It is a place like a static, read where the
read is written. Without `mut`, it cannot be assigned nor have a `*mut`
address taken, and it promises that its value never changes, which the
compiler may rely on: a global that other code changes must be declared
`extern static mut`.
`#[align(N)]` does not apply (`resolve::misplaced-attribute`): the
definition decides the alignment. A symbol of unknown size, as C's
`extern const uint8_t t[];`, is declared with a sized type
(`extern static t: u8;`) and turned into a slice with `slice`.

## Byte strings (ADR 0031)

```
let greeting: [u8; 6] = b"hello\n";      // a copy
let p: *const [u8; 2] = &raw const b"ok"; // no copy
```

- `b"..."` has type `[u8; N]`, `N` being the number of bytes after escapes
  are resolved: `b"a\x00b"` is `[u8; 3]`, `b""` is `[u8; 0]`. Nothing is
  appended; a C string is written `b"abc\0"`.
- A byte string is a **place in read-only memory**, like an unnamed
  `static` of alignment 1. `&raw const` gives its address without a copy,
  and indexing it (`b"abc"[i]`) reads one byte. Using it as a value copies
  it, as for any array. It cannot be assigned (`ty::assign-immutable`) nor
  have a `*mut` address taken (`ty::address-of-immutable`); writing to it
  through a cast pointer is not defined, as for a `static`.
- Each occurrence in the source is one object, wherever and however often
  it is evaluated, the same at compile time and at run time. Identical byte
  strings may share memory, with each other and with other read-only data,
  so whether two of them have the same address is not defined.

## Slices (ADR 0031)

A slice, `*const [T]` or `*mut [T]`, is the address of its first element
and its length in elements: 8 bytes with alignment 4, exactly the C struct
`struct { const T *ptr; size_t len; }`.

```
fn print(s: *const [u8]) {
    let mut i: usize = 0;
    while i < len(s) {
        putc((*s)[i]);
        i += 1;
    }
}

print(&raw const b"hello\n");
```

**Making a slice.**

- A pointer to an array **converts implicitly** to a slice of its
  elements, with length `N`: `*const [T; N]` to `*const [T]`, and
  `*mut [T; N]` to `*mut [T]` or `*const [T]`. It applies where `*mut T`
  converts to `*const T` (see "Typing"), keeps volatility, and converts only
  the outermost pointer.
- `slice(p, n)`, built in, is the slice of `n` elements (a `usize`)
  starting at `p`: a `*const T` gives a `*const [T]` and a `*mut T` a
  `*mut [T]`, with the same volatility. Nothing is checked; the program
  vouches for it, as for a pointer made from an integer. Sub-slices are
  made with it: `slice(&raw const (*s)[2], 3)`.

**Using a slice.**

- `len(s)`, built in, is the length of `s`, a `usize`, and reads no
  memory. Its argument is any slice, `*const` or `*mut`, volatile or not,
  or a pointer to an array, converted as above: `len(&raw const b"hi")`
  is 2.
- `(*s)[i]` is the place of element `i`, **checked**: an index not below
  `len(s)` calls the panic function, as for arrays. It can be read,
  written through a `*mut [T]` (`ty::write-through-const` through a
  `*const [T]`), and passed to `&raw`.
- `s[i]` on a slice is an error (`ty::slice-pointer-index`): indexing a
  pointer steps over whole pointees, and `[T]` has no size. Write
  `(*s)[i]`, or `(s as *const T)[i]` for an unchecked access.
- `*s` can only be indexed or passed to `&raw` (`&raw const *s` is the
  slice again); reading, writing or copying all of it is a `ty::unsized`.
  A slice has no fields: its length is `len(s)`, not `s.len`.

**A slice is not an address.** Only thin pointers are plain addresses;
a slice is an aggregate of two scalars wherever a rule depends on it:

- Slices cannot be compared (`ty::invalid-operand`); compare their
  addresses and lengths.
- Reading or writing a slice stored in memory through a volatile pointer
  is a `ty::volatile-aggregate`, since it takes two accesses. Through a
  volatile slice, `(*s)[i]` is one volatile access of the element.
- Casts follow the table below; every other pointer cast is as for thin
  pointers.

| From | To | |
|------|----|-|
| slice of `T` | any thin pointer (`*const T`, `*mut U`, `*const [T; N]`, ...) | the address of the first element; the length is dropped |
| slice of `T` | slice of `T` | changes mutability or volatility |
| pointer to `[T; N]` | slice of `T` | length `N`; may change mutability or volatility |
| slice of `T` | slice of `U`, `U` not `T` | `ty::invalid-cast` |
| thin pointer, other than to an array of `T` | slice of `T` | `ty::invalid-cast`; use `slice(p, n)` |
| slice, or integer | integer, or slice | `ty::invalid-cast` |

## Enums (ADR 0035, 0036, 0039, 0044)

```
enum Mode: u8 {
    Off = 0,
    Idle = 1,
    Run = 3,
}

enum Msg: u8 {
    Nop = 0,
    Led(bool) = 1,
    Write(u8, u16) = 2,
    Wide(u32) = 3,
}

let m: Mode = Mode::Run;
let code: u8 = m as u8; // 3
let w: Msg = Msg::Write(3, 0x1234);
```

**Declaration.**

- An enum is an item, in the namespace of its module. Its name is followed
  by `:` and its **integer type**, any of `u8 u16 u32 u64 usize i8 i16 i32
  i64 isize` (`resolve::enum-type` for any other type), and then its
  variants, separated by commas, with an optional trailing comma.
- **Both are required**: without the integer type, the declaration is
  `parser::enum-without-type`, and a variant without `= value` is
  `parser::variant-without-value`. No value is ever implied, so inserting
  a variant never renumbers the others.
- A variant's value is an expression of the enum's integer type, computed
  at compile time like an array length: `= 1 << 3`, `= BASE + 1`,
  `= Mode::Idle as u8 + 2`. A literal takes the integer type, so `= 300`
  in a `u8` enum is `ty::literal-out-of-range`, and `= -1` needs a signed
  type. A value that needs itself, directly or through other variants or
  constants, is a `ty::cycle`.
- Two variants with one name are `resolve::duplicate-variant`; two with
  one value are `ty::duplicate-discriminant`, naming both. An enum needs at
  least one variant (`resolve::empty-enum`).
- A value that fails to evaluate is reported once, like a constant, and so
  is a value given twice; uses of the enum report nothing more because of
  it (see "`match`" for what a `match` on it does).
- No attribute applies to an enum: `#[packed]` and `#[align(N)]` are
  `resolve::misplaced-attribute`.

**Payloads (ADR 0039).**

- A variant may carry a **payload**: one or more types in parentheses
  after its name, separated by commas, with an optional trailing comma,
  before its `= value`: `Write(u8, u16) = 2`. A payload after the value
  (`Write = 2(u8, u16)`) is `parser::unexpected-token`. The payload's
  values are **positional**: they have no names, and are told apart by
  their order.
- `Led() = 1`, parentheses without a type, is `parser::empty-payload`: a
  variant without payload is written without parentheses.
- An **enum with payloads** is one with at least one variant that has a
  payload; the others are **enums without payloads**. The two kinds differ
  in their operations, layout and calls, as below.
- A payload type is any type a field may have: integers, `bool`,
  pointers, slices, arrays, structs and enums of either kind, zero-sized
  types included. `!` is not (`ty::never-in-memory`), nor is `[T]`
  (`ty::unsized`). A payload type in error, such as these or an unknown
  name, is reported once, at that type, also in an enum nothing uses, and
  the enum then counts as erroneous: its uses, in `size_of`, in building a
  variant, in a `match` or as a parameter, report nothing more because of
  it. An enum with a payload type that reaches a failed array length or
  alignment, such as `*const Broken`, counts as erroneous too, although
  type errors in its uses are still reported (see "Failed lengths and
  alignments").
- An enum that contains itself by value, directly or through structs,
  arrays and other enums, is `ty::infinite-size`, which names every type
  of the cycle: `Cons(u8, List)` in `enum List` has no size, and
  `Cons(u8, *const List)` is the way to write it. An enum larger than half
  the address space is `ty::too-large`, as a struct is.
- `as` does not apply to an enum with payloads (below), so a variant's
  value cannot be computed from another variant of it:
  `= Msg::Nop as u8 + 1` is `ty::invalid-cast`. A constant holding the
  number serves both.

**Naming and building a variant.**

- `Mode::Run` is the variant `Run` of `Mode`, an expression of type `Mode`.
  It is a value, not a place, like a constant: `&raw const Mode::Run` is
  `ty::not-a-place`, and `Mode::Run = m` is `ty::not-assignable`.
- **`Msg::Write(a, b)`** builds the variant `Write` with the payload values
  `a` and `b`, in order. Each is checked against its payload type as an
  argument is against its parameter: a literal takes that type, and the
  implicit conversions of "Typing" apply. They run left to right. A
  variant without payload is named without parentheses, `Msg::Nop`, also
  in an enum with payloads. The result is a value, not a place, as above:
  `&raw const Msg::Write(3, 4)` is `ty::not-a-place`.
- The number of values must be the payload's: `Msg::Write(3)`,
  `Msg::Write(1, 2, 3)`, `Msg::Write` and `Msg::Nop(1)` are
  `ty::wrong-payload-count`, which says how many values the variant
  carries. So are `Msg::Nop()` and `Mode::Off()`: a variant without
  payload is named without parentheses. `Msg::Write` alone is no function
  either; there are no function values.
- A variant of an enum without payloads is known at compile time, and so
  is such a variant converted with `as` (see "Checks on values known at
  compile time"): `[u8; Mode::Run as usize]` is fine. No value of an enum
  with payloads is known, not even `Msg::Nop`.
- `Mode::Run` is a path (see "Modules and paths"), and so is
  `uart::Mode::Run`: the name before `::` is looked up among items only,
  as the name of a struct literal is, so a local variable named `Mode`
  does not hide the enum. A name that is nothing there is
  `resolve::unknown-path`, an item that is neither a module nor an enum is
  `resolve::not-an-enum`, and `Mode::Walk`, with no such variant, is
  `resolve::unknown-variant`. A variant ends the path: `Mode::Run::x` is
  `resolve::not-an-enum`. `Option` and `Result` are no items, and their
  variants have no path: `Option::None` is `resolve::not-an-enum` (see
  "`Option` and `Result`").
- Variants are visible wherever their enum is reachable, and only there;
  `.Run` where `Mode` is not is `ty::private-variant` (ADR 0050, see
  "Visibility").

**Variants named by the expected type (ADR 0044).**

```
let m: Mode = .Run;
let w: Msg = .Write(3, 0x1234);
send(.Led(true));
if m == .Off { ... }
```

- **`.Run` and `.Write(a, b)`**, a `.` followed by a variant's name and,
  for a variant with a payload, its values, name a variant of **the enum
  that is the expected type** where they appear, declared or built in
  (`Option`, `Result`). Each means what `Mode::Run` or `Msg::Write(a, b)`
  means there: the same type and value, and the same rules. The number of
  values is checked (`ty::wrong-payload-count`, as for `.Write(3)`,
  `.Write` and `.Nop()`), each value against its payload type, which is
  its expected type, so variants nest: `.Some(.Run)` where an
  `Option(Mode)` is expected. A trailing comma is allowed. The result is
  a value, not a place.
- `.V` takes its enum **exactly where a literal takes its type** (see
  "Typing"), including from a sibling: `if c { m } else { .Off }`, a later
  arm of a `match` or `break` of a loop, `[m, .Off]` and `[.Off, m]`,
  `m == .Run` and `.Run == m`. The one exception is the operand of `as`,
  where a literal takes the target type: nothing converts to an enum, so
  `.Run as u8` has no enum to take.
- **Where no type is expected**, `.V` is `ty::untyped-variant`: as the
  scrutinee of a `match`, the operand of `as` or of `?`, an expression
  statement, and in `.Off == .Run`, where it is reported at the first
  operand. Its help shows `E::V` when exactly one declared enum reachable
  there has a variant of that name, and, for `.None`, `.Some`, `.Ok` and
  `.Err`, how to write an `Option` or `Result` type where the value is
  expected; both when both apply.
- Where the expected type is not an enum, `.V` is `ty::mismatched-types`
  ("expected `usize`, found variant `.Run`"), as in `let x: u32 = .Run;`,
  `a[.Run]` and `if .Run {}`. Where the expected enum has no variant of that
  name, it is `ty::unknown-variant`, found by type checking, as an unknown
  field is. Their helps list only the variants and fields that may be named
  there (ADR 0050). Where the expected type is erroneous, nothing is
  reported. An operator that does not apply to the enum reports its own
  error, as for any operand of it: `.Run < m` is `ty::invalid-operand`.
- A `.V` in error still checks its values, against no type, so that a
  literal among them reports nothing more: `let x: u32 = .Write(3, 4);`
  and `.Bogus(1)` where a `Msg` is expected are one error each.
- A `.` and a name start a `.V` only where an operand starts; after an
  expression, `x.V` is a field access. A statement that starts with a
  block-like expression ends with it, so a `.Nop` on the line after
  `while c {}` starts a new expression.
- `E::V` keeps its meaning, and is how a variant is written where no type
  is expected. Diagnostics show the variants of declared enums as `E::V`,
  `E` being the enum's path (`uart::Mode::Run` for an enum of module
  `uart`), and those of `Option` and `Result`, which have no path, as `.V`.
- `Mode` alone is not an expression; like a struct's name alone, it is
  `resolve::unknown-name`.

**Operations on an enum without payloads.**

- `==` and `!=` compare two values of one enum type by their integers.
  Ordering (`<`, `<=`, `>`, `>=`), arithmetic, bitwise and shift operators,
  unary `-` and `!`, and the compound assignments are
  `ty::invalid-operand`: they go through the integer, as in
  `m as u8 < 3`. `&&` and `||` need `bool` operands
  (`ty::mismatched-types`), as for any other type that is not `bool`.
- A literal where an enum is expected, as in `m == 3` or `take(2)`, is
  `ty::mismatched-types`: write a variant, or compare the integer,
  `m as u8 == 3`.
- **`as` converts an enum to any integer type**, as its integer type would
  convert: with `enum Level: i8 { Low = -1 }`, `Level::Low as u32` is
  `0xFFFF_FFFF`. An enum converts neither to `bool` nor to a pointer.
- **Nothing converts to an enum** (`ty::invalid-cast`): not an integer,
  not another enum, not the enum itself. `3 as Mode` reports only this
  error. An integer becomes an enum through a `match`, with arms such as
  `3 => Mode::Run`.
- An enum has no fields: `m.f` and `offset_of(Mode, f)` are
  `ty::no-fields`.

**Operations on an enum with payloads.** It is an aggregate, as a struct
is:

- `let`, assignment and reading a place copy the whole value.
- `==`, `!=`, ordering, arithmetic, bitwise and shift operators, unary
  `-` and `!`, and the compound assignments are `ty::invalid-operand`;
  tell its variants apart with a `match`. `&&` and `||` need `bool`
  operands (`ty::mismatched-types`).
- `as` converts it to nothing, and nothing to it (`ty::invalid-cast`): it
  is not an integer, and its variant is read with a `match`.
- It has no fields: `m.f` and `offset_of(Msg, f)` are `ty::no-fields`.
  Payload values are reached only by patterns (see "`match`").

Enums of both kinds are valid types of constants, statics, fields, array
elements, payload values, parameters and return values.

**Layout and calls of an enum without payloads.** An enum without
payloads is stored as its integer type: it has that type's size and
alignment, and its value is that integer (`size_of(Mode)` is 1). Its C
twin is that integer type, or C23's `enum Mode : uint8_t { ... }`, as a
field, a parameter or a return value. Across calls it is a scalar, not an
aggregate: passed and returned as its integer type, extended by that
type's signedness (ADR 0006). Through a volatile pointer it is a scalar
too (see "Volatile accesses"). In constants and statics its value is
emitted as its integer, so a `static mut` holding a variant whose value is
0 goes to `.bss`.

**Layout of an enum with payloads.** Each variant is laid out as a struct
whose first field is the **tag**, of the enum's integer type, holding the
variant's value, followed by the payload values in order; a variant
without payload is a struct holding only the tag. The enum's alignment is
the largest alignment of these structs, and its size the largest of their
sizes, rounded up to a multiple of that alignment:

```
enum Msg: u8 { Nop = 0, Led(bool) = 1, Write(u8, u16) = 2, Wide(u32) = 3 }
// Nop:   tag at 0                       size 1, alignment 1
// Led:   tag at 0, bool at 1            size 2, alignment 1
// Write: tag at 0, u8 at 1, u16 at 2    size 4, alignment 2
// Wide:  tag at 0, u32 at 4             size 8, alignment 4
// Msg:   size 8, alignment 4
```

- The tag is at offset 0 in every variant. A payload value never shares
  bytes with it, and starts as close after it as its alignment allows.
- The bytes outside the tag and the payload of the variant held,
  including padding, are unspecified, as padding is: building or writing
  a variant may leave anything there. In constants and statics they are
  zero, so a `static mut` holding a variant without payload whose value is
  0 goes to `.bss`.
- There are no niches: the tag is always stored, whatever the payload
  types.
- A payload type with `#[align(N)]` raises the enum's alignment, as it
  raises a struct's, and such an enum cannot be a field of a packed struct
  (`ty::aligned-in-packed`).
- `size_of` and `align_of` give the size and alignment above.

Its C twin is a union of structs that each start with the tag, which
`tests/c_layout.rs` checks against clang (Rust's `#[repr(u8)]` on an enum
with fields gives the same layout):

```c
union Msg {
    uint8_t tag;
    struct { uint8_t tag; bool on; } led;
    struct { uint8_t tag; uint8_t reg; uint16_t value; } write;
    struct { uint8_t tag; uint32_t value; } wide;
};
```

Zero-sized payload types have no C equivalent; a C twin leaves out those
of alignment 1, which change no offset. A zero-sized payload of larger
alignment, as in `B(u8, [u32; 0])`, raises the variant's alignment and
size, and such an enum has no exact C twin.

**Calls and volatile accesses of an enum with payloads.** Across calls it
is an aggregate of its size and alignment (see "Aggregates by value"), as
its C union is: `Msg` goes in a register pair. So giving one variant a
payload changes the layout and the calling convention of the whole enum,
and its C twin from an integer type to a union. Through a volatile pointer
it is an aggregate too: reading or writing one whole is
`ty::volatile-aggregate`, and since it has no fields, a volatile pointer to
one reaches nothing (see "Volatile accesses").

**Values that are no variant (ADR 0036, 0039).** A value of an enum
without payloads is any value of its integer type; the variants name some
of them. A value of an enum with payloads is any bytes of its size: its
tag is any value of the integer type, and when the tag is a variant's
value, its payload values are read from their places as that variant lays
them out, each as a value of its type. When the tag is no variant's value,
the whole value is **no variant**, and nothing but its tag is ever read.
Since nothing converts to an enum, a program that only writes variants
never holds a value that is no variant, but memory written by other means
can: a pointer cast, a volatile read of a device register, an
`extern static`, the result of an `extern fn`, code outside the program.

- Reading, writing and copying such a value are defined, at run time and
  at compile time. For an enum without payloads, so are comparing it with
  `==` and `!=` and converting it with `as`, which behave as for its
  integer. Nothing is checked there.
- Only `match` tells the values apart: a variant pattern never matches a
  value that is no variant, at any position, and a `match` that no arm
  matches calls the panic function (see "`match`").
- The compiler never assumes that an enum holds a variant: as LLVM sees
  it, an enum, or a tag, ranges over its integer type's values (no range
  metadata on loads, no `assume`).

## `match` (ADR 0037, 0040 to 0044)

The examples of this section use these enums, not those of "Enums":

```
enum Mode: u8 { Off = 0, Run = 1 }

enum Msg: u8 {
    Nop = 0,
    Led(bool) = 1,
    Write(u8, u16) = 2,
    Wide(u32) = 3,
    SetMode(Mode) = 4,
    Clear(u8) = 5,
    Pair(u8, u8) = 6,
}
```

```
match state {
    State::Idle => start(),
    State::Run | State::Wait => {
        step();
    }
    State::Done => return,
}

let reply: u8 = match cmd {
    CMD_RESET => reset(),
    0x10 | 0x11 => read(cmd),
    b'q' => 0,
    let other: u8 => unknown(other),
};

match msg {
    Msg::Nop => 0,
    Msg::Led(true) => 1,
    Msg::Led(false) => 2,
    Msg::Write(0x10 | 0x11, let v: u16) => special(v),
    Msg::Write(let r: u8, 0) | Msg::Clear(let r: u8) => clear(r),
    Msg::Write(let r: u8, let v: u16) => write(r, v),
    Msg::SetMode(Mode::Run) => 4,
    _ => 5,
}
```

**Syntax.**

- `match scrutinee { arms }`, where an arm is `pattern => expression`. Arms
  are separated by commas, with an optional trailing comma.
- **An arm whose expression starts with a block-like expression** (`{ }`,
  `if`, `match`, `while`, `loop`) **ends with it**, as a statement does,
  and needs no comma. So after `0 => {}`, `-1 => {}` is the next arm, not
  a subtraction. To continue such an expression, put it in parentheses.
- As in the condition of `if`, a `{` after the scrutinee opens the arms,
  so a struct literal there needs parentheses.
- `match` is an expression and block-like: as a statement without `;` it
  must have type `()`.

**What can be matched.** The scrutinee is an integer, a `bool` or an enum,
with or without payloads. Any other type is `ty::match-type`: pointers,
slices, structs, arrays, `()` and `!`. The scrutinee has no expected type,
so `match 3 { ... }` is `ty::untyped-literal`.

**Patterns.** A pattern is checked against the type of its **position**:
the scrutinee, or a payload value of a variant that the pattern names at
an enclosing position, whose type that variant's declaration gives.

| Pattern | Matches |
|---------|---------|
| `_` | every value |
| `let name: T`, `let mut name: T` | every value, copied into a new variable |
| an integer literal, `-` and an integer literal | that value |
| a byte literal (`b'q'`) | that byte, at a `u8` position |
| `true`, `false` | that value |
| `Mode::Run` | that variant, without payload |
| `Msg::Write(p, q)` | that variant, with payload values that match `p` and `q` |
| `.Run`, `.Write(p, q)` | as `E::Run` and `E::Write(p, q)`, `E` being the position's enum (ADR 0044) |
| `NAME`, `uart::NAME` | the value of the constant the name or path names |
| `p \| q \| ...` | any of the alternatives |

- **Literals** take their position's type, as against any expected type:
  `300` at a `u8` is `ty::literal-out-of-range`, and `-1` at an unsigned
  type is `ty::negate-unsigned`. A byte literal is a `u8`. A literal
  against a type it does not have, such as `b'q'` against a `u32`, `0`
  against a `bool`, an enum or a struct, or `true` against an integer, is
  `ty::mismatched-types`.
- **A variant** must be of its position's enum (`ty::mismatched-types`
  otherwise), and is resolved as in an expression
  (`resolve::unknown-variant`, `resolve::not-an-enum`). Its payload
  patterns are checked against the types its own declaration gives, even
  when it does not fit its position.
- **A variant with a payload** (ADR 0040), `Msg::Write(p, q)`, matches a
  value whose tag is the value of `Msg::Write` and whose payload values
  match `p` and `q`, in order. Each is any pattern, including a variant
  of another enum with its own payload, or an or-pattern: patterns nest to
  any depth. A trailing comma is allowed. There must be a pattern for each
  value: `Msg::Write(_)`, `Msg::Write()` and a bare `Msg::Write` are
  `ty::wrong-payload-count`, and so are `Msg::Nop(_)`, `Msg::Nop()` and
  `Mode::Off()`, since a variant without payload is matched without
  parentheses. There is no `..`: `Msg::Write(_, _)` writes every
  position, so adding a value to a variant makes every pattern naming it
  an error.
- **`.Run` and `.Write(p, q)`** (ADR 0044) are variant patterns of **the
  enum that is the position's type**, and mean what `E::Run` and
  `E::Write(p, q)` mean there, for exhaustiveness, dead arms, bindings and
  code; they mix with `E::V` patterns, inside payloads and as
  alternatives. At a position whose type is not an enum, `.V` is
  `ty::mismatched-types` ("expected `u8`, found variant `.Run`"); with no
  such variant, `ty::unknown-variant`; at a position of erroneous type,
  nothing is reported. In each of these cases its payload patterns are
  checked against an erroneous type: a `let` among them binds its written
  type and reports nothing more, so `.Bad(let v: u32)` is one error. The
  number of patterns is checked as for `E::V`
  (`ty::wrong-payload-count`).
- **At a position whose type is a struct, an array, a pointer, a slice or
  `()`**, only `_` and `let` match. A literal or a variant there is
  `ty::mismatched-types`; a constant is one of the errors below.
- **A bare name is always a constant.** The leading `.` tells a `.V`
  pattern from it, so no name in a pattern changes meaning. It must name a
  `const` item; a
  name that names nothing is `resolve::unknown-name`, and one that names a
  local variable, a parameter or an item that is not a constant (a
  static, function, struct or enum) is `resolve::not-a-constant`; a module
  is `resolve::misused-module`. A local
  variable shadows a constant of the same name here as anywhere, so it is
  then `resolve::not-a-constant` too; to compare with the constant, write
  `let x: T => if x == NAME { ... }`.
- **A path** of several segments, such as `uart::LIMIT`, names a variant
  or a constant, as in an expression; one whose last segment names another
  item is `resolve::not-a-constant`, and one whose last segment names
  nothing `resolve::unknown-name`, or `resolve::unknown-variant` after an
  enum. A path followed by patterns in
  parentheses that names no variant, as `uart::LIMIT(p)`, is
  `resolve::unknown-variant`.
- **Only constants of an integer type, `bool` or an enum without payloads
  are patterns.** A constant of any other type, such as an enum with
  payloads, a struct or a pointer, is `ty::constant-pattern-type`,
  wherever it stands, and nothing else is reported about it; match an
  enum with payloads with its variant patterns. A constant of one of the
  allowed types must have its position's type (`ty::mismatched-types`
  otherwise), also at a struct, array, pointer, slice or `()` position: a
  `u8` constant where a struct is matched is `ty::mismatched-types`, not
  `ty::constant-pattern-type`. It matches its value, computed at compile
  time, whatever that value is: a constant of an enum type may hold a
  value that is no variant, and then matches exactly that value.
- **A new variable is always written with `let` and its type**, which must
  be its position's type, with no conversion (`ty::mismatched-types`);
  without the type, the pattern is `parser::let-without-type`. The variable
  holds a copy of the value at its position and, with `let mut`, may be
  assigned. It exists in its arm's expression only, where it shadows, as a
  value, any item or variable of the same name; it is not visible in the
  rest of the pattern: in `Msg::Pair(let X: u8, X)`, the second `X` is a
  constant named `X`. One name bound twice in one pattern, or in one
  alternative of an or-pattern, as in `Msg::Write(let a: u8, let a: u16)`,
  is `resolve::duplicate-binding`.
- **Or-patterns** list alternatives of any pattern, at any position:
  `Msg::Write(0x10 | 0x11, _)`. There are no parentheses around patterns.
- A **catch-all arm** is one whose pattern is `_`, a `let`, or an
  or-pattern with a `_` alternative.
- There are no range patterns (`b'0'..=b'9'`), guards (`pat if cond`),
  struct or array patterns, nor `..` in a payload; a range is written as a
  `let` arm with an `if`.

**Bindings in or-patterns (ADR 0041).** An or-pattern may bind variables,
in an alternative or inside one at any depth, when every alternative binds
the same names, each with the same type and mutability:

- Each alternative writes its own `let` with its type. Together they
  declare one variable per name, in the arm's expression, which holds a
  copy of the value at the position where the alternative that matched
  binds it. When several alternatives match, the first one, left to right,
  binds it: with `Msg::Pair(let a: u8, 7) | Msg::Pair(5, let a: u8)`,
  `Msg::Pair(5, 7)` gives `a` the value 5.
- A name that some alternatives bind and others do not, or that some bind
  with `let mut` and others with `let`, is `resolve::inconsistent-binding`,
  reported once per name and or-pattern, at the first alternative that
  binds it and the first that does not, or at the first `let` and the
  first whose mutability differs. So `let x: u8 | 3` is this error, and in
  `let x: u8 | let x: u8` the second alternative is dead
  (`ty::unreachable-pattern`).
- The first alternative's `let` gives the variable its type. Each later
  `let` of that name with another type is `ty::mismatched-types` at its
  type, and only that is reported about it, even when its type differs
  from its position's type too.
- The rule holds at every or-pattern: `Msg::Write(let r: u8, 0 | 1)` is
  fine, since the inner alternatives bind nothing, and
  `Msg::Write(let r: u8 | 3, _)` is `resolve::inconsistent-binding`. A
  name that an inner or-pattern binds inconsistently is reported there,
  and counts as bound by the alternative that contains it, so the outer
  or-pattern reports nothing more about it.

**Choosing an arm.** The scrutinee runs **once, before any arm**, and is
read where it is written: through a volatile pointer,
`match (*uart).status` is exactly one volatile read. The arms are tried in
order; the first whose pattern matches runs, and its value is the value of
the `match`. Patterns run no code: constants are already values, and every
pattern, at any depth, tests the one value the scrutinee gave, so the
order in which it tests its positions cannot be observed. A `match` that
no arm matches calls the panic function. Since every `match` is
exhaustive, only a value that is no variant can cause it, at a position
that an arm examines with a variant pattern: a tag, or a payload value of
an enum type (ADR 0036, 0040). A position matched only by `_` and `let`
is never examined: `Msg::SetMode(_)` matches a `Mode` that is no variant.
At compile time, a `match` that no arm matches is an `eval::panic` whose
cause is "enum value is no variant".

On an integer, a `bool` or an enum without payloads, a `match` compiles to
a switch on the scrutinee's integer (an LLVM `switch`), as a C `switch`
does. Its default is the catch-all arm, the call to the panic function on
an enum without one, and unreachable on a `bool` or an integer whose every
value is listed.

On an enum with payloads, a `match` tests its arms in order (ADR 0042).
It compiles to a switch on the tag of the scrutinee's copy, with a case
for each variant that an arm names at the top, as its pattern or as an
alternative of its or-pattern. Under each case is the **chain** of that
variant: in arm order, each top-level alternative that is the variant,
and then the first catch-all arm, or, without one, the call to the panic
function. The default of the tag switch goes to the first catch-all arm,
or else to the call to the panic function: the variants no arm names and
the tags that are no variant reach it. With `Msg::Write(1, _) => 0,
Msg::Led(true) | _ => 1`, the chain of `Msg::Write` is `Msg::Write(1, _)`
and then the second arm, that of `Msg::Led` is `Msg::Led(true)` and then
the second arm, and the default is the second arm.

Each alternative of a chain is tested in turn; the first that passes runs
its arm, and one that fails goes to the next alternative of the chain. An
alternative is tested by its payload positions, in the order they are
written, depth first:

- `_` and `let` test nothing;
- a literal or a constant is a switch on its position with one case, and
  a variant a switch with one case on its position's integer or tag,
  followed by the tests of its own payload positions;
- an or-pattern tests its alternatives in order: the first that passes
  goes on to the positions after the or-pattern, and one that fails goes
  to the next alternative; when the last fails, the pattern that contains
  the or-pattern fails. Alternatives written next to each other that are
  literals, constants or variants written without a payload are one switch
  with one case per value, as in `0x10 | 0x11` or `Mode::Off | Mode::Run`.

A test that fails never goes back into an earlier or-pattern: the
positions of a pattern are disjoint, and patterns read no memory, so
which of its alternatives passed does not change whether a later
position matches. A test at an enum position fails on a
value that is no variant, as on any variant it does not name. A chain
without a catch-all arm ends in the call to the panic function, even at a
variant whose arms test only `bool` and integer positions, which no value
reaches; nothing tells LLVM that a tag or a payload value holds a variant.

Each pattern written is tested in one place: the positions after an
or-pattern are generated once, every alternative that passes jumping to
them, and the arm's expression is generated once. The code of a `match`
is thus linear in its patterns, plus one jump per variant to its first
catch-all arm or to a panic call. An arm may test again a position that
an earlier arm of its chain tested, as
`Msg::Write(0x10 | 0x11, let v: u16)` after `Msg::Write(0x10, 7)`: at
`-O0`, a `match` costs what a chain of C `if`s costs, and `-O` merges
much of it.

A `let` copies its position into its variable where its position is
reached, before the tests after it, and a catch-all `let` arm copies the
scrutinee's copy into its variable where the arm is entered, at one place
for every chain and the tag default. A copy made for an alternative that
then fails is never read: in
`Msg::Write(let r: u8, 0) | Msg::Clear(let r: u8)`, `Msg::Write(5, 1)`
copies 5 into `r` before the `0` fails, and the arm's expression runs
only when a whole alternative passed, which copied every variable it
binds on its way. So when several alternatives match, the first one, left
to right, binds, since it is tested first.

**Typing.** As for `if`: with an expected type, each arm's expression is
checked against it, with the implicit conversions of "Typing"; without
one, the first arm whose type is not `!` gives the type and the others are
checked against it. Arms of type `!` fit any type, and a `match` whose arms
all have type `!` has type `!`.

**Exhaustiveness.** Every `match` is exhaustive: its arms together match
every value of the scrutinee's type, or the `match` is
`ty::non-exhaustive-match`, which lists what is missing. At every
position, the values of an enum are its variants, each with every value
of its payload, and not the values that are no variant; a `bool` needs
`true` and `false`, an integer every value of its type. At the top, an
integer in practice needs a catch-all arm, though listing all 256 values
of a `u8` or `i8` counts too, and its message asks for a `_` or `let` arm.

Otherwise the missing values are listed as patterns, **witnesses**. They
are found by examining positions in the order they are written: the
scrutinee first, and a variant's payload positions, left to right, in its
place. At each position, the arms still considered are those that match
what the witness has so far:

- if every one of them has `_` or `let` there, or none remains, the
  witness has `_` there; but a `match` without arms lists every variant of
  an enum scrutinee, and `true` and `false` for a `bool` one;
- at an enum position, each variant is examined in turn, in declaration
  order, the arms considered for it being those whose pattern there is
  that variant, a constant holding its value, `_` or `let`; a variant no
  arm is left for is written with `_` at each of its payload positions
  (`Msg::Wide(_)`);
- at a `bool` position, `true` and then `false`;
- at an integer position, each value the arms name, in the order they
  first name it, and then one value they do not name, if one exists: the
  one closest to zero, the non-negative one first.

A complete witness that some arm still matches is dropped. The search
stops at the fourth witness (ADR 0043): the first three are shown, in the
order found, followed by "and more" when a fourth exists; three or fewer
are all listed, joined with "and". With the arms `Msg::Nop`,
`Msg::Led(true)` and `Msg::Write(0, 5)`, the missing values are
`Msg::Led(false)`, `Msg::Write(0, 0)`, `Msg::Write(1, _)` and more, and a
`match` without arms on `Msg` lists `Msg::Nop`, `Msg::Led(_)`,
`Msg::Write(_, _)` and more. The message does not say how many more:
counting them would mean finding them all, and their number can grow
exponentially with the patterns. A witness writes the variants of declared
enums as `E::V`, `E` being the enum's path, whichever way the arms write
them, and those of `Option` and `Result` as `.V`: `.None`, `.Err(_)`,
`.Some(Mode::Run)`, `uart::Mode::Run`. When a shown witness names a
variant of an enum that is not reachable from the module of the `match`,
the help says to cover such values with `_` (ADR 0054, see
"Visibility").

**Dead arms.** An arm that matches no value the arms before it leave is
`ty::unreachable-pattern`, reported once for the arm, pointing at the first
earlier arm that covers it alone, if there is one. When only some
alternatives of an arm are dead, at any depth, each of those is reported:
in `Msg::Led(true | true)`, the second `true`. Examples: `0` twice;
`Mode::Run` after a constant whose value is `Mode::Run`; anything after a
catch-all; `_ | 3`; a `_` after `true` and `false`. For an enum, at every
position, the values are all those of its integer type, so a `_` after
every variant is not dead while values that are no variant remain, and
neither is `Msg::SetMode(_)` after `Msg::SetMode(Mode::Off)` and
`Msg::SetMode(Mode::Run)`.

Both checks examine once the values that the arms do not tell apart, so
or-patterns at many positions cost what they write. Deciding
exhaustiveness can still take time exponential in the patterns in the
worst case: arms over many `bool` positions can state any boolean
formula, and asking whether they cover every value asks whether it
always holds. No limit is imposed (ADR 0043).

**When the checks run.** Exhaustiveness and dead arms are part of type
checking: their errors appear with `--emit=typeck`, and a function with
one of them does not type check, so it is not lowered to MIR and the
checks of "Checks on values known at compile time" do not run on it. They
use the values of the constants and variants in the patterns, computed at
compile time. A `match` **needs the values of an enum** `E` (ADR 0040,
0044) when some alternative, at a position whose type is `E`, is a variant
pattern (`E::V` or `.V`) or a constant pattern, whatever its own type, or
when it has no arms and its scrutinee is of type `E`; it then uses the
value of every variant of `E`. The kind of a pattern counts, not whether
it fits: a constant that is `ty::constant-pattern-type` counts, and the
payload positions of a variant pattern have the types its own declaration
gives, so `H::X(Mode::Run)` where a `G` is matched needs the values of `G`
and of `Mode`, and is `ty::mismatched-types`. The payload positions of a
`.V` pattern have the types that the variant `V` of its position's enum
gives them; when its position is no enum, or that enum has no variant
`V`, they have no type, as have the extra values of a variant pattern
with too many. A position with no type needs nothing: a constant or a
`.V` there needs no enum, and an `E::V` there needs `E` and gives its own
payload positions their declared types. Positions matched only by `_` and
`let` need no value.

- A constant that fails to evaluate is reported once, and a `match` using
  it as a pattern is not checked for exhaustiveness or dead arms, so
  nothing more is reported. A constant whose evaluation needs the function
  whose `match` uses it as a pattern is a `ty::cycle`.
- A `match` that needs the values of an enum one of whose values failed to
  evaluate or repeats another is not checked for exhaustiveness or dead
  arms either: only the enum's errors are reported, and the build stops at
  them. The function still type checks, but is not lowered to MIR, with or
  without a catch-all arm: a variant may lack an arm at some position, and
  the `match` would panic on it as if it were no variant. An evaluation
  that needs the function fails without a diagnostic of its own, as for a
  body whose checks failed. A `match` that needs no value of such an enum
  is checked as usual: with `enum T: u8 { A = 0, B = 0 }`, the second `_`
  of `match t { _ => 1, _ => 2 }` is `ty::unreachable-pattern`, and so is
  the second `H::R(_)` of `match h { H::R(_) => 1, H::R(_) => 2 }` with
  `enum H: u8 { R(T) = 0 }`.
- A `match` that needs the values of an enum is a `ty::cycle` in a
  function that runs while one of them is computed, as `f` in
  `enum E: u8 { A = f(E::B), B = 1 }` when `f` matches an `E` against
  `E::B`, even with a `_` arm, or a value holding an `E` against
  `Holder::Of(E::B)`. One that needs no value adds no such dependence:
  with `fn f(e: E) -> u8 { match e { _ => 5 } }`, `E::A` is 5. Its
  scrutinee and arms are uses like any other, so `match E::A { _ => 5 }`
  there is still a `ty::cycle`.

## `Option` and `Result` (ADR 0045)

```
enum ParseError: u8 { Empty = 0, BadDigit(u8) = 1, Overflow = 2 }

fn checked_mul10(v: u32) -> Option(u32) {
    if v > 429_496_729 { return .None; }
    .Some(v * 10)
}

fn digit(c: u8) -> Result(u32, ParseError) {
    if c < b'0' || c > b'9' {
        return .Err(.BadDigit(c));
    }
    .Ok((c - b'0') as u32)
}

match digit(c) {
    .Ok(let v: u32) => v,
    .Err(.BadDigit(let b: u8)) => bad(b),
    .Err(_) => 0,
}
```

**The types.** `Option(T)` and `Result(T, E)` are built-in enums with
payloads, as if declared

```
enum Option(T): u8 { None = 0, Some(T) = 1 }
enum Result(T, E): u8 { Ok(T) = 0, Err(E) = 1 }
```

(not valid syntax: an enum with parameters needs M6). Everything "Enums"
and "`match`" say of an enum with payloads holds for them: their layout
and how they cross calls, that they are aggregates (no `==`, `as`, fields
or whole volatile access), that they may be constants, statics, fields,
array elements, payload values, parameters and return values, that no
value of them is known at compile time, that a tag other than 0 and 1 is
no variant, and how a `match` reads them.

**Writing them.**

- `Option(T)` and `Result(T, E)` are written wherever a type is: `let`,
  parameters, return types, fields, array elements, pointees
  (`*const Option(u32)`), constants, statics, payloads, layout queries,
  casts, `extern fn` signatures, `let` patterns, and as type arguments
  themselves (`Option(Option(u8))`). The type arguments are separated by
  commas, with an optional trailing comma.
- `Option` takes one type argument and `Result` two. Any other count, as
  in `Option`, `Option()`, `Option(u8, u8)` and `Result(u32)`, is
  `resolve::type-argument-count`, which says how many it takes; so are
  parentheses after any other type, which takes none: `u32(u8)`,
  `Point(u8)`, `u32()`.
- A **type argument** is any type a payload value may have (see "Enums"):
  integers, `bool`, pointers, slices, arrays, structs, enums of either
  kind, and zero-sized types (`Result((), ParseError)`). `!` is not
  (`ty::never-in-memory`), so there is no infallible `Result(u32, !)`
  until M6, nor is `[T]` (`ty::unsized`). A type argument in error, such
  as these or an unknown name, is reported once, also where nothing uses
  the type, and the whole type then counts as erroneous: its uses report
  nothing more because of it. One with a type argument that reaches a
  failed array length or alignment, as `Option(*const Broken)`, counts as
  erroneous too, although type errors in its uses are still reported (see
  "Failed lengths and alignments").
- **One type per set of arguments**: two `Option(T)` are the same type
  when their arguments are, compared once array lengths are computed, so
  `Option([u8; 4])` and `Option([u8; 2 + 2])` are one type, while
  `Option(u32)` and `Option(u16)` are two (`ty::mismatched-types`).
  Diagnostics write them with their arguments and computed lengths:
  `Result([u8; 4], ParseError)`.
- A type that contains itself through a type argument, as
  `struct Node { next: Option(Node) }`, is `ty::infinite-size`, labelled
  at the first declared struct or enum of the cycle, the `Option(Node)`
  shown in the path; `Option(*const Node)` is the way to write it. Having
  no declaration, an `Option(T)` or `Result(T, E)` larger than half the
  address space is `ty::too-large` at each place it is written, as an
  array type is, when every argument has a layout; one whose argument has
  none, too large itself or in a cycle, reports nothing more.
- `Option` and `Result` are names of built-in types, which no item may
  have (`resolve::duplicate-type`, see "Types").

**Their variants.** `.None`, `.Some(v)`, `.Ok(v)` and `.Err(e)` are
named by the expected type (ADR 0044, see "Enums"), in expressions and in
patterns. They have no path: `Option::None`, in an expression or a
pattern, is `resolve::not-an-enum`, whose help shows `.None`;
`Option(u32)::None` parses as a call followed by `::`, and is
`parser::unexpected-token`, with the same help when its arguments parse
as expressions. Diagnostics show these variants as `.V`, witnesses
included: `.None`, `.Err(_)`, `.Some(Mode::Run)`.

**Layout and calls.** A `u8` tag at offset 0, then the payload value as
close after it as its alignment allows, as for any enum with payloads:

| Type | Size | Alignment | Payload at |
|------|------|-----------|------------|
| `Option(u8)` | 2 | 1 | 1 |
| `Option(u32)`, `Option(*mut T)` | 8 | 4 | 4 |
| `Option(())` | 1 | 1 | 1, of size 0 |
| `Option([u32; 0])` | 4 | 4 | 4, of size 0 |
| `Option(u64)` | 16 | 8 | 8 |
| `Result(u32, ParseError)`, `ParseError` on `u8` without payloads | 8 | 4 | `Ok` 4, `Err` 1 |

There are no niches: address 0 is valid (ADR 0019), so `Option(*mut T)`
takes 8 bytes. Their C twins are unions of structs that each start with
the tag, which `tests/c_layout.rs` checks against clang:

```c
union OptionU32 {
    uint8_t tag;                                  /* 0: None */
    struct { uint8_t tag; uint32_t value; } some; /* 1: Some */
};
```

They cross calls as aggregates (see "Aggregates by value"): `Option(u8)`
in one register, `Option(u32)` and `Result(u32, ParseError)` in a
register pair, `Option(u64)` in memory.

`--emit=typeck` and `--emit=mir` list each `Option(T)` and `Result(T, E)`
the program uses as an enum, after the declared enums, sorted by their
displayed name.

**In M6** they become comptime functions in `core` returning enums with
these variants, values, tag type and order. Their layout, calling
convention and C twins do not change, and neither does any program.

## The `?` operator (ADR 0046)

```
fn shift_in(acc: u32, d: u32) -> Option(u32) {
    checked_add(checked_mul10(acc)?, d)
}

fn two_digits(a: u8, b: u8) -> Result(u32, ParseError) {
    .Ok(digit(a)? * 10 + digit(b)?)
}
```

- **Syntax.** `e?` is a postfix operator, binding as tightly as calls,
  indexing and field access: `f()?.x` is `(f()?).x`, `-f()?` is
  `-(f()?)`, `*p?` is `*(p?)`, and `f()??` applies it twice. A statement
  that starts with a block-like expression ends with it, so
  `match r { ... }?` at the start of a statement is
  `parser::unexpected-token`; write `(match r { ... })?`. A type ends at
  its last token, so `x as T?` is `parser::unexpected-token` too.
- **Types.** The operand is typed on its own, with no expected type, and
  must be an `Option(T)` or a `Result(T, E)`: anything else, a declared
  enum with payloads included, is `ty::try-operand`, and a `.V` operand
  is `ty::untyped-variant`. `e?` has type `T`.
- The enclosing function must return an `Option(U)` for an `Option`
  operand, and a `Result(U, E)` with **the same `E`** for a `Result`
  operand, `U` being any type. Same means identical: none of the implicit
  conversions of "Typing" applies, so a `Result(T, *mut u8)` operand in a
  function returning `Result(U, *const u8)` is an error. Otherwise,
  including in a function returning `()`, `!` or a declared type, it is
  `ty::try-return`, which shows the operand's type and the return type;
  for another `E` it says that errors are never converted (ADR 0008): a
  function that mixes error types converts them itself, with a `match`.
- An operand of type `!`, as `(loop {})?`, gives `e?` the type `!`, and an
  erroneous operand the erroneous type; neither reports anything, and the
  return type is not checked. An erroneous return type, or one whose type
  arguments are in error, reports nothing more either.
- **Meaning.** `e?` runs `e` once, where it is written, then reads its
  tag. On `Some` or `Ok`, its value is a copy of the payload. On `None`,
  the function returns `.None`; on `Err`, it returns `.Err(x)`, `x` being
  a copy of the payload. On a tag that is no variant, it calls the panic
  function, as a `match` without a catch-all arm does; at compile time,
  that is an `eval::panic` whose cause is "enum value is no variant". So
  `e?` behaves as

  ```
  match e {
      .Ok(let v: T) => v,
      .Err(let x: E) => return .Err(x),
  }
  ```

  with `.Some` and `.None` for an option. Since the function returns
  there, in `f(a()?, b())`, when `a()` fails, `b()` does not run.
- **At compile time.** `?` returns from the enclosing function, so it
  cannot appear where `return` cannot (see "Compile-time execution"): in a
  `comptime` expression or block, the initializer of a constant or a
  static, a variant's value, or a value required at compile time (an
  array length, a repeat count, the argument of `#[align(N)]`), it is
  `resolve::return-in-comptime`. A function using `?` may be called at
  compile time like any other.
- **Code.** A switch on the operand's tag with two cases, whose default
  calls the panic function; as for a `match`, nothing tells LLVM that the
  tag holds a variant.

## Layout (ADR 0022, 0039)

| Type | Size | Alignment |
|------|------|-----------|
| `u8 i8 bool` | 1 | 1 |
| `u16 i16` | 2 | 2 |
| `u32 i32 usize isize`, thin pointers | 4 | 4 |
| `u64 i64` | 8 | 8 |
| slices (`*const [T]`, `*mut [T]`) | 8 | 4 |
| `[T; N]` | `N * size(T)` | `align(T)` |
| an enum without payloads on the integer type `T` (ADR 0035) | `size(T)` | `align(T)` |
| an enum with payloads (ADR 0039), `Option(T)` and `Result(T, E)` (ADR 0045) | the largest variant's size, rounded up to the alignment | the largest variant's alignment |
| `()` | 0 | 1 |

`[T]` and `!` have no layout a program can ask for (see "Types"). Each
variant of an enum with payloads is laid out as a struct of its tag and its
payload values (see "Enums").

A struct's fields are laid out **in declaration order**, never reordered:
each at the next offset that is a multiple of its alignment. The struct's
alignment is its largest field alignment (1 with no fields), and its size
is rounded up to a multiple of it. This matches the target's C ABI for the
types C has; `tests/c_layout.rs` checks it against clang. Padding bytes have
unspecified values.

- `size_of(T)`, `align_of(T)` and `offset_of(T, field)` are `usize`
  constants; their argument is a type.
- `#[packed]` before a struct removes padding and gives it alignment 1.
- `#[align(N)]` before a struct raises its alignment to `N`, a power of two
  not below the natural alignment, and rounds its size up to `N`. An `N`
  that fails to evaluate or to type check, or is `ty::invalid-align`, is
  reported once, and the struct then counts as erroneous, as does every
  type that reaches it (see "Failed lengths and alignments"). Before a
  static, it raises the static's alignment by the same rules, without
  changing its size; there, an `N` in error leaves the type's alignment
  (see "Statics").
- `#[packed]` and `#[align(N)]` cannot be combined, and a packed struct
  cannot contain a type with `#[align(N)]`, at any depth: through fields,
  array elements or payload values.
- Every object is placed at an address aligned for its type, including
  over-aligned locals, except inside a packed struct.
- A place reached through a field of a packed struct, at any depth, may be
  misaligned. It is read and written with misaligned accesses; taking its
  address with `&raw` is an error when its type needs alignment.
- Zero-sized types (`()`, `struct Empty {}`, `[T; 0]`) occupy no memory.
  Their addresses are aligned but otherwise unspecified, and may coincide
  with other objects'.
- A `bool` in memory is a byte: storing writes 0 or 1, and any nonzero byte
  reads as `true`.

## Compile-time execution (ADR 0027, 0029, 0030)

The compiler runs ordinary Erebos code wherever a value must be known
before the program runs. Any function can be called there; whether it can
run is found out by running it.

```
const BAUD: u32 = 115_200;
const DIVISOR: u32 = clock_hz() / (16 * BAUD);
const UART: *mut volatile Uart = 0x1000_0000 as *mut volatile Uart;
const CRC_TABLE: [u32; 256] = crc_table(0xEDB8_8320);

comptime {
    assert(size_of(Header) == 7);
}

fn f() -> u32 {
    let d: u32 = comptime baud_divisor(48_000_000, BAUD);
    d
}
```

- **`const` items** have an explicit type and any initializer. A constant
  is a value, not a place: it has no address (`&raw const` of it, or of
  any element or field of it, is an error) and cannot be assigned. Using it
  whole copies it; indexing it or reading a field reads only that part,
  straight from read-only data. Every constant is evaluated, used or not.
  `const` inside a function is an error; use `let x: T = comptime ...;`.
- **The initializers of statics** run at compile time too (see "Statics").
- **`comptime expr`** evaluates `expr` at compile time. Like `return`, it
  covers the whole expression to its right: `comptime f(1) + y` is
  `comptime (f(1) + y)`; write `(comptime f(1)) + y` to limit it. It passes
  the expected type through. Its value is a value like the call it
  replaces, not a place.
- **`comptime { ... }`** items run their block when the program is
  compiled, typically for checks with `assert`.
- **`assert(cond)`** and **`assert(cond, msg)`**, built in: at run time a
  false condition calls the panic function; at compile time it fails the
  compilation (`eval::assertion-failed`). See "Messages in `assert`".
- **Values required at compile time:** array lengths, repeat counts
  (`[v; N]`), the argument of `#[align(N)]` and the values of a `memory`
  block (see "Memory maps") are `usize` expressions, such as
  `[u8; 2 * SECTOR]`. Two array types are the same when their
  lengths have the same value; diagnostics show the value.

What runs at compile time follows the target exactly (32-bit `usize`,
wrapping arithmetic, the layouts above), and cannot:

- use the enclosing function's variables (`ty::not-comptime`), nor `return`
  or `?` out of it (`resolve::return-in-comptime`);
- access memory through a volatile pointer, call an `extern fn`, or read or
  write through a pointer made from an integer (computing an address from
  one, as in `&raw mut (*UART).ctrl`, is fine);
- read or write a `static mut` (`eval::static-mut`), whose value is only
  known at run time, or an `extern static` (`eval::extern-static`), defined
  outside the program; taking the address of either is fine;
- write a `static` or a byte string (`eval::read-only-write`);
- convert a pointer into an object to an integer, or read its bytes as
  one, since its address exists only after linking, or copy or overwrite
  only some of a pointer's bytes (`eval::pointer-to-int`);
- compare pointers whose run-time answer is not known (see "Comparing
  pointers" below);
- compare, or access memory through, a pointer to a variable of a call
  that has returned (`eval::dangling-pointer`); copying it is fine;
- read bytes never written, such as the padding of a variable, or the
  bytes of an enum with payloads outside its variant's tag and payload
  (in a `static` or a constant those read as zero, as in the object
  file);
- call the panic function, explicitly or through a failed check; the panic
  function's body never runs at compile time;
- run more than 10,000,000 steps (MIR statements and terminators) or nest
  more than 256 calls. Each evaluation has its own budget.

A value that needs itself, directly or through other items, is a
`ty::cycle`; reading a `static` asks for its value, but taking its address
does not. Data the compiler emits has zero padding, so builds are
reproducible byte for byte.

### Failed lengths and alignments (ADR 0048)

```
const N: usize = 1 / 0;
struct Broken { x: [u8; N], y: u32 }

#[align(3)]
struct Odd { x: u32 }

enum E: u8 { A(*const Broken) = 0, B = 1 }
```

A **failed value** is an array length, or the argument of `#[align(N)]`
before a struct, that is reported in error, whatever the error: a failed
evaluation (a division by zero, a failed `assert`, the step limit, a
`ty::cycle`), a type error (`[u8; true]`, a length of type `u32`), or, for
an alignment, `ty::invalid-align` (not a power of two, as `#[align(3)]`,
too large, or below the struct's natural alignment, as `#[align(2)]`
would be on `Odd`). The error is reported once, where the value is
written.

A type **reaches** a failed value when it is, or contains at any depth,
an array whose length failed or a struct whose alignment failed: as a
field, an array element, a payload value, a type argument, or behind a
pointer or a slice. `[u8; N]`, `Broken`, `Odd`, `*const Broken`,
`*const [Odd]` and `Option(*const Broken)` all reach one. Such a type
counts as erroneous, as one containing `!` does (see "Types"):

- **Type errors are still reported.** Code using the type is type checked
  to the end, and reports its own errors: with `p: *const Broken`,
  `let q: *const Broken = 0 as *const u8;` is `ty::mismatched-types` and
  `(*p).z` is `ty::unknown-field`. Only what depends on the type's
  variants or layout reports nothing: a `match` on an enum that reaches a
  failed value is not checked for exhaustiveness or dead arms, and its
  payload patterns are checked against an erroneous type, so neither
  `match e { E::B => 0 }` nor a pattern `E::A(let v: u32)` is an error
  (see "`match`"); `?` with such an operand or return type checks nothing
  (see "The `?` operator"). A type that is itself in error, such as
  `[u8; N]`, reports nothing anywhere: `let a: [u8; N] = 5;` is no error.
- **Layout queries have no value.** `size_of`, `align_of` and `offset_of`
  of such a type have none, `size_of(*const Broken)` included, although a
  pointer's size does not depend on its pointee. What is computed from
  them is not evaluated and reports nothing: a constant, an `assert`, a
  variant's value, and another length or alignment, which then fails in
  turn, so that `struct V { a: [u8; size_of(Broken)] }` and
  `#[align(align_of(Odd))] struct U { x: u8 }` reach a failed value too,
  without an error of their own.
- **Code reaching it is not built.** A body (a function, the initializer
  of a constant or a static, a `comptime` expression or block) is not
  lowered to MIR when its return type, a parameter or local, the type of
  one of its expressions, or the operand of a `size_of`, `align_of` or
  `offset_of` in it reaches a failed value. It is then neither run at
  compile time nor checked for the values known at compile time (see
  "Checks on values known at compile time"): `7 / 0` in a function taking
  a `*const Broken` reports nothing, nor does a constant that calls it.
  Such errors appear once the failed value is fixed.
- **Errors that hold whatever the value are kept.** A type containing one
  that reaches a failed value is still checked, with a failed length
  counted as 0 and a failed alignment as the struct's natural one, the
  least either can be, so that each error found would be one whatever the
  value: a `#[packed]` struct containing a type with `#[align(N)]`, failed
  or not (`ty::aligned-in-packed`), an `#[align(N)]` below the alignment
  of a field that reaches a failed value (`ty::invalid-align`, as
  `#[align(2)] struct W { o: Odd }`), and a type too large
  (`ty::too-large`): an array, as `[Broken; 0x4000_0000]`, `Broken` being
  at least 4 bytes, and a struct, an enum or an `Option` or `Result` that
  grows too large from those least sizes. Each is reported where it would
  be for any other type.

The `#[align(N)]` of a static is no failed value: it is part of no type.
One in error is reported, and the static keeps the alignment of its type
(see "Statics").

Since whether a body is built depends on the lengths and alignments of
the types it mentions, behind pointers too, a length or alignment of `S`
computed by a function that takes a `*const S` is a `ty::cycle`, although
the layout of `S` does not depend on that function; this is revisited in
M6:

```
const N: usize = f(0 as *const S);
fn f(p: *const S) -> usize { 4 }
struct S { x: [u8; N] }
```

### Pointers in results (ADR 0030)

A result (a constant, the initial value of a static, a `comptime` value)
may hold pointers and slices into statics, `extern static`s and byte
strings. Their addresses exist only after linking: the data the compiler
emits holds them as relocations the linker fills in. So statics may point
at each other:

```
struct Node { next: *const Node, value: u32 }
static A: Node = Node { next: &raw const B, value: 1 };
static B: Node = Node { next: &raw const A, value: 2 };
```

- A pointer must point into its object or one past its end, and a slice
  must lie wholly inside its object; anything else is
  `eval::pointer-out-of-bounds`. An `extern static` is exempt: its declared
  size need not be its real one, so a pointer or slice into one may have
  any offset and length.
- A pointer into memory the evaluation itself created (its variables) is
  `eval::pointer-escapes`.

### Comparing pointers (ADR 0030)

A comparison at compile time gives the answer it would have at run time,
or is an error (`eval::pointer-compare`) where that answer is not known:

- Pointers into **the same object** compare by offset with `==` and `!=`.
  `<`, `<=`, `>` and `>=` are answered only when both offsets are between 0
  and the object's size, inclusive; outside, the address may have wrapped.
- Pointers into **different objects** are known to differ (`==` is false,
  `!=` true) only when both point strictly inside their objects, and
  neither object is a byte string or an `extern static`. Every other
  comparison between different objects is an error: any ordering, and `==`
  or `!=` when either pointer is one past the end of its object, either
  object has size zero, or either is a byte string or an `extern static`.
  At run time those may be equal: one object may start where another ends,
  byte strings may share memory, and two `extern static`s may name the
  same object.
- Pointers made from integers compare by address. Comparing one with a
  pointer into an object is an error.

### Messages in `assert` (ADR 0031, 0053)

```
comptime {
    assert(size_of(Header) == 7, &raw const b"Header must match the wire format");
}
```

- The message is a `*const [u8]`, or a pointer that converts to one (see
  "Slices").
- The message runs **only when the condition is false**, like the right
  operand of `||`: an exception to the left-to-right order of arguments.
- At compile time, `eval::assertion-failed` shows the message: printable
  ASCII as it is, other bytes as `\xNN`. A message that cannot be read
  reports that error instead.
- At run time, when the panic function takes a `*const core::PanicInfo`,
  the message reaches it as the `message` field (ADR 0053, see "Panic
  information"). When it takes no parameter, the message is still
  evaluated, and dropped, before it is called: what runs before the panic
  function does not depend on its signature.

## Expressions

### Operators, from tightest to loosest

| Precedence | Operators | Associativity |
|------------|-----------|---------------|
| 1 | calls `f(x)`, indexing `a[i]`, fields `s.f`, `?` (ADR 0046) | left |
| 2 | unary `-` `!` `*` (deref), `&raw const`, `&raw mut` | prefix |
| 3 | `as` | left |
| 4 | `*` `/` `%` | left |
| 5 | `+` `-` | left |
| 6 | `<<` `>>` | left |
| 7 | `&` | left |
| 8 | `^` | left |
| 9 | `\|` | left |
| 10 | `==` `!=` `<` `<=` `>` `>=` | cannot be chained |
| 11 | `&&` | left |
| 12 | `\|\|` | left |
| 13 | `=` `+=` `-=` `*=` `/=` `%=` `&=` `\|=` `^=` `<<=` `>>=` | right |
| 14 | `return`, `break` and `comptime` with a value | cover the whole expression to their right |

So `flags & MASK == 0` means `(flags & MASK) == 0`, `*p.x` means `*(p.x)`,
`-f()?` means `-(f()?)`, and `a < b < c` is an error.

### Typing

- Both operands of an arithmetic, bitwise or comparison operator have the
  same type; nothing converts implicitly (ADR 0008), with two exceptions:
  where a `*const T` is expected, a `*mut T` with the same `T` and
  volatility is accepted, since it only loses permission to write
  (ADR 0024); and where a slice is expected, a pointer to an array of its
  elements is accepted, with the array's length (ADR 0031, see "Slices").
  These apply wherever an expected type is known, such as a `let`, an
  argument (including an `assert` message), an assignment, `return`,
  `break` values, struct fields, payload values, array elements, `if`
  branches, `match` arms, the tail of a block and the operand of
  `comptime`. Only the
  outermost pointer converts, volatility never changes, and comparisons
  accept a `*mut T` and a `*const T` in either order.
- A literal takes the type expected where it appears; with no expected type
  it is an error. In `x + 1` the `1` takes the type of `x`. A literal
  operand of `as` takes the target type, or `usize` for a pointer target
  (ADR 0017). An array literal takes its element type from the expected
  type or from its first element with a type of its own.
- A variant named by the expected type, `.V` or `.V(args)`, takes its enum
  where a literal takes its type, from the same expected type: the
  initializer of a `let`, a constant or a static, the right side of an
  assignment, an argument, a payload value, a struct literal's field, an
  array element and a repeat operand, `return`'s value, the tail of a
  function or a block, `comptime`'s operand, the branches of an `if`,
  the arms of a `match` and the values of `break` where these have an
  expected type, and otherwise from a sibling: an
  earlier `if` branch, `match` arm or `break` of the same loop, another
  array element, the other operand of a binary operator. Unlike a literal,
  it takes nothing from `as` (ADR 0044, see "Enums").
- `+ - * / %` and unary `-` need integers; unary `-` needs a signed one.
- `& | ^ !` work on integers (bitwise) and `bool` (logical, without
  short-circuit).
- `&& ||` work on `bool` and short-circuit.
- `== !=` work on integers, thin pointers, `bool` and enums without
  payloads (two values of one enum type); `< <= > >=` on integers and thin
  pointers. Slices and enums with payloads cannot be compared, and enums
  without payloads only with `== !=` (ADR 0035, 0039).
- A literal is never an enum: `m == 3` is `ty::mismatched-types`.
- A value may be at most 2147483647 bytes, half the address space.

### Integer semantics (ADR 0007)

- `+ - *` and unary `-` wrap around, in every build.
- Shift amounts may have any integer type and are masked to the width of
  the shifted value (`x << n` is `x << (n % bits)`). An amount known at
  compile time that is out of range is an error (see below). `>>` is
  arithmetic on signed values.
- `/` and `%` truncate toward zero; the remainder has the sign of the
  dividend. A zero divisor calls the panic function (a divisor known at
  compile time to be zero is a compile-time error). For signed types,
  `MIN / -1` is `MIN` and `MIN % -1` is `0`.

### Checks on values known at compile time (ADR 0032, 0039)

An expression is **known at compile time** when it is built only from
literals, `const` items, variants of enums without payloads (ADR 0035),
written `E::V` or `.V` (ADR 0044),
`comptime` expressions, `size_of`, `align_of`, `offset_of`, unary `-` and
`!`, binary operators, `as` and parentheses. So `Mode::Run as u32` is
known; an `if` or a `match` is not. Aggregates are never known, and
neither is any value of an enum with payloads, not even a variant without
payload such as `Msg::Nop`, nor a constant or `comptime` value of its type
(ADR 0039): so every arm of a `match` on one is checked.
Nothing that reads memory is known: not `*p`, even through a constant
pointer (it may be a device register), not a read of a static, not
`&raw`. A pointer made from an integer is known by its address; a constant
pointing into a static is not known, since its address exists only after
linking.

These are errors when the operand is known, not only when it is a literal:

| Check | Code |
|-------|------|
| a shift amount outside 0 to the width of the shifted type minus 1, taken with its own type's sign (a negative amount is out of range) | `ty::shift-out-of-range` |
| a divisor of `/` or `%` that is zero | `ty::division-by-zero` |
| an array index not below the array's length | `ty::index-out-of-bounds` |

The compound assignments (`<<=`, `>>=`, `/=`, `%=`) are checked too. When
the operand is not a literal, the diagnostic shows its value
(`BITS is 32`). A known operand whose own computation fails, such as
`A / ZERO` in `x / (A / ZERO)`, reports that failure, once, and nothing
about the outer operation.

Code that cannot run under a condition known at compile time is not
checked by these rules (type checking still covers it):

- the branch an `if` never takes;
- the right operand of `&&` when the left one is known `false`, and of
  `||` when it is known `true`;
- the body of a `while` whose condition is known `false`;
- the message of an `assert` whose condition is known `true`;
- the arms a `match` whose scrutinee is known never takes (ADR 0037): it
  runs at most one arm, and only that one is checked. When no arm matches,
  which only an enum value that is no variant can cause, such as a
  constant of the enum's type holding one, no arm is checked, and the
  `match` panics where it runs. The scrutinee of an enum with payloads is
  never known, so this never applies to it.

```
const BITS: u32 = 32;
let mask: u32 = if BITS < 32 { (1 << BITS) - 1 } else { 0xFFFF_FFFF };
```

These checks run when a body is lowered to MIR, after type checking, for
every body that type checks (functions, and the initializers of constants
and statics, `comptime` expressions and blocks) and reaches no failed
array length or alignment (see "Failed lengths and alignments"), whatever
errors other bodies have; `--emit=typeck` stops before them. A body in
which they fail is not run at compile time: an evaluation that needs it
reports nothing more.

### Conversions with `as` (ADR 0019)

- Between integers: widening sign-extends a signed source and zero-extends
  an unsigned one; narrowing keeps the low bits; same width reinterprets.
- `bool` converts to integers (`true` is 1); nothing converts to `bool`
  (write `x != 0`).
- An enum without payloads converts to any integer type, as its integer
  type would (ADR 0035). Nothing converts to an enum, not even the enum
  itself: decode an integer with a `match`. An enum converts neither to
  `bool` nor to a pointer.
- An enum with payloads converts to nothing (ADR 0039): its variant is
  read with a `match`.
- Thin pointers convert only to and from `usize`; pointer-to-pointer casts
  may change mutability, volatility and pointee. Casts involving slices
  follow the table in "Slices".
- Structs and arrays do not convert.
- Nothing converts to `!`: no value of that type exists (ADR 0034).

### Pointers

- `*p` reads or, as an assignment target, writes through `p`. Writing needs
  `*mut`.
- A thin pointer is a plain address (`p as usize` is its address); a slice
  is an address and a length. Converting an integer to a pointer does not
  make it valid to dereference; alignment, existence and permissions are
  not checked (ADR 0019). Address 0 is an ordinary address.

### Volatile accesses (ADR 0013, 0023, 0039)

Through a volatile pointer, every access is to a scalar (an integer,
`bool`, enum without payloads or thin pointer; such an enum is accessed as
its integer type, ADR 0035) and is **exactly one load or store of the
scalar's width**, never split, merged, removed or reordered relative to
other volatile accesses. Volatility carries through fields and indexing:
`(*regs).chan[i].status` is one access of `status`'s width. `x += 1` is
one load and one store, not atomic. Otherwise the program is rejected:

- a whole struct, array or enum with payloads, or a slice stored in
  memory, cannot be accessed through a volatile pointer; through a
  volatile slice, `(*s)[i]` is one access of the element. An enum with
  payloads has no fields, so a volatile pointer to one reaches nothing;
  device registers are described with structs, integers and enums without
  payloads;
- `u64` and `i64`, and enums without payloads on them, cannot, since
  riscv32 has no 64-bit access: use two `u32` halves in the order the
  device needs; an enum, which nothing converts to, needs an integer type
  of at most 32 bits;
- a field reached through a packed struct cannot be, if its type needs
  alignment.

Volatile accesses are not ordered against ordinary ones, and no fences are
emitted: until M4, handing memory to a device (DMA) cannot be written with
a guarantee.

### Control flow

- `if cond { ... } else { ... }` is an expression; `cond` must be `bool`.
  Both branches have the same type; without `else`, the `if` has type `()`.
- `match value { pattern => expr, ... }` runs the first arm whose pattern
  matches; see "`match`".
- `while cond { ... }` has type `()`, even with a constant `true`.
- `loop { ... }` runs until `break`. Its type is that of its `break` values,
  or `!` if it has no `break`. `break value` is only allowed in `loop`.
- `continue` jumps to the next iteration; `break` and `continue` refer to
  the innermost loop.
- `return` or `return value` leaves the function.
- `break`, `continue` and `return` have type `!`.
- `e?` gives the value of an `Option` or a `Result`, or returns its
  `.None` or `.Err(x)` from the function; see "The `?` operator".

### Evaluation order (ADR 0018)

Operands, arguments, payload values, array elements and struct literal
fields run left to right, in the order written; `&&`/`||` may skip their
right side, and `assert` runs its message only when the condition is false
(ADR 0031). In an assignment the value runs before the place. In
`base[index]`, the base runs before the index, and the bounds check before
the access. A `match` runs its scrutinee once, before any arm, and its
patterns run no code (ADR 0037, 0040). A read
through a pointer, and a read of a static (ADR 0030), happens exactly where
it is written. A value that is not a place, used as the base of indexing
or a field, is read where it is written too: in `({ x })[{ x[0] = 9; 0 }]`
the block reads `x` before the index changes it. `e?` runs `e` where it is
written, and when it returns from the function, nothing after it runs: in
`f(a()?, b())`, `b()` does not run when `a()` fails (ADR 0046).

## Modules and paths (ADR 0049)

```
mod uart;                  // the items of uart.ers

mod regs {                 // the items between the braces
    pub const BASE: usize = 0x1000_0000;
}

fn base() -> usize {
    regs::BASE
}
```

**Modules.**

- `mod name { ... }` declares an **inline module**, holding the items
  between the braces, and `mod name;` a **file module**, holding the items
  of its own file. A module is an item of the module that declares it, its
  **parent**. The root file is the **root module**, and the modules form a
  tree under it.
- **Files.** In the root file, `mod uart;` reads `uart.ers` in the root
  file's directory. In any other file, `dir/uart.ers`, `mod regs;` reads
  `dir/uart/regs.ers`: the children of a file module live in a directory
  named after it. An inline module adds its name to that directory: in
  the root file, `mod a { mod b; }` reads `a/b.ers`. There is no `mod.ers`
  and no attribute choosing another path, so each module has exactly one
  file.
- Every directory and file name on that path must be the module's name
  exactly, also on file systems that ignore case: `mod Uart;` does not
  read `uart.ers`, nor `mod timer { mod regs; }` `Timer/regs.ers`. A file
  that is missing, whose name differs only in case, or that cannot be
  read, and a `mod` in the root file whose file would be the root file
  itself (`mod main;` in `main.ers`), are `resolve::module-file`, at the
  declaration, naming the path tried. Such a module is then empty, and
  the other errors of name resolution are still reported, so a use of its
  items is reported too.
- **File names.** Files are named relative to the root file's directory,
  with `/` between directories (`uart/regs.ers`), and the root file by its
  file name, however the command line wrote its path; on a file system
  that ignores case, as its directory lists it (`erebos build MAIN.ERS`
  names `main.ers`). Diagnostics and panic locations (see "Panic
  information") therefore do not depend on the directory the compiler
  runs in. The files of `core` are named `rt/core.ers` and
  `rt/builtins.ers`, so a module of the root module may not be named `rt`
  (`resolve::reserved-name`, ADR 0056), which would let a program file
  have one of their names; a module elsewhere, or a `use`, may.
- **Order.** Files are numbered in the order their `mod` declarations are
  met, depth first, starting from the root file; the files of `core` come
  after every file of the program. Diagnostics, sorted by file and offset,
  and dumps follow that order. A diagnostic may point into several files,
  each shown under its own name.
- Every kind of item may appear in any module, `comptime` blocks, modules,
  `use` declarations and the `memory` block included, and the entry and
  panic functions may be in any module. A `mod` inside a function body is
  `parser::local-mod`. No attribute applies to a module
  (`resolve::misplaced-attribute`).
- Module files are read even when a file has syntax errors, so that the
  syntax errors of every file are reported; any of them stops the
  compilation before names are resolved.

**Namespaces.**

- Each module has **one namespace**, holding its items, its child modules
  and the names its `use` declarations bring in. Two of these with one
  name are `resolve::duplicate-item`. The same name in two modules names
  two items.
- The **built-in names**, the types `bool`, the integer types, `Option`
  and `Result`, and the functions `assert`, `len` and `slice`, are seen
  from every module, as single names, and no item, module or `use` may
  take one (`resolve::duplicate-type`, `resolve::reserved-name`). Nor may
  any take `core`, the name by which every module names the module `core`
  (ADR 0052).
- A **single name**, such as `x` or `Config`, is looked up among the
  locals (in a body), then in the namespace of the module where it is
  written. It is **never looked up in a parent**: a child module writes
  `super::x`, or brings the name in with `use super::x;`. When a name is
  found nowhere but a module around the current one has it, the error's
  help writes the path to it.

**Paths.** A path is names separated by `::`: `uart::init`,
`uart::Mode::Run`, `crate::uart::regs::BASE`.

- Its first segment is one of:
  - `crate`: the root of the module tree the path is written in, the root
    module (inside `core`, the root of `core`);
  - `super`: the parent of the current module, and `super::super` its
    parent's parent; `super` appears only at the start of a path, and
    `a::super` and `crate::super` are `parser::unexpected-token`;
  - `core`: the root of `core`, from any module (see "`core`");
  - any other name: looked up in the namespace of the current module, or
    as a built-in name, never among locals.
- Each following segment is looked up in the namespace of the module
  named so far. When the path so far names an enum, the next segment is
  one of its variants and ends the path: `uart::Mode::Run`,
  `uart::Mode::Write(3, 4)`.
- Each segment must be visible from the module the path is written in
  (see "Visibility").
- Paths go wherever an item is named: types (`uart::Config`,
  `Option(uart::Config)`, `*const uart::regs::Block`), constants, statics,
  calls, struct literals (`uart::Config { baud: 115_200 }`), variants and
  patterns (`uart::Mode::Run`), and the type operand of `size_of`,
  `align_of` and `offset_of`. Locals, fields and the names declarations
  give are single names.
- `crate`, `super`, `mod`, `use` and `pub` are keywords; `core` is a
  reserved name.

The errors of a path say in which module each name was looked up ("not
found in module `uart`"):

| Path | Error |
|------|-------|
| a last segment that names nothing | the code of its position: `resolve::unknown-type` for a type or a struct literal, `resolve::unknown-function` for a callee, `resolve::unknown-name` for a value or a pattern, `resolve::unknown-variant` after an enum |
| a last segment that names an item of another kind, such as a constant as a type | the code of its position, labelled with what the item is ("a constant, not a type"); in a pattern, `resolve::not-a-constant` |
| a segment followed by `::` that names nothing | `resolve::unknown-path` |
| a segment followed by `::` that names neither a module nor an enum: a struct, a constant, a variant (`Mode::Run::x`), a built-in type or function (`u8::MAX`) | `resolve::not-an-enum` |
| a module where a type, a value, a function, a struct or, in a pattern, a constant or a variant is expected (`let x: uart`, `uart()`, `uart { }`) | `resolve::misused-module` |
| `super` in the root module, or `super::super` one level below it | `resolve::super-at-root` |

**Names in diagnostics and dumps.** An item outside the root module is
shown by its path from the root, `uart::init`, `uart::Config`,
`core::PanicInfo`, in diagnostics and in the `hir` and `typeck` dumps;
items of the root module keep their own names, so one-file programs show
what they showed before modules. The `mir` and `llvm-ir` dumps name
functions and statics by their symbols, `uart.init` (see "Symbols").

## `use` (ADR 0049, 0050)

```
use uart::putc;
use uart::regs::BASE as UART_BASE;
use crate::timer::{init, Tick, regs::CONTROL,};
pub use regs::Block;
```

- `use path;` brings the item or module the path names into the
  namespace of the module of the `use`, under the path's last name; `use
  path as name;` under `name`. Braces after `::` list several paths from
  one prefix, each with an optional `as`, with a trailing comma allowed.
  The paths between braces are names separated by `::`: braces do not
  nest, and `crate`, `super` or nothing between them is
  `parser::unexpected-token`.
- A `use` path resolves as every path does, from the module of the `use`:
  from its namespace, `crate`, `super` or `core`, each segment visible
  there. It may go through a name another `use` brought in. `use`
  declarations, like items, apply whatever their order and their files.
- A path made only of `crate`, `super` or `core`, as `use crate;` or `use
  super as parent;`, brings in no name and is `parser::unexpected-token`.
  A `use` inside a function body is `parser::local-use`.
- A `use` names an item or a module, never a variant
  (`resolve::use-variant`): bring in the enum and write `E::V`, or `.V`
  where the type is known (ADR 0044). A built-in name is no item either:
  `use u8 as byte;` and `use u8;` are `resolve::unknown-path`, the second
  reported once, and the built-in names are seen from every module
  already. A last segment that names nothing is
  `resolve::unknown-path` too.
- The name a `use` brings in is in its module's one namespace: no item,
  module or other `use` of that module may have it
  (`resolve::duplicate-item`), and it cannot be a reserved name or a
  built-in type's name.
- A `use` whose target can only be found through itself, as `use b::x;`
  in module `a` with `use a::x;` in module `b`, or `use x;` where nothing
  else is named `x`, is `resolve::use-cycle`, reported once, at the first
  `use` of the cycle in file order, then offset, naming the others.
- Once a `use` is reported in error, the uses of the name it brings in
  report nothing more.
- There are **no glob imports** (`use uart::*`) and **no prelude**: every
  name a module uses is written in that module or in the `use` that
  brings it in, and an item of `core` is written `core::x` or brought in
  with `use`.
- A `use` is private unless declared `pub`: `pub use` makes the name it
  brings in a `pub` name of its module, so that a module can offer items
  of its private children (see "Visibility").
- No attribute applies to a `use` (`resolve::misplaced-attribute`), and a
  `use` gives nothing a symbol.

## Visibility (ADR 0050, 0054)

```
pub struct Config {
    pub baud: u32,
    divisor: u16,
}

pub fn config(baud: u32) -> Config {
    Config { baud: baud, divisor: (48_000_000 / (16 * baud)) as u16 }
}
```

**Items.**

- An item, module or `use` is **private** unless declared `pub`. A
  private item is visible in the module that declares it and in that
  module's descendants; a `pub` item wherever its module is.
- `pub` comes after the attributes and before the item's keywords: `pub
  fn`, `pub extern fn`, `pub static mut`, `pub extern static`, `pub
  struct`, `pub enum`, `pub const`, `pub mod`, `pub use`, `pub panic fn`,
  `#[export] pub fn`. On a `comptime` or `memory` block, which has no
  name, and on the entry function, which nothing may call, it is
  `parser::misplaced-pub`.
  Before a field it is allowed (below); before anything else, such as a
  variant, a parameter, a `let` or another `pub`, it is
  `parser::unexpected-token`.
- A path is usable where **each of its segments** is visible: a `pub` item
  inside a private module is usable only where that module is. Naming an
  item that is not visible is `resolve::private-item`, pointing at the use
  and at the declaration; the item is still resolved, so nothing else is
  reported about it. `core` follows the same rules: its private items are
  its own.
- An item is **reachable** from a module when some path written there
  would name it and be usable, through `pub use` names too: an enum of a
  private module offered with `pub use` is reachable wherever the
  `pub use` is. What writes no path to the item, `.V` and fields, is
  judged by reachability.
- **`pub use`** makes the name it brings in `pub`. The item it names,
  through any other `use`s, must itself be declared `pub`
  (`resolve::private-reexport`): re-exporting cannot widen what an item
  declared. Only the item's declaration counts: a `pub use` may offer a
  `pub` item through a name that a private `use` brought in.
- The compiler calls the entry and panic functions whatever their
  visibility.

**Fields.**

- A **field is private** unless declared `pub`: visible where a private
  item of the struct's module would be, in that module and its
  descendants. A `pub` field is visible wherever its struct is reachable.
- Where a field is not visible, **no place may name it**: reading or
  writing `c.divisor`, `&raw const c.divisor` and
  `offset_of(Config, divisor)` are `ty::private-field`. So is naming a
  `pub` field of a struct that is not reachable there, such as a value of
  a private type returned by a `pub fn`.
- A **struct literal** of a struct with a private field, outside that
  field's visibility, is `resolve::private-field`, whether it gives the
  field or leaves it out: a literal gives every field, so it cannot be
  completed there. The error names every private field, and
  `resolve::missing-fields` is not reported for that literal. Such a
  struct is built outside its module only by a function of that module.
- Visibility changes no layout: private fields are laid out, copied and
  passed like any other, and `size_of` counts them.

**Variants.**

- The variants of an enum, and their payloads, are visible wherever the
  enum is reachable, and only there. `uart::Mode::Run` checks `Mode` as
  every path checks its segments; `.Run` (ADR 0044), which does not write
  the enum, is `ty::private-variant`, in an expression or a pattern, where
  the enum it names is not reachable. So `set(.Run)` and `match get() {
  .Run => ... }` in another module are rejected when `Mode` is private,
  even if `set` and `get` are `pub`: only the module of a private enum,
  and its descendants, make its values or name its variants.
- Where an enum is not reachable, **only `_`** matches its values without
  naming a variant, as a whole arm or inside a payload (`.Some(_)`). A
  `let` pattern names no variant, but its type, always written, cannot
  name the enum there (ADR 0054); a `let` of a reachable type that holds
  the enum, such as a `pub` enum with a payload of it, still covers the
  enum's values inside.
- A `match` that leaves values unmatched is `ty::non-exhaustive-match`,
  with its witnesses as usual. When a shown witness names, at any depth, a
  variant of an enum that is not reachable from the module of the `match`
  (`uart::Mode::Run`, `.Some(uart::Mode::Run)`), the help says to cover
  them with `_`, which names no variant, and proposes nothing else, even
  when other witnesses could be written: one `_` arm covers them all.

**What visibility is not.**

- It controls names, not memory: a private static may still be reached
  through a pointer the module hands out.
- It is checked where a name is used, never in signatures: a `pub fn` may
  take or return a private type. Outside the module the type cannot be
  named, so, with types always written (ADR 0008), such a value can only
  be passed on.
- It does not change symbols: a private function or static is still
  emitted, as an external symbol of the object file (see "Symbols").
- Evaluation at compile time is unaffected.

## Symbols (ADR 0051)

- The symbol of a **function with a body or a static** (`static`, `static
  mut`) is its **path joined by dots**: the names of the modules from the
  root of its tree down to it, then its own name. `fn init` in `uart` is
  `uart.init`; `static BASE` in `uart::regs` is `uart.regs.BASE`. The root
  module contributes no name, so **items of the root module have their own
  names** as symbols. The root of `core` contributes `core`.
- **`#[export]`** before a function with a body or a static gives it its
  **own name** as its symbol, whatever its module:

  ```
  #[export]
  pub fn uart_rx_ready() -> bool { ... }    // symbol `uart_rx_ready`
  ```

  On an item of the root module it changes nothing, and is allowed, so
  that an item can move between modules without changing its symbol. It
  takes no argument (`resolve::attribute-argument`), and given twice is
  `resolve::duplicate-attribute`. On any other item (`extern fn`,
  `extern static`, struct, enum, constant, module, `use`, `comptime`
  block) it is `resolve::misplaced-attribute`.
- An **`extern fn` or `extern static`** always has its own name, in any
  module: it names a symbol defined outside the program.
- The entry and panic functions follow the same rules; `_start` jumps to
  the entry function's symbol, whatever it is.
- A dot occurs in no Erebos or C identifier, so a path symbol never equals
  a plain name. C code names the root module's items and the exported
  ones; to call another function from C, mark it `#[export]`. Linker
  scripts and debuggers accept dots (`break 'uart.init'` in gdb).
- **Every symbol of the program is unique.** Two items with one symbol, in
  any modules, `core` included, are `resolve::duplicate-symbol`, pointing
  at both: a root `fn putc` and an `#[export] fn putc` in `uart`, or one
  `extern fn c_checksum` declared in two modules. The fix for the latter
  is to declare it once and bring it in with `use`; nothing checks that two
  declarations of one C function agree. Against a symbol of `core`, such
  as a root `fn memcpy`, the help names the function of `core` to call. Two
  items with one name in one module are `resolve::duplicate-item`, and are
  not reported again, nor is an item whose name starts with `__`, already
  `resolve::reserved-name`.
- **Reserved names.** LLVM gives symbols starting with `llvm.` a meaning
  of their own, so `llvm` is a reserved module name in the root module
  (`resolve::reserved-name`); a module of another module, or a root item,
  may be named `llvm`. `_start` stays reserved in every module, with or
  without `#[export]` (ADR 0012), and so are names starting with `__`
  (ADR 0052).
- Symbols are not visibility: every function and static, private or not,
  is emitted as an external symbol of the object file (ADR 0030). Byte
  strings and other constant data keep private names, and bodies run only
  at compile time are not emitted.

## Memory maps (ADR 0057)

```
const KIB: usize = 1024;

memory {
    region FLASH { origin: 0x0800_0000, size: 64 * KIB }
    region RAM { origin: 0x2000_0000, size: 16 * KIB }
    region SRAM2 { origin: 0x2000_4000, size: 16 * KIB }

    code in FLASH;
    data in RAM;
    stack in RAM { size: 2 * KIB }
}
```

A `memory` block gives the board's memory as **regions**, ranges of
addresses with a name, and says in **placements** what each region holds.
The compiler checks it and generates the linker script from it.

**The block.**

- A `memory` block is an item, in any module, so that a module describing
  a board can hold it. A program has at most one: a second one anywhere in
  the program is `resolve::duplicate-memory`. To be built into an
  executable, or to print its linker script (see "Building"), a program
  needs one (`link::no-memory-map`); the other stages do not.
- Its grammar, where regions and placements come in any order:

  ```
  memory_block = "memory" "{" { region | placement } "}"
  region       = "region" NAME "{" "origin" ":" expr "," "size" ":" expr [","] "}"
  placement    = "code" "in" NAME ";"
               | "data" "in" NAME ";"
               | "stack" "in" NAME "{" "size" ":" expr [","] "}"
  ```

  A syntax error in it is a `parser::unexpected-token`.
- `memory` starts a block only where an item starts and `{` follows it;
  the words inside (`region`, `origin`, `size`, `code`, `data`, `stack`,
  `in`) are recognized only there. Elsewhere all of them are ordinary
  names, of variables, functions, modules or fields.
- Inside a function body, `memory {` followed by `region` and a name, or
  by `code`, `data` or `stack` and `in`, is `parser::local-memory`.
  Anything else after `memory {` there is a struct literal, as
  `memory { data: 1 }` for a struct named `memory`.
- Attributes do not apply to a `memory` block
  (`resolve::misplaced-attribute`, or `resolve::unknown-attribute` for an
  attribute that does not exist), nor does `pub` (`parser::misplaced-pub`):
  it has no name and describes the whole program.

**Regions.**

- `origin` and `size` are `usize` values computed at compile time, like
  array lengths (see "Compile-time execution"), in the scope of the module
  holding the block: they may name its private constants and call its
  functions. A region covers `[origin, origin + size)`.
- Region names belong to the block. They are not items: a constant of the
  same name is no conflict, a name in an expression of the block is looked
  up among items as anywhere else, and code cannot name a region
  (`resolve::unknown-name`). Two regions of one name are
  `resolve::duplicate-region`, and a region named `INCLUDE`, a command of
  the linker script language, is `resolve::reserved-name` (`include` is
  fine).
- A region nothing is placed in is allowed: it documents memory the
  program uses by address.

**Placements.** Each of `code`, `data` and `stack` is placed exactly once:
twice is `resolve::duplicate-placement`, never is
`resolve::missing-placement`, and a placement naming no region of the
block is `resolve::unknown-region`.

- `code in R;` puts in `R`, in this order, the reset entry `_start` (see
  "Startup and memory"), the code (`.text`), the read-only data
  (`.rodata`) and the initial values of `.data`. `_start` is at `R`'s
  origin, which must be a multiple of 4 (`ty::misaligned-code`) and must
  be the address the board jumps to at reset, which the compiler cannot
  check. On QEMU `virt` run with `-bios none`, as the tests do, that is
  `0x8000_0000`, the start of its RAM; with QEMU's default firmware,
  OpenSBI, loaded there, such a program does not boot.
- `data in R;` puts in `R` the run addresses of `.data`, then `.bss`.
  `R` may be the code's region, for a board that runs from RAM: the
  initial values of `.data` are then at its run addresses, and nothing
  else is set aside for them.
- `stack in R { size: E }` reserves `E` bytes for the stack at the
  **start** of `R`, below anything else `R` holds. Its start is `R`'s
  origin rounded up to 16, the stack alignment of riscv32, and `E` must be
  a multiple of 16. The stack grows down, so one that overflows leaves its
  region below its origin, where many chips have no memory or memory that
  faults, instead of running over `.data` and `.bss`. Nothing detects the
  overflow at run time yet (it needs trap handlers, M4); on QEMU `virt`,
  where every region is part of one RAM, an overflow writes into the
  region below, if there is one. The stack cannot be in the code's region
  (`resolve::stack-with-code`), whose origin `_start` needs; a program on
  one region of RAM, as QEMU's, splits it in two.

**What is checked.** When names are resolved:

| Code | Case |
|------|------|
| `resolve::duplicate-memory` | a second `memory` block in the program |
| `resolve::duplicate-region` | two regions with one name |
| `resolve::unknown-region` | a placement names no region of the block |
| `resolve::duplicate-placement` | `code`, `data` or `stack` placed twice |
| `resolve::missing-placement` | `code`, `data` or `stack` not placed, reported at `memory`, once for each |
| `resolve::stack-with-code` | the stack placed in the code's region |
| `resolve::reserved-name` | a region named `INCLUDE` |

"Second" means later in the order of files, which are numbered depth
first (see "Modules and paths"), then of positions in a file: the error
points at the second block, region or placement, and labels the first.

Once the block's values are computed, during type checking, so whatever
stage `--emit` asks for after it:

| Code | Case |
|------|------|
| `ty::empty-region` | a region of size 0 |
| `ty::region-out-of-range` | a region whose end, `origin + size`, is above `0xFFFF_FFFF`, so that the address one past it, which the linker's end symbols hold, would not exist; the last byte of the address space cannot belong to a region |
| `ty::region-overlap` | two regions sharing an address; reported once per pair, at the later region, labeling the earlier |
| `ty::misaligned-code` | the code's region starting at an address that is not a multiple of 4 |
| `ty::stack-size` | a stack of size 0, or not a multiple of 16, or one that does not fit in its region once its start is rounded up |

A stack size that is both not a multiple of 16 and too large is reported
as not a multiple. A value whose computation failed is reported once,
where it is written, with the errors of any compile-time evaluation or a
`ty::mismatched-types`, and the checks that need it are skipped: a region
whose origin failed overlaps nothing.

**The linker script.** `erebos build --emit=linker-script` prints the
script the compiler passes to `ld.lld`:

- `MEMORY` lists every region in source order under its own name, so that
  the script and the linker's messages read like the source.
- In the code's region: `_start` in an output section of its own,
  `.text.start`, so that only its own alignment counts and an object given
  with `--link` whose code is aligned to 64 cannot move it off the origin;
  then `.text`, `.rodata` and the initial values of `.data`.
- In the stack's region: a section `.stack` without contents, from
  `__stack_start` to `__stack_end`, the bounds of the stack. In the data's
  region: `.data`, then `.bss`. `.stack` comes before both, so that it is
  first in its region whether or not that region also holds the data.
- The rest is as before (ADR 0030): `.data` and `.bss` aligned to 4 and
  padded inside, the symbols of the startup loops, the small-data
  sections, the sections that are not loaded, and a link error for a
  section the script does not place.

**Region overflow.** Only the linker knows how large the code is, so a
region too small for what it holds is found at link time: the compiler
reads `ld.lld`'s errors back and reports one `link::region-overflow` per
region, at the region's name in the source, naming the first section that
does not fit (the initial values of `.data` by that name, when the data
is elsewhere) and how many bytes the region lacks; its help lists what
the region holds, since the section that overflows is often not the one
to shrink. The linker's other errors are shown as `ld.lld` writes them,
after `error: linking failed:`, but for the sections it finds overlapping
when a region overflows: the regions being disjoint, sections overlap
only past the end of one, so those errors follow from the overflow and
are left out.

## Startup and memory (ADR 0012, 0030, 0057)

`_start`, generated by the compiler and visible in `--emit=llvm-ir`, is the
reset entry: it sits alone at the origin of the code's region (see "Memory
maps"), and does, in order:

1. set the stack pointer to `__stack_end`, the end of the stack the
   `memory` block declares;
2. fill `.bss` with zeros;
3. copy the initial values of `.data` from their load address to their
   run address; when the data shares the code's region the two are one,
   and each word is copied onto itself;
4. jump to the entry function, by its symbol (see "Symbols").

Nothing else runs before the entry function: there are no constructors and
no order of initialization. The loops work a word at a time between
symbols the linker script defines (`__bss_start`, `__bss_end`,
`__data_start`, `__data_end`, `__data_load`), and do nothing when a range
is empty. `_start` takes `__stack_end` from the script too, so the LLVM
IR of a program is the same whatever its board.

Object files given with `--link` may use the small-data sections
(`.srodata`, `.sdata`, `.sbss`, as GCC emits by default and clang with
`-msmall-data-limit`): they are gathered with `.rodata`, `.data` and
`.bss`, and initialized likewise. A section the script does not place is a
link error, not data left uninitialized. No `__global_pointer$` is
defined, so nothing is addressed relative to `gp`.

## `core` (ADR 0020, 0052)

`core` is a module tree written in Erebos and shipped inside the
compiler: its root is `rt/core.ers`, whose `mod builtins;` is
`rt/builtins.ers`, found as any file module is. Its files are embedded in
the compiler binary, and compiled and checked with every program.

- A path in any module of the program may start with `core`:
  `core::PanicInfo`, `use core::PanicReason;`. `core` is a reserved name:
  no item, module or `use` of the program may take it
  (`resolve::reserved-name`). Inside `core`, `crate` is the root of `core`,
  and nothing names the program.
- `core` holds:
  - `PanicInfo` and `PanicReason`, `pub` in its root (see "Panic
    information");
  - the **runtime support functions**, in its private module
    `core::builtins`: `memcpy`, `memmove` and `memset`, which LLVM calls
    for large copies and fills, `pub` and offered from the root with
    `pub use`, so that a program calls `core::memcpy`; and the 64-bit
    division functions riscv32 needs, `__udivdi3`, `__umoddi3`, `__divdi3`
    and `__moddi3`, private, since LLVM calls them and programs write `/`
    and `%`.

  Each support function is `#[export]`, so that its symbol is the name
  LLVM calls (see "Symbols"); their private helpers are not
  (`core.builtins.__erebos_udivmod64`). The compiler marks every function
  of `core` `no-builtins`, so that LLVM does not turn their loops back into
  calls to them.
- `Option`, `Result`, `assert`, `len` and `slice` are not in `core`: they
  stay built-in names, seen from every module.
- A program item whose symbol is one of `core`'s, such as a root `fn
  memcpy` or an `#[export] fn memset` in any module, is
  `resolve::duplicate-symbol`, whose help names the function of `core` to
  call; an item named with `__` is `resolve::reserved-name`.
- `core` has no entry or panic function. Every program includes all of
  it, the functions the program does not call too.
- The `--emit` dumps leave `core` out (see "Building"). An error reported
  in a file of `core` is a compiler bug.

## Panic information (ADR 0053)

```
use core::{PanicInfo, PanicReason};

panic fn on_panic(info: *const PanicInfo) -> ! {
    let reason: *const [u8] = match (*info).reason {
        PanicReason::DivisionByZero => &raw const b"division by zero",
        PanicReason::IndexOutOfBounds => &raw const b"index out of bounds",
        PanicReason::AssertionFailed => &raw const b"assertion failed",
        PanicReason::NoVariant => &raw const b"enum value is no variant",
    };
    print(reason);
    print(&raw const b" at ");
    print((*info).file);
    print(&raw const b":");
    print_u32((*info).line);
    loop {}
}
```

**The panic function's parameter.**

- The panic function takes no parameter, or one of exactly the type
  `*const core::PanicInfo`, however the path to it is written; the
  parameter may be `mut`, as any parameter. Any other parameter list is
  `ty::panic-param`: two parameters, a `*mut` or `*const volatile` pointer,
  `PanicInfo` by value, or a pointer to another struct, even one named
  `PanicInfo`.
- **Without the parameter, no panic information is built or emitted**: a
  program pays nothing for what its handler ignores. The message of an
  `assert` is still evaluated when the assertion fails, and dropped.
- The compiler's checks call the panic function directly. It may still be
  called explicitly, with the arguments its signature asks for: a caller
  of a handler with the parameter builds the `PanicInfo` itself. Code that
  panics on purpose writes `assert(false, msg)`, and the compiler fills in
  the information.

**`core::PanicInfo` and `core::PanicReason`.**

```
pub struct PanicInfo {
    pub file: *const [u8],
    pub line: u32,
    pub column: u32,
    pub reason: PanicReason,
    pub message: *const [u8],
}

pub enum PanicReason: u8 {
    DivisionByZero = 0,
    IndexOutOfBounds = 1,
    AssertionFailed = 2,
    NoVariant = 3,
}
```

`PanicInfo` takes 28 bytes, aligned to 4: `file` at offset 0, `line` at
8, `column` at 12, `reason` at 16 and `message` at 20. A `comptime` block
in `core` asserts this layout, and `tests/layout` checks it against
clang's.

- **`file`** is the name of the source file holding the check, as
  diagnostics name it (see "Modules and paths"): relative to the root
  file's directory, with `/` between directories (`main.ers`,
  `uart/regs.ers`), and `rt/core.ers` or `rt/builtins.ers` in `core`. It
  depends neither on the directory the compiler runs in nor on how the
  command line wrote the root file's path, so builds stay reproducible.
- **`line`** and **`column`** locate the first byte of the token that
  performs the check, both counted from 1, the column in bytes (the
  character, in ASCII sources), so that nested checks, as in `a[i][j]` or
  `a / b / c`, each have their own. Parentheses change nothing: `(a / b)`
  is located at its `/`.

  | Reason | Checked by | Located at |
  |--------|------------|------------|
  | `DivisionByZero` | `a / b`, `a % b`, `a /= b`, `a %= b` (ADR 0007) | the operator |
  | `IndexOutOfBounds` | `a[i]`, `(*s)[i]` (ADR 0021, 0031) | the `[` |
  | `AssertionFailed` | `assert(c)`, `assert(c, msg)` (ADR 0029, 0031) | `assert` |
  | `NoVariant` | a `match` without a catch-all arm (ADR 0036, 0042) | `match` |
  | `NoVariant` | `e?` (ADR 0046) | the `?` |

- **`reason`** is the cause of the check.
- **`message`** is the message of `assert(c, msg)`, computed only when the
  assertion fails (see "Messages in `assert`"); for every other check, and
  for an `assert` without one, it is an empty slice, whose address is
  unspecified.

**Where the information lives.**

- For every check but an `assert` with a message, every field is known at
  compile time, and the `PanicInfo` is **read-only data** the compiler
  emits, one per check (LLVM may merge identical ones); the call passes
  its address. The file names are byte strings, one per file: the name of
  every file of the program and of `core` is emitted, and `-O` removes
  those no check uses.
- An `assert` with a message builds its `PanicInfo` in the frame of the
  failing function, on the failing path only, once the message is
  computed, and passes its address. The panic function never returns, so
  the pointer stays valid while it runs.
- The handler must not write through `info`: it points at read-only
  memory or into another function's frame.
- Each check costs about 28 bytes of read-only data when the handler
  takes the parameter, and nothing when it does not.

**At compile time** nothing changes: a failed check fails the evaluation
with `eval::panic` and its cause, or `eval::assertion-failed` for an
`assert` (see "Compile-time execution"); the panic function's body never
runs there, and no `PanicInfo` is read.

A new kind of check would add a variant to `PanicReason`; a handler that
matches the reason without a catch-all arm, as above, would then fail to
compile until it handles it.

## Building

```
erebos build program.ers [-o program.elf] [-O] [--link file.o]... [--emit=<stage>]
```

`program.ers` is the root file: the files of its modules are read from its
directory, and every file is named relative to it (see "Modules and
paths"). `-O` enables LLVM optimizations. `--link` adds an object file
defining `extern fn`s and `extern static`s; it may be repeated. `--emit`
prints a compiler stage instead: `tokens`, `ast`, `hir`, `typeck`, `mir`,
`linker-script` or `llvm-ir`.

- Building an executable and `linker-script` need the program's `memory`
  block (`link::no-memory-map`, see "Memory maps"); the other stages do
  not. `linker-script` prints the script `ld.lld` is given.
- `tokens` and `ast` show the root file alone, before any module file is
  read; the later stages show the whole program.
- The dumps leave `core` out: `mir` and `llvm-ir` show a function of `core`
  the program calls as a declaration (`extern fn memcpy(...)`,
  `declare ptr @memcpy`), and the others not at all.
- `hir` and `typeck` show the items outside the root module by their
  paths, and `mir` and `llvm-ir` show functions and statics by their
  symbols (see "Symbols"); the `hir` dump starts with the module tree.
- `typeck` and `mir` start with the `memory` block, as written but with
  its values computed and its placements in the order `code`, `data`,
  `stack`, then every struct's layout and every enum with its integer
  type and the values of its variants, an enum with payloads also with
  the offset of each payload value, its size and its alignment, then each
  `Option(T)` and `Result(T, E)` used, sorted by name, and list the bodies
  run at compile time as `comptime fn`, named by path, a variant's value
  as `comptime fn Mode::Run` and the values of the `memory` block as
  `comptime fn origin`, `size` and `stack_size`.

See `CONTRIBUTING.md` for setup and running on QEMU.

## Diagnostics

Every error has a stable code, shown with the message.

| Code | Meaning |
|------|---------|
| `lexer::unexpected-character` | a character that starts no token |
| `lexer::unterminated-comment` | `/*` without its `*/` |
| `lexer::empty-int` | base prefix without digits (`0x`) |
| `lexer::invalid-digit` | digit not valid in the literal's base |
| `lexer::leading-zero` | decimal literal starting with `0` (use `0o`) |
| `lexer::int-too-large` | literal does not fit in 64 bits |
| `lexer::invalid-byte` | malformed byte literal, or a character or escape not allowed in a byte literal or byte string |
| `lexer::unterminated-byte` | byte literal without closing `'` |
| `lexer::unterminated-byte-string` | `b"` without its closing `"` on the same line |
| `parser::unexpected-token` | a token where it cannot appear, such as `_` as a name or a value, `pub` before a variant, or a `use` of a path made only of `crate`, `super` or `core` |
| `parser::unclosed-block` | `{` without its `}` |
| `parser::let-without-type` | `let` without `: Type`, also in a pattern |
| `parser::chained-comparison` | `a < b < c` |
| `parser::local-const` | `const` inside a function body |
| `parser::local-static` | `static` or `extern static` inside a function body |
| `parser::local-mod` | `mod` inside a function body |
| `parser::local-use` | `use` inside a function body |
| `parser::local-memory` | a `memory` block inside a function body: `memory {` followed by `region` and a name, or by `code`, `data` or `stack` and `in` |
| `parser::misplaced-pub` | `pub` on a `comptime` block, a `memory` block or the entry function |
| `parser::enum-without-type` | an enum without `: Type` |
| `parser::variant-without-value` | a variant without `= value` |
| `parser::empty-payload` | a variant written `V()`, a payload without types |
| `resolve::module-file` | the file of `mod name;` does not exist, cannot be read, has a name that differs in case, or is the root file |
| `resolve::unknown-name` | a name, or a path's last segment, that names no value where one is expected, also in a pattern; an enum's or struct's name alone; a name only a module around the current one has |
| `resolve::unknown-type` | a type, or the struct of a literal, whose name or last path segment names nothing or an item of another kind |
| `resolve::unknown-path` | a path segment followed by `::` that names nothing; a `use` whose last segment names nothing, or that names a built-in name |
| `resolve::misused-module` | a module where a type, a value, a function, a struct or, in a pattern, a constant or a variant is expected |
| `resolve::super-at-root` | `super` in the root module, or `super::super` one level below it |
| `resolve::type-argument-count` | `Option` or `Result` with the wrong number of type arguments, as `Option`, `Option()` or `Result(u32)`, or parentheses after a type that takes none, as `u32(u8)` |
| `resolve::unknown-function` | call to a function that does not exist, or to an item that is not a function |
| `resolve::not-a-function` | call to a variable |
| `resolve::call-entry` | call to the entry function |
| `resolve::duplicate-item` | two items, modules or `use` names with one name in one module |
| `resolve::duplicate-symbol` | two items of the program, `core` included, with one symbol |
| `resolve::duplicate-type` | any item, module or `use` name named like a built-in type: `bool`, an integer type, `Option` or `Result` |
| `resolve::use-variant` | a `use` naming an enum variant |
| `resolve::use-cycle` | `use` declarations that can only be resolved through themselves |
| `resolve::private-item` | naming an item that is not visible here |
| `resolve::private-reexport` | `pub use` of an item that is not `pub` |
| `resolve::private-field` | a struct literal of a struct with a field that is not visible here |
| `resolve::duplicate-param` | two parameters with one name |
| `resolve::mut-extern-param` | a `mut` parameter in an `extern fn` |
| `resolve::duplicate-field` | a field declared, or given in a literal, twice |
| `resolve::unknown-field` | a struct literal names a field the struct does not have |
| `resolve::missing-fields` | a struct literal leaves fields out |
| `resolve::enum-type` | an enum whose type after `:` is not an integer type |
| `resolve::duplicate-variant` | two variants with one name |
| `resolve::empty-enum` | an enum without variants |
| `resolve::unknown-variant` | `E::V` where `E` has no variant `V`; a path followed by patterns in parentheses that names no variant |
| `resolve::not-an-enum` | `X::V` where `X` names neither a module nor an enum: a struct, a constant, a variant, a built-in type or function, or `Option` or `Result`, which are no items and whose variants are written `.V` |
| `resolve::not-a-constant` | a name or path in a pattern that is not a constant: a local variable, parameter, static, function, struct or enum |
| `resolve::duplicate-binding` | one name bound twice in one pattern, or in one alternative of an or-pattern |
| `resolve::inconsistent-binding` | a name not bound by every alternative of an or-pattern, or bound with `let mut` by only some |
| `resolve::reserved-name` | an item, module or `use` named `_start`, `core`, with a name starting with `__`, or like a built-in function (`assert`, `len`, `slice`); a module of the root module named `llvm` or `rt`; a region named `INCLUDE` |
| `resolve::unknown-attribute` | an attribute that does not exist |
| `resolve::misplaced-attribute` | an attribute on an item it does not apply to |
| `resolve::attribute-argument` | an attribute with a missing or extra argument |
| `resolve::duplicate-attribute` | an attribute given twice |
| `resolve::conflicting-attributes` | `#[packed]` with `#[align(N)]` |
| `resolve::missing-entry`, `resolve::multiple-entries` | not exactly one `entry fn` in the program |
| `resolve::missing-panic`, `resolve::multiple-panics` | not exactly one `panic fn` in the program |
| `resolve::duplicate-memory` | a second `memory` block in the program |
| `resolve::duplicate-region` | two regions with one name in a `memory` block |
| `resolve::unknown-region` | a placement names no region of its `memory` block |
| `resolve::duplicate-placement` | `code`, `data` or `stack` placed twice |
| `resolve::missing-placement` | `code`, `data` or `stack` not placed |
| `resolve::stack-with-code` | the stack placed in the code's region |
| `resolve::outside-loop` | `break`/`continue` outside a loop |
| `resolve::break-value-in-while` | `break value` in a `while` |
| `resolve::return-in-comptime` | `return` or `?` in code run at compile time on its own: a `comptime` expression or block, the initializer of a constant or static, a variant's value, an array length, a repeat count, an `#[align(N)]` argument or a value of a `memory` block |
| `ty::mismatched-types` | a value of the wrong type, or a `.V` where the expected type, or the position's type, is not an enum |
| `ty::untyped-literal` | a literal or array literal with no expected type |
| `ty::untyped-variant` | a `.V` with no expected type, as a `match` scrutinee, the operand of `as` or `?`, or an expression statement |
| `ty::unknown-variant` | a `.V` where the expected enum, or the position's enum, has no variant of that name |
| `ty::literal-out-of-range` | literal does not fit its type |
| `ty::invalid-operand` | operator applied to a type it does not accept, such as a comparison of slices or of enums with payloads |
| `ty::negate-unsigned` | unary `-` on an unsigned value |
| `ty::shift-out-of-range` | shift amount known at compile time outside 0 to the width minus 1 |
| `ty::division-by-zero` | divisor known at compile time to be zero |
| `ty::invalid-cast` | `as` between types that do not convert, a slice cast the table refuses, any cast to `!` or to an enum, or from an enum with payloads |
| `ty::pointer-cast-width` | thin pointer cast to or from an integer type other than `usize` |
| `ty::deref-non-pointer` | `*` on a value that is not a pointer |
| `ty::cannot-index` | indexing a value that is not an array or a pointer |
| `ty::index-out-of-bounds` | index known at compile time not below the array's length |
| `ty::slice-pointer-index` | indexing a slice pointer directly (`s[i]`); write `(*s)[i]` |
| `ty::no-fields` | field access on a value that is not a struct |
| `ty::unknown-field` | field access to a field the struct does not have |
| `ty::private-field` | a place or `offset_of` naming a field that is not visible here |
| `ty::private-variant` | `.V` naming a variant of an enum that is not reachable here |
| `ty::write-through-const` | assignment through a `*const` pointer |
| `ty::not-assignable` | assignment to something that is not a place, or to part of a temporary |
| `ty::assign-immutable` | assignment to a variable without `mut`, a `static`, an `extern static` without `mut`, or a byte string |
| `ty::not-a-place` | `&raw` of a value that is not a place, or of a constant or any element or field of it |
| `ty::address-of-immutable` | `&raw mut` of a place that cannot be written |
| `ty::packed-field-address` | `&raw` of a possibly misaligned place inside a packed struct |
| `ty::volatile-too-wide` | volatile access wider than the target's widest access |
| `ty::volatile-aggregate` | volatile access to a whole struct, array or enum with payloads, or to a slice stored in memory |
| `ty::volatile-packed-field` | volatile access to a possibly misaligned place inside a packed struct |
| `ty::too-large` | a value larger than half the address space |
| `ty::infinite-size` | structs and enums that contain themselves, directly, through each other or through `Option` and `Result` |
| `ty::unsized` | `[T]` where a size is needed, or `*s` read or written whole |
| `ty::never-in-memory` | `!` as a pointee, slice element, field, payload type, type argument of `Option` or `Result`, array element, the type of a static, `extern static`, `const` or `comptime` value, a layout query operand, or under `&raw` |
| `ty::invalid-align` | `#[align(N)]` on a struct or static with `N` not a power of two, too large or below the natural alignment |
| `ty::aligned-in-packed` | a packed struct containing a type with `#[align(N)]`, at any depth, such as an enum with such a payload |
| `ty::not-comptime` | a run-time variable where a compile-time value is required |
| `ty::cycle` | an item or variant whose value or layout needs itself, including a `match` that needs the values of an enum (a variant pattern, `E::V` or `.V`, or a constant pattern at a position of its type, or no arm on it) in a function that runs while one of them is computed |
| `ty::duplicate-discriminant` | two variants of an enum with one value |
| `ty::match-type` | a `match` on a type other than an integer, `bool` or enum |
| `ty::constant-pattern-type` | a constant pattern of a type other than an integer, `bool` or an enum without payloads |
| `ty::wrong-payload-count` | a variant, written `E::V` or `.V`, built or matched with a number of values other than its payload's, or one without payload written with parentheses |
| `ty::non-exhaustive-match` | a `match` whose arms leave values unmatched |
| `ty::unreachable-pattern` | an arm, or an alternative, that matches no value the patterns before it leave |
| `ty::try-operand` | `?` on a value that is neither an `Option` nor a `Result` |
| `ty::try-return` | `?` in a function whose return type is not an `Option` (for an `Option` operand), or a `Result` with the operand's error type (for a `Result` operand) |
| `ty::if-without-else` | `if` without `else` where a value is expected |
| `ty::wrong-arg-count` | call with the wrong number of arguments |
| `ty::param-type` | parameter of type `()` or `!` |
| `ty::special-fn-params` | entry function with parameters |
| `ty::panic-param` | a panic function whose parameters are not none or one `*const core::PanicInfo` |
| `ty::entry-returns`, `ty::panic-returns` | entry or panic function that can return |
| `ty::empty-region` | a region of size 0 |
| `ty::region-out-of-range` | a region whose end is above `0xFFFF_FFFF` |
| `ty::region-overlap` | two regions sharing an address |
| `ty::misaligned-code` | the code's region starting at an address that is not a multiple of 4 |
| `ty::stack-size` | a stack of size 0, not a multiple of 16, or that does not fit in its region |
| `eval::panic` | compile-time code reached the panic function: a zero divisor, an index out of bounds, an enum value that is no variant in a `match` that no arm matches or under `?`, an explicit call |
| `eval::assertion-failed` | `assert` with a false condition at compile time, with its message if it has one |
| `eval::step-limit` | compile-time code ran more than 10,000,000 steps |
| `eval::call-depth` | compile-time code nested more than 256 calls |
| `eval::volatile-access` | a volatile access at compile time |
| `eval::extern-call` | a call to an `extern fn` at compile time |
| `eval::invalid-access` | reading or writing through a pointer made from an integer, or outside an object |
| `eval::static-mut` | reading or writing a `static mut` at compile time |
| `eval::extern-static` | reading or writing an `extern static` at compile time |
| `eval::read-only-write` | writing to a `static` or a byte string at compile time |
| `eval::pointer-to-int` | a pointer into an object converted to an integer, its bytes read as one, or only some of them copied or overwritten |
| `eval::pointer-compare` | comparing pointers whose run-time answer is not known at compile time |
| `eval::dangling-pointer` | comparing or accessing through a pointer to a variable of a call that has returned |
| `eval::uninit-read` | reading bytes never written, such as padding |
| `eval::pointer-escapes` | a compile-time result holding a pointer into the evaluation's own memory |
| `eval::pointer-out-of-bounds` | a compile-time result holding a pointer outside its object and not one past its end, or a slice not inside its object |
| `codegen::llvm` | LLVM rejected the target; an environment problem |
| `link::no-memory-map` | building an executable, or printing its linker script, for a program without a `memory` block |
| `link::region-overflow` | what is placed in a region does not fit in it, found by the linker |
