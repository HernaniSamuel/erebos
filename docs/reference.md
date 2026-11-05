# Erebos language reference (M4.1)

This describes the language as the compiler implements it today. The
reasoning behind each rule lives in the ADRs (`docs/adr/`), cited as
"ADR NNNN". When this document and the compiler disagree, that is a bug in
one of them.

## Programs

A program is a **root file** (`.ers`), the one given to `erebos build`,
plus the files of the modules it declares (ADR 0049), plus the module
`core`, shipped inside the compiler (ADR 0052). Each file is a list of
items, in any order: functions, external functions, structs, bit-field
structs (see "Bit-field structs"), enums, constants, statics, external
statics, `comptime` blocks, modules (`mod`), `use` declarations, the
`memory` block, peripheral types and peripherals (see "Peripherals"). The
root file is the **root module**, and the modules it declares, inline or
in their own files, form a tree under it; each module has its own
namespace (see "Modules and paths"). Items may refer to each other
whatever their order and their files (ADR 0028). The program must contain
exactly one **entry function** and one **panic function**, in any of its
modules, and, to be built into an executable, exactly one **`memory`
block**, which gives the board's memory and what each part of it holds
(ADR 0057, see "Memory maps"):

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

- `entry fn` is where execution starts, after `_start`, which `core` defines
  (see "Startup and memory" below), has set up the stack, `.bss` and
  `.data`. It takes no parameters, must return `!`, and cannot be called from
  Erebos; an `asm` may jump to it (`sym entry`, see "Inline assembly").
- `panic fn` runs when an operation cannot produce a result, such as
  division by zero, an index out of bounds, a failed `assert`, or an enum
  value that is no variant, met by a `match` that no arm matches or by `?`,
  or a value that does not fit the bit field it is written to
  (ADR 0014, 0036, 0040, 0046, 0059). It must return `!`, and may be called
  explicitly. It takes **no parameter, or one of type
  `*const core::PanicInfo`**, through which it learns why and where the
  program panicked (ADR 0053, see "Panic information"):

  ```
  panic fn on_panic(info: *const core::PanicInfo) -> ! { ... }
  ```

A build is for one of two targets, chosen with `erebos build --target
<name>`: `riscv32imac-unknown-none-elf` (RV32IMAC, the default) or
`thumbv7m-none-eabi` (ARMv7-M, as on the Cortex-M3); see "Targets" below
(ADR 0010, 0073 to 0077). The language is the same on both. The board is
not part of the target: each program declares its memory in its `memory`
block.

## Lexical structure

- **Comments:** `// to end of line` and `/* block */`; block comments nest.
- **Identifiers:** `[A-Za-z_][A-Za-z0-9_]*`, except `_` alone. No item,
  module or `use` of the program may take a name starting with `__`, which
  the runtime support functions of `core` use, nor `_start`, nor `core`, in
  any module (`resolve::reserved-name`, ADR 0020, 0052); a module of the
  root module may not be named `llvm` (see "Symbols") or `rt` (see
  "Modules and paths").
