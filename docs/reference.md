# Erebos language reference (M2.3b)

This describes the language as the compiler implements it today. The
reasoning behind each rule lives in the ADRs (`docs/adr/`), cited as
"ADR NNNN". When this document and the compiler disagree, that is a bug in
one of them.

## Programs

A program is one source file (`.ers`) plus the runtime support functions
(`rt/builtins.ers`, ADR 0020). It is a list of items, in any order:
functions, external functions, structs, constants, statics, external
statics and `comptime` blocks.
Items share one namespace, and may refer to each other whatever their
order (ADR 0028). It must contain exactly one **entry function** and
one **panic function**:

```
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
  division by zero or an index out of bounds (ADR 0014). It takes no
  parameters, must return `!`, and may be called explicitly.

The target is RV32IMAC on QEMU `virt` (ADR 0010).

## Lexical structure

- **Comments:** `// to end of line` and `/* block */`; block comments nest.
- **Identifiers:** `[A-Za-z_][A-Za-z0-9_]*`. Names starting with `__` are
  reserved for the runtime, and so are the C names it defines (`memcpy`,
  `memmove`, `memset`) and `_start`.
- **Keywords:** `align_of as break comptime const continue else entry
  extern false fn if let loop mut offset_of panic return size_of static
  struct true volatile while`. `raw` is a keyword only right after `&`.
- **Built-in functions:** `assert`, `len` and `slice` are reserved names:
  no item may have one of them. A call to one of these names is always the
  built-in; variables may still be named so (ADR 0031).
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
| `[T; N]` | an array of `N` values of type `T`; `N` is a `usize` computed at compile time (ADR 0021, 0029) |
| `*const [T]`, `*mut [T]` | slices: the address of `T` values and how many there are (ADR 0031); `volatile` as for any pointer |
| `Name` | a struct declared with `struct Name { ... }` (ADR 0021) |

Every `let`, parameter and return type is written explicitly; there is no
inference (ADR 0008). `-> ()` may be omitted (ADR 0015). Arrays and structs
are **aggregates**, and so are slices where a rule depends on it.

A pointer to a sized type is a **thin pointer**: a plain address. `[T]` on
its own is **unsized**: it exists only behind a pointer, and a variable,
parameter, return value, field or array element of type `[T]`, a `static`,
`extern static` or `const` of that type, a layout query (`size_of`,
`align_of`, `offset_of`) of it, and a slice of `[T]` are errors
(`ty::unsized`). `[[u8; 4]]` is a slice of
arrays, and fine.

**`!` is not a type of memory** (ADR 0034). No value of type `!` exists,
so it is an error (`ty::never-in-memory`) as the pointee of a pointer
(`*const !`), the element of an array or slice type (`[!; 0]`,
`*const [!]`), a field type, the type of a `static`, `extern static`,
`const` or `comptime` value, an operand of `size_of`, `align_of` or
`offset_of`, and under `&raw` (`&raw const x` with `x: !`). Each `!`
written there is reported once, also where nothing uses it, and the type
containing it causes no further error. `!` stays valid as the type of an
expression, as a return type (`-> !`) and in a `let` whose initializer
never finishes (`let x: ! = loop {};`). An array literal or repeat with an
element of type `!` (`[loop {}; 3]`) has type `!`; its other elements are
checked against the expected element type, or the first element whose type
is not `!`. Use `()` for a type without data.

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
- All functions use the C ABI of the target (ADR 0006); symbol names are not
  mangled, so C code can call any Erebos function by its name.

### Aggregates by value (ADR 0025)

Structs and arrays are parameters and return values like any other type,
passed as the riscv32 C ABI says:

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
returned in `a0`, `a1` (ADR 0031).

### External functions (ADR 0026)

```
extern fn c_checksum(data: *const u8, n: usize) -> u32;
```

declares a function defined outside the program, in C, assembly or a
vendor library, which the linker finds; `erebos build --link file.o` adds
the object file. It is called like any function. Its parameters cannot be
`mut`, and nothing checks the declaration against the definition: as in C,
a mismatch is undefined.

