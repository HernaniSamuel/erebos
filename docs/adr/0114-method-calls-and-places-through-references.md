# 0114. Method calls, and places through a reference

- Status: Accepted
- Date: 2026-11-13
- Amends: 0008 (`x.f()` borrows its receiver; a field or an index through a
  reference dereferences it), 0105 (`r.f` and `r[i]` through a reference are
  no longer errors; the base of a field, an index and a method call is a
  position of a reference)

## Context

ADR 0113 declares the functions of a type and their receivers, `self`,
`mut self`, `&self` and `&mut self`, and names them `T::f`. This ADR is the
call `x.f(args)`.

Methods waited for references (roadmap, ADR 0094): a method that takes
`&mut self` needs `x` borrowed, and doing that at `x.f()` without writing `&mut`
is an implicit conversion, which ADR 0008 forbids. ADR 0094 sets "`x.f()`
borrowing `x` as the receiver asks" as the direction and leaves the decision,
as an amendment of ADR 0008, to M5.5.

ADR 0105 writes fields and elements through a reference as through a
pointer, `(*r).f` and `(*r)[i]`; `r.f` is `ty::no-fields` and `r[i]`
`ty::cannot-index`, and it says that M5.5 is where reading through a receiver
gets shorter and that relaxing this breaks no program. Inside a method, the
receiver is a reference in most methods, so every field is `(*self).f`.

Rust adjusts the receiver of a method call by any number of automatic
dereferences followed by at most one borrow, and dereferences fields
automatically through references and smart pointers; it borrows the receiver
of `v.push(v.len())` in two phases, so that the argument may read `v`. Zig
takes the address of the receiver when the parameter is a pointer, and
dereferences one level of pointer for fields.

## Decision

### The call `x.f(args)`

`x.f(a, b)` calls the function `f` of the type of `x`, passing a receiver
made from `x` followed by `a` and `b`.

1. **The type.** Let `X` be the type of `x`. If `X` is a reference, `&T`,
   `&mut T` or a volatile one, the type is its referent `T` and the
   **receiver place** is `*x`; otherwise the type is `X` and the receiver
   place is `x`. Only one level: a reference never refers to a reference
   (ADR 0105). A raw pointer is never followed: `p.f()` is an error whose
   help writes `(*p).f()`.
2. **The function.** `f` is looked up among the functions of that type (ADR
   0113). A type that has none, or no `f`, is `ty::unknown-method`; when `T`
   has a field named `f`, the help says that a field is not called, and on a
   slice or an array, `x.len()` gets the help that its length is the built-in
   `len(x)` (ADR 0031, 0108). A
   function without a receiver is `ty::not-a-method`, whose help writes
   `T::f(...)`. A function not visible where the call is written is
   `ty::private-method`, pointing at the call and at the function (ADR 0050,
   0113).
3. **The receiver.** With `P` the receiver place, the first argument is,
   by `f`'s receiver:

   | Receiver | First argument |
   |----------|----------------|
   | `self`, `mut self` | `P`, moved or copied |
   | `&self` | `&P` |
   | `&mut self` | `&mut P` |

   exactly as if it were written there, so every rule of those expressions
   holds, with its code: `&mut P` of a place that cannot be written (a `let`
   without `mut`, a `static`, a place through a `&T`) or of a value that is
   not a place is `ty::mut-borrow-of-immutable` (ADR 0105); `&P` of a value
   that is not a place borrows a local of its own (ADR 0105); `P` of a
   non-copy type through a reference is `ty::move-out-of-place` (ADR 0097);
   `&mut P` of a `static mut` is a borrow of it, under ADR 0078, 0093 and
   0111; `&mut UART0` of a peripheral is a reference to its type (ADR 0110).
   When the receiver place is volatile, `&P` and `&mut P` are volatile
   references (ADR 0110), which no receiver takes: `ty::mismatched-types`,
   with a help saying that a method has no volatile receiver.
   A **register** keeps no address (ADR 0058, 0110): a method of a
   bit-field struct called on a register, `UART0.lsr.tx_ready()`, works with
   `self` by value (one volatile read of the register, then a copy), and with
   `&self` or `&mut self` is `ty::register-address`, whose help says that a
   method taking `self` by value reads a register. Bit-field structs are copy
   (ADR 0059, 0096), so `self` by value is the natural receiver of their
   methods. On a bit-field struct held in a variable, every receiver works.
   A register reached through a volatile pointer or reference is still a
   register: `ty::register-address`, not the volatile case above.