- **Keywords:** `align_of as asm break comptime const continue crate else
  entry enum extern false fn if let loop match mod mut offset_of panic pub
  return size_of static struct super true use volatile while`. `raw` is a
  keyword only right after `&`. `memory`, and the words inside a `memory`
  block (`region`, `origin`, `size`, `code`, `data`, `stack`, `in`), are no
  keywords: they are recognized only in their place (see "Memory maps"),
  and are ordinary names elsewhere. Nor are `peripheral`, which starts an
  item only where an item starts and a name follows it; the words
  `read`, `write` and `at`, recognized only inside the two `peripheral`
  items and, `at` before a field's bits, `read` and `write` before a
  field's name and starting its effects, in a bit-field struct; `step`
  after an entry's offset and `over` after a peripheral's address;
  `clears`, `sets`, `toggles` and `modifies`, recognized only in an
  effect; and `in` after an entry's offset (see "Bit-field structs" and
  "Peripherals"). Nor are `in`, `out`, `lateout`, `inout`, `sym`,
  `clobber_abi`, `options`, `reg` and the names of the options, recognized
  only where an operand or an option of `asm` starts (see "Inline
  assembly"); elsewhere they are ordinary names, `in` included.
- **`..`** ends a literal of a bit-field struct, giving the fields it
  leaves out their defaults, and **`..=`** joins the two ends of a bit
  field's bits (see "Bit-field structs"); neither has another use, and
  there are no range expressions. `0..=1` is three tokens, and `...` is
  `..` followed by `.`.
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
- **String literals** (ADR 0079): `"..."` has the characters, escapes and
  errors of a byte string, and exists only inside the parentheses of `asm`,
  as a template, the name of a register or the name of an ABI (see "Inline
  assembly"). It has no type and no value. Anywhere else, a string literal is
  `parser::string-outside-asm`, whose help shows `b"..."`.
- There are no `char`, text or floating-point values; a byte string is
  bytes, not Unicode text (ADR 0016).

## Types

| Type | Meaning |
|------|---------|
| `u8 u16 u32 u64 usize` | unsigned integers; `usize` is pointer-sized (32 bits) |
| `i8 i16 i32 i64 isize` | two's-complement signed integers |
| `bool` | `true` or `false` |
| `()` | the unit type: no value to speak of |
| `!` | never: the type of an expression that does not finish, never of memory (ADR 0034) |
| `*const T`, `*mut T` | raw pointers |
| `*const volatile T`, `*mut volatile T` | raw pointers whose accesses are volatile (ADR 0013, 0023); never to a peripheral type, whose pointers are volatile without the word (ADR 0058) |
| `[T; N]` | an array of `N` values of type `T`; `N` is a `usize` computed at compile time (ADR 0021, 0029), so a literal too large for `usize` is `ty::literal-out-of-range`, as it is in `#[align(N)]` |
| `*const [T]`, `*mut [T]` | slices: the address of `T` values and how many there are (ADR 0031); `volatile` as for any pointer |
| `Name`, `uart::Name` | a struct declared with `struct Name { ... }`, named by a path (ADR 0021, 0049) |
| `Name`, `uart::Name` | a bit-field struct declared with `struct Name: u8 { ... }`: fields at bit positions of its integer type, `u8`, `u16`, `u32` or `u64`, in which it is stored; a scalar (ADR 0059, see "Bit-field structs") |
| `Name`, `uart::Name` | an enum declared with `enum Name: u8 { ... }`: without payloads, stored as its integer type (ADR 0035); with payloads, a tag and the payload values of its variant (ADR 0039) |
| `Option(T)`, `Result(T, E)` | built-in enums with payloads: a `T` or nothing, and a `T` or an error `E` (ADR 0045, see "`Option` and `Result`") |
| `Name`, `uart::Name` | a peripheral type declared with `peripheral Name { ... }`: a device's registers, the type of a peripheral and the pointee of pointers, but never the type of a value (ADR 0058, see "Peripherals") |

Every `let`, parameter and return type is written explicitly; there is no
inference (ADR 0008). `-> ()` may be omitted (ADR 0015). Arrays, structs
and enums with payloads are **aggregates**, and so are slices where a rule
depends on it. A bit-field struct is not: it is a **scalar**, stored,
passed and accessed as its integer type (ADR 0059).

The names of the built-in types, `bool`, the integer types, `Option` and
`Result`, are no item's: a struct, enum, function, constant, static,
peripheral type, peripheral, module or `use` named so, in any module, is
`resolve::duplicate-type` (ADR 0029, 0045, 0049, 0058). They are seen
from every module, as a single name: `uart::u32` is no type.

A pointer to a sized type, or to a peripheral type, is a **thin
pointer**: a plain address. `[T]` on its own is **unsized**: it exists
only behind a pointer, and a variable, parameter, return value, field,
array element or payload value of type `[T]`, a `static`, `extern static`
or `const` of that type, a layout query (`size_of`, `align_of`,
`offset_of`) of it, and a slice of `[T]` are errors (`ty::unsized`).
`[[u8; 4]]` is a slice of arrays, and fine.

**A peripheral type is no type of a value** (ADR 0058). Like `[T]`, it
exists only behind a pointer (`*mut Uart`, a thin pointer), and as the
type of a peripheral. As the type of a variable, parameter, return value,
field, array or slice element, payload value or type argument of `Option`
or `Result`, the type of a `static`, `extern static` or `const`, an
operand of `size_of`, `align_of` or `offset_of`, or the target of a cast
(`0 as Uart`), it is `ty::peripheral-by-value`, reported once, and the
type containing it causes no further error. `Option(*mut Uart)` and a
field of type `*mut Uart` are fine. As a register's type it is
`ty::register-type` (see "Peripherals"). The one place a peripheral type
is a type besides these is a cluster's type in another peripheral type,
alone or as the element of one array, `[Context; 2]` (ADR 0064): an array
of a peripheral type anywhere else, `*const [Context; 2]` included, is
`ty::peripheral-by-value`.

**`!` is not a type of memory** (ADR 0034, 0039). No value of type `!`
exists, so it is an error (`ty::never-in-memory`) as the pointee of a
pointer (`*const !`), the element of an array or slice type (`[!; 0]`,
`*const [!]`), a field type (a bit field's too), a payload type, a
register's type, the type of a `static`, `extern static`, `const` or
`comptime` value, an operand of `size_of`, `align_of` or `offset_of`, and
under `&raw` (`&raw const x` with `x: !`). Each `!` written there is
reported once, also where nothing uses it, and the type containing it
causes no further error. `!` stays valid as the type of an expression, as
a return type (`-> !`) and in a `let` whose initializer never finishes
(`let x: ! = loop {};`). An array literal or repeat with an element of
type `!` (`[loop {}; 3]`) has type `!`; its other elements are checked
against the expected element type, or the first element whose type is not
`!`. Use `()` for a type without data.

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
- All functions use the C ABI of the target (ADR 0006): the riscv32 one or
  the AAPCS (see "Targets"). A function of the root module has its own
  name as its symbol, and so has one marked `#[export]`, so C code can call
  it by that name; a function of another module is named by its path,
  `uart.init` (ADR 0051, see "Symbols").
- `#[naked]` makes a function whose body is one `asm` expression and has no
  code of the compiler's around it (ADR 0079, see "Inline assembly").

### Aggregates by value (ADR 0025, 0039, 0075)

Structs, arrays and enums with payloads are parameters and return values
like any other type, passed as the C ABI of the target says, so that Erebos
and C call each other with them. On `riscv32imac-unknown-none-elf`:

| Aggregate | Argument | Return value |
|-----------|----------|--------------|
| size 0 | not passed | nothing |
| up to 4 bytes | one register | `a0` |
| up to 8 bytes | a register pair | `a0`, `a1` |
| larger | a pointer to a copy the caller makes | memory the caller provides, through a hidden first argument |

On `thumbv7m-none-eabi` the AAPCS decides, with the **natural alignment** of
the aggregate: the largest alignment of its fields, elements or variants,
each counted with its own `#[align(N)]`, but without the aggregate's own
`#[align(N)]` (1 for a `#[packed]` struct):

| Aggregate | Argument | Return value |
|-----------|----------|--------------|
| size 0 | not passed | nothing |
| 1 to 64 bytes, natural alignment up to 4 | its words, as C's `[N x i32]`, in `r0` to `r3`, then on the stack | size 1, 2, 3 or 4: in `r0`; larger: memory the caller provides, through a hidden first argument |
| 1 to 64 bytes, natural alignment 8 or more | its double words, as `[N x i64]`: it starts at an even register or an 8-aligned stack slot | as above |
| more than 64 bytes | copied into the argument area, aligned to its natural alignment, 4 or 8 (`byval`); its first words may travel in registers | memory the caller provides |

Where an argument that does not fit the registers left is split, its first
words go in registers and the rest on the stack; the rest of the placement
is the AAPCS's, which the compiler does not restate. The `#[align(N)]` of a
struct changes where it is stored, not how it is passed: a struct of four
words with `#[align(16)]` goes as `[4 x i32]`, and one holding a field of
that type as `[2 x i64]`. An aggregate of more than 64 bytes whose own
alignment exceeds that of its argument area is copied by the callee into a
slot of its own alignment, so that its address always has the alignment its
type promises. The bytes of padding, of a register beyond the aggregate
and of a return in `r0` beyond the aggregate's size are unspecified.

On both targets a parameter is always the callee's own copy: changing it
never changes the caller's value. Arrays pass like a struct of the same
layout; C, which has no arrays by value, wraps them in a struct. A slice
passes like the C struct `struct { const T *ptr; size_t len; }`: on riscv32
in a register pair, and returned in `a0`, `a1` (ADR 0031); on thumbv7m as
two words, and returned through memory. An enum with payloads passes by its
size and alignment, as its C twin, a union, does (see "Enums"); it is
never of size 0, since its tag has a size. Enums without payloads and
bit-field structs are not aggregates: they pass as their integer type
(see "Bit-field structs").

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
  values run in the order written. Only a literal of a bit-field struct
  may leave fields out, with `..` (see "Bit-field structs"); `..` in a
  literal of an ordinary struct is `resolve::missing-default`, its fields
  having no defaults.
- A field is private to the struct's module unless declared `pub`
  (`pub x: i32`): outside, it cannot be named, and the struct cannot be
  built with a literal (ADR 0050, see "Visibility"), except that a literal
  of a bit-field struct may leave out invisible fields with `..`.
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
  `(*s)[i]` with a slice `s` (see "Slices"). A pointer to a peripheral
  type cannot be indexed (see "Pointers").
- `let`, assignment and reading a place copy aggregates whole. `==` and
  `!=` do not apply to them; compare their parts.
- A struct may contain itself only behind a pointer (`next: *mut Node`).
- In the condition of `if` and `while`, a `{` opens the body, so a struct
  literal there needs parentheses: `if (Point { x: 0, y: 0 }).x == 0 { }`.

### Addresses: `&raw`

`&raw const place` and `&raw mut place` give a `*const T` or `*mut T` to a
place: a variable, a static, a peripheral, a byte string, `*pointer`,
`a[i]`, `p[i]`, `(*s)[i]` or a field. `&raw mut` needs a mutable place:
not a variable without `mut`, a `static`, an `extern static` without
`mut`, a byte string, part of a temporary, nor anything reached through a
`*const` pointer (`ty::address-of-immutable`). A peripheral is not `mut`,
yet `&raw mut UART0` is allowed: what may be written is its registers'
business (ADR 0058). The result is never a volatile pointer; one to a
peripheral type, as `&raw mut UART0` gives, is accessed volatile through
its pointee (see "Peripherals"). A register has no address
(`ty::register-address`), nor has a register array, an element of one or
a cluster array taken whole; a cluster, single or an element of a cluster
array, has one: `&raw mut PLIC.ctx[1]` is a pointer to the cluster's
peripheral type (ADR 0064). Nor has a bit field, a field of a
bit-field struct, which is no place in memory (`ty::bit-field-address`,
see "Bit-field structs"). A plain `&` is reserved for references (M5).

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

## Bit-field structs (ADR 0059, 0062)

```
enum WordLength: u8 { Five = 0, Six = 1, Seven = 2, Eight = 3 }

struct Lcr: u8 {
    pub word_length: WordLength at 0..=1 = .Five,
    pub two_stop_bits: bool at 2 = false,
    pub parity: bool at 3 = false,
    pub break_control: bool at 6 = false,
    pub dlab: bool at 7 = false,
}

let mut l: Lcr = Lcr { word_length: .Eight, .. };
l.dlab = true;
let bits: u8 = l as u8;      // 0x83
let m: Lcr = 0x03 as Lcr;
```

A **bit-field struct** is a struct whose name is followed by `:` and an
**integer type**, `u8`, `u16`, `u32` or `u64`, in which it is stored. Each
field gives the **bits** it occupies after `at`, and may give a
**default** after `=`; effects (see "Peripherals") come between the two.
A field may also declare an **access mode** before its name (ADR 0065):

```
bit_struct = "struct" NAME ":" type "{" [ bit_field { "," bit_field } [ "," ] ] "}"
bit_field  = [ "pub" ] [ "read" | "write" ] NAME ":" type "at" bits [ read_effect ] [ write_effect ] [ "=" expr ]
bits       = end [ "..=" end ]
```

An `end` is an expression without assignment, so the `=` after the bits
always starts the default: `at 7 = false` is bit 7 with the default
`false`. A syntax error, such as a field without `at`, `at 0..1` (a
half-open range is no bits), `at` or `=` in an ordinary struct's field,
`read write` before a field's name (a field has one mode word at most) or
a mode after the name, is `parser::unexpected-token`.

**Field access modes (ADR 0065).** `read` makes a field **read-only** and
`write` **write-only**; a field without a mode is read and written as its
register allows. The words are recognized by position only: `read: bool
at 0` is a field named `read`, and `read write: bool at 0` a read-only
field named `write`. A mode checks accesses only in a volatile place, a
register or a place reached through a volatile pointer (see "Peripherals");
in a local, a static or a value, modes mean nothing, and a field is read
and written whatever its mode. A literal may name a `read` field and `..`
gives it its default. The `typeck` and `mir` dumps show a field's mode.

**Bits and field types.**

- Bits are numbered from 0, the least significant. `at 7` is bit 7 alone;
  `at 0..=1` is bits 0 to 1, both included, lowest first. Both ends are
  `u32` values computed at compile time, in the scope of the struct's
  module (see "Compile-time execution"), so constants name them. Bits no
  field covers are **reserved**.
- A field's type is `bool`, `u8`, `u16`, `u32`, `u64` or an enum without
  payloads on one of those four. Any other type is `ty::bit-field-type`
  (a type already in error is not also reported). A `bool` takes exactly
  one bit; an integer type, and an enum's integer type, need at least as
  many bits as the field has, so that a read of the field is a value of
  its type (`ty::bit-field-width`, also for a `bool` of several bits);
  and every variant of an enum must fit in the field's bits
  (`ty::bit-field-width`, once per variant).
- `at 3..=1` is `ty::reversed-bit-range`, and a field reaching past the
  last bit of the integer type `ty::bit-field-outside`. Fields may not
  share a bit: `ty::bit-field-overlap`, once per pair, at the later field,
  labeling the earlier. Overlapping fields are no failed value: each can
  still be read and written.
- Fields are private to the struct's module unless `pub`. A bit-field
  struct needs a field (`resolve::empty-bit-struct`), and a type after
  `:` other than the four integers is `resolve::bit-struct-type`. Two
  fields with one name are `resolve::duplicate-field`. `#[packed]` and
  `#[align(N)]` are `resolve::misplaced-attribute`: the layout is the
  integer's.
- A bit position or a field type that needs the struct's own bits is
  `ty::cycle`: `at L as u32` with `const L: Lcr`, `size_of(Lcr)` in its
  bit positions or field types (also through a struct holding an `Lcr`),
  or an enum field type with a variant computed from `size_of(Lcr)`.
  A default may use its own struct's size, layout and bits
  (`= size_of(Lcr) == 1`); it is `ty::cycle` only when it needs its own
  value, as through a literal that takes it (ADR 0062).

**Values and literals.**

- A bit-field struct is a type of values, like any struct: a local, a
  parameter, a return value, a field, an element, a payload, a constant
  or a static may have it. It is a **scalar**, not an aggregate: stored,
  passed and accessed as its integer type.
- A literal names every field once, or ends with `..`, after a comma when
  fields come before it, which gives every field it leaves out its
  **default**. `..` is allowed where it leaves out no field. The values
  run in the order written. Reserved bits are 0 in a value built by a
  literal.
- A field left out without a default is `resolve::missing-default`, and
  so is `..` in a literal of an ordinary struct. A literal without `..`
  that leaves fields out is `resolve::missing-fields`, whose help offers
  `..` when every field left out has a default or a write effect.
- A **default** is a value of the field's type computed at compile time,
  once, in the scope of the struct's module. One that fails to evaluate,
  has the wrong type or does not fit in its field
  (`ty::field-value-out-of-range`) is reported once, at the default: a
  literal that takes it reports nothing more, and the body holding that
  literal is not built. It is no failed value of the type. A field with a
  write effect has no default (`resolve::write-effect-default`): `..`
  gives it the value whose write has no effect (see "Peripherals").
- **Visibility.** `..` may leave out a field that is not visible where
  the literal is written, when it has a default or a write effect. A
  literal that names an invisible field, or leaves one out with neither,
  is `resolve::private-field` (ADR 0050).
- `l.f` reads the field: its bits as the field's type, an integer
  zero-extended, a `bool` that is `true` when the bit is 1, an enum
  whose value is those bits. `l.f = v` writes the field, and the other
  fields and the reserved bits keep their values; `l.f += 1` reads the
  field and writes it. A read of a field of a value that is not a place,
  as `(0x83 as Lcr).word_length`, is fine; a write to one is
  `ty::not-assignable`. Unknown and private fields are
  `ty::unknown-field` and `ty::private-field`, as for any struct.
- **A bit field is not a place in memory**: `&raw const l.f` and `&raw mut
  l.f` are `ty::bit-field-address`, also when `l` is a register (not also
  `ty::register-address`), and `offset_of(Lcr, f)` is
  `ty::bit-field-offset`. `&raw` of the whole value is a pointer to its
  integer's bytes, as for any value.
- `==`, `!=`, arithmetic and bitwise operators do not apply
  (`ty::invalid-operand`); a program compares fields, or integers made
  with `as`, which shows that reserved bits take part. A `match` on a
  bit-field struct is `ty::match-type`; a `match` on one of its fields is
  a `match` on that field's type.
- **Enum fields.** An enum field holds whatever its bits hold, so a field
  read is a source of values that are no variant besides memory
  (ADR 0035, 0036; see "Enums"): `(0x83 as Lcr).word_length` is whatever
  bits 0 and 1 of `0x83` are. Such a value is defined as for memory: a
  `match` without a catch-all arm calls the panic function on it. A field
  whose every bit pattern is a variant never gives one.

**Values that do not fit.** A field of `n` bits holds the values from 0 to
2ⁿ − 1, and a value written to it must fit.

- A value known at compile time (see "Checks on values known at compile
  time") that does not fit is `ty::field-value-out-of-range`, in a
  literal, an assignment or a default; a known enum variant always fits.
  A compound assignment is never reported at compile time: the value it
  writes back depends on the field's value.
- Any other value is checked when it is written: if it does not fit, the
  program calls the panic function, with the reason `FieldOutOfRange`
  located at the field's name in the literal or the assignment (see
  "Panic information"). An enum value that is no variant can fail the
  check too. A `bool`, and an integer whose type has exactly the field's
  bits, always fit and have no check; with `-O`, LLVM removes checks it
  proves redundant, as for `x & 0xF`.
- A compound assignment checks the value it writes back: `l.count += 1`
  on a field holding its maximum panics instead of wrapping into the
  neighbouring bits. A program that wants the low bits of a wider value
  masks them itself.

**Conversions.** `as` converts a bit-field struct to any integer type, as
its integer type would (`l as u32` is its bits zero-extended), and an
integer to a bit-field struct only from its own integer type: `0x83 as
Lcr`, `x as Lcr` with `x: u8`. A literal operand takes that integer type.
Every integer is a value of the struct, reserved bits kept. From any
other type, another integer type included, it is `ty::invalid-cast`; so is
`l as Lcr`, and a conversion to `bool`, a pointer, an enum or another
bit-field struct. See "Conversions with `as`".

**Layout, calls and volatile accesses.**

- A bit-field struct has the size and alignment of its integer type, and
  its fields have no byte offset. Its C twin is that integer type. A
  field of an ordinary struct may have it, laid out as that integer.
- It passes and returns as its integer type, zero-extended (ADR 0006,
  0025).
- Through a volatile pointer it is a scalar, accessed in one load or store
  of its integer's width. One on `u64` is `ty::volatile-too-wide`, and
  one of more than a byte reached through a `#[packed]` struct
  `ty::volatile-packed-field`. Reading a field, `(*p).dlab`, is one load
  of the whole value; writing one, `(*p).dlab = true`, is one load and one
  store, not atomic (see "Volatile accesses").

**Evaluation order.** `l.f = v` runs the value, its check, then whatever
the place needs (a pointer, an index and its bounds check), then the read
of the whole value, then the write. `l.f op= v` runs the value, the place,
the read of the whole value, the new field value and its check, then the
write: on a volatile place, a value that does not fit calls the panic
function after the load and before the store. A literal runs its field
values in the order written, each followed by its check; the defaults need
no code. In MIR a bit-field struct is its integer: a field read is a
shift and a mask, a write a mask and an or.

**At compile time.** Bit-field structs work in constants, statics,
`comptime` blocks and expressions as any value. **No value of a
bit-field struct is known at compile time**, in the sense of "Checks on
values known at compile time": not a literal, a constant of the type, nor
`0x83 as Lcr`, and neither is a field read from one nor an integer made
from one with `as`. So `x << (L as u32)` with `const L: Lcr` is checked as
a shift amount that is not known. A field write that does not fit, at
compile time, fails the evaluation (`eval::panic`, "value does not fit
its field").

**Failed values.** A bit position, and a field's type, that are reported
in error are failed values (ADR 0048, see "Failed lengths and
alignments"), and so is a field whose bits are reversed
(`ty::reversed-bit-range`) or reach past the integer
(`ty::bit-field-outside`), or whose width does not suit its type
(`ty::bit-field-width`), or an enum field with a variant whose value
failed. A bit-field struct with a failed value **reaches a failed
value**: its own error is the only one reported, and a body whose locals
or expressions have the type is not built into MIR. An overlap and a
default in error are no failed values.

**What is checked.** When names are resolved: `resolve::bit-struct-type`,
`resolve::empty-bit-struct`, `resolve::duplicate-field`,
`resolve::misplaced-attribute`, `resolve::write-effect-default`,
`resolve::missing-fields`, `resolve::missing-default` and
`resolve::private-field`, as above. Once the values are computed, during
type checking, with the checks that need a failed value skipped, for
every bit-field struct of the program, used or not:
`ty::bit-field-type`, `ty::reversed-bit-range`, `ty::bit-field-outside`,
`ty::bit-field-width`, `ty::bit-field-overlap`,
`ty::field-value-out-of-range` for defaults and `ty::cycle`. When code is
type checked and built: `ty::bit-field-address`, `ty::bit-field-offset`,
`ty::invalid-cast`, `ty::invalid-operand`, `ty::match-type`,
`ty::field-value-out-of-range` for known values written to a field, and,
in a volatile place, `ty::write-to-read-only` and `ty::read-of-write-only`
for a field's access mode (see "Peripherals"). The
`typeck` and `mir` dumps list each bit-field struct after the structs
(see "Building").

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
A read of an enum field of a bit-field struct is a second source (ADR 0059):
the field holds whatever its bits hold, so a value made from an integer
with `as` can give one without memory.

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
  `const` item; a name that names nothing is `resolve::unknown-name`, and
  one that names a local variable, a parameter or an item that is not a
  constant (a static, function, struct or enum) is
  `resolve::not-a-constant`; a module is `resolve::misused-module`. A
  local variable shadows a constant of the same name here as anywhere, so
  it is then `resolve::not-a-constant` too; to compare with the constant,
  write `let x: T => if x == NAME { ... }`.
- **A path** of several segments, such as `uart::LIMIT`, names a variant
  or a constant, as in an expression; one whose last segment names another
  item is `resolve::not-a-constant`, and one whose last segment names
  nothing `resolve::unknown-name`, or `resolve::unknown-variant` after an
  enum. A path followed by patterns in parentheses that names no variant,
  as `uart::LIMIT(p)`, is `resolve::unknown-variant`.
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
positions of a pattern are disjoint, and patterns read no memory, so which
of its alternatives passed does not change whether a later position
matches. A test at an enum position fails on a value that is no variant,
as on any variant it does not name. A chain without a catch-all arm ends
in the call to the panic function, even at a variant whose arms test only
`bool` and integer positions, which no value reaches; nothing tells LLVM
that a tag or a payload value holds a variant.

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
| a bit-field struct on the integer type `T` (ADR 0059) | `size(T)` | `align(T)` |
| an enum with payloads (ADR 0039), `Option(T)` and `Result(T, E)` (ADR 0045) | the largest variant's size, rounded up to the alignment | the largest variant's alignment |
| `()` | 0 | 1 |

`[T]`, `!` and peripheral types have no layout a program can ask for (see
"Types"); a peripheral type's registers, register arrays and clusters are
placed at their offsets, and it has an extent and an alignment instead
(see "Peripherals"). Each
variant of an enum with payloads is laid out as a struct of its tag and its
payload values (see "Enums"). A bit-field struct's fields have no byte
offset (`offset_of` of one is `ty::bit-field-offset`); the struct itself
may be a field of an ordinary struct, laid out as its integer, and its C
twin is that integer type (`tests/c_layout.rs` checks a struct holding
them).

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
  (`[v; N]`), the argument of `#[align(N)]`, the values of a `memory`
  block (see "Memory maps"), and the offsets of registers and addresses
  of peripherals (see "Peripherals") are `usize` expressions, such as
  `[u8; 2 * SECTOR]`; the bit positions of bit fields are `u32`
  expressions, and their defaults values of the field's type (see
  "Bit-field structs"). Two array types are the same when their lengths
  have the same value; diagnostics show the value.

What runs at compile time follows the target exactly (32-bit `usize`,
wrapping arithmetic, the layouts above), and cannot:

- use the enclosing function's variables (`ty::not-comptime`), nor `return`
  or `?` out of it (`resolve::return-in-comptime`);
- access memory through a volatile pointer, or a register however it is
  reached (`eval::volatile-access`), call an `extern fn`, or read or write
  through a pointer made from an integer (computing an address from one,
  as in `&raw mut (*UART).ctrl`, is fine; `&raw mut UART0` is such a
  pointer, see "Peripherals");
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

### Failed lengths and alignments (ADR 0048, 0058)

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

A register's offset, a register's type in error and a peripheral's
address are failed values too (ADR 0058): an offset or an address whose
evaluation fails or that is no `usize`, and a register type that is `!`,
`[T]`, reaches a failed value, or is `ty::register-type` (see
"Peripherals", which also says when a register type reaching a failed
value is reported). So are the `step` and the array length of a register
array or a cluster array in error, and a cluster whose type reaches a
failed value (ADR 0064). So are the bit positions and field types of a
bit-field struct in error, and fields whose bits are reversed, outside
the integer or too narrow or wide for their type (ADR 0059, 0062; see
"Bit-field structs"). A register whose bit-field struct type fails keeps
its width: its peripheral type does not fail with it, and only bodies
using that register are left out.

A type **reaches** a failed value when it is, or contains at any depth, an
array whose length failed, a struct whose alignment failed, a bit-field
struct with a failed value, or a peripheral type with a failed offset or
register type: as a field, an array element, a payload value, a type
argument, or behind a pointer or a slice. `[u8; N]`, `Broken`, `Odd`,
`*const Broken`, `*const [Odd]` and `Option(*const Broken)` all reach one.
So does a peripheral type that contains itself through clusters, whose
`ty::infinite-size` explains it (see "Peripherals").
Such a type counts as erroneous, as one containing `!` does (see "Types"),
and so does a peripheral whose type reaches a failed value or whose
address failed:

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
  `offset_of` in it reaches a failed value, or when it names an erroneous
  peripheral. It is then neither run at compile time nor checked for the
  values known at compile time (see "Checks on values known at compile
  time"): `7 / 0` in a function taking a `*const Broken` reports nothing,
  nor does a constant that calls it. Such errors appear once the failed
  value is fixed.
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

For the same reason, a register's offset whose computation names a
pointer to its own peripheral type, as `read status: u32 at (0x4 as *mut
Regs) as usize` in `Regs`, is a `ty::cycle`, whichever of the offset and
the type's layout is computed first (see "Peripherals").

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
  volatility is accepted, since it only loses permission to write (ADR
  0024); and where a slice is expected, a pointer to an array of its
  elements is accepted, with the array's length (ADR 0031, see "Slices").
  These apply wherever an expected type is known, such as a `let`, an
  argument (including an `assert` message), an assignment, `return`,
  `break` values, struct fields, payload values, array elements, `if`
  branches, `match` arms, the tail of a block and the operand of
  `comptime`. Only the outermost pointer converts, volatility never
  changes, and comparisons accept a `*mut T` and a `*const T` in either
  order.
- A literal takes the type expected where it appears; with no expected type
  it is an error. In `x + 1` the `1` takes the type of `x`. A literal
  operand of `as` takes the target type, `usize` for a pointer target, or
  the struct's integer type for a bit-field struct target (ADR 0017,
  0059). An array literal takes its element type from the expected
  type or from its first element with a type of its own.
- A variant named by the expected type, `.V` or `.V(args)`, takes its enum
  where a literal takes its type, from the same expected type: the
  initializer of a `let`, a constant or a static, the right side of an
  assignment, an argument, a payload value, a struct literal's field, an
  array element and a repeat operand, `return`'s value, the tail of a
  function or a block, `comptime`'s operand, the branches of an `if`, the
  arms of a `match` and the values of `break` where these have an expected
  type, and otherwise from a sibling: an earlier `if` branch, `match` arm
  or `break` of the same loop, another array element, the other operand of
  a binary operator. Unlike a literal, it takes nothing from `as` (ADR
  0044, see "Enums").
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

Known issue, not in Erebos: with `-O` on riscv32 with the M extension,
LLVM's RISC-V backend (seen in 22.1 and in 23.1.3, and from plain C with
clang) can drop the high word of an `i64` product. It needs two or more
`i64` multiplications by a large constant such as `-(2^k - 1)`, with `k`
from 34 to 60, in one basic block; the emitted IR is valid and has no
`nsw`, `nuw` or `inbounds`, and `-O0` is correct. Until LLVM fixes it, the
result of such a program built with `-O` may differ from the same program
built without it, and from `comptime`. See `docs/open-findings.md`.

### Checks on values known at compile time (ADR 0032, 0039)

An expression is **known at compile time** when it is built only from
literals, `const` items, variants of enums without payloads (ADR 0035),
written `E::V` or `.V` (ADR 0044), `comptime` expressions, `size_of`,
`align_of`, `offset_of`, unary `-` and `!`, binary operators, `as` and
parentheses. So `Mode::Run as u32` is known; an `if` or a `match` is not.
Aggregates are never known, and neither is any value of an enum with
payloads, not even a variant without payload such as `Msg::Nop`, nor a
constant or `comptime` value of its type (ADR 0039): so every arm of a
`match` on one is checked. Nothing that reads memory is known: not `*p`,
even through a constant pointer (it may be a device register), not a read
of a static or of a register, not `&raw`. A pointer made from an integer
is known by its address; a constant pointing into a static is not known,
since its address exists only after linking. A bit-field struct is a
scalar but its values are not known either (ADR 0059): not a literal of
one, a constant or `comptime` value of its type, `0x83 as Lcr`, a field
read from one, nor an integer made from one with `as`.

These are errors when the operand is known, not only when it is a literal:

| Check | Code |
|-------|------|
| a shift amount outside 0 to the width of the shifted type minus 1, taken with its own type's sign (a negative amount is out of range) | `ty::shift-out-of-range` |
| a divisor of `/` or `%` that is zero | `ty::division-by-zero` |
| an array index not below the array's length, a register array's or a cluster array's included | `ty::index-out-of-bounds` |
| a value written to a bit field, by a literal or an assignment (not a compound assignment), that does not fit in the field's bits | `ty::field-value-out-of-range` |

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
value (see "Failed lengths and alignments"), whatever errors other bodies
have; `--emit=typeck` stops before them. A body in which they fail is not
run at compile time: an evaluation that needs it reports nothing more.

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
- A bit-field struct converts to any integer type, as its integer type
  would, and an integer type converts to it only from its own: `0x83 as
  Lcr` and `x as Lcr` with `x: u8` (ADR 0059). A literal operand takes
  that integer type. Reserved bits keep what the integer holds. From
  another integer type, from itself (`l as Lcr`), from `bool` or an enum,
  or to `bool`, a pointer, an enum or another bit-field struct, it is
  `ty::invalid-cast`: narrow or widen first, as `x as u8 as Lcr`. See
  "Bit-field structs".
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
- A pointer to a peripheral type, `u: *mut Uart`, reaches one device:
  `(*u).thr` is a register (see "Peripherals"), and `u[i]`, or `&raw mut
  u[i]`, is `ty::peripheral-index`, since a peripheral type has no size,
  so no stride; another device of the same type is reached through a
  pointer to it. A cluster array is indexed at its entry, where its step
  is known: `(*u).ctx[i]`, not through a pointer to its element.

### Volatile accesses (ADR 0013, 0023, 0039, 0058)

An access is volatile when it goes through a volatile pointer, and when
it reaches a register of a peripheral, from the peripheral or through a
pointer to its peripheral type, which is written without `volatile` (see
"Peripherals"). A register access is one volatile load or store of the
register's width, with the guarantee below, and needs none of the checks
below: a register is a `u8`, a `u16` or a `u32`. Device registers are
declared as peripherals; volatile pointers stay for memory shared with a
device that is not a register (a DMA buffer, a frame buffer, memory
another core writes) and for addresses known only at run time (ADR 0013,
amended by 0058).

Through a volatile pointer, every access is to a scalar (an integer,
`bool`, enum without payloads, bit-field struct or thin pointer; such an
enum or struct is accessed as its integer type, ADR 0035, 0059) and is
**exactly one load or store of the scalar's width**, never split, merged,
removed or reordered relative to other volatile accesses, register
accesses included. Volatility carries through fields and indexing:
`(*regs).chan[i].status` is one access of `status`'s width. `x += 1` is
one load and one store, not atomic. Otherwise the program is rejected:

- a whole struct, array or enum with payloads, or a slice stored in
  memory, cannot be accessed through a volatile pointer; through a
  volatile slice, `(*s)[i]` is one access of the element. An enum with
  payloads has no fields, so a volatile pointer to one reaches nothing;
- `u64` and `i64`, and enums without payloads and bit-field structs on
  them, cannot, since neither target makes a 64-bit access in one bus
  access (on ARMv7-M, `ldrd` and `strd` are one instruction that makes
  two): use two `u32` halves in the order the device needs; an enum,
  which nothing converts to, needs an integer type of at most 32 bits, and
  so does a bit-field struct, which only its integer converts to
  (`ty::volatile-too-wide`);
- a field reached through a packed struct cannot be, if its type needs
  alignment, a bit-field struct on `u16` or `u32` included.

A field of a bit-field struct is read through a volatile pointer with one
load of the whole value, and written with one load and one store of it:
`(*p).dlab = true` is a read-modify-write, not atomic. Whether other
fields are written with a neutral value is a matter of write effects (see
"Peripherals").

Volatile accesses are not ordered against ordinary ones, and the compiler
emits no fence of its own. Handing memory to a device (DMA), or reading
memory a device wrote, is written with the fences of `core::arch` (ADR
0081, see "`core`"): a driver that fills a descriptor with ordinary writes
and then rings a doorbell calls `core::arch::fence()` between them,

```
DESCRIPTORS[0] = Descriptor { addr: buf, len: 64, flags: 1 };
core::arch::fence();
DEVICE.doorbell = 1;
```

and one that reads the buffer after a "done" status calls it between the two
reads. `compiler_fence()` stops the compiler from moving accesses across it
and emits no instruction; `fence()` also orders the accesses in the
hardware (`fence iorw, iorw` on riscv32, `dmb sy` on thumbv7m). Both are
`asm` (see "Inline assembly") and run at run time only.

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
(ADR 0031). In an assignment the value runs before the place. In an
assignment to a bit field, `l.f = v`, the value and its check run first,
then the place, then the read of the whole value, then the write; in `l.f
op= v` the check of the new field value comes after the read (ADR 0059,
see "Bit-field structs"). In `base[index]`, the base runs before the
index, and the bounds check before the access; so it is for the index of a
register array or a cluster array (ADR 0064). A `match` runs its
scrutinee once, before any arm, and its patterns run no code (ADR 0037,
0040). A read through a pointer, a read of a static (ADR 0030) and a read
of a register (ADR 0058) happen exactly where they are written. A value
that is not a place, used as the base of indexing or a field, is read
where it is written too: in `({ x })[{ x[0] = 9; 0 }]` the block reads `x`
before the index changes it. `e?` runs `e` where it is written, and when
it returns from the function, nothing after it runs: in `f(a()?, b())`,
`b()` does not run when `a()` fails (ADR 0046). In `asm`, the operands and
the places of the outputs run left to right before the code, and the outputs
are stored after it, left to right (ADR 0079, see "Inline assembly").

## Inline assembly (ADR 0079, 0082)

Some instructions have no expression in the language: a control register
(`csrr`, `mrs`), `wfi`, a fence, a semihosting request. `asm` is an
expression that copies a template of assembly into the program, with
operands that carry values in and out of registers:

```
let mut cause: u32 = 0;
asm("csrr {}, mcause", out(reg) cause, options(nomem, nostack));

let mut x: u32 = 5;
asm("add {x}, {x}, {y}", x = inout(reg) x, y = in(reg) 3 as u32,
    options(pure, nomem, nostack));

asm("wfi", options(nomem, nostack));
```

The template is assembly for one target, so a program that holds `asm`
builds for the targets whose instructions it uses (the tests say it with `//
targets:`, see "Targets"); `core::arch` has the portable wrappers (see
"`core`").

### Syntax

```
asm_expr  = "asm" "(" STRING { "," STRING } { "," operand }
            [ "," "options" "(" option { "," option } [","] ")" ] [","] ")"
operand   = [ NAME "=" ] ( "in" "(" where ")" expr
                         | ( "out" | "lateout" ) "(" where ")" out_place
                         | "inout" "(" where ")" expr [ "=>" out_place ]
                         | "sym" ( path | "entry" )
                         | "const" expr )
          | "clobber_abi" "(" STRING ")"
where     = "reg" | STRING
out_place = place | "_"
option    = "pure" | "nomem" | "readonly" | "nostack"
          | "preserves_flags" | "noreturn"
```

- `asm` starts an expression, which is a statement with a `;` (`asm(...);`)
  or the tail of a block. Its type is `()`, or `!` with the option
  `noreturn`. A trailing comma is allowed after the last argument.
  `options(...)` must be the last argument: an operand after it is
  `parser::unexpected-token`.
- The **templates** are the string literals first in the parentheses (see
  "Lexical structure"). They are joined with line breaks into one template,
  so `asm("nop", "nop")` is two instructions. A template holds ASCII bytes
  from `0x01` to `0x7F`; a byte outside them, written with `\xNN` or `\0`, is
  `ty::asm-template`. The compiler does not read the instructions: the
  template is copied into the object with the placeholders replaced, and a
  `$` or any other character is written as it is.
- The **operands** come in three groups, in this order: operands without a
  name, then named operands, then operands with an explicit register and
  `clobber_abi`, which have no name and no number. An operand out of its
  group, or a name on an operand with an explicit register or on
  `clobber_abi`, is `parser::unexpected-token`. A name is an identifier
  followed by `=`; `in`, `out` and the other words of `asm` are valid names.
- The words `in`, `out`, `lateout`, `inout`, `sym`, `clobber_abi`,
  `options`, `reg` and the option names are recognized only in these
  positions; elsewhere they are ordinary names.

### Operands

| Operand | Meaning |
|---------|---------|
| `in(where) e` | passes the value of `e` in a register |
| `out(where) p` | stores into the place `p`, or nowhere with `_`, what the register holds when the code ends |
| `lateout(where) p` | as `out`, with a register an input may also use, since it is written only after every input is read |
| `inout(where) e` | passes `e` and stores the register back into `e`, which must be a place, or into the place after `=>` (`inout(reg) x => y`, `inout(reg) x => _`) |
| `sym path` | the symbol of a function, an external function, a static or an external static (see "Symbols"); the item counts as used, so it is kept in the object. Any other item is `ty::asm-sym`; a name that names nothing is `resolve::unknown-name` |
| `sym entry` | the symbol of the entry function, whatever its module and name (ADR 0083): `entry` is a keyword, so no path means it, and nothing follows it (`sym entry::x` is `parser::unexpected-token`). Allowed in any `asm`, also of a program and in a naked function; it is a jump the template writes, so `resolve::call-entry` does not apply |
| `const e` | a value of any integer type, `u64` and `i64` included, computed at compile time (ADR 0027) and written in decimal, with a `-` when negative. A run-time variable is `ty::not-comptime` |
| `clobber_abi("C")` | every register a call of a C function may change is overwritten (see "Registers") |

- `where` is `reg`, which lets the compiler choose a general-purpose
  register, or a string naming one register (see "Registers").
- The **type** of an `in`, `out`, `lateout` or `inout` operand is an integer
  of at most 32 bits (`usize` and `isize` included) or a thin raw pointer
  (to a sized type, not a slice). Anything else (`u64`, `bool`, an enum, a
  bit-field struct, an array, a struct, a pointer to a slice) is
  `ty::asm-operand-type`; convert with `as`. The two sides of an `inout ...
  =>` have one type. An output cannot go to a bit field of a bit-field
  struct, which has no place in memory (`ty::asm-operand-type`): write it to
  a variable first. There is no expected type, so a literal needs `as`
  (`in(reg) 3 as u32`, `const 0x100 as u32`).
- The place of an `out`, `lateout` or `inout` follows the rules of an
  assignment (`ty::assign-immutable`, `ty::write-through-const`,
  `ty::not-assignable`), so a local it writes is `let mut`; the expression of
  an `inout` without `=>` must be a place, or it is `ty::not-assignable`.
- **Widths.** An input narrower than 32 bits fills its register extended by
  its signedness: zeros above a `u8` or `u16`, copies of the sign bit above an
  `i8` or `i16`. An output narrower than 32 bits takes the low bits of its
  register.
- **Evaluation order (ADR 0018).** The operand expressions and the output
  places (their indices, with their checks, and the calls in them) are
  evaluated left to right before the code runs; a check that fails there
  calls the panic function and the code does not run. After the code the
  outputs are stored left to right, so of two outputs to one place the right
  one stays. The place of an `inout` without `=>` is evaluated once: read
  before the code and written after it, so `inout(reg) A[f()]` calls `f` and
  checks the index once.

### Placeholders

`{}` takes the next operand of the first group (those without a name or a
register), `{N}` the `N`-th of them, counting from 0, and `{name}` the
named one; `{{` and `}}` are the braces. A placeholder may appear any number
of times. It becomes the register the compiler chose for the operand, the
symbol of a `sym`, or the value of a `const`. Nothing else goes between the
braces: `{0:x}` and `{ }` are no placeholders. An operand with an explicit
register cannot be placed, since the template writes the register itself
(naming a register in braces is a placeholder that names no operand, whose
help says so), and every other operand must appear at least once. The
mistakes are `ty::asm-template`: a placeholder naming no operand, an unused
operand, an unmatched brace, a `{}` or `{N}` past the last operand without a
name, two operands with one name, and a byte outside `0x01` to `0x7F`. The
label is on the placeholder, through the escapes of the literal, or on the
operand.

### Registers

A string names one register, by the architecture's name or the ABI's.
`ty::asm-register` reports an unknown name, listing the target's, and a
register the compiler keeps for itself:

| | riscv32 | thumbv7m |
|-|---------|----------|
| Names | `x0` to `x31`, `zero`, `ra`, `sp`, `gp`, `tp`, `t0` to `t6`, `s0` (`fp`) to `s11`, `a0` to `a7` | `r0` to `r15`, `sb` (`r9`), `sl` (`r10`), `fp` (`r11`), `ip` (`r12`), `sp` (`r13`), `lr` (`r14`), `pc` (`r15`) |
| Reserved | `x0` (`zero`), `x2` (`sp`), `x3` (`gp`), `x4` (`tp`), `x8` (`s0`, `fp`), `x9` (`s1`, LLVM's base pointer) | `r6` (LLVM's base pointer), `r7` (the frame pointer in Thumb code), `r13` (`sp`), `r15` (`pc`) |
| `clobber_abi("C")` | `ra`, `t0` to `t6`, `a0` to `a7` | `r0` to `r3`, `r12`, `lr`, and the condition flags |

Two names of one register are the same register, so `a0` and `x10` conflict.
Also `ty::asm-register`: one register named twice among the inputs, or twice
among the outputs; a register named by an `in` and by a plain `out`, which
promises a register no input uses (write `inout`, or `lateout` when the output
may reuse the input's register); and an output on `reg`, `lateout(reg)` or
`inout(reg)` beside `clobber_abi`, since the compiler could choose a register
the call overwrites (an `in(reg)` is fine). A register that `clobber_abi`
covers may also be an explicit operand: as an output it is that output, not
a clobber; as an input only, its value is lost when the code ends, as with
`inout(...) e => _`. The registers the code does not list must be as it
found them, the stack pointer balanced.

### Meaning and options

The code is copied into the object as written. Without options, an `asm`
expression:

- runs every time it is evaluated, in program order relative to calls,
  volatile accesses (ADR 0023) and other `asm` expressions;
- may read and write any memory the program can reach, so it is a compiler
  fence: no access to statics or memory reached through pointers moves across
  it;
- may push to the stack, so the compiler aligns the stack pointer as the C ABI
  wants before it;
- may change the condition flags (the ARM ones; riscv32 has none).

The code must not jump out of the template unless `noreturn` says so; breaking
a promise is not defined, and the compiler cannot see it. The compiler may
copy the template, by inlining or by unrolling a loop, so labels inside it
must be local (`1:` and `1b`, `1f`).

| Option | Promise | Effect |
|--------|---------|--------|
| `nomem` | reads and writes no memory | memory accesses may move across it; not a compiler fence |
| `readonly` | reads memory, writes none | writes may not move across it, reads may |
| `pure` | has no effect but its outputs, which depend only on its inputs and, with `readonly`, memory | may be removed when its outputs are unused, or evaluated once for equal inputs; needs `nomem` or `readonly` and an output |
| `nostack` | does not touch the stack | the stack is not aligned for it |
| `preserves_flags` | leaves the condition flags as they were | no effect on riscv32, nor beside `clobber_abi("C")`, which covers the flags |
| `noreturn` | never ends | the expression has type `!`; no output operands |

Options that contradict each other or the operands (`nomem` with `readonly`,
`pure` without an output or without `nomem` or `readonly`, `noreturn` with
an output, `out(reg) _` and `inout ... => _` counting as outputs), an option
given twice, `clobber_abi` given twice, and an ABI other than `"C"` (the
only one that exists, ADR 0006) are `ty::asm-options`. Modifiers in
placeholders (`{0:x}`), `inlateout` and ABIs other than C do not exist.

### Naked functions

```
#[export]
#[naked]
fn __aeabi_uldivmod() {
    asm(
        "push {{r4, lr}}",
        "sub sp, sp, #16",
        "add r4, sp, #8",
        "str r4, [sp]",
        "bl {div}",
        "ldrd r2, r3, [sp, #8]",
        "add sp, sp, #16",
        "pop {{r4, pc}}",
        div = sym crate::builtins::__erebos_udivmoddi4,
        options(noreturn),
    );
}
```

`#[naked]` makes a function with **no code of the compiler's**: no prologue,
no epilogue, no stack frame. Its body is exactly one `asm` expression, as a
statement or as the tail of the block, with `noreturn` and only `sym` and
`const` operands, and does everything, returning included
(`ty::naked-body` for a body that is not that: another statement or item, no
`asm`, no `noreturn`, an `in`, `out`, `inout` or `clobber_abi`). The
parameters and the return type say how C calls it, by the C ABI of the target,
and the template finds the arguments where the ABI puts them; the body
cannot name a parameter, and a run-time variable in a `const` operand is
`ty::not-comptime`. A naked function is called like any other and is the one
way to write a function with a convention no signature has. `#[naked]` goes on
a function with a body, and combines with `#[export]`; on an `extern fn`,
the entry function, the panic function or any other item it is
`resolve::misplaced-attribute`, and twice
`resolve::duplicate-attribute`. LLVM gets the function with the `naked` and
`noinline` attributes.

### At compile time, and errors of the assembler

`asm` cannot run at compile time: an evaluation that reaches one, a call of a
naked function and of `core::arch`'s fences included, is `eval::asm`, with
the `asm` as the first label (inside `core`'s file for the fences). A
function that holds `asm` is still type checked and can be called at run
time.

The compiler checks operands, registers, types and placeholders; LLVM's
assembler reads the instructions when the object is built, so `--emit=typeck`,
`mir` and `llvm-ir` accept a template that the build rejects. An instruction
the assembler rejects (an unknown mnemonic, a register that does not exist, an
immediate out of range) is `codegen::asm`, at the `asm` expression, with the
assembler's message and the position in the template. Each `asm` is reported
once however many copies the optimizer made, in the order of the source, and
the compiler exits with an error and no object. Some errors exist only in the
whole object: a non-local label defined twice by inlining, an instruction that
fails only with the registers chosen, or more `reg` operands than there are
free registers. When the compiler cannot tell which `asm` LLVM rejected, it
reports `codegen::asm` without a label, with LLVM's message and a help that
says so. An error that is not about inline assembly stays `codegen::llvm`
(ADR 0082). A template that makes LLVM abort the process instead of
reporting (LLVM's `report_fatal_error`) is outside this: it ends the
compiler.

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
  two items. Peripheral types and peripherals are items like any other
  (ADR 0058): a peripheral type named like a struct of its module, or a
  peripheral named like its own type, is `resolve::duplicate-item`.
  The names of a peripheral type's entries (registers, register arrays and
  clusters) belong to it, as field names to their struct.
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
  `Option(uart::Config)`, `*const uart::regs::Block`, `*mut
  uart::Uart16550`), constants, statics, peripherals (`uart::UART0.thr`),
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
  struct`, `pub enum`, `pub const`, `pub peripheral`, `pub mod`, `pub
  use`, `pub panic fn`, `#[export] pub fn`. On a `comptime` or `memory`
  block, which has no name, and on the entry function, which nothing may
  call, it is `parser::misplaced-pub`. Before a field or a register it is
  allowed (below); before anything else, such as a variant, a parameter, a
  `let` or another `pub`, it is `parser::unexpected-token`.
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
  The exception is a bit-field struct (ADR 0059): `..` may leave out a
  field that is not visible, when it has a default or a write effect. A
  literal that names an invisible field, or leaves one out with neither,
  is still `resolve::private-field`.
- Visibility changes no layout: private fields are laid out, copied and
  passed like any other, and `size_of` counts them.
- **Registers** follow the rules of fields (ADR 0058, 0064): a register,
  a register array or a cluster is
  private to the module of its peripheral type unless declared `pub`
  (`pub read lsr: u8 at 5`), and a `pub` one is visible wherever its
  peripheral type is reachable, and only there. Where a register is not
  visible, no place may name it, through a peripheral or through a
  pointer (`ty::private-register`): a `pub` peripheral of a private
  peripheral type is reachable, but its `pub` registers are not seen
  through it, as the `pub` fields of a `pub static` of a private struct
  are not. A driver module can keep its registers private and offer
  functions.

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
  `extern static`, struct, enum, constant, peripheral type, peripheral,
  module, `use`, `comptime` block) it is `resolve::misplaced-attribute`.
- An **`extern fn` or `extern static`** always has its own name, in any
  module: it names a symbol defined outside the program.
- The entry and panic functions follow the same rules. `sym entry` (see
  "Inline assembly") is the entry function's symbol, whatever it is, and
  it is how `core`'s `_start` jumps to it.
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
  without `#[export]`, as a function, a `static`, an `extern fn` or an
  `extern static`, because `core::arch` defines it (ADR 0083): the program gets the one
  `resolve::reserved-name`, whose help says so, and not also a
  `resolve::duplicate-symbol` against an item of `core` it cannot name. So
  are names starting with `__` (ADR 0052).
- Symbols are not visibility: every function and static, private or not,
  is emitted as an external symbol of the object file (ADR 0030). Byte
  strings and other constant data keep private names, and bodies run only
  at compile time are not emitted.
- Peripheral types and peripherals have no symbol: nothing is emitted for
  them, and a peripheral's address appears in the code that uses it
  (ADR 0058).

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
  (`.rodata`) and the initial values of `.data`. The reset entry starts at
  `R`'s origin (on thumbv7m, with the two words of a vector table first, so
  that `_start` is 8 bytes after it). The origin must be a multiple of the
  target's code alignment, 4 on riscv32 and 128 on thumbv7m
  (`ty::misaligned-code`), and must be the address where the board starts
  at reset (it jumps there on riscv32, and reads the vector table there on
  thumbv7m), which the compiler cannot check. On QEMU `virt` run with
  `-bios none`, as the tests do, that is `0x8000_0000`, the start of its
  RAM; with QEMU's default firmware, OpenSBI, loaded there, such a program
  does not boot. On QEMU `mps2-an385` it is `0x0`. The 128 of thumbv7m is
  the alignment ARMv7-M requires of a vector table (`VTOR` holds multiples
  of 128), since the reset entry of that target starts with one (see
  "Targets").
- `data in R;` puts in `R` the run addresses of `.data`, then `.bss`.
  `R` may be the code's region, for a board that runs from RAM: the
  initial values of `.data` are then at its run addresses, and nothing
  else is set aside for them.
- `stack in R { size: E }` reserves `E` bytes for the stack at the
  **start** of `R`, below anything else `R` holds. Its start is `R`'s
  origin rounded up to the target's stack alignment, 16 on riscv32 and 8
  on thumbv7m, and `E` must be a multiple of it. The stack grows down, so
  one that overflows leaves its region below its origin, where many chips
  have no memory or memory that faults, instead of running over `.data` and
  `.bss`. Nothing detects the
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
| `ty::misaligned-code` | the code's region starting at an address that is not a multiple of the target's code alignment (4 on riscv32, 128 on thumbv7m) |
| `ty::stack-size` | a stack of size 0, or not a multiple of the target's stack alignment (16 on riscv32, 8 on thumbv7m), or one that does not fit in its region once its start is rounded up |

These two read the target of the build (`--target`): a code origin of
`0x40` is an error only on thumbv7m, and a stack of 24 bytes only on
riscv32, and the message names the target's value. A stack size that is both
not a multiple of the alignment and too large is reported as not a
multiple. A value whose computation failed is reported once,
where it is written, with the errors of any compile-time evaluation or a
`ty::mismatched-types`, and the checks that need it are skipped: a region
whose origin failed overlaps nothing.

**The linker script.** `erebos build --emit=linker-script` prints the
script the compiler passes to `ld.lld`:

- `MEMORY` lists every region in source order under its own name, so that
  the script and the linker's messages read like the source.
- In the code's region: the reset entry, which is `_start` in an output
  section of its own, `.text.start`, so that only its own alignment counts
  and an object given with `--link` whose code is aligned to 64 cannot move
  it off the origin; on thumbv7m the two words of the vector table come
  first, in `.vector_table`, which `KEEP` retains (ADR 0083). Then `.text`,
  `.rodata` and the initial values of `.data`.
- In the stack's region: a section `.stack` without contents, from
  `__stack_start` to `__stack_end`, the bounds of the stack. In the data's
  region: `.data`, then `.bss`. `.stack` comes before both, so that it is
  first in its region whether or not that region also holds the data.
- The rest is as before (ADR 0030): `.data` and `.bss` aligned to 4 and
  padded inside, the symbols of the startup loops, the small-data
  sections, the sections that are not loaded, and a link error for a
  section the script does not place.
- Two places depend on the target (ADR 0074): the script's first line,
  `OUTPUT_ARCH(riscv)` or `OUTPUT_ARCH(arm)`, and the section of the object's
  attributes that is not loaded, `.riscv.attributes` or `.ARM.attributes`.
  On thumbv7m the sections `.ARM.exidx*` and `.ARM.extab*`, the unwind
  tables that every function of an ARM object has, are discarded
  (`/DISCARD/`): nothing unwinds. Any other section the script does not name
  is still an error.

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

## Startup and memory (ADR 0030, 0057, 0083)

`_start` is the reset entry. It is written in Erebos, a private `#[export]`
`#[naked]` function of `core::arch` (see "`core`"), in `rt/arch/riscv32.ers`
and `rt/arch/armv7m.ers`: that is where to read it, since `--emit=llvm-ir`
leaves `core` out and so does not show it. The compiler names
`core::arch::_start` by path and puts it in the section `.text.start`, alone
at the origin of the code's region (see "Memory maps"; on thumbv7m two words
of a vector table come first, see "Targets"). It does, in order:

1. set the stack pointer to `__stack_end`, the end of the stack the
   `memory` block declares;
2. fill `.bss` with zeros;
3. copy the initial values of `.data` from their load address to their
   run address; when the data shares the code's region the two are one,
   and each word is copied onto itself;
4. jump to the entry function, through `sym entry` (see "Inline assembly").

Nothing else runs before the entry function: there are no constructors and
no order of initialization. The loops work a word at a time between
symbols the linker script defines (`__bss_start`, `__bss_end`,
`__data_start`, `__data_end`, `__data_load`), test their bounds before the
first word and do nothing when a range is empty. `_start` takes
`__stack_end` from the script too, so the LLVM IR of a program is the same
whatever its board. The jump reaches the entry function wherever it is in
the address space: on riscv32 it is a `tail` (`auipc` and `jalr`, not a
`j`, which reaches only 1 MiB each way), on thumbv7m a `movw` and `movt`
into a register and `bx`.

A program cannot define `_start`, replace it or run code before it (see
"Symbols"). Code that must run early, such as raising a clock, runs first in
the entry function, after `.data` is copied.

Object files given with `--link` may use the small-data sections
(`.srodata`, `.sdata`, `.sbss`, as GCC emits by default and clang with
`-msmall-data-limit`): they are gathered with `.rodata`, `.data` and
`.bss`, and initialized likewise. A section the script does not place is a
link error, not data left uninitialized. No `__global_pointer$` is
defined, so nothing is addressed relative to `gp`.

## Targets (ADR 0073 to 0077)

`erebos build --target <name>` chooses the machine the program is built
for. There are two:

| | `riscv32imac-unknown-none-elf` (the default) | `thumbv7m-none-eabi` |
|-|---------------------------------------------|----------------------|
| Machine | RV32IMAC, little-endian | ARMv7-M, Thumb-2, as on the Cortex-M3, little-endian, no floating-point unit |
| LLVM | `riscv32-unknown-none-elf`, `generic-rv32`, `+m,+a,+c` | `thumbv7m-none-eabi`, `cortex-m3` |
| C ABI | the riscv32 one (ADR 0006, 0025) | the AAPCS, base standard (ADR 0075) |
| Pointers, `usize`, widest access | 32 bits | 32 bits |
| Stack alignment | 16 | 8 |
| Code alignment | 4 | 128 |
| 64-bit `/` and `%` | LLVM's | a call to `core` |

An unknown name is an error that lists the known ones: ``error: unknown
target `cortex-m3`; the targets are riscv32imac-unknown-none-elf,
thumbv7m-none-eabi``. Without the flag the target is riscv32, so a program
written for an ARM board and built without it gets the riscv32 checks and
code. The names say an instruction set and an ABI, not a board; the board is
the program's `memory` block and its peripherals. Only `erebos build` takes
the flag; `erebos svd` writes source that does not depend on the target.
Objects given with `--link` must be built for the same target, which
nothing checks before the linker. The language is the same on both: a
program that compiles for one has the same types, layouts and values on the
other; what differs is the machine code, the calling convention with C, the
reset entry and the memory map checks (`ty::misaligned-code`,
`ty::stack-size`, see "Memory maps"). The pointer width stays 32 bits on
every target, which `usize` and the layouts rely on.

**The reset entry of thumbv7m (ADR 0074).** A Cortex-M does not start at an
address: at reset it reads two words at the address of its vector table
(`0x0` on `mps2-an385`), the initial stack pointer and the address of the
reset handler, whose bit 0 is set to mark Thumb code. The reset entry is
therefore two sections at the origin of the code's region: `.vector_table`
first, which holds those two words, `__stack_end` and `_start`, and then
`.text.start`, which holds `_start` (ADR 0083). The compiler emits the two
words as module-level assembly, which `--emit=llvm-ir` shows as `module asm`;
`_start` is `core`'s, in Thumb assembly (see "Startup and memory"), so it is
8 bytes after the origin. The code's origin must be a multiple of 128 because
of the table. `_start` does what it does on riscv32: it sets the stack
pointer again (the processor already loaded it, but a program that jumps back
to `_start`, as a startup test does, gets a fresh stack), zeroes `.bss`,
copies `.data`, and jumps to the entry function. The table has no other
entry: an exception (a fault, an NMI) reads its handler from the words after
the second, which are `_start`'s code, faults again and locks the processor
up. Exception handlers and the rest of the vector table come with M4.3.

**Calls and arithmetic.**

- Aggregates cross calls as the AAPCS says (see "Aggregates by value").
  Integers narrower than 32 bits and `bool` are extended, and `u64` and `i64`
  take an even and an odd register or 8 aligned bytes of the stack, as in C.
- `/` and `%` on `u64` and `i64` call `core`'s `__udivdi3`, `__umoddi3`,
  `__divdi3` and `__moddi3` on thumbv7m (ADR 0076), instead of an LLVM
  division: LLVM would call `__aeabi_uldivmod` and `__aeabi_ldivmod`, whose
  convention (the quotient in `r0` and `r1`, the remainder in `r2` and `r3`)
  no function with the C ABI has. The call is in `--emit=llvm-ir`, after the
  checks of the divisor, and costs `core`'s loop of 64 steps even for a
  constant divisor, which on riscv32 `-O` mostly expands inline. Divisions
  of 32 bits and fewer stay instructions on both targets (`udiv` and `sdiv`
  exist on ARMv7-M). Constant evaluation is the same on both.
- C code linked into a thumbv7m program divides 64-bit values: clang calls
  `__aeabi_uldivmod` and `__aeabi_ldivmod`, and `core` defines them (ADR
  0080), as naked functions of `core::arch` (see "Inline assembly") that
  take the dividend in `r0` and `r1` and the divisor in `r2` and `r3`, and
  return the quotient in `r0` and `r1` and the remainder in `r2` and `r3`,
  the remainder with the sign of the dividend. Each calls a function of
  `core::builtins` in Erebos that returns the quotient and stores the
  remainder through a pointer, reusing the loop of `__udivdi3`. Erebos code
  does not call them: it divides through the functions of ADR 0076, as
  above. A zero divisor gives what `core`'s loop gives (a quotient of all
  ones and the dividend as the remainder, before signs); C leaves it
  undefined, and the EABI's call of `__aeabi_ldiv0` is not made. On riscv32
  C code calls `core`'s four functions by their libgcc names, and
  `__aeabi_*` are not defined. A program of its own cannot define either
  name: names starting with `__` are reserved on every target
  (`resolve::reserved-name`).