## Statements and blocks

```
let x: u32 = 1;          // immutable
let mut y: u32 = 2;      // mutable
x + y;                   // expression statement
```

A block `{ ... }` is an expression (ADR 0015). Its value is its final
expression without `;`; with no such expression it has type `()`, or `!` if
it cannot reach its end. A block-like expression (`if`, `while`, `loop`,
`{ }`) may stand as a statement without `;`, but must then have type `()`.

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

- A static is a top-level item with an explicit type and an initializer,
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
- Every static is evaluated and emitted, used or not, with its own name as
  its symbol: C code can declare `extern uint32_t TICKS;`.
- **Where they live.** A `static` goes to `.rodata`. A `static mut` goes to
  `.data`, or to `.bss` when its initial value is all zero bytes. Padding
  in initial values is zero.
- **Read-only memory.** The compiler assumes a `static` never changes and
  may use its value instead of reading it. Writing to one through a pointer
  made by a cast is not defined; at compile time it is an
  `eval::read-only-write`.
- `#[align(N)]` before a static raises its alignment, with `N` following
  the struct rules (`ty::invalid-align`). Unlike on a struct, it does not
  change the size: `DMA_BUF` is 512 bytes at an address that is a multiple
  of 64. `#[packed]` does not apply to statics
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

## Layout (ADR 0022)

| Type | Size | Alignment |
|------|------|-----------|
| `u8 i8 bool` | 1 | 1 |
| `u16 i16` | 2 | 2 |
| `u32 i32 usize isize`, thin pointers | 4 | 4 |
| `u64 i64` | 8 | 8 |
| slices (`*const [T]`, `*mut [T]`) | 8 | 4 |
| `[T; N]` | `N * size(T)` | `align(T)` |
| `()` | 0 | 1 |

`[T]` and `!` have no layout a program can ask for (see "Types").

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
  not below the natural alignment, and rounds its size up to `N`. Before a
  static, it raises the static's alignment by the same rules, without
  changing its size.
- `#[packed]` and `#[align(N)]` cannot be combined, and a packed struct
  cannot contain a type with `#[align(N)]`.
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
  (`[v; N]`) and the argument of `#[align(N)]` are `usize` expressions,
  such as `[u8; 2 * SECTOR]`. Two array types are the same when their
  lengths have the same value; diagnostics show the value.

What runs at compile time follows the target exactly (32-bit `usize`,
wrapping arithmetic, the layouts above), and cannot:

- use the enclosing function's variables (`ty::not-comptime`), nor `return`
  out of it;
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
- read bytes never written, such as the padding of a variable (the padding
  of a `static` reads as zero, as in the object file);
- call the panic function, explicitly or through a failed check; the panic
  function's body never runs at compile time;
- run more than 10,000,000 steps (MIR statements and terminators) or nest
  more than 256 calls. Each evaluation has its own budget.

A value that needs itself, directly or through other items, is a
`ty::cycle`; reading a `static` asks for its value, but taking its address
does not. Data the compiler emits has zero padding, so builds are
reproducible byte for byte.

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

### Messages in `assert` (ADR 0031)

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
- At run time, the panic function takes no parameters yet (M2.6), so the
  message is evaluated and dropped before it is called.

## Expressions

### Operators, from tightest to loosest

| Precedence | Operators | Associativity |
|------------|-----------|---------------|
| 1 | calls `f(x)`, indexing `a[i]`, fields `s.f` | left |
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
and `a < b < c` is an error.

### Typing

- Both operands of an arithmetic, bitwise or comparison operator have the
  same type; nothing converts implicitly (ADR 0008), with two exceptions:
  where a `*const T` is expected, a `*mut T` with the same `T` and
  volatility is accepted, since it only loses permission to write
  (ADR 0024); and where a slice is expected, a pointer to an array of its
  elements is accepted, with the array's length (ADR 0031, see "Slices").
  These apply wherever an expected type is known, such as a `let`, an
  argument (including an `assert` message), an assignment, `return`,
  `break` values, struct fields, array elements, `if` branches, the tail
  of a block and the operand of `comptime`. Only the outermost pointer converts, volatility never
  changes, and comparisons accept a `*mut T` and a `*const T` in either
  order.
