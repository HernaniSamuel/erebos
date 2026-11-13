# 0113. Functions of a type

- Status: Accepted
- Date: 2026-11-13
- Amends: 0049 (a type before `::` names one of its functions; `use` names
  none; `self`, `Self` and `impl` become keywords), 0050 (the visibility of
  functions of a type), 0051 (their symbols), 0102 (a function of a type may
  be named `end`)

## Context

M5.5 brings methods (roadmap, ADR 0094). Until now every function is an item
of a module, and a driver's functions take their receiver as an ordinary
parameter: `ring::push(r: &mut Ring, b: u8)`, `uart::putc(u: &mut Uart16550,
byte: u8)`. Modules keep the names apart (ADR 0049), but every caller names
the module and the function, and the type the function is about appears only
in its signature.

ADR 0094 leaves to M5.5 `impl` blocks, the receivers `self`, `&self` and
`&mut self`, and `x.f()`. The roadmap's pending decisions add: functions of a
type without a receiver (`T::new()`); where an `impl` may be and its
visibility; and whether a function of a type may be named after a built-in
such as `end` or `len` (ADR 0102 reserves `end` for items). This ADR settles
the declarations: what a function of a type is, where it is written, how it
is named. ADR 0114 settles the call `x.f()`, which borrows the receiver, and
fields and indexes through a reference.

What other languages do:

- **Rust**: inherent `impl` blocks anywhere in the crate that defines the
  type, several per type; `Self` names the type; `self`, `&self`, `&mut self`
  and typed receivers (`self: Box<Self>`); every associated function is
  callable as `T::f(x, ...)`; associated constants and types.
- **Go**: methods are declared in the package of their type, one at a time,
  with the receiver written as a parameter.
- **Zig**: functions declared inside a struct are its namespace; `x.f()`
  passes `x` (or its address) as the first parameter when the types allow.

## Decision

### `impl` blocks

```
pub struct Ring {
    buf: [u8; 8],
    head: usize,
    count: usize,
}

impl Ring {
    pub fn new() -> Self {
        Ring { buf: [0; 8], head: 0, count: 0 }
    }

    pub fn push(&mut self, b: u8) -> bool { ... }

    pub fn count(&self) -> usize {
        self.count
    }
}
```

- `impl Name { fns }` is an item. `Name` is one name, never a path, and must
  name a **struct** (bit-field structs included), an **enum** or a
  **peripheral type** (clusters included, ADR 0064) **declared in the same
  module** as the `impl`, not one brought in by `use`. Anything else (a
  built-in type, `Option`, a type of another module, a constant, a static, a
  peripheral such as `UART0` rather than its type, a module) is
  `resolve::impl-type`; a name that names nothing is `resolve::unknown-type`.
  Either error is reported once, at the name, and the block's functions are
  not lowered, so that their `Self` and `self` give no further errors
  (the driver stops after lowering when lowering reports an error).
- A type may have **several** `impl` blocks, in its module, in any order, as
  items are (ADR 0049).
- An `impl` takes no `pub` (`parser::misplaced-pub`) and no attribute
  (`resolve::misplaced-attribute`). Inside a function body it is
  `parser::local-impl`, as `mod` and `use` are.
- Between its braces are only **functions with a body**: `fn` and `pub fn`,
  with the attributes `#[align(N)]` (ADR 0087) and `#[naked]` (ADR 0079).
  Any other item, an `extern fn`, an `entry fn`, a `panic fn` or an
  `interrupt fn` is `parser::impl-item`. `#[export]` on one is
  `resolve::misplaced-attribute`.
- `core` may have `impl` blocks for its own types, under the same rules.

These functions are the **functions of the type**. They are not items of the
module: the module's namespace does not hold them, and a single name never
names one. Each type has a namespace of its functions; for an enum, its
variants are in the same namespace (`Mode::Run` and `Mode::parse`). Two
functions of one type with one name, in one `impl` or in two, or a function
and a variant of one enum with one name, are `resolve::duplicate-item`. A
function may have the name of a field: `x.count` is the field, `x.count()`
the function (ADR 0114).

### Receivers

A function of a type whose first parameter is written in one of these forms
has a **receiver**, and is called a **method**:

| Written | Parameter | Type |
|---------|-----------|------|
| `self` | `self` | `Self` |
| `mut self` | `self`, assignable | `Self` |
| `&self` | `self` | `&Self` |
| `&mut self` | `self` | `&mut Self` |

- The receiver is the **first parameter**: its type comes from the `impl`, so
  it is written without one, and it is otherwise an ordinary parameter, a
  local named `self` (ADR 0008 is not touched: the type is the `impl`'s,
  not inferred).
- `self` anywhere else in a parameter list, or in a function that is not in
  an `impl`, is `parser::misplaced-self`. Other forms (`self: Ring`,
  `&volatile self`, `&&self`) are `parser::unexpected-token`.
- The rules for parameters hold: `self` by value of a peripheral type is
  `ty::peripheral-by-value` (ADR 0058), so a peripheral type's methods take
  `&self` or `&mut self`; a reference receiver is bound once, as every
  reference is (ADR 0105).