- LLVM copies and fills memory on thumbv7m through the EABI's functions,
  which `core` defines (see "`core`").

**Tests on both targets (ADR 0077).** The tests run the e2e programs on QEMU
`virt` (`qemu-system-riscv32`) for riscv32 and on QEMU `mps2-an385`
(`qemu-system-arm`: a Cortex-M3 with 4 MiB of RAM at `0x0` and at
`0x2000_0000` and a CMSDK UART at `0x4000_4000`) for thumbv7m. A program
reaches its machine through `mod board;`: the harness supplies the file
`tests/boards/<target>.ers`, which holds the machine's `memory` block and
`putc`, `exit` and `SCRATCH`; a program for a real board writes its own the
same way. The language has no conditional compilation, so a program that
needs one machine says it in a header, `// targets: <name>`, and runs only
there. A program that holds one target's assembly, or names an item of
`core::arch` specific to one target, lists only that target (ADR 0079,
amending 0077). On `mps2-an385` the board stops QEMU through semihosting,
with `asm("bkpt 0xab", ...)` in Erebos and no C file (the harness still links
a board's C file when a board has one). C files of the tests are compiled
with `clang --target=thumbv7m-none-eabi -mcpu=cortex-m3 -mfloat-abi=soft
-ffreestanding` for thumbv7m. The ui tests take `// target: <name>`, riscv32
by default.

## Peripherals (ADR 0058)

```
const UART_BASE: usize = 0x1000_0000;

peripheral Uart16550 {
    read rbr: u8 at 0x00,
    write thr: u8 at 0x00,
    read write ier: u8 at 0x01,
    read lsr: u8 at 0x05,
}

peripheral UART0: Uart16550 at UART_BASE;
peripheral UART1: Uart16550 at UART_BASE + 0x100;

fn putc(u: *mut Uart16550, byte: u8) {
    while (*u).lsr & 0x20 == 0 {}
    (*u).thr = byte;
}

fn hello() {
    UART0.ier |= 0x01;
    UART0.thr = b'h';
    putc(&raw mut UART1, b'i');
}
```

A **peripheral type**, `peripheral NAME { registers }`, describes a
device's registers, each at an offset, with a width and an access mode.
A **peripheral**, `peripheral NAME: TYPE at ADDRESS;`, is an instance of
a peripheral type at an address. Two instances of one design, as `UART0`
and `UART1`, are two peripherals of one type, and a driver taking a
pointer to the type serves both.

**The items.**

- Their grammar:

  ```
  peripheral_type = "peripheral" NAME "{" [ entry { "," entry } [ "," ] ] "}"
  entry           = register | cluster
  register        = [ "pub" ] access NAME ":" type "at" expr [ "step" expr ] [ read_effect ] [ "in" NAME ]
  cluster         = [ "pub" ] NAME ":" type "at" expr [ "step" expr ] [ "in" NAME ]
  access          = "read" | "write" | "read" "write"
  peripheral      = "peripheral" NAME ":" type "at" expr [ "over" path ] ";"
  ```

  A syntax error in either is a `parser::unexpected-token`.
- `peripheral` starts an item only where an item starts and a name follows
  it: a `{` after the name makes a peripheral type, a `:` a peripheral,
  and anything else is an error at that token. `read`, `write`, `at`,
  `step` and `over` are recognized only inside the items. An entry starts,
  after an optional `pub`, with a name followed by `:`, which makes it a
  cluster of that name (it may be named `read`), or otherwise with its
  access mode, which makes it a register whose name is the identifier
  just before `:`. So `read write: u8 at 0` is a `read` register named
  `write`, `read: Context at 0` a cluster named `read`, and `write read
  x: u8 at 0`, a `write` register named `read` with `x` where `:` should
  be, is an error. A register written without its access mode, `rbr: u8
  at 0`, is a cluster, and a cluster of a type that is no peripheral type
  is `resolve::not-a-peripheral-type`, whose help names the missing access
  mode when the type is one a register may have (see "Clusters"). After an
  entry's offset, `step` starts a step, then `read` a read effect (a
  register only; a cluster takes none) and `in` a group (below), in that
  order, and `clears`, `sets`, `toggles` and `modifies` are words only in
  an effect. After a peripheral's address, `over` and a path name the
  peripheral it is a view of. Elsewhere all of them are ordinary names, of
  variables, functions, fields or modules, and `read(x)` in a body is a
  call. A `step` is the word after the offset only when no `:` follows it,
  and so is `in`, so that a cluster named `step` or `in` after a missing
  comma is the next entry, and the comma is the error reported.
