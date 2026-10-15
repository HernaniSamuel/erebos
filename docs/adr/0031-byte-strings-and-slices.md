# 0031. Byte strings and slices

- Status: Accepted
- Date: 2026-10-15
- Amends: 0008 (an implicit conversion to slices), 0018 (the message of
  `assert` runs only if it fails), 0019 (only thin pointers are
  addresses), 0021 (indexing a slice pointer), 0022 (unsized types, and
  the layout of slices), 0023 (a slice is an aggregate for volatile
  accesses), 0025 (slices across calls), 0029 (`assert` takes a message)

## Context

ADR 0016 gives `b"hello"` the type `[u8; 5]` but leaves the literal for
when arrays exist. With arrays alone, a function can only take text of one
length: `fn print(s: *const [u8; 5])` accepts five bytes, and there are no
generics until M6. C passes a pointer and a length, as two parameters
that nothing ties together. Erebos needs a value that carries both.

ADR 0029 also left `assert` without a message until byte strings exist.

## Decision

### Byte strings

```
let greeting: [u8; 6] = b"hello\n";      // a copy
let p: *const [u8; 2] = &raw const b"ok"; // no copy
```

- `b"..."` has type `[u8; N]`, where `N` is the number of bytes it holds
  after escapes are resolved: `b"a\x00b"` is `[u8; 3]`, `b""` is `[u8; 0]`.
  Nothing is appended; a C string is written `b"abc\0"`.
- Its characters follow ADR 0016: printable ASCII written directly, and
  the escapes `\n \r \t \\ \' \" \0 \xNN`. A line break inside it is an
  error; write `\n`.
- A byte string is a **place in read-only memory**, like an unnamed
  `static` (ADR 0030) of alignment 1. `&raw const` gives its address
  without a copy, and indexing it (`b"abc"[i]`) reads one byte from it.
  Using it as a value copies it, as for any array. It cannot be written:
  `&raw mut` is `ty::address-of-immutable`, and writing to it through a
  cast pointer is not defined, as for a `static`.
- Each occurrence of a byte string in the source is one object, wherever
  and however often it is evaluated. Identical byte strings may share
  memory, with each other and with other read-only data holding the same
  bytes, so whether two of them have the same address is not defined; at
  compile time, comparing a pointer into a byte string with a pointer into
  another object is an `eval::pointer-compare` (ADR 0030).
- At compile time, a byte string is read-only memory with an address,
  like a `static`: it can be read and indexed, a pointer to it can be the
  result of an evaluation, and writing to it is an
  `eval::read-only-write`. It is the same object in every evaluation, and
  the one the program uses at run time.

### Slices

`[T]` is a sequence of `T` values whose length is known only at run time.
It exists only behind a pointer, `*const [T]` or `*mut [T]` (with
`volatile` as for any pointer), a **slice**:

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

**Representation.** A slice is the address of its first element and its
length in elements: `{ ptr: *const T, len: usize }`, 8 bytes with
alignment 4 on riscv32, exactly the C struct
`struct { const T *ptr; size_t len; }`. Across calls it follows ADR 0025:
two registers, as that struct would.

**A slice is not an address.** ADR 0019 says every pointer is a plain
address as wide as `usize`, convertible both ways without loss; that now
holds for **thin pointers** only, those to sized types. A slice is an
aggregate of two scalars, and is treated as one wherever a rule depends on
it:

- **Volatile** (ADR 0023): reading or writing a slice stored in memory
  through a volatile pointer is a `ty::volatile-aggregate`, as for a
  struct, since it takes two accesses. Accesses to elements through a
  volatile slice are single accesses, as below.
- **Comparisons:** slices cannot be compared (`ty::invalid-operand`);
  compare their addresses and lengths.