4. **The arguments** are checked against the other parameters, as in any
   call. `ty::wrong-arg-count` counts them without the receiver.

So `r.push(1)` with `r: Ring` is `Ring::push(&mut r, 1)`, and with `r: &mut
Ring` it is `Ring::push(&mut *r, 1)`: a reborrow, which lends `r` for the call
(ADR 0106). `UART0.putc(b'x')` is `Uart16550::putc(&mut UART0, b'x')`. Inside
a method, `self.push(b)` reborrows `*self`.

### Order, and the borrow check

- The receiver is the first argument, and arguments run left to right (ADR
  0018): `x` runs first, and its borrow is taken there, before the other
  arguments run.
- The borrow check sees the call as written with the receiver explicit: the
  borrow is a borrow of `P`, made where `x` is written, and lent to the call.
  There is no two-phase borrow: `v.push(v.len())` reads `v` while the `&mut`
  borrow of the receiver is in use, which is `analysis::borrow-conflict`, with
  the labels of ADR 0106 (the borrow at `v`, the read at `v.len()`). It is
  written `let n: usize = v.len(); v.push(n);`. Taking the receiver's borrow
  after the arguments, as Rust does, can come later without breaking
  programs.

### Fields and indexes through a reference

- A **field** of a reference, `r.f` with `r` of type `&T` or `&mut T`
  (volatile or not), is the field of what it refers to: `(*r).f`.
- An **index** or a **range in an index** of a reference, `r[i]` and
  `r[a..b]`, is that of what it refers to: `(*r)[i]` and `(*r)[a..b]` (ADR
  0105, 0108).
- **Where a reference may be.** The base of a field, of an index, of a range
  in an index and of a method call is a new position of an expression of a
  reference type, added to ADR 0105's list beside the operand of `*`: any
  expression of a reference type may be there, as it may be dereferenced, so
  `(if c { &a } else { &b }).count` and `(if c { &a } else { &b }).get()` are
  `(*(if ...)).count` and a call on `*(if ...)`.
- One level, since a reference never refers to a reference. The place is the
  one `(*r)` gives, with its mutability, volatility, checks and borrows: `r.f
  = 1` needs `r` a `&mut`, `&mut r.buf[0..4]` borrows `(*r).buf`, and the
  borrow check sees `(*r)` (ADR 0106, 0107).
- **Raw pointers are unchanged**: `p.f` is `ty::no-fields` with the help
  `(*p).f`, `p[i]` on a thin pointer steps over whole pointees (ADR 0021), and
  `s[i]` on a raw slice is `ty::slice-pointer-index` (ADR 0031). A raw pointer
  is unchecked, and its dereference stays written; a reference is checked, and
  the dereference hides nothing that could fail.
- `(*r).f` and `(*r)[i]` stay valid and mean the same.

Within a method, `self.count` and `self.buf[at]` therefore read and write
through the receiver. Note the difference with pointers: with `p` of type
`*const [u8; 4]`, `p[1]` is the next array after `*p` (ADR 0021), while with
`r` of type `&[u8; 4]`, `r[1]` is an element of `*r`. The types differ (`[u8;
4]` and `u8`), so code moved from one to the other mostly fails to check
rather than changing meaning.

### In MIR

Nothing new. Type checking records, for a method call, the function called and
how its receiver is made (moved or copied, or borrowed, from `x` or from
`*x`), and for a field or an index, that its base is dereferenced; MIR
building writes the same statements as for the explicit form: a `Ref` of the
receiver place with the span of `x`, a `Deref` projection, a call. The
validator, the analyses, compile-time evaluation and code generation see
nothing they have not seen.

### Diagnostic codes

| Code | Meaning |
|------|---------|
| `ty::unknown-method` | `x.f()` where the type of `x` (or its referent) has no function `f` |
| `ty::not-a-method` | `x.f()` where `f` is a function of the type without a receiver |
| `ty::private-method` | `x.f()` where `f` is not visible here, a `pub` `f` of a type that is not reachable here included |

