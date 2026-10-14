# Erebos language reference (M2.2)

This describes the language as the compiler implements it today. The
reasoning behind each rule lives in the ADRs (`docs/adr/`), cited as
"ADR NNNN". When this document and the compiler disagree, that is a bug in
one of them.

## Programs

A program is one source file (`.ers`) plus the runtime support functions
(`rt/builtins.ers`, ADR 0020). It is a list of items, functions, external
functions and structs, in any order. It must contain exactly one **entry function** and
one **panic function**:

```
entry fn main() -> ! {
    loop {}
}

panic fn on_panic() -> ! {
    loop {}
}
```

- `entry fn` is where execution starts (after the provisional `_start`,
  ADR 0012). It takes no parameters, must return `!`, and cannot be called.
- `panic fn` runs when an operation cannot produce a result, such as
  division by zero or an index out of bounds (ADR 0014). It takes no
  parameters, must return `!`, and may be called explicitly.

The target is RV32IMAC on QEMU `virt` (ADR 0010).

## Lexical structure

- **Comments:** `// to end of line` and `/* block */`; block comments nest.
- **Identifiers:** `[A-Za-z_][A-Za-z0-9_]*`. Names starting with `__` are
  reserved for the runtime, and so are the C names it defines (`memcpy`,
  `memmove`, `memset`) and `_start`.
- **Keywords:** `align_of as break const continue else entry extern false
  fn if let loop mut offset_of panic return size_of struct true volatile
  while`.
  `raw` is a keyword only right after `&`.
- **Integer literals** (ADR 0016): `42`, `0x2A`, `0o52`, `0b10_1010`; `_`
  separates digits. A decimal literal with a leading zero (`017`) is an
  error. Values must fit in 64 bits. There are no suffixes (`1u8`).
- **Byte literals:** `b'a'` has type `u8`. Printable ASCII is written
  directly; escapes are `\n \r \t \\ \' \" \0` and `\xNN` (two hex digits).
- There are no `char`, string or floating-point literals.

## Types

| Type | Meaning |
|------|---------|
| `u8 u16 u32 u64 usize` | unsigned integers; `usize` is pointer-sized (32 bits) |
| `i8 i16 i32 i64 isize` | two's-complement signed integers |
| `bool` | `true` or `false` |
| `()` | the unit type: no value to speak of |
| `!` | never: an expression that does not finish |
| `*const T`, `*mut T` | raw pointers |
| `*const volatile T`, `*mut volatile T` | raw pointers whose accesses are volatile (ADR 0013, 0023) |
| `[T; N]` | an array of `N` values of type `T`; `N` is an integer literal (ADR 0021) |
| `Name` | a struct declared with `struct Name { ... }` (ADR 0021) |

Every `let`, parameter and return type is written explicitly; there is no
inference (ADR 0008). `-> ()` may be omitted (ADR 0015). Arrays and structs
are **aggregates**.

## Functions

```
fn add(a: u32, mut b: u32) -> u32 {
    b += a;
    b
}
```

- Functions may be called before their declaration, and may recurse.
- Arguments are checked against parameter types with no conversion.
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
no arrays by value, wraps them in a struct.

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
  below the length, the program calls the panic function. A constant index
  out of bounds is a compile-time error.
- **Pointer indexing is not.** With `p: *const T` or `*mut T`, `p[i]` is the
  place at address `p + i * size_of(T)`, computed with wrapping arithmetic,
  as in C. `(*p)[i]` with `p: *mut [T; N]` is the checked form.
- `let`, assignment and reading a place copy aggregates whole. `==` and
  `!=` do not apply to them; compare their parts.
- A struct may contain itself only behind a pointer (`next: *mut Node`).
- In the condition of `if` and `while`, a `{` opens the body, so a struct
  literal there needs parentheses: `if (Point { x: 0, y: 0 }).x == 0 { }`.

### Addresses: `&raw`

`&raw const place` and `&raw mut place` give a `*const T` or `*mut T` to a
place: a variable, `*pointer`, `a[i]`, `p[i]` or a field. `&raw mut` needs
a mutable place. The result is never volatile. A plain `&` is reserved for
references (M5).