- **Casts** follow ADR 0019, where a pointer cast may change mutability,
  volatility and pointee, except where a length would be invented or
  reinterpreted:

  | From | To | |
  |------|----|-|
  | slice of `T` | any thin pointer (`*const T`, `*mut U`, `*const [T; N]`, ...) | the address of the first element; the length is dropped |
  | slice of `T` | slice of `T` | changes mutability or volatility |
  | pointer to `[T; N]` | slice of `T` | length `N`; may change mutability or volatility; the explicit form of the conversion below |
  | slice of `T` | slice of `U`, `U` not `T` | `ty::invalid-cast` |
  | thin pointer, other than to an array of `T` | slice of `T` | `ty::invalid-cast`; use `slice(p, n)` |
  | slice, or integer | integer, or slice | `ty::invalid-cast` in either direction |

**Making a slice.**

- A pointer to an array converts **implicitly** to a slice of the same
  elements: `*const [T; N]` to `*const [T]`, and `*mut [T; N]` to
  `*mut [T]` or `*const [T]`, with length `N`. It applies in the
  positions where ADR 0024 applies, keeps volatility, and converts only the
  outermost pointer. The array's length is known, so nothing is lost or
  reinterpreted. This amends ADR 0008, like ADR 0024.
- `slice(p, n)`, built in, makes a slice of `n` elements starting at `p`:
  a `*const T` gives a `*const [T]` and a `*mut T` a `*mut [T]`, with the
  same volatility. Nothing is checked: like a pointer made from an integer
  (ADR 0019), the program vouches for it. It covers memory the program
  knows of from elsewhere (a buffer described by hardware, a region given
  by C) and sub-slices: `slice(&raw const (*s)[2], 3)`.

**Using a slice.**

- `len(s)`, built in, is the length of `s`, a `usize`. It reads no memory.
  Its argument is any slice, `*const` or `*mut`, volatile or not, or a
  pointer to an array, converted to a slice as above, keeping its
  volatility, with `T` taken from the argument: `len(&raw const b"hi")` is
  2.
- `(*s)[i]` is the place of element `i`, **checked** against the length:
  an index not below `len(s)` calls the panic function, with the same
  cause as for arrays (ADR 0021). It can be read, written through a
  `*mut [T]`, and passed to `&raw`. Through a volatile slice, it is a
  volatile access of the element (ADR 0023).
- `s[i]` on a slice is an error (`ty::slice-pointer-index`). Indexing a
  pointer steps over whole pointees (ADR 0021), and `[T]` has no size.
  The help suggests `(*s)[i]`, or `(s as *const T)[i]` for an unchecked
  access.
- `*s` can only be indexed or passed to `&raw`; reading or copying all of
  it is an error (`ty::unsized`).

**`[T]` is unsized.** A variable, parameter, return value, field or array
element of type `[T]`, a `static`, `extern static` or `const` of that
type, and `size_of`, `align_of` or a slice of `[T]`, are errors
(`ty::unsized`). `[[u8; 4]]` is a slice of arrays, and fine. A symbol of
unknown size, as C's `extern const uint8_t t[];`, is declared with a sized
type (`extern static t: u8;`) and turned into a slice with `slice`; an
unsized `extern static` can be allowed later without breaking programs.

**At compile time,** slices work as at run time. The rules on pointers
(ADR 0029, 0030) apply to the address inside: a slice of a byte string or
a static can be the result of an evaluation if it lies inside its object
(`eval::pointer-out-of-bounds` otherwise), and a slice of the
evaluation's own memory cannot (`eval::pointer-escapes`).

### Messages in `assert`

`assert(cond, msg)` takes an optional second argument of type
`*const [u8]`:

```
comptime {
    assert(size_of(Header) == 7, &raw const b"Header must match the wire format");
}
```

- `msg` is evaluated only if the assertion fails, like the right operand
  of `&&`. This is an exception to ADR 0018, where every argument runs, left
  to right, before the call.
- At compile time, `eval::assertion-failed` shows the message, with bytes
  outside printable ASCII written as `\xNN`. A message that cannot be read
  reports that error instead.
- At run time, the panic function has no parameters until M2.6, so the
  message is evaluated and dropped. M2.6 passes it on (ADR 0014).

### Built-in names

`len` and `slice` are built-in functions, like `assert`: items cannot have
those names (`resolve::reserved-name`). A call named `len` or `slice` is
always the built-in; variables may still have those names.

### Diagnostic codes