- Inside a function body, `peripheral` followed by a name is
  `parser::local-peripheral`, for either item: no expression starts with
  two names.
- Both are items, in any module, in its one namespace (see "Modules and
  paths"). A peripheral type is named like a struct, by a path, where a
  type is written; a peripheral like a static, where a value is. Both are
  private unless `pub`, and are brought in with `use`.
- Attributes do not apply to either (`resolve::misplaced-attribute`, or
  `resolve::unknown-attribute` for one that does not exist). Neither has
  a symbol or storage: nothing is emitted for them (see "Symbols").

**Registers.**

- A register's **access mode** comes before its name: `read` (it is
  read-only), `write` (write-only) or `read write`. There is no default:
  an entry without one is a cluster (see "Register arrays and clusters").
- Its **type** is `u8`, `u16` or `u32`, an unsigned integer as wide as
  the register; a bit-field struct (see "Bit-field structs") whose integer
  type is one of those; or an enum without payloads whose integer type is
  one of those (ADR 0060). Its width is that integer type's. Any other
  type, as a `u64` bit-field struct, an enum on `i8` or an enum with
  payloads, is `ty::register-type` (see "What is checked" below). An
  array of such a type is a register array (see "Register arrays"); a
  peripheral type, or an array of one, is `ty::register-type` too, its
  help saying that a cluster has no access mode.
- Its **offset**, after `at`, is a `usize` computed at compile time, like
  an array length (see "Compile-time execution"), in the scope of the
  module holding the peripheral type: it may name that module's private
  constants and call its functions.
- A register is private to the module of its peripheral type unless
  declared `pub`, as a field is (see "Visibility").
- Two entries of one type (registers, register arrays and clusters) cannot
  share a name (`resolve::duplicate-register`, at the second, labeling the
  first), whatever their modes: a `read` and a `write` register at one
  offset take two names, as `rbr` and `thr`. Entries of two types may
  share a name.
- A peripheral type may have no entry: `peripheral Empty {}`.

**Peripherals.**

- The type of a peripheral is a path naming a peripheral type. Another
  type (a struct, an enum, a built-in type, or a pointer, array or slice
  type, even a pointer to a peripheral type) is
  `resolve::not-a-peripheral-type`; a name that names nothing, or an item
  that is no type, stays `resolve::unknown-type`.
- Its **address**, after `at`, is a `usize` computed at compile time in
  the scope of its module, like an offset. It may be computed from
  another peripheral's, as `(&raw const UART0 as usize) + 0x100`.
- Its `over`, after the address, names another peripheral it is a second
  view of (see "Peripherals over others").
- Every offset and address is computed, and every peripheral type and
  peripheral checked, whether code uses them or not.

**Layout.**

- An entry's **elements** are its one register or cluster, or the `N`
  elements of an array, element `i` at `offset + i * step`. An element
  **occupies** `[start, start + size)`, `size` being a register's width or
  a cluster's extent; an array of 0 elements occupies nothing.
- A peripheral type's **extent** is the end of the last byte any of its
  entries occupies, `offset + width` for a register, wherever it is
  declared, and 0 for an empty type. It is computed exactly: a `u32` at
  `0xFFFF_FFFC` gives an extent of `0x1_0000_0000`, which no `usize` holds.
  Since an extent is only compared with `0x1_0000_0000`, it **saturates**
  above that, where nested arrays could pass 2⁶⁴, and a diagnostic shows
  such an extent as past 2³². Its **alignment** is the largest of its
  registers' widths and its clusters' alignments, and 1 for an empty type;
  every entry counts, an array of 0 elements too. `Uart16550` above has
  extent 6 and alignment 1.
- A peripheral covers `[address, address + extent)`; one of an empty type
  covers no byte.
- An entry's offset must be a multiple of its element's alignment, a
  register's width or a cluster type's alignment
  (`ty::misaligned-register`), and a peripheral's address a multiple of
  its type's alignment (`ty::misaligned-peripheral`), so that every
  register access is aligned, as its one instruction assumes (ADR 0019,
  0023). A step must be a multiple of it too (`ty::misaligned-step`).
- Two entries of one type may share no byte (`ty::register-overlap`),
  except when both are registers or register arrays, one `read` and the
  other `write`: a read can then only mean one of them and a write the
  other, as the 16550's `rbr` and `thr` at offset 0. A `read write`
  register shares bytes with no other, and a cluster, which has no access
  mode, with none. This applies to two entries in the same alternate
  group, or both in none (see "Alternate groups" below). A cluster
  element occupies its extent, the gaps between its registers included, so
  a register inside a cluster's range is an overlap even where the
  cluster has no register there. Entries whose elements occupy nothing
  take part in no overlap check, nor do those whose step is too small.
- Two peripherals may share no byte (`ty::peripheral-overlap`), and a
  peripheral must end at `0x1_0000_0000` or below
  (`ty::peripheral-out-of-range`): unlike a region, it needs no address
  one past its end, so its last byte may be the last of the address
  space. A peripheral that is `over` another is not checked for overlap
  with it (see "Peripherals over others").