`ty::no-fields` and `ty::cannot-index` no longer arise because the base is a
reference: they arise for the referent, when it has no fields (`r.f` with `r:
&u32`) or cannot be indexed (`r[i]` with `r: &u32`), naming the referent's
type with a label saying it is reached through the reference; their helps
that write `(*r)` go. `ty::mismatched-types` gains the help for a volatile
receiver place, `ty::register-address` the help for a method on a register,
and `ty::unknown-method` the help for `len`. The new codes join the catalog of ADR 0069.

### Conformance tests

- **Parser snapshots**: `x.f()`, `x.f(a, b)`, `a.b.f()`, `x.f().g()`,
  `x.f()?`, `x.f()[i]`, and `x.f` without parentheses still a field.
- **ui snapshots** (`--emit=typeck` and MIR twins): each receiver form called
  on a local, a `let mut`, a parameter, a field, an array element, a
  temporary, a `&T` and a `&mut T` parameter, `*r`, a `static mut` inside
  `critical`, a peripheral and a cluster; `self.f()` inside methods of each
  receiver; each error above, and `&mut self` on a `let` without `mut`,
  through a `&T` and on a temporary (`ty::mut-borrow-of-immutable`), `self` by
  value through a reference of a non-copy type (`ty::move-out-of-place`),
  a call through a volatile reference (`ty::mismatched-types`) and through a
  raw pointer (`ty::unknown-method`); `s.len()` on a reference to a slice
  (`ty::unknown-method` with its help); a bit-field struct's method on a
  register by value (accepted) and by `&self` (`ty::register-address`, with
  its help), and on a local by each receiver (accepted); a call and a field
  on the value of an `if` of a reference type; `v.push(v.len())` and a `&mut`
  receiver held while the place is used (`analysis::borrow-conflict`); a
  method lent a `static mut` that it also names
  (`analysis::borrow-conflict-in-call`); a linear value consumed by `t.wait()`
  and used after (`analysis::use-after-move`).
- **ui snapshots** for places through a reference: `r.f`, `r.f = v`, `r.f
  += v`, `r[i]`, `r[a..b]`, `&r.f`, `&mut r[i]`, `len(&r.buf)`, through `&T`,
  `&mut T`, a reference to a slice and a volatile reference; `r.f = v` through
  a `&T` (`ty::write-through-const`); `p.f` and `s[i]` through raw pointers
  still rejected; the MIR of `r.f` equal to that of `(*r).f`.
- **End to end:** the ring buffer and the UART drivers of the roadmap written
  as methods, on both targets at both levels.

## Consequences

- `x.f()` and `self.count` read as in Rust, and a driver's callers write
  `UART0.write(&b"hi")` and `ring.push(b)`.
- What `x.f()` does to `x` is not written at the call: whether it moves,
  copies or borrows `x`, and mutably or not, is in `f`'s signature. The borrow
  check and the analyses see it all the same, and their labels point at `x`.
- `v.push(v.len())`, and `self.set(self.get() + 1)` in a method taking `&mut
  self`, must be split in two (`let n: u32 = self.get() + 1; self.set(n);`)
  until the receiver's borrow is taken after the arguments.
- ADR 0105's `(*r).f` remains the explicit form; programs may mix both, and
  existing programs keep their meaning.
- A method is not called on a volatile place, nor through a raw pointer
  without writing `(*p)`.

## Alternatives considered

- **No automatic receiver through a reference: `(*r).push(b)`** — one rule
  fewer, but every method calling another on its receiver writes
  `(*self).f()`, the common case.
- **Two-phase borrows** — `v.push(v.len())` accepted, as Rust does since
  2018, but the borrow analysis must tell a reservation from an activation;
  it can be added without breaking programs once a program needs it.
- **Automatic dereference through raw pointers too** — shorter, but the
  dereference of a raw pointer is the one access the compiler does not check,
  and writing it is what marks it.
- **Fields through `self` only, other references keeping `(*r).f`** — a rule
  that depends on a name, for the same operation.
- **Fields but not indexes** — avoids the difference between `p[i]` and
  `r[i]`, but `(*s)[i]` on a reference to a slice is as common in drivers as
  fields, and the types catch most confusions.
- **Any number of dereferences, as Rust's autoderef** — references never
  nest, so one level is all there is.