`&raw const p[i]` is how a pointer is offset by `i` elements.

## Layout (ADR 0022)

| Type | Size | Alignment |
|------|------|-----------|
| `u8 i8 bool` | 1 | 1 |
| `u16 i16` | 2 | 2 |
| `u32 i32 usize isize`, pointers | 4 | 4 |
| `u64 i64` | 8 | 8 |
| `[T; N]` | `N * size(T)` | `align(T)` |
| `()` | 0 | 1 |

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
  not below the natural alignment, and rounds its size up to `N`.
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

So `flags & MASK == 0` means `(flags & MASK) == 0`, `*p.x` means `*(p.x)`,
and `a < b < c` is an error.

### Typing

- Both operands of an arithmetic, bitwise or comparison operator have the
  same type; nothing converts implicitly (ADR 0008), with one exception:
  where a `*const T` is expected, a `*mut T` with the same `T` and
  volatility is accepted, since it only loses permission to write
  (ADR 0024). Only the outermost pointer converts, and comparisons accept
  the two in either order.
- A literal takes the type expected where it appears; with no expected type
  it is an error. In `x + 1` the `1` takes the type of `x`. A literal
  operand of `as` takes the target type, or `usize` for a pointer target
  (ADR 0017). An array literal takes its element type from the expected
  type or from its first element with a type of its own.
- `+ - * / %` and unary `-` need integers; unary `-` needs a signed one.
- `& | ^ !` work on integers (bitwise) and `bool` (logical, without
  short-circuit).
- `&& ||` work on `bool` and short-circuit.
- `== !=` work on integers, pointers and `bool`; `< <= > >=` on integers and
  pointers.
- A value may be at most 2147483647 bytes, half the address space.

### Integer semantics (ADR 0007)

- `+ - *` and unary `-` wrap around, in every build.
- Shift amounts may have any integer type and are masked to the width of
  the shifted value (`x << n` is `x << (n % bits)`). A constant amount that
  is out of range is an error. `>>` is arithmetic on signed values.
- `/` and `%` truncate toward zero; the remainder has the sign of the
  dividend. A zero divisor calls the panic function (a literal zero divisor
  is a compile-time error). For signed types, `MIN / -1` is `MIN` and
  `MIN % -1` is `0`.

### Conversions with `as` (ADR 0019)

- Between integers: widening sign-extends a signed source and zero-extends
  an unsigned one; narrowing keeps the low bits; same width reinterprets.
- `bool` converts to integers (`true` is 1); nothing converts to `bool`
  (write `x != 0`).
- Pointers convert only to and from `usize`; pointer-to-pointer casts may
  change mutability, volatility and pointee.
- Aggregates do not convert.

### Pointers

- `*p` reads or, as an assignment target, writes through `p`. Writing needs
  `*mut`.
- A pointer is a plain address (`p as usize` is its address). Converting an
  integer to a pointer does not make it valid to dereference; alignment,
  existence and permissions are not checked (ADR 0019). Address 0 is an
  ordinary address.

### Volatile accesses (ADR 0013, 0023)

Through a volatile pointer, every access is to a scalar and is **exactly
one load or store of the scalar's width**, never split, merged, removed or
reordered relative to other volatile accesses. Volatility carries through
fields and indexing: `(*regs).chan[i].status` is one access of `status`'s
width. `x += 1` is one load and one store, not atomic. Otherwise the
program is rejected:

