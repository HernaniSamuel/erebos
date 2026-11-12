# 0105. References

- Status: Accepted; amended by 0108 (borrowing a place of type `[T]`; a reference to a slice passed as a slice), 0110 (references to a `static mut`, a peripheral, a cluster and a volatile place; volatile references; a `break` out of `critical`)
- Date: 2026-11-11
- Amends: 0008 (a `&mut T` converts to a `&T` where ADR 0024's conversion
  applies), 0097 ("Places a value is moved out of": not a place through a
  reference), 0101 ("Where a linear value may not be held": not written through
  a reference)

## Context

ADR 0094 sets the model of references for M5: `&T` and `&mut T`, made by
`&place` and `&mut place`, with `*r` the place they refer to; second-class
(a parameter and a `let` without `mut`, and the expressions that give them
their values, nothing else); `&T` copy, `&mut T` not, lent to a call without
being moved; a reference becomes a raw pointer only with `as`; the value of a
`critical` block is never a reference. It leaves the rest to M5.4.

M5.4 is split in three (roadmap, "M5 steps"): M5.4a gives references to
places that are not volatile and not `static mut`, with the borrow check;
M5.4b slices behind references; M5.4c references to statics shared with
handlers, inside `critical`, and to volatile places and registers. This ADR
is the language of references in M5.4a; ADR 0106 is the borrow check, and
ADR 0107 their MIR and the analysis.

Where the language stands:

- `&raw const place` and `&raw mut place` give raw pointers (ADR 0021). The
  parser rejects `&` without `raw`, keeping it for references. `&raw` takes a
  place, or a part of a temporary, which then points into a temporary holding
  the value; `&raw mut` of a part of a temporary is
  `ty::address-of-immutable`.
- Fields and elements through a pointer are written `(*p).f` and `(*p)[i]`;
  there is no automatic dereference (`p.f` is `ty::no-fields`), and `p[i]` on
  a thin pointer steps over whole pointees, unchecked (ADR 0021).
- The one implicit conversion between pointer types is from `*mut T` to
  `*const T` (ADR 0024), with the conversion of a pointer to an array to a
  slice (ADR 0031), both amending ADR 0008.
- A value of a non-copy type is not moved out of a place through a pointer
  (ADR 0097), and a value of a linear type is not written through one (ADR
  0101): the value already there would be lost without being consumed.

## Decision

### Types and expressions

- `&T` is a **shared reference** to a `T`, `&mut T` a **mutable reference**.
  `&&T` is read as `& &T` and rejected below (a reference to a reference), not
  as a parse error.
- `&e` and `&mut e` make one. In an expression, `&raw` followed by `const` or
  `mut` is still a raw address (ADR 0021); `&raw` followed by anything else
  borrows a place whose path starts with a variable named `raw`.
- `*r` is the place `r` refers to, as `*p` is for a pointer: it can be read,
  written when `r` is a `&mut`, borrowed, and passed to `&raw`.
- Fields and elements through a reference are written as through a pointer:
  `(*r).f` and `(*r)[i]`. `r.f` is `ty::no-fields` and `r[i]` is
  `ty::cannot-index`, each with a help that writes `(*r)`. No automatic
  dereference amends ADR 0008; methods (M5.5) are where `x.f()` will borrow
  its receiver.

### What may be borrowed

`&place` borrows a place: a variable, a parameter, a field or an element of
one, a `static` or an `extern static` without `mut`, a byte string, or a place
through a reference or a raw pointer that is not volatile (`&*p`, `&(*p).f`,
`&p[i]`). A reference made through a raw pointer is the program's word, as
`slice(p, n)` is (ADR 0031): the borrow check follows nothing through a raw
pointer (ADR 0106).

`&mut place` needs a place that can be written, by the rules of `&raw mut`:
not a variable or parameter without `mut`, a `static`, an `extern static`
without `mut`, a byte string, nor a place through a `&T` or a `*const T`;
otherwise `ty::mut-borrow-of-immutable`.

**A value that is not a place** (`&f()`, `&Config { .. }`, `&5`, `&CONST`) is
borrowed `&` from a variable of its own: MIR building writes the value to a
new local, which nothing else uses, and borrows it (ADR 0107). The value lives
as long as any MIR local does, the whole call of the function, so the
reference never outlives it. `&mut` of a value that is not a place is
`ty::mut-borrow-of-immutable`, as `&raw mut` of a part of a temporary is
`ty::address-of-immutable`: a write through it would reach nothing the
program can read.

**A part of a value that is not a place** (`&f().x`, `&TABLE[i]` with `TABLE`
a `const`, `&(comptime t())[1]`) is borrowed the same way: the whole value is
written to a new local, and the part of it is borrowed. `&mut` of such a part
is `ty::mut-borrow-of-immutable`, as `&raw mut` of it is
`ty::address-of-immutable`.

**A zero-sized referent** (`&()`, `&Empty` with no fields, `&[u8; 0]`) is a
pointer as `&raw` gives one for the same place: an address that is never read
or written through, since an access of zero bytes touches no memory.