- A literal takes the type expected where it appears; with no expected type
  it is an error. In `x + 1` the `1` takes the type of `x`. A literal
  operand of `as` takes the target type, or `usize` for a pointer target
  (ADR 0017). An array literal takes its element type from the expected
  type or from its first element with a type of its own.
- `+ - * / %` and unary `-` need integers; unary `-` needs a signed one.
- `& | ^ !` work on integers (bitwise) and `bool` (logical, without
  short-circuit).
- `&& ||` work on `bool` and short-circuit.
- `== !=` work on integers, thin pointers and `bool`; `< <= > >=` on
  integers and thin pointers. Slices cannot be compared.
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

### Checks on values known at compile time (ADR 0032)

An expression is **known at compile time** when it is built only from
literals, `const` items, `comptime` expressions, `size_of`, `align_of`,
`offset_of`, unary `-` and `!`, binary operators, `as` and parentheses.
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
- the message of an `assert` whose condition is known `true`.

```
const BITS: u32 = 32;
let mask: u32 = if BITS < 32 { (1 << BITS) - 1 } else { 0xFFFF_FFFF };
```

These checks run when a body is lowered to MIR, after type checking, for
every body that type checks (functions, and the initializers of constants
and statics, `comptime` expressions and blocks), whatever errors other
bodies have; `--emit=typeck` stops before them. A body in which they fail
is not run at compile time: an evaluation that needs it reports nothing
more.

### Conversions with `as` (ADR 0019)

- Between integers: widening sign-extends a signed source and zero-extends
  an unsigned one; narrowing keeps the low bits; same width reinterprets.
- `bool` converts to integers (`true` is 1); nothing converts to `bool`
  (write `x != 0`).
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

### Volatile accesses (ADR 0013, 0023)

Through a volatile pointer, every access is to a scalar and is **exactly
one load or store of the scalar's width**, never split, merged, removed or
reordered relative to other volatile accesses. Volatility carries through
fields and indexing: `(*regs).chan[i].status` is one access of `status`'s
width. `x += 1` is one load and one store, not atomic. Otherwise the
program is rejected:

- a whole struct or array, or a slice stored in memory, cannot be
  accessed through a volatile pointer; through a volatile slice,
  `(*s)[i]` is one access of the element;
- `u64` and `i64` cannot, since riscv32 has no 64-bit access: use two
  `u32` halves in the order the device needs;
- a field reached through a packed struct cannot be, if its type needs
  alignment.

Volatile accesses are not ordered against ordinary ones, and no fences are
emitted: until M4, handing memory to a device (DMA) cannot be written with
a guarantee.

### Control flow

- `if cond { ... } else { ... }` is an expression; `cond` must be `bool`.
  Both branches have the same type; without `else`, the `if` has type `()`.
- `while cond { ... }` has type `()`, even with a constant `true`.
- `loop { ... }` runs until `break`. Its type is that of its `break` values,
  or `!` if it has no `break`. `break value` is only allowed in `loop`.
- `continue` jumps to the next iteration; `break` and `continue` refer to
  the innermost loop.
- `return` or `return value` leaves the function.
- `break`, `continue` and `return` have type `!`.

### Evaluation order (ADR 0018)

Operands, arguments, array elements and struct literal fields run left to
right, in the order written; `&&`/`||` may skip their right side, and
`assert` runs its message only when the condition is false (ADR 0031). In
an assignment the value runs before the place. In `base[index]`, the base
runs before the index, and the bounds check before the access. A read
through a pointer, and a read of a static (ADR 0030), happens exactly where
it is written. A value that is not a place, used as the base of indexing
or a field, is read where it is written too: in `({ x })[{ x[0] = 9; 0 }]`
the block reads `x` before the index changes it.