- `self` by value moves the value, or copies it if the type is copy (ADR
  0096). It is how a method consumes a linear value (ADR 0102): `fn wait(self)
  { ...; end(self); }`.
- No volatile receiver exists (`&volatile self`); a method is not called on a
  volatile place (ADR 0114). Adding them later breaks no program.

### `Self`

Inside an `impl`, `Self` is its type, written wherever the type's name could
be: in signatures and bodies, as a type (`-> Self`, `&mut Self`,
`*const Self`), in a struct literal (`Self { .. }`), and before `::` (`Self::new()`,
`Self::Run` for an enum). `Self` outside an `impl` is
`resolve::unknown-self`, as is `self` in an expression of a function that
has no receiver.

### Naming a function of a type

- A path whose segment names a struct, a bit-field struct or a peripheral
  type, followed by `::`, names a **function of that type** with its next
  segment, which ends the path: `Ring::new()`, `ring::Ring::new()`,
  `Self::new()`. Before an enum, the next segment is a variant or a function
  (ADR 0049 has variants only).
- **Every** function of a type is callable so, methods included, with the
  receiver passed as the first argument, of exactly its parameter's type:
  `Ring::push(&mut r, 1)`, `Ring::count(&r)`. Such a call is an ordinary call
  (ADR 0026's rules on arguments, ADR 0105's conversions); nothing is
  borrowed for it.
- A function of a type is named where any function may be: in a call, and in
  a `sym` operand of `asm` (`sym Ring::push`, its symbol, ADR 0079). There
  are no function values.
- A segment after a type that names no function (and, for an enum, no
  variant) is `resolve::unknown-function` in a call, saying in which type it
  was looked up, and the error of its position elsewhere (ADR 0049). A
  function of a type named where a type, a value or a pattern is expected
  (`let x: Ring::new`, `Ring::new` as a value) is the error of that position,
  as for any function, with a label calling it "a function of `Ring`".
  `resolve::not-an-enum` for a segment followed by `::` now covers what is
  neither a module, an enum nor a type that may have functions, and says so.
  `Option::None` keeps the help of ADR 0045.
- A `use` cannot name a function of a type (`resolve::use-type-function`), as
  it cannot name a variant (ADR 0049): the type is always written,
  `Ring::new()` or with `use ring::Ring;` first.

### Names

- A function of a type may have the name of a built-in function or type
  (`end`, `len`, `assert`, `slice`, `u32`, `Option`): it is only ever named
  after `T::` or `x.`, so `end(x)` written alone is still the built-in
  (ADR 0102), and `t.end()` or `Transfer::end(t)` the function of the type.
- The rules on names that protect symbols still hold: `_start`
  (`resolve::reserved-name`, ADR 0083) and names starting with `__` (ADR
  0052).
- `llvm` becomes a reserved name for a struct, a bit-field struct, an enum
  and a peripheral type of the root module, as it is for a module there (ADR
  0051, `resolve::reserved-name`): the symbols of its functions would start
  with `llvm.`, which LLVM gives a meaning of its own. In other modules the
  symbol starts with the module's path and the name is allowed.
- In the `hir` and `typeck` dumps, the functions of a type come in the order
  of their `impl` blocks and, within one, as written; blocks are ordered by
  file and offset (ADR 0049, "Order").
- In diagnostics and in the `hir` and `typeck` dumps, a function of a type is
  shown by its type's path and its name, `ring::Ring::push`, and `Ring::push`
  for a type of the root module (ADR 0049).

### Visibility