- Peripherals are not checked against the regions of the `memory` block:
  one placed over a region is not detected, and writing its registers
  writes that memory.

**Accesses.** A register is reached from a peripheral, `UART0.thr`, or
through a pointer to a peripheral type, `(*u).thr`. Either is a place of
the register's type:

- **Each read is one volatile load of the register's width at its address,
  the device's plus its offset, and each write one volatile store**, with
  the guarantee of "Volatile accesses": never split, merged, widened,
  narrowed, duplicated, removed or reordered relative to other volatile
  accesses, those through volatile pointers included, and not ordered
  against ordinary accesses. Two reads of one register are two loads, and
  a read of `rbr` then a write of `thr`, at one address, happen in that
  order.
- Reading a `write` register is `ty::read-of-write-only`, and writing a
  `read` register `ty::write-to-read-only`. A read is any use of the
  register's value: an operand, an initializer, an argument, a condition,
  a `match` scrutinee, or an expression statement, as `UART0.lsr;`. A
  compound assignment, `UART0.ier |= 1`, reads then writes, one load then
  one store, not atomic: it needs `read write`, and on another mode is
  reported as the access that mode forbids.
- Through a `*const` pointer to a peripheral type, registers are read;
  writing one is `ty::write-through-const`, as through any `*const`
  pointer, and writing a `read` register through one reports both
  errors, each a fault of its own.
- A name that is no entry of the type is `ty::unknown-register`, whose
  help lists the entries visible there; an entry that is not visible
  there is `ty::private-register`. `u.thr`, with `u` a pointer, is
  `ty::no-fields`: dereference it first, `(*u).thr`.
- `&raw` of a register is `ty::register-address`: the pointer would reach
  the register without its access mode. Pass the device instead, as
  `&raw mut UART0`.
- A peripheral, or `*u`, read, written or passed as a whole (`let x: u8 =
  UART0;`, `take(*u)`, `UART0 = 0`, `UART0 += 1`) is
  `ty::peripheral-by-value`, once: a device is reached one register at a
  time. Written whole through a `*const` pointer, `*c = 0` is that error
  alone. `UART0[0]` is `ty::cannot-index`.

**Register arrays and clusters (ADR 0064).** A device often repeats a
register, or a block of registers, at a fixed distance. With the PLIC of
QEMU's `virt`:

```
peripheral Plic {
    pub read write priority: [u32; 96] at 0x0000,
    pub read pending: [u32; 3] at 0x1000,
    pub enables: [PlicEnables; 2] at 0x2000 step 0x80,
    pub ctx: [PlicContext; 2] at 0x20_0000 step 0x1000,
}

peripheral PlicEnables {
    pub read write word: [u32; 3] at 0x00,
}

peripheral PlicContext {
    pub read write threshold: u32 at 0x00,
    pub read write claim: u32 at 0x04 read modifies,
}

peripheral PLIC: Plic at 0x0C00_0000;

fn route(c: *mut PlicContext, level: u32) {
    (*c).threshold = level;
}

fn setup(source: usize) {
    PLIC.priority[source] = 1;                 // one store at 0x0C00_0000 + 4 * source
    PLIC.enables[0].word[source / 32] |= 1 << (source as u32 % 32);
    route(&raw mut PLIC.ctx[1], 0);            // one driver for every context
    let claimed: u32 = PLIC.ctx[0].claim;      // one load
}
```

- A register whose type is an array, `[T; N]` with `T` a register type, is
  a **register array**: `N` registers of type `T`, the first at the
  register's offset and each next one `step` bytes further. `N` is an array
  length. `step`, after the offset, is a `usize` computed at compile time
  in the same scope; left out, it is the register's width. The distance
  need not be the width: `a: [u32; 4] at 0 step 8` is four registers 8
  bytes apart. Every element has the array's access mode, read effect and
  group, and is read and written as a register of type `T` declared with
  them.
- An array of arrays, `[[u32; 3]; 2]`, is no register type
  (`ty::register-type`, whose help names clusters): a register repeated in
  two dimensions is a register array in a cluster array, as `enables`
  above.
- An entry **without an access mode** is a **cluster**. Its type is a
  peripheral type, `PlicContext`, or an array of one, `[PlicContext; 2]`
  (a **cluster array**), and it places that type's registers at its
  offset. It may be `pub`, and be in an alternate group; it has no read
  effect of its own, its registers have theirs. The cluster's type is a
  type declared on its own, so a driver of `PlicContext` serves every
  context through a pointer to it, as it would a peripheral of that type.
  Clusters nest: a cluster's type may hold clusters.
- **A cluster array needs `step`** (`resolve::missing-step`): a peripheral
  type's extent grows when a register is added at its end, and a step taken
  from it would move every element but the first without a word changing at
  the cluster. A single cluster takes no `step`, and neither does a
  register that is no array (`resolve::misplaced-step`).
- Any other type of a cluster is `resolve::not-a-peripheral-type`, as for a
  peripheral, and is the entry's only error (not also a missing or
  misplaced step). When the type is one a register may have (`u8`, `u16`
  or `u32`, a bit-field struct or an enum without payloads on one of them,
  or an array of one), the entry is most likely a register written without
  its access mode, and the help says that a register starts with `read`,
  `write` or `read write`. A `u64`, a struct or a pointer type gets a help
  on what a cluster's type is.
- Only one array level is a cluster array: `[[Context; 2]; 2]` is
  `resolve::not-a-peripheral-type`.
- A peripheral type that contains itself through clusters, directly or
  through others, is `ty::infinite-size`, naming every type of the cycle,
  with a help of its own: the cycle is broken by removing a cluster, since
  a register cannot hold a pointer. Every type of the cycle has no layout
  and reaches a failed value (see "Failed values" below).
- **Steps.** The elements of one array may not overlap: a step smaller than
  the element's size is `ty::step-too-small`, whatever the length, as the
  declaration states the step. A step of 0 is therefore accepted only for
  elements of size 0, an empty cluster type. A step of 2 for a `u32` is
  both misaligned and too small, and both are reported: each is a fault of
  its own. Neither is a failed value: the type keeps its layout, so the
  bodies using it are still checked and built.

*Accesses to them.*

- **`P.a[i]`**, with `a` a register array, is the place of element `i`, a
  register of type `T`, read and written as above (access mode, read
  effect, group, fields); `i` is a `usize`. **`P.c.r`** and **`P.c[i].r`**,
  with `c` a cluster or a cluster array, are the register `r` of a
  cluster element, `PLIC.ctx[1].threshold`, and the same holds through a
  pointer, `(*u).ctx[1].threshold`. Visibility is checked at each step:
  `ctx` where `Plic`'s entries are visible, `threshold` where
  `PlicContext`'s are.
- **The index is checked.** One not below the array's length calls the
  panic function with the reason `IndexOutOfBounds`, located at the `[`
  (see "Panic information"); one known at compile time is
  `ty::index-out-of-bounds`. The index runs after the place it indexes
  and before the access, as for arrays: in `PLIC.ctx[i()].threshold =
  v()`, `v()` runs first (the value of an assignment runs before its
  place), then `i()`, the bounds check, and the store. There is no
  unchecked access to a register array. With `-O`, LLVM removes a check it
  proves redundant, as for `i & 3` indexing an array of 4.
- **The address** of element `i` is the peripheral's, plus the entry's
  offset, plus `i * step`, computed with wrapping `usize` arithmetic; a
  register's access through it is one volatile load or store, as any.
- **`&raw const P.c` and `&raw mut P.c[i]`** give a `*const PlicContext` or
  a `*mut PlicContext` to the element's address. Through a `*const`
  pointer, `&raw mut` of a cluster is `ty::address-of-immutable`, as for
  any place reached through one. `&raw` of a register array or of a cluster
  array taken whole, or of an element of a register array, is
  `ty::register-address`: no pointer type holds an array's step.
- **An entry that is an array is reached only through an index, and a
  cluster only through a register or `&raw`.** `P.a`, `P.c`, and a cluster
  array's element `P.c[i]`, read or written as a whole, are
  `ty::peripheral-by-value`, once. Indexing an entry that is no array, a
  single cluster `P.c[0]` or a register `P.r[0]`, is `ty::cannot-index`
  (a `write` register indexed is not also read). An index that is not a
  `usize` is `ty::mismatched-types`.
- Indexing a pointer to a peripheral type stays `ty::peripheral-index`: a
  cluster array is indexed at the entry, where its step is known.
- **At compile time**, a register access through any of these is
  `eval::volatile-access`. `&raw` of a cluster element in a constant, a
  static's initializer or a `comptime` block is a pointer made from an
  integer, as `&raw` of a peripheral is: `&raw const PLIC.ctx[1] as usize`
  is `0x0C20_1000`. An index out of bounds there is `eval::panic`.

**Registers with fields (ADR 0060).** With `WordLength` and `Lcr` as in
"Bit-field structs":

```
struct Lsr: u8 {
    pub data_ready: bool at 0,
    pub overrun: bool at 1 read clears,
    pub thr_empty: bool at 5,
}

peripheral Uart16550 {
    pub read rbr: u8 at 0x00 read modifies,
    pub write thr: u8 at 0x00,
    read write lcr: Lcr at 0x03,
    read lsr: Lsr at 0x05,
    read write dll: u8 at 0x00 in dlab,
    read write dlm: u8 at 0x01 in dlab,
}

peripheral UART0: Uart16550 at 0x1000_0000;

fn setup() {
    UART0.lcr = Lcr { word_length: .Eight, .. };   // one store
    UART0.lcr.dlab = true;                         // one load, one store
    UART0.dll = 0x80;
    UART0.lcr.dlab = false;
    let status: Lsr = UART0.lsr;                   // one load
    if status.overrun { }
}
```

- `UART0.lcr` read is one load, giving an `Lcr`; `UART0.lcr = v` is one
  store of `v`'s bits, reserved bits included. `UART0.lsr.data_ready`
  reads the register, one load, and gives the field. A register whose type
  is an enum is read and written whole; a value read that is no variant is
  defined (see "Enums").
- **A field assignment**, `UART0.lcr.dlab = true`, or a compound
  assignment to a field, is one load of the register, then one store, with
  nothing between them but the change of the field (and the neutral values
  of write effects, below): the value and its check run first, as for any
  bit field (see "Bit-field structs"). It is not atomic. It reads and
  writes, so it needs a `read write` register: on a `read` one it is
  `ty::write-to-read-only`, and on a `write` one `ty::read-of-write-only`,
  labeled as read by the field assignment. A field read from a `write`
  register is `ty::read-of-write-only` too.
- A field without an access mode is read and written as its register
  allows; one with a mode follows it too (below).
- `&raw` of a register's bit field is `ty::bit-field-address` alone.

**Field access modes (ADR 0065).** A bit field may be `read` (read-only) or
`write` (write-only) (see "Bit-field structs"):

```
struct Status: u32 {
    pub read busy: bool at 0,
    pub write start: bool at 1,
    pub mode: u8 at 4..=5 = 0,
    pub write clear: bool at 8 write 1 clears,
}

peripheral Device {
    read write status: Status at 0x0,
}

peripheral DEV: Device at 0x1000_0000;

fn go() {
    DEV.status.mode = 1;          // fine: one load, one store
    DEV.status.start = true;      // fine: a write-only field is written
    let b: bool = DEV.status.busy;                   // fine
    let mut s: Status = DEV.status;
    s.busy = true;                // fine: a local, modes mean nothing
}
```

- **In a volatile place, a register or a place reached through a volatile
  pointer, a field's mode is checked as a register's is.** A field
  assignment or a compound assignment to a `read` field is
  `ty::write-to-read-only`: `DEV.status.busy = true`. A read of a `write`
  field, `DEV.status.start`, or a compound assignment to one, is
  `ty::read-of-write-only`. A `write` field assigned with `=` and a `read`
  field read are never reported. In ordinary memory, a local or a static,
  modes mean nothing, as effects do not.
- **The register's own mode is checked too.** When both forbid the access
  with the same code, as a field assignment to a `read` field of a `read`
  register, it is one diagnostic labeling the register and the field: fixing
  one cause leaves the other, and its help says so. When they forbid it
  with different codes, each is reported: a field assignment to a `read`
  field of a `write` register is `ty::read-of-write-only` for the register
  (the assignment reads it) and `ty::write-to-read-only` for the field, and
  a compound assignment to a `write` field of a `read` register is
  `ty::write-to-read-only` for the register and `ty::read-of-write-only` for
  the field. A field assignment to a `write` field of a `write` register
  is still `ty::read-of-write-only` for the register, which reads itself to
  keep the other bits.
- A field assignment to another field keeps the bits of `read` and `write`
  fields without a write effect as read, as reserved bits are kept: the
  hardware ignores the write of a read-only field, and what a write-only
  field reads back is written back. **A field with a write effect gets the
  value that has no effect, whatever its mode**: `DEV.status.mode = 1`
  writes `clear` as 0.
- A whole read gives every field and a whole write writes every field: a
  literal may name a `read` field, and `..` gives it its default. A mode
  combines with effects without a check (a `read` field with a write
  effect, a `write` field with a read effect), and a field may have a mode
  its register lacks (a `write` field in a `read` register, which no access
  reaches): both are accepted and document the device.

**Write effects.** A field of a bit-field struct may declare what a write
does to it, after its bits and its read effect, if any:

```
write_effect = "write" INTEGER ( "clears" | "sets" | "toggles" )

struct Status: u32 {
    pub enable: bool at 0 = false,
    pub overrun: bool at 3 write 1 clears,
    pub done: bool at 4 write 1 clears,
}
```

`write 1 clears` is SVD's `oneToClear`, `write 0 sets` its `zeroToSet`, and
so on for the six combinations. The bit written is an integer literal of
value 0 or 1 (`write 0x1 clears` is `write 1 clears`); another value is
`parser::unexpected-token`. A field has at most one write effect, and no
default (`resolve::write-effect-default`).

- **In a field assignment to a volatile place, every other field with a
  write effect is written with the value that has no effect**: all zeros
  for `write 1 ...`, all ones for `write 0 ...`. A volatile place is a
  register, or a place reached through a volatile pointer. The assigned
  field gets the value assigned, even if it has an effect itself;
  reserved bits and fields without a write effect keep the value read.
  ```
  TIMER.status.enable = true;     // overrun and done written 0: unchanged
  TIMER.status.overrun = true;    // clears overrun alone
  let s: Status = TIMER.status;
  TIMER.status = s;               // clears every flag that was set
  ```
- Writing a whole value writes it as it is. A literal built with `..`
  gives every field with a write effect that it leaves out the value that
  has no effect, so it clears, sets or toggles nothing it does not name.
  This holds wherever the literal goes, a register or a local.
- A **compound assignment to a field with a write effect** in a volatile
  place is `ty::compound-write-effect`: it would write back every bit it
  read.
- In ordinary memory, a local or a static, effects mean nothing: a field
  assignment changes that field alone.

**Read effects.** A register, after its offset, or a field, after its
bits, may declare what a read does:

```
read_effect = "read" ( "clears" | "sets" | "modifies" )
```

`read clears` and `read sets` are SVD's `readAction` `clear` and `set`;
`read modifies` is any other change. For the compiler all three mean that
**a read has effects**. A register's reads have effects when it declares a
read effect, or when its type is a bit-field struct with a field that does;
so do reads through a volatile pointer of a place whose type is such a
struct. A field and a register each have at most one. Read effects mean
nothing in ordinary memory, and one on a `write` register is accepted as
documentation: the reads of a `write` register never count as having
effects.

- **Such a place is never read without the read being written**: a field
  assignment or a compound assignment to it, or to one of its fields
  (also a compound assignment to a whole register that declares the
  effect), is `ty::read-with-effects`. The program reads the register into
  a local, which shows the read, and writes what it means. Explicit reads
  are unchanged: each is one volatile load, so `UART0.lsr.thr_empty` reads
  the register, and its effects happen where it is written. A program
  polling it loses the error bits that read cleared; one that wants them
  reads `lsr` into a local.
- When several apply, each is reported: a field assignment to a `read`
  register whose reads have effects is `ty::write-to-read-only` and
  `ty::read-with-effects`, and a compound assignment to a write-effect
  field of a register whose reads have effects is
  `ty::compound-write-effect` and `ty::read-with-effects`. On a `write`
  register with a read effect, a field assignment is
  `ty::read-of-write-only` alone.

**Reset values.** A field's default (see "Bit-field structs") is its reset
value: `Fcr { enable: true, .. }` writes the register as it is after
reset but for `enable`, and for the fields with write effects, which have
no default and are written with the value that has no effect. A register's
reset value is not declared apart from its fields' defaults; an integer
register has none.

**Alternate groups (ADR 0061).** A register may name an **alternate
group** after its offset and its read effect: `read write dll: u8 at 0x00
in dlab`. The group's name is a label of the peripheral type, as SVD's
`alternateGroup`: it names no item, is not resolved, is in no namespace,
and may coincide with a register's name. A register without `in` is in no
group.

- **Two registers are checked for overlap only when they are in the same
  group, or both in none**, with the rule above. So `dll` may share bytes
  with `rbr` and `thr`, and two registers of `dlab` may not share one
  with each other. The help of `ty::register-overlap` names `in GROUP`.
- Accesses to a register in a group are checked and built as any other:
  the compiler does not know which mode the device is in, and setting DLAB
  before reaching `dll` is the program's business, as it is in C.
  Misalignment, the peripheral type's extent and its alignment count every
  register, grouped or not. A group where nothing overlaps only documents
  a mode.
- The `typeck` and `mir` dumps show a register's group and read effect.
- A cluster may be in a group too, after its offset and its step:
  `pub ctx: [Context; 2] at 0x100 step 0x40 in dlab`. It is checked for
  overlap against the entries of its group, or of none, like a register,
  with a cluster's mode being none (see "Layout").

**Peripherals over others (ADR 0066).** A peripheral may name, after its
address, the peripheral it overlaps, as SVD's `alternatePeripheral` does,
or as a cluster's type declared as a peripheral of its own over the device
that holds it:

```
peripheral APB_CTRL: ApbCtrl at 0x6002_6000;
peripheral RNG: Rng at 0x6002_6000 over APB_CTRL;

peripheral PLIC: Plic at 0x0C00_0000;
peripheral PLIC_M0: PlicContext at 0x0C20_0000 over PLIC;
```

- `over` names a peripheral by a path (see "Modules and paths"), resolved
  and checked for visibility as any path; it may name a peripheral
  declared later. A path naming nothing is `resolve::unknown-name` or
  `resolve::unknown-path`, a peripheral not visible
  `resolve::private-item`, a module `resolve::misused-module`, and a name
  of anything else (a constant, a struct, a peripheral type, a variant)
  `resolve::not-a-peripheral`. A peripheral naming itself is
  `resolve::over-itself`.
- **Two peripherals are not checked for overlap when one names the other,
  or both name the same one**: the second view and the first, and two
  alternative views of one device. Any other pair is checked as before, so
  with `C over B` and `B over A`, `C` is checked against `A`. A peripheral
  `over` another that it does not overlap is accepted: the word then
  documents a view.