The rules of `&raw` that hold for any address hold for references, with the
same codes: a bit field is `ty::bit-field-address`, a register, a register
array or a cluster array `ty::register-address` (ADR 0058, 0059, 0064), a
possibly misaligned field of a `#[packed]` struct `ty::packed-field-address`
(ADR 0022), a place of type `!` `ty::never-in-memory` (ADR 0034).

Left to M5.4c, and errors until then:

- `&` or `&mut` of a `static mut` or an `extern static mut` is
  `ty::reference-to-static-mut`; the help says to use `&raw` inside `critical`
  as today (ADR 0078, 0093).
- `&` or `&mut` of a place through a volatile pointer, or of a peripheral or a
  cluster, is `ty::reference-to-volatile`; the help says to use the pointer.

### Where a reference may be

ADR 0094 lists the positions; this ADR makes them checkable. A reference type
may be written only as:

- the type of a parameter that is not `mut`, of any function: one with a body,
  an `extern fn`, a `#[export]` or `#[naked]` function, a `comptime` function
  (the parameter is not `comptime`);
- the type of a `let` without `mut`.

It may not be written as the type of a field, a payload value, an array or
slice element, a type argument (`Option(&T)`, `size_of(&T)`, a `comptime T:
type` argument), a return type, a `static`, `extern static` or `const`, a
`comptime` parameter, the pointee of a pointer, or the referent of a
reference (`&&T`, `&*const &T`). Each is `ty::reference-position`, at the
written type, with a label naming the position. So is a `mut` parameter or a
`let mut` of a reference type: a reference is bound once (ADR 0094), and a
help says to declare a new `let`.

An expression of a reference type may be:

- an argument of a call;
- the initializer of a `let`;
- the operand of `*`, of `as` (to a raw pointer, below), or of an expression
  statement;
- the value of an `if`, a `match`, a block, or a `loop` through its `break`
  values, that is itself in one of these positions, such as `let r: &u32 = if
  c { &a } else { &b };`.

The other positions are rejected by the rules they already have (a return
type, a field type or an assignment to a `let` without `mut` are errors
before the value is), except these, which are `ty::reference-position` at the
expression:

- **an expression whose type holds a reference below its top**, at the
  innermost expression that makes that type: an array literal of references
  (`[&a, &b]`); `&e`, `&mut e` or `&raw` of an `e` of a reference type
  (`&r`, `&&x`, `&(if c { &a } else { &b })`, `&raw const r`), whose type
  would be a reference to a reference or a pointer to one. `&f(&x)` is not
  one: no function returns a reference, so it borrows a temporary of `f`'s
  return type. The expressions above it (`*&r`,
  `(&raw const r) as *const u8`) are not reported again. No local of such a
  type reaches MIR (ADR 0107, R1);
- the value of a `critical` block (ADR 0094), with the help that a reference
  must not outlive the protection it was taken in;
- a `comptime` expression of a reference type: a value computed at compile
  time is never a reference (ADR 0094).

The panic function keeps its parameter, `*const core::PanicInfo` (ADR 0053);
the entry function and handlers take no parameters (ADR 0084), so they hold no
reference from their callers.

### Kinds, conversions, casts and comparisons

- `&T` is copy, whatever `T` is; `&mut T` is not copy and not linear (ADR
  0094, 0096). A `let r2: &mut T = r;` moves `r`. Passing a variable or a
  parameter of type `&mut T` as an argument lends it for the call, and does
  not move it (ADR 0094, 0106).
- **`&mut T` converts to `&T`** where `*mut T` converts to `*const T` (ADR
  0024), in the same positions; a reference never holds another, so there is
  no inner reference to convert. The
  reference given is a shared borrow of `*r` for as long as it is used (ADR
  0106), so the `&mut` it came from is not moved. This amends ADR 0008, as
  ADR 0024 and 0031 did; the conversion is between two views of one place and
  computes nothing.
- **Casts.** `r as P`, with `P` a raw pointer type, is allowed exactly where
  `p as P` is for a `p` of type `*const T` (from `&T`) or `*mut T` (from
  `&mut T`) (ADR 0019, 0031): the cast ends what the borrow check follows,
  and the raw pointer is unchecked from there, as every raw pointer is. So
  `&T as *mut T` is allowed, as `*const T as *mut T` is. A reference casts to
  nothing but a raw pointer: `r as usize`, `r as bool` and every other target
  are `ty::invalid-cast`, with a help giving `r as *const T as usize`. A cast
  to a reference type, from anything, is `ty::invalid-cast`, with a help
  giving `&*p`. An `asm` operand is not a reference (ADR 0079's operand types
  are unchanged): the program casts it to a raw pointer, which the borrow
  check does not follow into the assembly.
- **Comparisons.** `==` and `!=` (and the orderings) on references are
  `ty::invalid-operand`, with a help giving `*a == *b` to compare the values
  and `a as *const T == b as *const T` to compare the addresses. Neither
  meaning is implied.

### Moves and linear values through a reference