| Code | Meaning |
|------|---------|
| `lexer::unterminated-byte-string` | `b"` without its closing `"` on the same line |
| `ty::unsized` | `[T]` where a size is needed |
| `ty::slice-pointer-index` | indexing a slice pointer directly (`s[i]`) |

Malformed characters and escapes in byte strings are
`lexer::invalid-byte`, as in byte literals.

### Conformance tests

- **ui snapshots** for each code above; the implicit conversion in every
  position of ADR 0024 and in `len`, from `*mut [T; N]` and with
  volatility; a
  conversion that would change the element type or the volatility; `[u8]`
  as a variable, field, parameter and in `size_of`; `s[i]` with its help;
  invalid casts; `&raw mut` and assignment to a byte string; escapes and
  lengths of byte strings in the `typeck` dump; a failing `assert` with a
  message at compile time.
- **LLVM IR snapshots:** a byte string as private read-only data, shared
  when repeated; a slice parameter as two registers; `(*s)[i]` checked
  against the length; `len(s)` reading no memory.
- **Layout:** a struct holding a `*const [u8]` checked against clang
  (`tests/layout`).
- **End to end:** a slice passed to and returned from a C function, as a
  C struct; an index out of bounds in a slice reaching the panic function;
  `slice` over part of a buffer.
- **Differential tests** gain functions over slices and byte strings,
  such as summing the bytes of a message.
- **ui snapshots** for a volatile read of a slice stored in memory,
  comparisons of slices, each row of the cast table, `[u8]` as the type
  of a `static`, `extern static` and `const`, `len` of a volatile slice,
  and a slice of the evaluation's own memory as a result
  (`eval::pointer-escapes`).
- **Identity of byte strings:** a function returning `&raw const b"x"`
  gives the same pointer when called at compile time and at run time,
  checked on QEMU by comparing a `comptime` call with a run-time one.
- **The milestone program:** a `print` function writing byte strings and
  numbers to the UART, through slices, using statics (ADR 0030).

## Consequences

- Text and buffers of any length can be passed with one parameter, and
  accesses through it are checked.
- A byte string is an array, so passing one where a slice is expected takes
  `&raw const b"..."`, in every `print` and every `assert` message. That is
  the price of `b"..."` being `[u8; N]` (roadmap); references (M5) may make
  it lighter.
- "A pointer" now means two things: a thin pointer is an address; a slice
  is an address and a length.
- A checked access through a slice is written `(*s)[i]`, and an unchecked
  one through a plain pointer `p[i]`: the type tells them apart, as in
  ADR 0021, at the cost of a heavier syntax for the checked one. References
  (M5) will likely revisit this.
- `len` and `slice` become reserved item names. Modules (M2.6) may move
  the built-ins into `core`.
- Sub-slices are built with `slice` and `&raw`; ranges (`s[2..5]`) can come
  later.
- Unicode text stays out (ADR 0016): a byte string is bytes.

## Alternatives considered

- **`s[i]` as the checked access on slices** — shorter, but the same
  syntax would be checked on a slice and unchecked on a pointer to an
  array, which ADR 0021 avoided.
- **An explicit cast from array pointer to slice** — the conversion loses
  nothing, and every call with a byte string would need
  `as *const [u8]`, the noise ADR 0024 removed for `*mut` to `*const`.
- **A field, `s.len`** — reads like a struct field access through a
  pointer, which would be `(*s).len`; methods wait for M5.
- **Zero-terminated strings, as C** — the length must be found by
  scanning, a missing terminator overruns memory, and binary data cannot
  hold zeros. Slices carry the length.
- **`compile_error(msg)`, as Zig's `@compileError`** — Zig analyzes a
  function only for the arguments it is called with, so a branch can
  reject some of them. Erebos checks every function once (ADR 0028), so it
  would fail every function containing it; `assert` with a message covers
  today's uses. It returns with `comptime` parameters (M6).
- **Constants pointing at memory the evaluation created**, as Zig allows
  — the memory would have to be emitted as read-only data even when the
  pointer is `*mut`. Statics and byte strings give memory an address
  explicitly.