- Nothing else changes: each peripheral's alignment and range are checked,
  and accesses are built as to any peripheral. The compiler does not know
  which view the device shows, as for alternate groups. An `over` written
  by mistake hides a real overlap with the peripheral it names.
- The help of `ty::peripheral-overlap` names `over` for a second view of a
  device. The `typeck` dump shows the peripheral named (`peripheral RNG:
  Rng at 0x60026000 over APB_CTRL covers ...`); the `mir` dump does not,
  since `over` changes no access.

**Pointers to peripheral types.** `&raw const UART0` and `&raw mut UART0`
give a `*const Uart16550` and a `*mut Uart16550`, through which a driver
serves any instance, as `putc` above does:

- A peripheral is not `mut`, and `&raw mut` of one is allowed: what may
  be written is its registers' business. `&raw mut *u` and `&raw const
  *u` give the pointer back.
- Accesses through such a pointer are volatile because they reach
  registers, so the pointer is written without `volatile`: `*mut volatile
  Uart16550` is `ty::volatile-peripheral`, wherever the type is written,
  and is then taken as `*mut Uart16550`, so that nothing more is reported
  where it is used.
- They are thin pointers like any other: a `*mut` one is accepted where a
  `*const` one is expected (see "Typing"), and they are compared, held in
  variables, fields, statics and `Option`, and passed by value.
- Pointer casts to and from them are allowed, as any pointer cast
  (ADR 0019): `0x1000_0000 as *mut Uart16550` makes one without a
  peripheral, through which registers are reached as through `&raw mut
  UART0`, and `&raw mut UART0 as *mut u8` reaches the device's memory
  without the registers' widths, access modes or volatility. The cast
  says so where it is written.
- Indexing one is `ty::peripheral-index` (see "Pointers").
- `slice(u, n)` of one is `ty::peripheral-by-value`, as `*mut [Uart16550]`
  written is: a peripheral type is no slice element. It is reported once,
  and `len` of the slice, or indexing it, adds no error.

**At compile time.**

- A register access is `eval::volatile-access`, as any volatile access
  is, through a pointer made from an integer too.
- `&raw const UART0` and `&raw mut UART0` are pointers made from an
  integer, the peripheral's address (ADR 0030): `&raw mut UART0 as usize
  == 0x1000_0000` holds in a `comptime` block, and a constant or a static
  may hold one, as `static CONSOLE: *mut Uart16550 = &raw mut UART0;`,
  emitted as that address.
- An offset or an address whose computation needs itself, as `peripheral
  A: T at &raw const A as usize;`, is `ty::cycle`, as any value computed
  at compile time. So is an offset whose computation names a pointer to
  its own peripheral type, as `read status: u32 at (0x4 as *mut Regs) as
  usize` in `Regs`, whichever of the offset and the type's layout is
  computed first, as for a struct's array length naming a pointer to the
  struct (see "Failed lengths and alignments").

**Failed values.** An entry's offset, a register's type in error, the
`step` and the length of a register array or a cluster array in error, a
cluster whose type reaches a failed value, and a peripheral's address are
failed values (see "Failed lengths and alignments"), each reported once,
where it is written:

- A register's type is in error when it is `!` (`ty::never-in-memory`,
  with a help of its own), `[T]` (`ty::unsized`), a type reaching a
  failed value, as a `[u8; N]` whose length failed, or no register type
  (`ty::register-type`). A type already in error is not also
  `ty::register-type`.
- A peripheral type with a failed offset, step, length or register type, a
  cluster that reaches a failed value, or one in a `ty::infinite-size`
  cycle, which has no layout, reaches a failed value, and so does a type
  containing a pointer to it; a peripheral whose type reaches one, or whose
  address failed, is erroneous. Code that names such a peripheral, or whose
  types reach such a peripheral type, is not built (see "Failed lengths and
  alignments"). A misaligned or too-small `step` is no failed value: the
  type keeps its layout.
- The checks of "Layout" that need a failed value are skipped: an entry
  whose offset, step, length or type failed takes part in no other check,
  and neither does an erroneous peripheral. A misaligned register, and a
  misaligned or out-of-range peripheral, still take part in the overlap
  checks.
- An offset or an address computed from an erroneous peripheral, as
  `(&raw const BROKEN as usize) + 0x100` with `BROKEN`'s address failed,
  fails in turn without an error of its own.

A register type that is no register type and mentions a peripheral type,
behind pointers or inside other types (`*mut B`, `B` itself, a struct
holding a `*mut B`), stays silent only when that type's failure is
explained by an error reported elsewhere, so that a program in error
always reports one:

- A peripheral type's failure is **explained** when it has an offset, a
  step or a length that failed, a cluster array without `step`, a cluster
  whose type is in error, is in a `ty::infinite-size` cycle (which is its
  explanation, whatever the other values are), or has a register type in
  error in itself (`!`, `[T]`, a type reaching a failed length or
  alignment) or that is no register type and mentions no peripheral type
  (`u64`, `bool`, `*mut u32`, a struct), or, transitively, a register whose
  type, no register type, reaches a type whose failure is explained. So
  `read write r: *mut A at 0` with `A` in a cluster cycle reports nothing
  more.
- A register type reaching a type whose failure is explained stays
  silent: with `peripheral B { read write s: u64 at 0 }`, `peripheral A {
  read write r: *mut B at 0 }` and `peripheral C { read write t: *mut A at
  0 }`, only the `u64` is reported, whatever the order of the
  declarations.
- One reaching only types whose failure nothing explains is
  `ty::register-type`: a pointer to its own type (`*mut Regs` in `Regs`)
  or that type itself, two types whose registers only point to each
  other, a pointer to a type whose only failures are such registers, a
  pointer to a peripheral type without failure, and a cycle through a
  struct. Each of these types still reaches a failed value.

**What is checked.** When names are resolved:

| Code | Case |
|------|------|
| `parser::local-peripheral` | a peripheral type or a peripheral inside a function body |
| `resolve::duplicate-register` | two entries with one name in a peripheral type |
| `resolve::not-a-peripheral-type` | the type of a peripheral, or of a cluster, names a type that is no peripheral type (a cluster's may be an array of one); the help of a cluster names the access mode when the type is a register's |
| `resolve::missing-step` | a cluster array without `step` |
| `resolve::misplaced-step` | `step` on a register or a cluster that is no array |
| `resolve::not-a-peripheral` | `over` naming something that is no peripheral |
| `resolve::over-itself` | a peripheral naming itself after `over` |
| `resolve::misplaced-attribute` | an attribute on a peripheral type or a peripheral |

A name of nothing after `over` is `resolve::unknown-name` or
`resolve::unknown-path`, and a peripheral not visible
`resolve::private-item`, as for any path. Names shared with other items of the module are `resolve::duplicate-item`
(see "Modules and paths").

Once the values are computed, during type checking, so whatever stage
`--emit` asks for after it, with a check that needs a failed value
skipped:

| Code | Case |
|------|------|
| `ty::register-type` | a register whose type is not `u8`, `u16` or `u32`, or a bit-field struct or an enum without payloads on one of them, or an array of such a type, at the type; an array of arrays, with a help naming clusters; a peripheral type or an array of one, with a help saying a cluster has no access mode |
| `ty::misaligned-register` | an entry's offset that is not a multiple of its element's alignment (a register's width, a cluster type's alignment), at the offset, labeling the type |
| `ty::misaligned-step` | a `step` that is not a multiple of the element's alignment, at the step |
| `ty::step-too-small` | a `step` smaller than the element's size, whatever the length, at the step |
| `ty::register-overlap` | two entries of one alternate group, or both of none, sharing a byte, unless both are registers or register arrays, one `read` and the other `write`; once per pair, at the later entry in the type, naming the first shared byte and labeling the earlier |
| `ty::infinite-size` | a peripheral type that contains itself through clusters, naming every type of the cycle |
| `ty::misaligned-peripheral` | a peripheral's address that is not a multiple of its type's alignment, at the address, labeling the type |
| `ty::peripheral-out-of-range` | a peripheral whose end, `address + extent`, is above `0x1_0000_0000` |
| `ty::peripheral-overlap` | two peripherals sharing a byte, unless one is `over` the other or both are `over` the same one; once per pair, at the later peripheral, labeling the earlier |

"Later" for peripherals is as for the `memory` block: in the order of
files, numbered depth first (see "Modules and paths"), then of positions
in a file. Ranges and offsets are shown in hexadecimal.

When code is type checked:

| Code | Case |
|------|------|
| `ty::peripheral-by-value` | a peripheral type where a value would have it (see "Types"), an array of one anywhere but as a cluster's type, or a peripheral, `*u`, register array, cluster or cluster array's element read, written or passed as a whole |
| `ty::volatile-peripheral` | `volatile` on a pointer to a peripheral type |
| `ty::peripheral-index` | indexing a pointer to a peripheral type |
| `ty::cannot-index` | an index of an entry that is no array (and of any value that is no array or pointer) |
| `ty::unknown-register` | a name that is no entry of the peripheral type |
| `ty::private-register` | an entry not visible where it is named |
| `ty::write-to-read-only` | a write, a compound assignment or a field assignment to a `read` register; a field assignment or a compound assignment to a `read` field in a volatile place |
| `ty::read-of-write-only` | a read, a compound assignment or a field assignment of a `write` register, or a field read from one; a read of a `write` field, or a compound assignment to one, in a volatile place |
| `ty::register-address` | `&raw` of a register, of a register array or a cluster array taken whole, or of an element of a register array |
| `ty::bit-field-address` | `&raw` of a bit field, a register's too |
| `ty::read-with-effects` | a field assignment or a compound assignment to a place whose reads have effects, or to one of its fields |
| `ty::compound-write-effect` | a compound assignment to a field with a write effect in a volatile place |
| `ty::index-out-of-bounds` | an index known at compile time not below the length of a register array or a cluster array |

**The code.** A register reached from a peripheral is accessed at a
constant address, the peripheral's plus the register's offset, with no
symbol, storage or indirection for the peripheral; one reached through a
pointer, at the pointer plus the offset. An element of a register array or
a cluster array is at that address plus `i * step`, `i` being checked
against the length first (see "Register arrays and clusters"). Until M5, any code that can name
a peripheral reaches it, as with a `static mut`; ownership of peripherals
is M5's.

## Peripherals from SVD (ADR 0070 to 0072)

```
erebos svd <input.svd> [-o <output.ers>]
```

`erebos svd` reads an SVD file, the XML vendors publish to describe a chip,
and writes an Erebos source file: a module the program declares, as `mod
esp32c3;`, kept with the program's sources and built as any other. **The
compiler never reads SVD**: `erebos build` depends on `.ers` files alone, so
a build is still a function of its sources, and a chip's peripherals can be
read, reviewed and edited as Erebos. The generator is the module `svd` of the
compiler's crate; it is no stage of the compiler, and `erebos build` never
runs it.

**The command.**

- `-o` names the output. Left out, it is in the input's directory, named
  after the input's file name without its last extension, made a module's
  name by the steps of "How names are converted" below, with the extension
  `.ers`: `esp32-c3.svd` gives `esp32_c3.ers`, `32bit.svd` `_32bit.ers`,
  `match.svd` `match_.ers`, `a.b.svd` `a_b.ers`, and a file without
  extension `plain` gives `plain.ers`. An input whose name leaves nothing
  for an identifier (`@@.svd`) is an error asking for `-o`.
- An existing output is replaced. An output that is the input (the same
  file, whatever the spelling of the path) is an error, reported before
  anything is read. **Nothing is written when an error is reported.**
- Warnings are printed to standard error and the module is written: the
  exit status is a failure for an error only. A bad command line (an option
  `svd` does not take, such as `-O`, or no input) exits with status 2.
- **The output is a function of the input's bytes and its file name**: no
  date, no directory, `\n` line endings, and an order fixed by the SVD file.
  Generating twice gives the same bytes, and a new version of a vendor's file
  shows as a diff of the generated module. The format (below) is the
  generator's, not the language's: it may change from one version of the
  compiler to the next, and a kept module is then generated again.
- The file must be UTF-8 (`svd::xml` otherwise, without a span). Elements are
  known by their local name, namespaces ignored; a DTD is refused.

**The header.** The module starts with three comment lines: the generator and
the input's file name, `Device: NAME, version V.` (without the version when
the device has none), and that edits are lost when it is generated again.
The device's `licenseText`, when it has one, follows as a comment, each line
with its white space at the end removed (and blank lines at either end of
the text dropped). Text of the SVD file in a comment has its line breaks and
other control characters replaced by spaces, so that it never leaves the
comment.

**The format.** Four spaces of indentation per module. Items and entries are
in the syntax of "Peripherals", every optional part in the grammar's order
(`step`, read effect, `in`; for a field its mode, bits, read effect, write
effect, default). Offsets, addresses and steps are in hexadecimal, `0x` then
upper-case digits, in groups of four from the right separated by `_` when
there are more than four (`0x6000_0000`, `0x20`; zero is `0x0`). Bit
positions are decimal, and so are integer defaults below 10 (hexadecimal
from 10, as offsets are). An element's `description` is a `//` comment above
each item it makes, its white space collapsed and the text wrapped at 79
columns (a word longer than a line is alone on its line); an item with a
comment is set apart from its neighbors by a blank line.

### Where items go, and how they are named (ADR 0071)

The module holds, in the order of the SVD file, **a module per peripheral
type**, then **the peripherals**, all `pub`. A peripheral type's module holds
the type, then, in the order of its registers, the bit-field struct of each
register that has fields and the enums of its fields, then **a module per
cluster type**, which holds the cluster's type and its registers' items in
the same way. A path from one generated module to another goes through
`super`, so the file works wherever the program declares it.

**How names are converted.** An item is named from the `name` of one SVD
element, in these steps:

1. **The index.** An item that is one element of a repeated SVD element (a
   peripheral, register, cluster or field with `dim`, written one per
   index) has `%s`, or `[%s]`, replaced by its index as `dimIndex` writes
   it: `CH%s_CONF0` with index 2 is `CH2_CONF0`. Any other item (an array, or
   a type, module or enum shared by every index) has `[%s]` and `%s`
   removed: `PRIORITY[%s]` is `PRIORITY`.
2. **The characters.** Each character that is not an ASCII letter, digit or
   `_` is replaced by `_`; each run of `_` is made one, and `_` at either end
   removed.
3. **The case**, by the kind of item. **Pascal case** splits a name at `_`
   and writes each part with its first character upper and the rest lower
   (`RND_DATA` is `RndData`); **lower case** lowers every letter.
4. **A leading digit** gets a `_` before it.
5. **Names Erebos gives a meaning**: a name that is a keyword of the lexer,
   `core`, or a built-in type (`bool`, `u8` to `u64`, `i8` to `i64`,
   `usize`, `isize`, `Option`, `Result`) gets a `_` after it: `continue`
   is `continue_`. The keywords are the lexer's table itself. Words that are
   names by position only (`read`, `write`, `at`, `step`, `in`, `over`) stay
   as they are: `GPIO`'s `IN` is the register `in`.
6. **Names that meet.** Two items of one scope with one name after
   conversion: the later one, in the order the file is written, gets `_`
   after it, as many times as it takes. The scopes are the module (the file's
   modules and peripherals together, as the language has one namespace for
   them, and a module's type, structs and enums), a peripheral type's entries,
   a struct's fields and an enum's variants. Groups (`in dlab`) are labels
   in no scope.

| Item | Case | Example |
|---|---|---|
| peripheral | as in the SVD | `UART0`, `TIMG0` |
| peripheral type | Pascal case of the peripheral it is declared for | `Uart0` |
| its module | lower case of that peripheral | `uart0` |
| register, cluster, field | lower case | `rxfifo_rst`, `ch2_conf0` |
| register array, cluster array | lower case | `priority` |
| cluster type, and its module | Pascal case, lower case | `Ch`, `ch` |
| a register's bit-field struct | Pascal case of the register | `Conf0` |
| a field's enum | the struct's name, then Pascal case of the field | `LcrWls` |
| an enum's variant | Pascal case of the enumerated value | `Eight` |
| an alternate group | lower case | `in dlab` |

A register's struct that meets the peripheral type it is declared next to is
renamed, as the struct of `RTC_CNTL`'s register `RTC_CNTL` is `RtcCntl_`. A
peripheral type is shared by the peripherals derived from the one it is
named for. One SVD element repeated by a list or an array is one register
type, and one field repeated by `dim` is one enum.

Steps 2 and 4 change what the SVD wrote, and a name meeting another is
renamed: either is the warning `svd::renamed`, pointing at the `<name>` of
the element (at the register, for an alternate group), and labeling the other
item when two meet. Step 5 is not warned. A name with nothing left after
step 2 is the error `svd::invalid-name`, but for an enumerated value, which
is then no variant.

### What each construct becomes (ADR 0072)

| SVD | Erebos |
|---|---|
| `peripheral` | `peripheral NAME: TYPE at baseAddress;`, with a type of its own in a module |
| `dim` on a peripheral | one peripheral per index, `dimIncrement` apart, all of one type |
| `derivedFrom` on a peripheral | its base's registers and what the base states, then its own; one that states no `registers` and none of `size`, `access`, `resetValue`, `resetMask` shares its base's type |
| `alternatePeripheral` | `over` that peripheral; a list is `over` its first element |
| `register` | `access NAME: TYPE at addressOffset`, the type being the unsigned integer of its `size`, or its bit-field struct when it has fields |
| `dim` on a register, `[%s]` name | a register array, `step dimIncrement` written when it is not the register's width |
| `dim` on a register, `%s` name, or a `dimIndex` other than `0` to `dim - 1` | one register per index (`dimIndex` as `2-3`, `A-C` or `A,B,C`, or `0` to `dim - 1`), at `addressOffset + i * dimIncrement` |
| `alternateGroup` | `in` that group in lower case |
| `alternateRegister`, `alternateCluster` | `in` the entry's own name: a group of its own (the one it names is not looked up); a register with both is in the `alternateGroup` |
| `readAction` on a register or a field | `read clears` (`clear`), `read sets` (`set`), `read modifies` (`modify`, `modifyExternal`) |
| `cluster` | a cluster of a peripheral type of its own, in a module, its registers' offsets counted from it; `dim` with `[%s]` is a cluster array with its `step`, with `%s` one cluster per index |
| `field` | `NAME: TYPE at BITS`, bits from `bitOffset` and `bitWidth`, `lsb` and `msb`, or `bitRange` (the first of the three the field states), `at 3` for one bit and `at 0..=7` for more |
| `dim` on a field | one field per index, `dimIncrement` bits apart, sharing one enum |
| a field's type | its enum when it has one; otherwise `bool` for one bit, and the smallest of `u8`, `u16`, `u32`, `u64` that holds the bits |
| `modifiedWriteValues` (field's, or its register's) | the field's write effect: `oneToClear` is `write 1 clears`, and so for the six |
| `resetValue` and `resetMask` | the field's default: `true`, `false`, an integer or `.Variant`; none when it has a write effect, when the mask leaves one of its bits unknown, or when its enum has no variant of that value |
| `enumeratedValues` | an enum on the field's integer type, a variant per `enumeratedValue` that has a `value`, in the file's order |
| `enumeratedValues` with `derivedFrom` | the enum written for the set it names, when the merged values equal the base's and the integer type is the same; otherwise a set of its own |

`size`, `access`, `resetValue` and `resetMask` are inherited from the device,
the peripheral and the clusters to their registers, as SVD has it, a register
stating its own over theirs; a `resetMask` stated nowhere covers every bit
of the register, and the bits of either above the register's size are not
read. A register without a `resetValue` at any level has no reset value,
and its fields no default; nothing is lost, so nothing is warned. A
register's **mode** is its `access`, inherited; when no element states one, it
is taken from its fields: `read` when there is at least one field and every
one is `read-only`, `write` when every one is `write-only` or `writeOnce`,
`read write` otherwise (a field that states no access counts for neither). A field's mode is written only where it differs from the
register's (`read-only` is `read`, `write-only` is `write`; `read-write`
takes no word).