- A value of a non-copy type is not moved out of `*r` or a place through it,
  for the reason it is not moved out of a place through a pointer: the owner
  would hold a value that is gone. It is `ty::move-out-of-place`, whose label
  names the reference. This extends ADR 0097's list. A copy value is read as
  from any place.
- A value of a linear type is not written to `*r` or a place through it: the
  value already there would be dropped without being consumed. It is
  `ty::linear-through-pointer`, whose message says "through a reference".
  This extends ADR 0101's rule. A linear value may be borrowed, `&` or `&mut`;
  a borrow consumes nothing.

### Signatures and the ABI

A reference is passed, at every call and on every target, exactly as a raw
pointer to its referent is: one register or stack word (ADR 0006, 0025,
0075). `extern fn`, `#[export]` and `#[naked]` functions may take references:
the C side vouches for what it does with the address, as it vouches for
linear values (ADR 0101) and for raw pointers; on the Erebos side the call
lends the reference for its duration, as every call does (ADR 0106). No
function returns a reference (ADR 0094), and code generation gives references
no LLVM attribute that a raw pointer lacks (`noalias`, `nonnull`,
`dereferenceable`): what the borrow check proves is not yet passed to the
optimizer.

### At compile time

`comptime` functions may take and use references, and the evaluator runs a
borrow as it runs `&raw` (ADR 0107), with the same rules: a borrow of a
`static` at compile time is what `&raw const` of it is (ADR 0029, 0030), and a
read through it is what a read through that pointer is, an error where that
read is. No evaluation gives a reference as its result (above).

### Diagnostic codes

New: `ty::reference-position`, `ty::mut-borrow-of-immutable`,
`ty::reference-to-static-mut`, `ty::reference-to-volatile`. Existing codes
gain the cases above, with helps naming references: `ty::no-fields`,
`ty::cannot-index`, `ty::deref-non-pointer` ("not a pointer or a
reference"), `ty::write-through-const` (a write through a `&T`),
`ty::invalid-cast`, `ty::invalid-operand`, `ty::move-out-of-place`,
`ty::linear-through-pointer`, `ty::bit-field-address`,
`ty::register-address`, `ty::packed-field-address`, `ty::never-in-memory`.
`parser::unexpected-token` no longer has the case of `&` without `raw`.

### Conformance tests

- Each new code has a ui test and a snippet in the catalog of
  `tests/rejected.rs` (ADR 0069); each position of `ty::reference-position`
  is in a ui test.
- A program taking `&` and `&mut` of a variable, a field, an element, a
  `static`, a byte string, a temporary and `*p`, and reading and writing
  through them, runs on QEMU on both targets at `-O0` and `-O` (the e2e
  harness, ADR 0077).
- An `extern fn` and a `#[export]` function taking references, called across
  the C ABI, run on both targets.

## Consequences

- Functions that took `*mut T` to change the caller's value, and `*const T` to
  read a large one, can take references, checked (ADR 0106). `core` and the
  tests move to them in M5.7.
- `r.f` must be written `(*r).f`; M5.5's methods are where reading through a
  receiver gets shorter. Relaxing this later breaks no program.
- `&mut` of a temporary, comparisons of references, `static mut` and volatile
  places are errors that later steps may lift without breaking programs.
- A value borrowed with `&f()` takes a stack slot of its own for the whole
  call of the function; `-O` reuses it as for any local.
- Nothing is passed to LLVM's alias analysis; a later ADR may add `noalias`
  on `&mut` parameters and `readonly` on `&` ones, once the check is trusted.

## Alternatives considered

- **Automatic dereference, `r.f` and `r[i]`** — shorter in drivers, but an
  amendment of ADR 0008 for syntax M5.5 will revisit, and `r[i]` (checked,
  through a reference) and `p[i]` (unchecked pointer arithmetic) would look
  alike and mean different things.
- **Only places under `&`** — the most conservative; a computed value needs a
  `let` first. Rejected because `&raw` already accepts a temporary's part and
  MIR building can give the value a local of its own at no cost to the check.
- **`&mut` of a temporary, as Rust** — the write would reach a value no one
  reads again; kept an error, as `&raw mut` of one is.
- **`&mut T` to `&T` written `&*r`** — no amendment of ADR 0008, but every
  call of a reader with a `&mut` in hand would say it; the conversion is the
  one ADR 0024 already makes for pointers.
- **Comparing references by address, or by value as Rust** — each would make
  one of the two meanings implicit; an error with both spellings in the help
  is lifted later without breaking programs.
- **References forbidden in `extern fn` and `#[export]` signatures** — the
  boundary with C would stay raw, but the C side is trusted for raw pointers
  and linear values already, and the reference costs nothing at the ABI.
- **`&*p` forbidden, or left to M5.4c** — without it, memory a program knows
  of from elsewhere could never be reached by a reference, and M5.7 could not
  move `core` to them. Volatile pointers wait for M5.4c, which decides
  references to volatile places.
- **References to every static now** — a `static mut` is shared with handlers
  under ADR 0078's rule, which M5.4c extends to references; an immutable one
  is never written, so its borrow conflicts with nothing and costs nothing.