## Startup and memory (ADR 0012, 0030)

`_start`, generated by the compiler and visible in `--emit=llvm-ir`, runs
at the reset address and does, in order:

1. set the stack pointer to the end of RAM;
2. fill `.bss` with zeros;
3. copy the initial values of `.data` from their load address to their
   run address;
4. jump to the entry function.

Nothing else runs before the entry function: there are no constructors and
no order of initialization. The loops work a word at a time between
symbols the linker script defines (`__bss_start`, `__bss_end`,
`__data_start`, `__data_end`, `__data_load`), and do nothing when a range
is empty.

**Memory layout until M3.** The generated linker script lays the program
out as on a board whose program is in flash, splitting QEMU's 128 MiB of
RAM in two regions:

| Region | Addresses | Holds |
|--------|-----------|-------|
| `IMAGE` | `0x8000_0000` to `0x83FF_FFFF` | `.text`, `.rodata`, and the load image of `.data` |
| `RAM` | `0x8400_0000` to `0x87FF_FFFF` | `.data`, `.bss`, and the stack, growing down from `0x8800_0000` |

So `.data` is copied on every boot, on QEMU too. A program and its `.data`
image must fit in `IMAGE`, and `.data`, `.bss` and the stack in `RAM`. In
M3, memory maps declared in the language replace this split.

Object files given with `--link` may use the small-data sections
(`.srodata`, `.sdata`, `.sbss`, as GCC emits by default and clang with
`-msmall-data-limit`): they are gathered with `.rodata`, `.data` and
`.bss`, and initialized likewise. A section the script does not place is a
link error, not data left uninitialized. No `__global_pointer$` is
defined, so nothing is addressed relative to `gp`.

## Runtime support functions (ADR 0020)

Every program includes `rt/builtins.ers`: the 64-bit division functions
riscv32 needs (`__udivdi3`, `__umoddi3`, `__divdi3`, `__moddi3`) and
`memcpy`, `memmove` and `memset`, which LLVM calls for large copies and
fills. Programs may call them; they cannot define functions with these
names.

## Building

```
erebos build program.ers [-o program.elf] [-O] [--link file.o]... [--emit=<stage>]
```