**`derivedFrom`.** A name without `.` names an element of the same kind with
the same parent (a sibling peripheral, a register or cluster of the same
peripheral or cluster, a field of the same register); a set of enumerated
values is named by its own `name`, among the sets of every field of the same
register. A name with `.` is a path from the device
(`PERIPHERAL.REGISTER.FIELD`, through clusters by their names, and
`PERIPHERAL.REGISTER.FIELD.SET` for a set). The derived element is the base's children and what the
base states itself, then the derived element's own over them; what the base
inherits from its parents is not copied, the derived element inherits from
its own parents. A child of the derived element with the `name` of one of the
base's replaces it, where the base had it; the others are added after. A name
that finds nothing is `svd::unknown-reference`, and elements deriving from
each other `svd::derived-cycle`.

**Not read.** `addressUnitBits` and `width` (every offset counts bytes),
`addressBlock`, `groupName`, `headerStructName`, `prependToName`,
`appendToName`, `dataType`, `cpu` and `interrupt`: the generator writes no
interrupts (they are M4's). An `alternateRegister` or `alternateCluster`
naming nothing is no error.

### What has no declaration

What Erebos cannot declare is **declared as close as the language allows,
with a comment `// svd: ...` above the item saying what was left out, and
the warning `svd::not-declared` at the SVD element**. The whole file is
generated, and what it lost is shown both in the code and when it is
generated. A note that wraps repeats `// svd: ` on each line, so that a
search finds them all. They follow the item's description. Each case:

- **A register whose `size` is not 8, 16 or 32** is left out: no entry, no
  struct, only a block of comments with its description and why. (An array
  or a list of it too.)
- **`writeOnce` and `read-writeOnce`**, on a register or a field, are `write`
  and `read write`: that it can be written once is not declared.
- **A reset value that is not 0 on a register without fields**, and a
  **`modifiedWriteValues` other than `modify` on a register without
  fields**: there is no field to carry them. A register with fields has them
  carried by the fields.
- **`writeConstraint` and `protection`**, on a register, and `writeConstraint`
  on a field, at their own elements.
- **`modifiedWriteValues` `clear` and `set`** on a field, also when it is the
  register's that gives them: the field has no write effect (the language
  has none without a neutral value). A field assignment to another field of
  the register triggers it. An explicit `modify` is SVD's default, read as an
  absent one: no comment, no warning.
- **A reset value that is no variant of the field's enum**: the field has no
  default.
- **An enumerated value with don't-care bits** (`#1x0`), **one whose value
  another variant has**, and **one whose name has nothing left after step 2**
  are left out of the enum, the comment standing where the variant would be.
  An enum left with no variant is not written, and the field is an integer.
  An `enumeratedValue` without a `name`, or only with `isDefault`, is no
  variant, and nothing is warned.
- **Two sets of enumerated values on one field**, or two of any `usage`: a
  field has one type, so it is an integer (`bool` for one bit), with the
  comment and the warning at the field, labeling each set.

**Values that are no SVD value.** An `access`, `readAction`,
`modifiedWriteValues` or `usage` whose value SVD does not define (the
GD32VF103's `access` `write`, the CH32V003's `read/clear`) is read as
absent, with the warning `svd::invalid-value` at it and a comment at the
item: the rules above apply as if the element were not there. So a field
with `access` `write` takes its register's mode, and a register with one,
when no parent states an access, the mode its fields imply. The generator
does not guess what the vendor meant. An `isDefault` that is not `true`,
`false`, `1` or `0` is warned too and read as `false`, without a comment.

**Peripherals that share bytes.** The generator computes the extent of each
peripheral type as the compiler does, and examines the peripherals in the
order of the file. Let `S` be the earlier peripherals a peripheral `P` shares
a byte with. When `S` is empty, `P` has no `over`. Otherwise, the
candidates are the peripherals of the file other than `P` such that every
peripheral of `S` is the candidate or is written `over` it; when there is
one, `P` is written `over` **the first candidate in the order of the file**,
with the comment `// svd: the SVD does not say that it is over ...: found from
the bytes it shares with ...`, and no warning. A candidate need not share a
byte with `P`. When no candidate exists, as a peripheral covering two that do
not overlap each other, **`P` is written without `over`**, the warning
`svd::overlap` names the peripherals of `S`, and the generated module does
not compile: the compiler reports `ty::peripheral-overlap` there, as it
would for the same peripherals written by hand. An `over` the SVD states
(`alternatePeripheral`) is written as it is, and looks for no bytes. In the
ESP32-C3's file `APB_CTRL` comes first, so `RNG` is `over APB_CTRL`.

**What the generator does not check.** It does not repeat the compiler's
checks, but for the extents `over` needs: a field past its register's bits,
two fields sharing a bit, a misaligned offset, a step smaller than its
register, two registers at one offset or an address at or past 2³² generate
without an error, and are reported when the generated module is compiled,
at the generated code.

**Limits.** A hostile file must not overflow the stack or fill the memory, so
the reader limits: elements stand at most 32 levels deep (the peripheral
being 1; real files reach 6); at most 32 elements wait on a `derivedFrom` at
once; `derivedFrom` copies at most 262 144 elements in all; and a list (the
elements of a `dim` that is not an array, and a `dimIndex`) has at most
65 536 indices. The first three are `svd::too-large`, the last
`svd::invalid-dim`. A file of `n` equal names gets an output of `n²` bytes,
as step 6 of the names asks for ("as many times as it takes"), which no limit
here bounds. A peripheral that shares bytes with many earlier ones (a list of
`n` at one address) names the first 8 in its `// svd:` comment and in the
labels of `svd::overlap`, and then "and N more", so that its output stays
linear in `n`; finding them still compares every pair, in time quadratic in
the number of peripherals (at most 65 536 for a list).

### An example

`tests/e2e/svd_virt/virt.svd` describes the devices of QEMU `virt` the e2e
program of that directory uses, and `virt.ers` is what `erebos svd` writes
from it, kept so that its changes are reviewed (a test requires that the two
agree). The 16550's line control register, in the SVD:

```
<register>
  <name>LCR</name>
  <description>Line control: the format of a word and the divisor latch.</description>
  <addressOffset>0x3</addressOffset>
  <resetValue>0x0</resetValue>
  <fields>
    <field>
      <name>WLS</name>
      <description>Word length.</description>
      <bitRange>[1:0]</bitRange>
      <enumeratedValues>
        <enumeratedValue><name>FIVE</name><description>5 bits.</description><value>0</value></enumeratedValue>
        <!-- SIX, SEVEN -->
        <enumeratedValue><name>EIGHT</name><description>8 bits.</description><value>3</value></enumeratedValue>
      </enumeratedValues>
    </field>
    <field>
      <name>DLAB</name>
      <description>Divisor latch access: DLL and DLM replace RBR, THR and IER.</description>
      <bitOffset>7</bitOffset>
      <bitWidth>1</bitWidth>
    </field>
  </fields>
</register>
```

and the module, where the device's `size` 32 and `access` `read-write` and
the peripheral's `size` 8 are inherited (the register's mode, `read write`,
is the device's):

```
pub mod uart0 {
    // The 16550 UART: the console of the machine.
    pub peripheral Uart0 {
        // Line control: the format of a word and the divisor latch.
        pub read write lcr: Lcr at 0x3,
        // ...
    }

    pub struct Lcr: u8 {
        // Word length.
        pub wls: LcrWls at 0..=1 = .Five,

        // Divisor latch access: DLL and DLM replace RBR, THR and IER.
        pub dlab: bool at 7 = false,
    }

    pub enum LcrWls: u8 {
        // 5 bits.
        Five = 0,

        // 8 bits.
        Eight = 3,
    }
}
```

(The file has the four variants and two more fields.) The PLIC's `PRIORITY[%s]`
is a register array, `CTX[%s]` a cluster array with its own module, and
`PLIC_M0`, which the SVD does not say is over `PLIC`, is found from the bytes
it shares:

```
pub mod plic {
    pub peripheral Plic {
        // The priority of each source; QEMU keeps 3 bits.
        pub read write priority: [u32; 96] at 0x0,
        // ...
        // One context of hart 0: machine mode, then supervisor mode.
        pub ctx: [ctx::Ctx; 2] at 0x20_0000 step 0x1000,
    }

    pub mod ctx {
        // One context of hart 0: machine mode, then supervisor mode.
        pub peripheral Ctx {
            // The lowest priority the context takes.
            pub read write threshold: u32 at 0x0,
            // ...
        }
    }
}

// The machine mode context of the PLIC alone: a second view of bytes PLIC
// covers.
// svd: the SVD does not say that it is over `PLIC`: found from the bytes it
// svd: shares with `PLIC`.
pub peripheral PLIC_M0: plic_m0::PlicM0 at 0xC20_0000 over PLIC;
```

The program declares the module with `mod virt;`, brings in `virt::uart0::Lcr`
and writes `UART0.lcr = Lcr { wls: .Eight, .. };`, the `..` taking the defaults
the reset value gave.

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
    `pub use`, so that a program calls `core::memcpy`; the twelve
    functions of the ARM EABI, which LLVM calls instead of those three on
    `thumbv7m-none-eabi` (ADR 0074); and the 64-bit division functions,
    `__udivdi3`, `__umoddi3`, `__divdi3` and `__moddi3`, private, since
    programs write `/` and `%`.
  - The EABI functions are `__aeabi_memcpy`, `__aeabi_memmove`,
    `__aeabi_memset` and `__aeabi_memclr`, each also with the suffixes `4`
    and `8`, which promise a destination (and a source) aligned to 4 or
    8 bytes. Each calls `memcpy`, `memmove` or `memset` and ignores the
    promise, and none returns a value. `__aeabi_memcpy` and
    `__aeabi_memmove` take `(dest: *mut u8, src: *const u8, n: usize)`,
    `__aeabi_memclr` takes `(dest: *mut u8, n: usize)`, and `__aeabi_memset`
    takes `(dest: *mut u8, n: usize, c: i32)`: its length comes before the
    value, not after as in `memset`, and only the low byte of `c` is
    stored. They are private, and exist on
    every target: on riscv32 nothing calls them, and they cost a few
    hundred bytes, as the other support functions do where they are unused.
  - The division functions are called by LLVM on riscv32, where it does not
    expand a division by a constant, and by C code linked into a riscv32
    program; on thumbv7m code generation calls them itself (see
    "Targets"). They never divide 64-bit values themselves: all four go
    through `__erebos_udivmod64` to the restoring loop,
    `__erebos_udivmoddi4`.
  - The functions `__erebos_udivmoddi4(n: u64, d: u64, rem: *mut u64) ->
    u64` and `__erebos_divmoddi4(n: i64, d: i64, rem: *mut i64) -> i64`
    return the quotient and store the remainder. They are `pub` inside the
    private `builtins`, so that `core::arch` names them in `sym` operands and
    programs cannot, and they are not `#[export]` (their symbols are
    `core.builtins.__erebos_udivmoddi4` and
    `core.builtins.__erebos_divmoddi4`). They are what `__aeabi_uldivmod` and
    `__aeabi_ldivmod` call (see below).

  Each support function is `#[export]`, so that its symbol is the name
  LLVM calls (see "Symbols"); their private helpers are not
  (`core.builtins.__erebos_udivmod64`). The compiler marks every function
  of `core` `no-builtins`, so that LLVM does not turn their loops back into
  calls to them.
- **`core::arch`** (ADR 0080, 0081) is the part of `core` that needs an
  instruction of the target, written with `asm` (see "Inline assembly"). It
  is a public module whose file is chosen by the build's target, not by the
  rule of ADR 0049:

  | Target | File of `core::arch` |
  |--------|----------------------|
  | `riscv32imac-unknown-none-elf` | `rt/arch/riscv32.ers` |
  | `thumbv7m-none-eabi` | `rt/arch/armv7m.ers` |

  Both files are embedded in the compiler, a build reads only its own
  target's, and diagnostics name it by that path. The files have no modules
  of their own, and `core::arch` follows every other rule of `core`: private
  items are private. A **portable item** is declared in every file with the
  same kind, name, signature and meaning, so a program that uses only portable
  items builds for every target; every other item is **specific** to its
  target, and a program that names one builds only for it: on another target
  the path names nothing, and the error is the one of any missing item
  (`resolve::unknown-function` for a call), not a message that it exists
  elsewhere. A test
  builds a program that calls every portable item on every target, at both
  optimization levels.

  The portable items are:

  - `pub fn compiler_fence()`: emits no instruction. No access to memory that
    other code can see (statics, memory reached through pointers), ordinary
    or volatile, moves across it in the generated code. It orders nothing in
    the hardware, which is enough for code and a handler on one core.
  - `pub fn fence()`: a compiler fence and a hardware fence. Every access to
    memory or to a device before it, ordinary or volatile, is observed by
    every other observer (a device, a DMA engine, another bus master) before
    any access after it. It is `fence iorw, iorw` on riscv32 and `dmb sy` on
    thumbv7m. It orders; it does not wait for a write to reach a device, nor
    make a cache coherent (neither target of the tests has a data cache).

  Both are an `asm` with `options(nostack, preserves_flags)` and no option
  about memory, and calling either at compile time is `eval::asm`. Finer
  fences (`fence w,o`, `dmb st`, `dsb`, `isb`) are not portable: a program
  that needs one writes its `asm`, and `core::arch` may gain target-specific
  ones with the step that needs them. See "Volatile accesses" for the hand-off
  of memory to a device.

  Specific to thumbv7m, private and `#[export]`: the naked functions
  `__aeabi_uldivmod` and `__aeabi_ldivmod` (see "Targets"). Each file begins
  with the comment lines `// portable: <declaration>` that list its portable
  items.

  Every file also defines the reset entry, the private `#[export]` `#[naked]`
  function `_start` (ADR 0083, see "Startup and memory"). It is in no list of
  portable items: it exists on every target, but no program can name it, and
  only the compiler does, by the path `core::arch::_start`, to put it in the
  section `.text.start`. A file of `core::arch` without it is a bug of the
  compiler, not a diagnostic.
- `Option`, `Result`, `assert`, `len` and `slice` are not in `core`: they
  stay built-in names, seen from every module.
- A program item whose symbol is one of `core`'s, such as a root `fn
  memcpy` or an `#[export] fn memset` in any module, is
  `resolve::duplicate-symbol`, whose help names the function of `core` to
  call; an item named with `__` is `resolve::reserved-name`.
- `core` has no entry or panic function. Every program includes all of
  it, the functions the program does not call too, and the `core::arch` file
  of its target.
- The `--emit` dumps leave `core` out (see "Building"). An error reported
  in a file of `core` is a compiler bug, except `eval::asm` for a compile-time
  call of a function of `core::arch`, whose first label is the `asm` inside
  the target's file.

## Panic information (ADR 0053)

