# Erebos language reference (M1)

This describes the language as the compiler implements it today. The
reasoning behind each rule lives in the ADRs (`docs/adr/`), cited as
"ADR NNNN". When this document and the compiler disagree, that is a bug in
one of them.

## Programs

A program is one source file (`.ers`) plus the runtime support functions
(`rt/builtins.ers`, ADR 0020). It is a list of functions, and must contain
exactly one **entry function** and one **panic function**:

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
  division by zero (ADR 0014). It takes no parameters, must return `!`, and
  may be called explicitly.

The target is RV32IMAC on QEMU `virt` (ADR 0010).

## Lexical structure

- **Comments:** `// to end of line` and `/* block */`; block comments nest.
- **Identifiers:** `[A-Za-z_][A-Za-z0-9_]*`. Names starting with `__` are
  reserved for the runtime, and `_start` for the startup code.
- **Keywords:** `as break const continue else entry false fn if let loop mut
  panic return true volatile while`.
- **Integer literals** (ADR 0016): `42`, `0x2A`, `0o52`, `0b10_1010`; `_`
  separates digits. A decimal literal with a leading zero (`017`) is an
  error. Values must fit in 64 bits.
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
| `*const volatile T`, `*mut volatile T` | raw pointers whose accesses are volatile (ADR 0013) |

Every `let`, parameter and return type is written explicitly; there is no
inference (ADR 0008). `-> ()` may be omitted (ADR 0015).

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
- All functions use the C ABI of the target (ADR 0006); symbol names are not
  mangled.

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

## Expressions

### Operators, from tightest to loosest

| Precedence | Operators | Associativity |
|------------|-----------|---------------|
| 1 | calls `f(x)` | |
| 2 | unary `-` `!` `*` (deref) | prefix |
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

So `flags & MASK == 0` means `(flags & MASK) == 0`, and `a < b < c` is an
error.

### Typing

- Both operands of an arithmetic, bitwise or comparison operator have the
  same type; nothing converts implicitly.
- A literal takes the type expected where it appears; with no expected type
  it is an error. In `x + 1` the `1` takes the type of `x`. A literal
  operand of `as` takes the target type, or `usize` for a pointer target
  (ADR 0017).
- `+ - * / %` and unary `-` need integers; unary `-` needs a signed one.
- `& | ^ !` work on integers (bitwise) and `bool` (logical, without
  short-circuit).
- `&& ||` work on `bool` and short-circuit.
- `== !=` work on integers, pointers and `bool`; `< <= > >=` on integers and
  pointers.

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

### Pointers

- `*p` reads or, as an assignment target, writes through `p`. Writing needs
  `*mut`.
- Accesses through a `volatile` pointer are performed exactly once and in
  program order.
- A pointer is a plain address (`p as usize` is its address). Converting an
  integer to a pointer does not make it valid to dereference; alignment,
  existence and permissions are not checked (ADR 0019). Address 0 is an
  ordinary address.

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

Operands and arguments run left to right; `&&`/`||` may skip their right
side. In an assignment the value runs before the place. A read through a
pointer happens exactly where it is written.

## Building

```
erebos build program.ers [-o program.elf] [-O] [--emit=<stage>]
```

`-O` enables LLVM optimizations. `--emit` prints a compiler stage instead:
`tokens`, `ast`, `hir`, `typeck`, `mir` or `llvm-ir` (dumps leave out the
runtime). See `CONTRIBUTING.md` for setup and running on QEMU.

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
| `resolve::unknown-type` | type name that does not exist |
| `resolve::unknown-function` | call to a function that does not exist |
| `resolve::not-a-function` | call to a variable |
| `resolve::call-entry` | call to the entry function |
| `resolve::duplicate-function` | two functions with one name |
| `resolve::duplicate-param` | two parameters with one name |
| `resolve::reserved-name` | `_start` or a name starting with `__` |
| `resolve::missing-entry`, `resolve::multiple-entries` | not exactly one `entry fn` |
| `resolve::missing-panic`, `resolve::multiple-panics` | not exactly one `panic fn` |
| `resolve::outside-loop` | `break`/`continue` outside a loop |
| `resolve::break-value-in-while` | `break value` in a `while` |
| `ty::mismatched-types` | a value of the wrong type |
| `ty::untyped-literal` | a literal with no expected type |
| `ty::literal-out-of-range` | literal does not fit its type |
| `ty::invalid-operand` | operator applied to a type it does not accept |
| `ty::negate-unsigned` | unary `-` on an unsigned value |
| `ty::shift-out-of-range` | constant shift amount not below the width |
| `ty::division-by-zero` | literal zero divisor |
| `ty::invalid-cast` | `as` between types that do not convert |
| `ty::pointer-cast-width` | pointer cast to or from a type other than `usize` |
| `ty::deref-non-pointer` | `*` on a value that is not a pointer |
| `ty::write-through-const` | assignment through a `*const` pointer |
| `ty::not-assignable` | assignment to something that is not a place |
| `ty::assign-immutable` | assignment to a variable without `mut` |
| `ty::if-without-else` | `if` without `else` where a value is expected |
| `ty::wrong-arg-count` | call with the wrong number of arguments |
| `ty::param-type` | parameter of type `()` or `!` |
| `ty::special-fn-params` | entry or panic function with parameters |
| `ty::entry-returns`, `ty::panic-returns` | entry or panic function that can return |
| `codegen::llvm` | LLVM rejected the target; an environment problem |