- A function of a type without `pub` is visible in the module that declares
  the type (the `impl`'s module) and its descendants, as a private item is
  (ADR 0050). One with `pub` is visible wherever the type is reachable, and
  only there: as for `.V` of a private enum (ADR 0050), a `pub` method of a
  type that is not reachable is not called through a value either
  (`m::make().f()` is `ty::private-method`, ADR 0114, when `m`'s type is
  private), so a module keeps
  the operations of its private types to itself.
- `T::f` is usable where each segment is visible, `f` included; otherwise
  `resolve::private-item`, pointing at the use and at the function. A
  method named by `x.f()` is judged the same way (ADR 0114).
- As for other items, visibility is checked where a name is used, never in
  signatures, and changes no symbol.

### Symbols

The symbol of a function of a type is the path of its type joined by dots,
then its name (ADR 0051): `push` of `Ring` in module `ring` is
`ring.Ring.push`; of a `Ring` of the root module, `Ring.push`; of a type of
`core`, `core.T.f`. A type and a module of one name cannot share a module's
namespace (`resolve::duplicate-item`), so such a symbol never equals the
symbol of an item, nor of another function of a type. `#[export]` does not
apply (above), so every function of a type has a path symbol.

### Diagnostic codes

| Code | Meaning |
|------|---------|
| `parser::local-impl` | `impl` inside a function body |
| `parser::impl-item` | an item other than a function with a body inside an `impl` |
| `parser::misplaced-self` | a `self` parameter that is not the first parameter of a function in an `impl` |
| `resolve::impl-type` | `impl` of a name that is not a struct, an enum or a peripheral type declared in this module |
| `resolve::unknown-self` | `Self` outside an `impl`, or `self` in a function without a receiver |
| `resolve::use-type-function` | a `use` naming a function of a type |

`parser::misplaced-pub` gains `pub impl`; `resolve::misplaced-attribute` an
attribute on an `impl` and `#[export]` on a function of a type;
`resolve::reserved-name` a type named `llvm` in the root module;
`resolve::duplicate-item` two functions of one type, or a function and a
variant, with one name; `resolve::not-an-enum` its wording. The codes of the
call `x.f()` are ADR 0114's. The new codes join the catalog of ADR 0069.

### What does not change

A function of a type is a function: MIR, compile-time evaluation, code
generation and the ABI (ADR 0006) see a function whose first parameter is the
receiver, as if it had been written `fn push(r: &mut Ring, b: u8)`. The
analyses of ADR 0094 and the check of shared statics (ADR 0093) are
unchanged. It may run at compile time like any function, for example as a
static's initializer (`static mut R: Ring = Ring::new();`). What other ADRs
say of functions holds for it without amendment: a `sym` operand names it by
its path as it names any function (ADR 0079), and `#[align(N)]` (ADR 0087)
and `#[naked]` apply as to any function with a body.

### Conformance tests

- **Parser snapshots**: `impl` with several functions, each receiver form,
  `Self` in each position, attributes on its functions; each `parser::` code.
- **ui snapshots**: each code above; `impl` of a struct, a bit-field struct,
  an enum with payloads, a peripheral type and a cluster's type, and of a type
  of another module, a `use`d type, a built-in type and a peripheral
  (rejected, with a body using `Self` and `self` that gives no further error);
  two `impl` blocks of one type; a function and a variant of one name; `T::f`
  and `Self::f` called from the module, a descendant (a private one too), a
  sibling (rejected when private) and through `use` of the type; a method
  called as `T::f(&mut x)`; a function of a type named as a type and as a
  value; functions named `end`, `len` and `u32`, with `end(x)` still the
  built-in beside them; `_start` and a root `struct llvm` rejected, a `struct
  llvm` in a submodule accepted; `self` by value of a peripheral type
  (`ty::peripheral-by-value`); a linear struct consumed by a `self` method;
  a function of a type as a static's initializer; `sym Ring::push` in `asm`.
- **LLVM IR snapshots**: the symbols of functions of types of the root module
  and of a nested module. `core` has no `impl` today, so its form
  (`core.T.f`) is tested when it gains one.

## Consequences

- A driver groups its functions with its type, and callers write
  `Ring::new()` without naming the module's functions one by one.
- `self`, `Self` and `impl` can no longer name anything; no test or `core`
  item used them.
- A type's functions are all in its module. A driver cannot add methods to a
  type another module declares, such as a peripheral type generated by
  `erebos svd` (ADR 0070): it wraps the calls in its own functions, or the
  methods are written in the generated module. Allowing an `impl` in another
  module, as Rust does within a crate, can come later without breaking
  programs; M5.6, which decides handles to peripherals, is where it may be
  needed.
- **Getters.** References are not returned (ADR 0094), so a getter returns a
  copy: `fn count(&self) -> usize`. A part of non-copy type has no getter: the
  type offers methods that do the work in place through `&self` or `&mut
  self`, or makes the field `pub` so callers borrow it (`&r.buf`, ADR 0114).
  Returning a reference tied to the receiver is the deferred idea of the
  roadmap ("Returning a reference tied to the one reference parameter"), and
  would break no program.
- No associated constants or types, and no functions of `Option`, `Result`,
  arrays or built-in types; generic types are M6's.

## Alternatives considered

- **`impl` anywhere in the program, as Rust within a crate** — lets a driver
  add methods to a generated peripheral type, but the functions of a type are
  then found across the whole program, and two modules adding one name
  collide far from each other. The narrow rule is the one that can be widened
  later without breaking programs.
- **`impl` in the type's module or its descendants** — a middle way, but a
  child module already reaches its parent's private items; the gain over one
  module is small for a second rule.
- **Methods as ordinary items of the module, called `x.f()` when the first
  parameter fits (Zig, Nim's uniform call syntax)** — no `impl`, but which
  function `x.f()` calls would depend on what the module imports, and two
  types in one module could not both have a `count`.
- **No `Self`** — one keyword fewer, but every signature repeats the type's
  name, and renaming the type touches every function.
- **Typed receivers (`self: &Ring`)** — one form for all, as Rust's arbitrary
  self types, but the four short forms cover every receiver the language has.
- **Built-in names reserved for functions of a type too** — one rule for all
  names, but `t.end()` or `s.len()` cannot be confused with the built-in, which
  is only ever called by a single name.
- **`#[export]` on a function of a type** — C could call a method by a name
  of its own; nothing needs it, and a plain function with `#[export]` calling
  the method does the same.
- **`use` of a function of a type** — `use ring::Ring::new;` would make a bare
  `new()` a function of `Ring`, hiding the type the call is about.