```
use core::{PanicInfo, PanicReason};

panic fn on_panic(info: *const PanicInfo) -> ! {
    let reason: *const [u8] = match (*info).reason {
        PanicReason::DivisionByZero => &raw const b"division by zero",
        PanicReason::IndexOutOfBounds => &raw const b"index out of bounds",
        PanicReason::AssertionFailed => &raw const b"assertion failed",
        PanicReason::NoVariant => &raw const b"enum value is no variant",
        PanicReason::FieldOutOfRange => &raw const b"value does not fit its field",
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
    FieldOutOfRange = 4,
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
  | `IndexOutOfBounds` | `a[i]`, `(*s)[i]` (ADR 0021, 0031), `P.a[i]` and `P.c[i]` with `a` a register array and `c` a cluster array (ADR 0064) | the `[` |
  | `AssertionFailed` | `assert(c)`, `assert(c, msg)` (ADR 0029, 0031) | `assert` |
  | `NoVariant` | a `match` without a catch-all arm (ADR 0036, 0042) | `match` |
  | `NoVariant` | `e?` (ADR 0046) | the `?` |
  | `FieldOutOfRange` | a value written to a bit field that does not fit its bits, by a literal, an assignment or a compound assignment (ADR 0059) | the field's name |

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
erebos build program.ers [-o program.elf] [-O] [--link file.o]... [--target <name>]
                         [--emit=<stage>]
```

`program.ers` is the root file: the files of its modules are read from its
directory, and every file is named relative to it (see "Modules and
paths"). `-O` enables LLVM optimizations. `--link` adds an object file
defining `extern fn`s and `extern static`s; it may be repeated. `--target`
names the machine to build for, `riscv32imac-unknown-none-elf` (the default)
or `thumbv7m-none-eabi` (see "Targets"); it may be given once, and it needs
a name. `--emit` prints a compiler stage instead: `tokens`, `ast`, `hir`,
`typeck`, `mir`, `linker-script` or `llvm-ir`, those of the chosen target.

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
  `stack`, then every struct's layout, every bit-field struct (see below)
  and every enum with its integer type and the values of its variants, an
  enum with payloads also with the offset of each payload value, its size
  and its alignment, then each `Option(T)` and `Result(T, E)` used, sorted
  by name, and list the bodies run at compile time as `comptime fn`, named
  by path, a variant's value as `comptime fn Mode::Run`, the values of the
  `memory` block as `comptime fn origin`, `size` and `stack_size`, a
  register's offset as `comptime fn offset`, a peripheral's address as
  `comptime fn address`, a bit position of a bit field as `comptime fn
  bits` and a bit field's default as `comptime fn default`.
- Both list each peripheral type, by path, with its entries' types and
  offsets, its extent and its alignment, and each peripheral with its type
  and address (see "Peripherals"). `typeck` lists them between the
  `memory` block and the structs, with each entry's visibility and mode
  and the range each peripheral covers; `mir` lists them after the enums
  and before the statics, numbering the entries as the projections that
  reach them name them (`(*_1).1`). Each entry also shows its alternate
  group; `typeck` shows a register's read effect too, in the order of the
  syntax (`read wide: u32 at 0x4 read clears in ctrl`), while `mir` shows
  only the group, after the offset (`3 dll: u8 at 0x0 in dlab`). An entry
  that is an array shows its type with its length and its step: `typeck`
  after the offset, with the step in force (the width by default), as in
  `pub read write priority: [u32; 96] at 0x0 step 0x4`, and `mir` before
  it, as in `0 priority: [u32; 96] step 0x4 at 0x0`; a cluster shows no
  mode (`pub ctx: [PlicContext; 2] at 0x200000 step 0x1000`). A type
  whose extent passes 2³² shows `extent past 2³²` in `typeck`. A
  peripheral `over` another shows it in `typeck` after the address (`peripheral
  RNG: Rng at 0x60026000 over APB_CTRL covers [...)`), and not in `mir`.
- Both list each bit-field struct, after the structs and before the enums:
  `typeck` as `struct Lcr: u8 { dlab: bool at 7 = false, ... }`, with each
  field's mode (`read busy: bool at 0`), type, bits, effects and default
  (the variant's path for an enum); `mir` with each field's mode and type,
  an enum's shown as its integer, and its bits, as `struct Lcr: u8 { dlab:
  bool at 7 }`, without defaults or effects. A bit field access is labeled
  `bit field NAME` in `typeck`, and a literal `struct Lcr` or `struct Lcr
  ..` when it ends with `..`.

```
erebos svd <input.svd> [-o <output.ers>]
```

writes the Erebos module of an SVD file, and no executable (see "Peripherals
from SVD"). It exits with status 1 for an error of the generator and 2 for a
bad command line, as `build` does.

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
| `lexer::invalid-byte` | malformed byte literal, or a character or escape not allowed in a byte literal, byte string or string literal |
| `lexer::unterminated-byte` | byte literal without closing `'` |
| `lexer::unterminated-byte-string` | `b"` or `"` without its closing `"` on the same line |
| `parser::unexpected-token` | a token where it cannot appear, such as `_` as a name or a value, `pub` before a variant, or a `use` of a path made only of `crate`, `super` or `core`; in a bit-field struct, a field without `at`, half-open bits (`at 0..1`), a second effect of a kind, an effect word that is none, `write 2 clears`; in an ordinary struct, `at` or `=` after a field's type; a `..` that is not last in a literal; in a peripheral type, an entry that starts with neither an access mode nor a name followed by `:`, a `step` without its value or after the read effect or the group, a read effect on a cluster, `over` without a path or after the `;`; `read write` on a bit field, or a mode after its name; in `asm`, a missing template, a word that is no operand or option, a missing `,` or `)`, an empty `options()`, an operand out of its group or after `options(...)`, a name on an operand with an explicit register or on `clobber_abi` |
| `parser::unclosed-block` | `{` without its `}` |
| `parser::let-without-type` | `let` without `: Type`, also in a pattern |
| `parser::chained-comparison` | `a < b < c` |
| `parser::local-const` | `const` inside a function body |
| `parser::local-static` | `static` or `extern static` inside a function body |
| `parser::local-mod` | `mod` inside a function body |
| `parser::local-use` | `use` inside a function body |
| `parser::local-memory` | a `memory` block inside a function body: `memory {` followed by `region` and a name, or by `code`, `data` or `stack` and `in` |
| `parser::local-peripheral` | a peripheral type or a peripheral inside a function body: `peripheral` followed by a name |
| `parser::misplaced-pub` | `pub` on a `comptime` block, a `memory` block or the entry function |
| `parser::enum-without-type` | an enum without `: Type` |
| `parser::variant-without-value` | a variant without `= value` |
| `parser::empty-payload` | a variant written `V()`, a payload without types |
| `parser::string-outside-asm` | a string literal anywhere but inside `asm`; use `b"..."` for bytes |
| `resolve::module-file` | the file of `mod name;` does not exist, cannot be read, has a name that differs in case, or is the root file |
| `resolve::unknown-name` | a name, or a path's last segment, that names no value where one is expected, also in a pattern; an enum's or struct's name alone; a name only a module around the current one has; the path of a `sym` operand that names nothing |
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
| `resolve::private-field` | a struct literal of a struct with a field that is not visible here, unless `..` leaves it out and it has a default or a write effect |
| `resolve::duplicate-param` | two parameters with one name |
| `resolve::mut-extern-param` | a `mut` parameter in an `extern fn` |
| `resolve::duplicate-field` | a field declared, or given in a literal, twice |
| `resolve::unknown-field` | a struct literal names a field the struct does not have |
| `resolve::missing-fields` | a struct literal leaves fields out; its help offers `..` when every field left out of a bit-field struct has a default or a write effect |
| `resolve::missing-default` | `..` in a literal that leaves out a visible field without a default or a write effect, or `..` in a literal of an ordinary struct |
| `resolve::bit-struct-type` | a bit-field struct whose type after `:` is not `u8`, `u16`, `u32` or `u64` |
| `resolve::empty-bit-struct` | a bit-field struct without fields |
| `resolve::write-effect-default` | a default on a field with a write effect |
| `resolve::enum-type` | an enum whose type after `:` is not an integer type |
| `resolve::duplicate-variant` | two variants with one name |
| `resolve::empty-enum` | an enum without variants |
| `resolve::unknown-variant` | `E::V` where `E` has no variant `V`; a path followed by patterns in parentheses that names no variant |
| `resolve::not-an-enum` | `X::V` where `X` names neither a module nor an enum: a struct, a constant, a variant, a built-in type or function, or `Option` or `Result`, which are no items and whose variants are written `.V` |
| `resolve::not-a-constant` | a name or path in a pattern that is not a constant: a local variable, parameter, static, peripheral, function, struct or enum |
| `resolve::duplicate-binding` | one name bound twice in one pattern, or in one alternative of an or-pattern |
| `resolve::inconsistent-binding` | a name not bound by every alternative of an or-pattern, or bound with `let mut` by only some |
| `resolve::reserved-name` | an item, module or `use` named `_start`, `core`, with a name starting with `__`, or like a built-in function (`assert`, `len`, `slice`); a module of the root module named `llvm` or `rt`; a region named `INCLUDE` |
| `resolve::unknown-attribute` | an attribute that does not exist |
| `resolve::misplaced-attribute` | an attribute on an item it does not apply to, as `#[packed]` or `#[align(N)]` on a bit-field struct, or `#[naked]` on an `extern fn`, the entry function, the panic function or anything but a function with a body |
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
| `resolve::duplicate-register` | two entries (registers, register arrays or clusters) with one name in a peripheral type |
| `resolve::not-a-peripheral-type` | the type of a peripheral, or of a cluster, names a type that is no peripheral type (a cluster's may be an array of one); the help of a cluster names the access mode when the type is a register's |
| `resolve::missing-step` | a cluster array without `step` |
| `resolve::misplaced-step` | `step` on a register or a cluster that is no array |
| `resolve::not-a-peripheral` | `over` naming something that is no peripheral |
| `resolve::over-itself` | a peripheral naming itself after `over` |
| `resolve::outside-loop` | `break`/`continue` outside a loop |
| `resolve::break-value-in-while` | `break value` in a `while` |
| `resolve::return-in-comptime` | `return` or `?` in code run at compile time on its own: a `comptime` expression or block, the initializer of a constant or static, a variant's value, an array length, a repeat count, an `#[align(N)]` argument, a value of a `memory` block, a register's offset or a peripheral's address |
| `ty::mismatched-types` | a value of the wrong type, or a `.V` where the expected type, or the position's type, is not an enum |
| `ty::untyped-literal` | a literal or array literal with no expected type |
| `ty::untyped-variant` | a `.V` with no expected type, as a `match` scrutinee, the operand of `as` or `?`, or an expression statement |
| `ty::unknown-variant` | a `.V` where the expected enum, or the position's enum, has no variant of that name |
| `ty::literal-out-of-range` | literal does not fit its type |
| `ty::invalid-operand` | operator applied to a type it does not accept, such as a comparison of slices or of enums with payloads, or any operator on bit-field structs |
| `ty::negate-unsigned` | unary `-` on an unsigned value |
| `ty::shift-out-of-range` | shift amount known at compile time outside 0 to the width minus 1 |
| `ty::division-by-zero` | divisor known at compile time to be zero |
| `ty::invalid-cast` | `as` between types that do not convert, a slice cast the table refuses, any cast to `!` or to an enum, or from an enum with payloads; to a bit-field struct from anything but its integer type (itself included), or from one to anything but an integer type |
| `ty::pointer-cast-width` | thin pointer cast to or from an integer type other than `usize` |
| `ty::deref-non-pointer` | `*` on a value that is not a pointer |
| `ty::cannot-index` | indexing a value that is not an array or a pointer, a register or a single cluster included |
| `ty::index-out-of-bounds` | index known at compile time not below the array's length, a register array's or a cluster array's included |
| `ty::slice-pointer-index` | indexing a slice pointer directly (`s[i]`); write `(*s)[i]` |
| `ty::no-fields` | field access on a value with neither fields nor registers |
| `ty::unknown-field` | field access to a field the struct does not have |
| `ty::private-field` | a place or `offset_of` naming a field that is not visible here |
| `ty::private-variant` | `.V` naming a variant of an enum that is not reachable here |
| `ty::write-through-const` | assignment through a `*const` pointer |
| `ty::not-assignable` | assignment to something that is not a place, or to part of a temporary; an `inout` operand of `asm` without `=>` whose expression is not a place |
| `ty::assign-immutable` | assignment to a variable without `mut`, a `static`, an `extern static` without `mut`, or a byte string |
| `ty::not-a-place` | `&raw` of a value that is not a place, or of a constant or any element or field of it |
| `ty::address-of-immutable` | `&raw mut` of a place that cannot be written |
| `ty::packed-field-address` | `&raw` of a possibly misaligned place inside a packed struct |
| `ty::volatile-too-wide` | volatile access wider than the target's widest access, a bit-field struct on `u64` included |
| `ty::volatile-aggregate` | volatile access to a whole struct, array or enum with payloads, or to a slice stored in memory |
| `ty::volatile-packed-field` | volatile access to a possibly misaligned place inside a packed struct, a bit-field struct on `u16` or `u32` included |
| `ty::too-large` | a value larger than half the address space |
| `ty::infinite-size` | structs and enums that contain themselves, directly, through each other or through `Option` and `Result`; peripheral types that contain themselves through clusters |
| `ty::unsized` | `[T]` where a size is needed, or `*s` read or written whole |
| `ty::never-in-memory` | `!` as a pointee, slice element, field, payload type, type argument of `Option` or `Result`, array element, register type, the type of a static, `extern static`, `const` or `comptime` value, a layout query operand, or under `&raw` |
| `ty::invalid-align` | `#[align(N)]` on a struct or static with `N` not a power of two, too large or below the natural alignment |
| `ty::aligned-in-packed` | a packed struct containing a type with `#[align(N)]`, at any depth, such as an enum with such a payload |
| `ty::not-comptime` | a run-time variable where a compile-time value is required, in a `const` operand of `asm` too |
| `ty::cycle` | an item or variant whose value or layout needs itself, a bit-field struct whose bits (positions, field types, the variants of an enum field type) need themselves, or a default that needs its own value, including a `match` that needs the values of an enum (a variant pattern, `E::V` or `.V`, or a constant pattern at a position of its type, or no arm on it) in a function that runs while one of them is computed |
| `ty::duplicate-discriminant` | two variants of an enum with one value |
| `ty::match-type` | a `match` on a type other than an integer, `bool` or enum, a bit-field struct included |
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
| `ty::misaligned-code` | the code's region starting at an address that is not a multiple of the target's code alignment (4 on riscv32, 128 on thumbv7m) |
| `ty::stack-size` | a stack of size 0, not a multiple of the target's stack alignment (16 on riscv32, 8 on thumbv7m), or that does not fit in its region |
| `ty::register-type` | a register whose type is not `u8`, `u16` or `u32`, or a bit-field struct or an enum without payloads on one of them, or an array of such a type; an array of arrays, a peripheral type or an array of one |
| `ty::misaligned-register` | an entry's offset that is not a multiple of its element's alignment |
| `ty::misaligned-step` | a `step` that is not a multiple of the element's alignment |
| `ty::step-too-small` | a `step` smaller than the element's size |
| `ty::register-overlap` | two entries of one alternate group, or both of none, sharing a byte, unless both are registers or register arrays, one `read` and the other `write` |
| `ty::misaligned-peripheral` | a peripheral's address that is not a multiple of its type's alignment |
| `ty::peripheral-out-of-range` | a peripheral whose end is above `0x1_0000_0000` |
| `ty::peripheral-overlap` | two peripherals sharing a byte, unless one is `over` the other or both are `over` the same one |
| `ty::peripheral-by-value` | a peripheral type where a value would have it, an array of one anywhere but as a cluster's type, or a peripheral, `*u`, register array, cluster or cluster array's element read, written or passed as a whole |
| `ty::volatile-peripheral` | `volatile` on a pointer to a peripheral type |
| `ty::peripheral-index` | indexing a pointer to a peripheral type |
| `ty::unknown-register` | a name that is no entry of the peripheral type |
| `ty::private-register` | an entry that is not visible here |
| `ty::write-to-read-only` | a write, a compound assignment or a field assignment to a `read` register; a field assignment or a compound assignment to a `read` field in a volatile place |
| `ty::read-of-write-only` | a read, a compound assignment or a field assignment of a `write` register, or a field read from one; a read of a `write` field, or a compound assignment to one, in a volatile place |
| `ty::register-address` | `&raw` of a register, of a register array or a cluster array taken whole, or of an element of a register array |
| `ty::bit-field-address` | `&raw` of a bit field, a register's too |
| `ty::read-with-effects` | a field assignment or a compound assignment to a place whose reads have effects, or to one of its fields |
| `ty::compound-write-effect` | a compound assignment to a field with a write effect in a volatile place |
| `ty::bit-field-type` | a bit field whose type is not `bool`, `u8`, `u16`, `u32`, `u64` or an enum without payloads on one of these four |
| `ty::reversed-bit-range` | bits written `at a..=b` with `a > b` |
| `ty::bit-field-outside` | a bit field reaching past the last bit of its struct's integer type |
| `ty::bit-field-width` | a `bool` field of more than one bit, an integer field wider than its type or than its enum's integer type, or an enum variant that does not fit in its field |
| `ty::bit-field-overlap` | two bit fields sharing a bit; once per pair, at the later field |
| `ty::bit-field-offset` | `offset_of` of a bit field |
| `ty::field-value-out-of-range` | a value, known at compile time, written to a bit field by a literal or an assignment, or a default, that does not fit in the field's bits |
| `ty::asm-template` | in the template of an `asm`: a placeholder naming no operand, an unused operand, an explicit register named in the template, an unmatched brace, a `{}` or `{N}` past the last operand without a name, two operands with one name, a byte outside `0x01` to `0x7F` |
| `ty::asm-register` | an unknown or reserved register in an `asm` operand; one named twice among the inputs or among the outputs, or by an `in` and an `out`; an output on `reg` beside `clobber_abi` |
| `ty::asm-operand-type` | an operand of `asm` that is not an integer of at most 32 bits or a thin pointer, an output into a bit field, the two sides of an `inout ... =>` of different types |
| `ty::asm-sym` | `sym` naming something other than a function or a static: a constant, a struct, a module, an enum, a variant, a peripheral |
| `ty::asm-options` | options of `asm` that contradict each other or the operands (`nomem` with `readonly`; `pure` without an output or without `nomem` or `readonly`; `noreturn` with an output), an option or `clobber_abi` given twice, an ABI other than `"C"` |
| `ty::naked-body` | the body of a `#[naked]` function that is not exactly one `asm` expression with `noreturn` and only `sym` and `const` operands |
| `eval::panic` | compile-time code reached the panic function: a zero divisor, an index out of bounds, an enum value that is no variant in a `match` that no arm matches or under `?`, a value that does not fit the bit field it is written to, an explicit call |
| `eval::assertion-failed` | `assert` with a false condition at compile time, with its message if it has one |
| `eval::step-limit` | compile-time code ran more than 10,000,000 steps |
| `eval::call-depth` | compile-time code nested more than 256 calls |
| `eval::asm` | compile-time code reached an `asm` expression: directly, through a call, or a naked function, `core::arch`'s fences included |
| `eval::volatile-access` | a volatile access at compile time, through a volatile pointer or to a register |
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
| `codegen::asm` | an instruction LLVM's assembler rejects, at the `asm` expression with the assembler's message, or without a label when the error exists only in the whole object (a non-local label defined twice by inlining, no register left for the operands) |
| `codegen::llvm` | LLVM rejected the target, or reported an error that is not about inline assembly; an environment problem |
| `link::no-memory-map` | building an executable, or printing its linker script, for a program without a `memory` block |
| `link::region-overflow` | what is placed in a region does not fit in it, found by the linker |

**Diagnostics of `erebos svd`.** The generator reports through the same
diagnostics, with spans into the SVD file. A diagnostic has a severity, an
error or a **warning**: an error stops the generation and nothing is written;
a warning is shown and the module is written. **Only the generator emits
warnings**; the compiler reports errors alone. The codes of the generator are
below, the four warnings marked.

| Code | Meaning |
|------|---------|
| `svd::xml` | a file that is not well-formed XML, with a DTD, or not UTF-8 (without a span, then) |
| `svd::not-svd` | a root element other than `device`, or no `peripherals` |
| `svd::missing-element` | an element SVD requires and no parent supplies: the device's `name`, a peripheral's `name` or `baseAddress`, a register's `name`, `addressOffset` or `size`, a cluster's `name` or `addressOffset`, a field's `name` or bits, an enumerated value with neither `value` nor `isDefault` |
| `svd::invalid-number` | a number SVD's syntax does not allow (decimal, `0x`, `#` for binary), one that does not fit where 32 bits are needed, a `bitRange` that is not `[msb:lsb]`, a `bitWidth` of 0, an `msb` below the `lsb` |
| `svd::invalid-dim` | a `dim` of 0 or without `dimIncrement`, a `dimIndex` that is no list or range, goes down or has another length than `dim`, a `[%s]` or `%s` missing from a repeated name, a list of more than 65 536 indices |
| `svd::unknown-reference` | a `derivedFrom` naming no element of its kind, or an `alternatePeripheral` naming no peripheral |
| `svd::derived-cycle` | elements deriving from each other |
| `svd::too-large` | a file that passes a limit of the reader: elements more than 32 levels deep (nested or through `derivedFrom`), more than 32 elements waiting on a `derivedFrom`, or more than 262 144 elements copied by `derivedFrom` |
| `svd::invalid-name` | a name with nothing left after its characters are replaced, an empty one included; not for an enumerated value |
| `svd::renamed` (warning) | an item renamed because its SVD name has characters no identifier has or starts with a digit, or because another of its scope has its name |
| `svd::not-declared` (warning) | an SVD construct the generated item does not declare: a register of a size other than 8, 16 or 32, `writeOnce`, a reset value or `modifiedWriteValues` on a register without fields, `writeConstraint`, `protection`, `modifiedWriteValues` `clear` or `set`, a reset value that is no variant, an enumerated value with don't-care bits, a duplicate value or no name, two sets on a field |
| `svd::invalid-value` (warning) | an `access`, `readAction`, `modifiedWriteValues`, `usage` or `isDefault` whose value SVD does not define, read as absent (as `false` for `isDefault`) |
| `svd::overlap` (warning) | a peripheral sharing bytes with earlier ones that one `over` cannot name |