`-O` enables LLVM optimizations. `--link` adds an object file defining
`extern fn`s and `extern static`s; it may be repeated. `--emit` prints a
compiler stage instead: `tokens`, `ast`, `hir`, `typeck`, `mir` or
`llvm-ir` (dumps leave out the runtime; `typeck` and `mir` start with every
struct's layout, and list the bodies run at compile time as `comptime fn`).
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
| `parser::unexpected-token` | a token where it cannot appear |
| `parser::unclosed-block` | `{` without its `}` |
| `parser::let-without-type` | `let` without `: Type` |
| `parser::chained-comparison` | `a < b < c` |
| `parser::local-const` | `const` inside a function body |
| `parser::local-static` | `static` or `extern static` inside a function body |
| `resolve::unknown-name` | name not in scope |
| `resolve::unknown-type` | type or struct name that does not exist |
| `resolve::unknown-function` | call to a function that does not exist |
| `resolve::not-a-function` | call to a variable |
| `resolve::call-entry` | call to the entry function |
| `resolve::duplicate-item` | two items (functions, structs, constants, statics) with one name |
| `resolve::duplicate-type` | a struct named like a built-in type |
| `resolve::duplicate-param` | two parameters with one name |
| `resolve::mut-extern-param` | a `mut` parameter in an `extern fn` |
| `resolve::duplicate-field` | a field declared, or given in a literal, twice |
| `resolve::unknown-field` | a struct literal names a field the struct does not have |
| `resolve::missing-fields` | a struct literal leaves fields out |
| `resolve::reserved-name` | `_start`, a name starting with `__`, a runtime function's name, or a built-in function's (`assert`, `len`, `slice`) |
| `resolve::unknown-attribute` | an attribute that does not exist |
| `resolve::misplaced-attribute` | an attribute on an item it does not apply to |
| `resolve::attribute-argument` | an attribute with a missing or extra argument |
| `resolve::duplicate-attribute` | an attribute given twice |
| `resolve::conflicting-attributes` | `#[packed]` with `#[align(N)]` |
| `resolve::missing-entry`, `resolve::multiple-entries` | not exactly one `entry fn` |
| `resolve::missing-panic`, `resolve::multiple-panics` | not exactly one `panic fn` |
| `resolve::outside-loop` | `break`/`continue` outside a loop |
| `resolve::break-value-in-while` | `break value` in a `while` |
| `resolve::return-in-comptime` | `return` in code run at compile time |
| `ty::mismatched-types` | a value of the wrong type |
| `ty::untyped-literal` | a literal or array literal with no expected type |
| `ty::literal-out-of-range` | literal does not fit its type |
| `ty::invalid-operand` | operator applied to a type it does not accept, such as a comparison of slices |
| `ty::negate-unsigned` | unary `-` on an unsigned value |
| `ty::shift-out-of-range` | shift amount known at compile time outside 0 to the width minus 1 |
| `ty::division-by-zero` | divisor known at compile time to be zero |
| `ty::invalid-cast` | `as` between types that do not convert, a slice cast the table refuses, or any cast to `!` |
| `ty::pointer-cast-width` | thin pointer cast to or from an integer type other than `usize` |
| `ty::deref-non-pointer` | `*` on a value that is not a pointer |
| `ty::cannot-index` | indexing a value that is not an array or a pointer |
| `ty::index-out-of-bounds` | index known at compile time not below the array's length |
| `ty::slice-pointer-index` | indexing a slice pointer directly (`s[i]`); write `(*s)[i]` |
| `ty::no-fields` | field access on a value that is not a struct |
| `ty::unknown-field` | field access to a field the struct does not have |
| `ty::write-through-const` | assignment through a `*const` pointer |
| `ty::not-assignable` | assignment to something that is not a place, or to part of a temporary |
| `ty::assign-immutable` | assignment to a variable without `mut`, a `static`, an `extern static` without `mut`, or a byte string |
| `ty::not-a-place` | `&raw` of a value that is not a place, or of a constant or any element or field of it |
| `ty::address-of-immutable` | `&raw mut` of a place that cannot be written |
| `ty::packed-field-address` | `&raw` of a possibly misaligned place inside a packed struct |
| `ty::volatile-too-wide` | volatile access wider than the target's widest access |
| `ty::volatile-aggregate` | volatile access to a whole struct or array, or to a slice stored in memory |
| `ty::volatile-packed-field` | volatile access to a possibly misaligned place inside a packed struct |
| `ty::too-large` | a value larger than half the address space |
| `ty::infinite-size` | structs that contain themselves, directly or through each other |
| `ty::unsized` | `[T]` where a size is needed, or `*s` read or written whole |
| `ty::never-in-memory` | `!` as a pointee, slice element, field, array element, the type of a static, `extern static`, `const` or `comptime` value, a layout query operand, or under `&raw` |
| `ty::invalid-align` | `#[align(N)]` on a struct or static with `N` not a power of two, too large or below the natural alignment |
| `ty::aligned-in-packed` | a packed struct containing a type with `#[align(N)]` |
| `ty::not-comptime` | a run-time variable where a compile-time value is required |
| `ty::cycle` | an item whose value or layout needs itself |
| `ty::if-without-else` | `if` without `else` where a value is expected |
| `ty::wrong-arg-count` | call with the wrong number of arguments |
| `ty::param-type` | parameter of type `()` or `!` |
| `ty::special-fn-params` | entry or panic function with parameters |
| `ty::entry-returns`, `ty::panic-returns` | entry or panic function that can return |
| `eval::panic` | compile-time code reached the panic function: a zero divisor, an index out of bounds, an explicit call |
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