- a whole struct or array cannot be accessed through a volatile pointer;
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
right, in the order written; `&&`/`||` may skip their right side. In an
assignment the value runs before the place. In `base[index]`, the base runs
before the index, and the bounds check before the access. A read through a
pointer happens exactly where it is written.

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
`extern fn`s; it may be repeated. `--emit` prints a compiler stage instead:
`tokens`, `ast`, `hir`, `typeck`, `mir` or `llvm-ir` (dumps leave out the
runtime; `typeck` and `mir` start with every struct's layout). See
`CONTRIBUTING.md` for setup and running on QEMU.

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
| `lexer::invalid-byte` | malformed byte literal or escape |
| `lexer::unterminated-byte` | byte literal without closing `'` |
| `parser::unexpected-token` | a token where it cannot appear |
| `parser::unclosed-block` | `{` without its `}` |
| `parser::let-without-type` | `let` without `: Type` |
| `parser::chained-comparison` | `a < b < c` |
| `resolve::unknown-name` | name not in scope |
| `resolve::unknown-type` | type or struct name that does not exist |
| `resolve::unknown-function` | call to a function that does not exist |
| `resolve::not-a-function` | call to a variable |
| `resolve::call-entry` | call to the entry function |
| `resolve::duplicate-function` | two functions with one name |
| `resolve::duplicate-type` | two structs with one name, or a struct named like a built-in type |
| `resolve::duplicate-param` | two parameters with one name |
| `resolve::mut-extern-param` | a `mut` parameter in an `extern fn` |
| `resolve::duplicate-field` | a field declared, or given in a literal, twice |
| `resolve::unknown-field` | a struct literal names a field the struct does not have |
| `resolve::missing-fields` | a struct literal leaves fields out |
| `resolve::reserved-name` | `_start`, a name starting with `__`, or a runtime function's name |
| `resolve::unknown-attribute` | an attribute that does not exist |
| `resolve::misplaced-attribute` | an attribute on an item it does not apply to |
| `resolve::attribute-argument` | an attribute with a missing or extra argument |
| `resolve::duplicate-attribute` | an attribute given twice |
| `resolve::conflicting-attributes` | `#[packed]` with `#[align(N)]` |
| `resolve::missing-entry`, `resolve::multiple-entries` | not exactly one `entry fn` |
| `resolve::missing-panic`, `resolve::multiple-panics` | not exactly one `panic fn` |
| `resolve::outside-loop` | `break`/`continue` outside a loop |
| `resolve::break-value-in-while` | `break value` in a `while` |
| `ty::mismatched-types` | a value of the wrong type |
| `ty::untyped-literal` | a literal or array literal with no expected type |
| `ty::literal-out-of-range` | literal does not fit its type |
| `ty::invalid-operand` | operator applied to a type it does not accept |
| `ty::negate-unsigned` | unary `-` on an unsigned value |
| `ty::shift-out-of-range` | constant shift amount not below the width |
| `ty::division-by-zero` | literal zero divisor |
| `ty::invalid-cast` | `as` between types that do not convert |
| `ty::pointer-cast-width` | pointer cast to or from a type other than `usize` |
| `ty::deref-non-pointer` | `*` on a value that is not a pointer |
| `ty::cannot-index` | indexing a value that is not an array or a pointer |
| `ty::index-out-of-bounds` | constant index not below the array's length |
| `ty::no-fields` | field access on a value that is not a struct |
| `ty::unknown-field` | field access to a field the struct does not have |
| `ty::write-through-const` | assignment through a `*const` pointer |
| `ty::not-assignable` | assignment to something that is not a place, or to part of a temporary |
| `ty::assign-immutable` | assignment to a variable without `mut` |
| `ty::not-a-place` | `&raw` of a value that is not a place |
| `ty::address-of-immutable` | `&raw mut` of a place that cannot be written |
| `ty::packed-field-address` | `&raw` of a possibly misaligned place inside a packed struct |
| `ty::volatile-too-wide` | volatile access wider than the target's widest access |
| `ty::volatile-aggregate` | volatile access to a whole struct or array |
| `ty::volatile-packed-field` | volatile access to a possibly misaligned place inside a packed struct |
| `ty::too-large` | a value larger than half the address space |
| `ty::infinite-size` | structs that contain themselves, directly or through each other |
| `ty::invalid-align` | `#[align(N)]` with `N` not a power of two, too large or below the natural alignment |
| `ty::aligned-in-packed` | a packed struct containing a type with `#[align(N)]` |
| `ty::if-without-else` | `if` without `else` where a value is expected |
| `ty::wrong-arg-count` | call with the wrong number of arguments |
| `ty::param-type` | parameter of type `()` or `!` |
| `ty::special-fn-params` | entry or panic function with parameters |
| `ty::entry-returns`, `ty::panic-returns` | entry or panic function that can return |
| `codegen::llvm` | LLVM rejected the target; an environment problem |
