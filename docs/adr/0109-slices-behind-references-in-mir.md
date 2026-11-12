# 0109. Slices behind references in MIR

- Status: Accepted
- Date: 2026-11-12
- Amends: 0095 (S7, T1, T2 and T5 for places of type `[T]` and for casts
  from a reference to a slice; the rule U1), 0107 ("Types, rvalues and
  places": a reference to a slice; "What MIR building writes": the conversion
  to a slice; R3; "The borrow analysis": paths through `Index` of a `[T]` and
  the subslices, and how their sites are described)

## Context

ADR 0108 is the language of slices behind references: `&[T]` and `&mut [T]`,
the conversion of a reference to an array, `(*s)[i]`, `len`, and sub-slices
written as a range in an index. This ADR says how MIR holds them, what the
validator checks, and how the borrow analysis reads them.

MIR today (ADR 0031, 0095, 0107):

- A raw slice is `Ty::Ptr` whose pointee is `Ty::Slice(T)`; `Ty::slice_elem`
  and `Ty::slice_address_ty` recognize it. `Rvalue::Slice { ptr, len }` makes
  one from a thin pointer and a length, `SliceAddress` and `SliceLen` read its
  two parts.
- `(*s)[i]` through a raw slice is built as `_n = len(s)`, a check of
  `_i < _n`, then the place `(*_a).Offset(_i)` with `_a = address(s)`: a place
  through a thin raw pointer. `&raw const *s` is a copy of `s`. No place of
  type `[T]` reaches MIR.
- The conversion of a pointer to an array to a slice is `Rvalue::Slice { ptr,
  len: N }`.
- `Ty::Ref` is a reference with the layout of a thin pointer; `Rvalue::Ref`
  borrows a place. The borrow analysis (ADR 0107) gives each `Ref` a site
  with a path of `Field`, `Elem` and the `Deref` of a reference; a place
  through a raw pointer has no path. A local holding a reference may hold the
  sites of: its own `Ref`, a copy or a move, and a reborrow through a
  reference; any other write sets it to nothing.

So a reference to a slice built as raw slices are would lose the borrow
check: `&mut (*s)[i]` would borrow a place through the raw `address(s)`, with
no path, and keep nothing `s` holds in use; and `Rvalue::Slice` from a
reference would be a write that sets the result to nothing. MIR needs places
of type `[T]` with a path through the reference, and each borrow as one
`Ref`.

## Decision

### Types

`Ty::Ref` whose referent is `Ty::Slice(T)` is a **reference to a slice**. It
has the layout, the LLVM type and the passing of the raw slice of its
mutability (ADR 0108): `Ty::slice_elem` and `Ty::slice_address_ty` recognize
it as they recognize a raw slice, so `SliceAddress` and `SliceLen` read it,
`Use` copies or moves it whole, and the ABI code of ADR 0025 and 0075 passes
it as the C struct. Its kind is a reference's: copy when shared, not when
mutable (ADR 0107). Dumps print `&[T]` and `&mut [T]`.

### Places of type `[T]`

Two projections are added:

- `Subslice(from, to)`: the elements of the array or `[T]` in the place from
  the one at index `from` up to but not including the one at `to`, with
  `from` and `to` the `usize` values in the two locals. MIR building has
  already checked `from <= to <= len`. Its type is `[T]`.
- `ConstSubslice(from, to)`: the same, for bounds known in range of an array:
  `from <= to <= N`. Its type is `[T]`.

And `Index(i)` applies to a place of type `[T]` as it applies to an array,
giving the element; MIR building has already checked `i` against the length.
`ConstIndex` stays for arrays.

A place of type `[T]` is reached by the `Deref` of a slice, raw or reference,
or by a `Subslice` or `ConstSubslice`. It is never read or written whole (T3
already says so); it is followed by `Index` or a subslice, or it is the whole
place of a `Ref` or an `AddressOf`, which then gives a slice: a reference to a
slice, or a raw slice, of the place's element type.

**The length of a place of type `[T]`** is the slice's for the `Deref` of a
slice, `to - from` for a `ConstSubslice`, and the `usize` subtraction of the
two locals for a `Subslice` (which cannot wrap, since MIR building checked
`from <= to`). MIR building checks an index or a range against that length,
whatever lies below it: `(*s)[2..4][3]` is checked against `4 - 2`, not
`len(s)`.

Code generation lowers a place of type `[T]` to an address and that length:
the `Deref` of a slice gives the slice's two parts, a subslice of an array or
of a `[T]` gives the address of element `from` (the base's address plus
`from` times the element's size) and `to - from`; `Index` of a `[T]` gives
the address of the element; `Ref` and `AddressOf` of a `[T]` place store
the two parts as the slice. Compile-time evaluation does the same with its
pointers and lengths (ADR 0031): a subslice of a static or a byte string is a
slice inside that object.

### What MIR building writes

- **`(*s)[i]` with `s` a reference to a slice**: `_n = len(s)`, the check of
  `_i < _n` as today, then the place `(*s)[_i]`: `Deref` of the reference
  followed by `Index`, so the place has a path rooted at `s`. A `&mut [T]`
  variable as the base is read in place, as ADR 0107 says of a `&mut` base.
  Through a raw slice the place stays `(*_a).Offset(_i)` (ADR 0031): no
  path is needed through a raw pointer, and its dumps do not change.
- **`&*s` and `&mut *s`**: `_t = &*s` or `_t = &mut *s`, the `Ref` of the
  `[T]` place. Through a raw slice too; `&raw const *s` stays a copy of `s`.
- **The length of a base**, for an index or a range on a place of type
  `[T]`, is computed when the base is built, before the index or the bounds
  are evaluated: `_len = len(s)` for `*s` (with `s` the place holding the
  slice, a temporary when ADR 0098 puts the base in one), `_len = Sub(_b,
  _a)` for a `Subslice` (`Binary(Sub)`, wrapping as every `-`, ADR 0007),
  the constant `to - from` for a `ConstSubslice`. An index on such a base is
  `_ok = Lt(_i, _len)`, its check, then `base[_i]`.
- **A sub-slice** `base[a..b]`: the base place is built, with its length
  (`N` for an array), then `a` (or `0`) and `b` (or the length) into new
  `usize` locals, then `_ok = Le(_a, _b)`, its check, `_ok2 = Le(_b, _len)`,
  its check, both with the cause `IndexOutOfBounds` at the `[`, then the
  place `base[_a.._b]`. On an array, each bound known at compile time (ADR
  0032) is checked against `N` while building, and both against each other
  when both are known (`ty::index-out-of-bounds`); two integer literals in
  range, an omitted bound counting as the literal `0` or `N`, give
  `ConstSubslice(a, b)` with no check, as a literal index gives
  `ConstIndex` (`Builder::array_index`); any other range is checked at run
  time. `&`, `&mut`, `&raw const` and `&raw mut` of the place are a `Ref` or
  an `AddressOf` of it. A sub-slice of a part of a value that is not a place
  borrows from the new local that holds the whole value (ADR 0107).
- **Casts from a reference to a slice** (ADR 0108): `Cast` from `&[T]` or
  `&mut [T]` to the raw slice of the same mutability and element type, as
  `Builder::cast` already casts any reference to the raw pointer of its
  mutability first; the rest of the cast is then the raw slice's (`Use` to a
  slice of other mutability, `SliceAddress` and a `Cast` to a thin pointer).
- **The conversion of a reference to an array** (ADR 0108) is a `Ref` of
  `ConstSubslice(0, N)`, as the conversion of `&mut T` to `&T` is a `Ref`
  (ADR 0107): of `place[0..N]` directly when the converted expression is
  `&place` or `&mut place`, so the conversion adds no site; otherwise
  `_t = &(*q)[0..N]` or `_t = &mut (*q)[0..N]`, with `q` the place holding
  the reference to the array (the variable or parameter, or the temporary
  holding the value converted). A `&mut [T; N]` variable passed where a
  `&mut [T]` or a `&[T]` is expected is lent by that `Ref`, as ADR 0107 lends
  a `&mut T`.
- **`len(e)`**: of a place of a reference type, `SliceLen` of the place, which
  moves nothing; of a reference to an array, `e` evaluated for its effects,
  then the constant `N`; of other operands, as ADR 0031 says.

`Rvalue::Slice` stays the raw conversion and `slice(p, n)`, and never gives a
reference.

### Rules of the validator

ADR 0095 changes:

- **S7**: the two locals of `Subslice` exist.
- **T1**: `Deref` applies to a pointer or a reference, thin or to a slice,
  giving the pointee or referent; `Index` to an array or a `[T]`, `ConstIndex`
  to an array, below its length; `Subslice` to an array or a `[T]`, and
  `ConstSubslice(from, to)` to an array with `from <= to <= N`, both giving
  `[T]` of its element type.
- **T2**: the two locals of `Subslice` are `usize`.
- **T5**: `Ref` and `AddressOf` of a place of type `[T]` give a reference to a
  slice or a raw slice of `T`, as `mutable` says; a reference to a slice fits
  only the same type, as every reference does (ADR 0107), so the conversion
  of ADR 0108 is always a `Ref`. `Cast` may go from a reference to a slice to
  the raw slice of the same mutability and element type, the one cast whose
  sides are slices; every other `Cast` keeps the rule that neither side is
  an aggregate. `SliceAddress` and `SliceLen` read a slice, raw or reference;
  `Slice` is assigned to a raw slice place only.

And gains:

| Id | Rule |
|----|------|
| U1 | a place of type `[T]` is followed only by `Index`, `Subslice` or `ConstSubslice`, or is the whole place of a `Ref` or an `AddressOf`; no operand, destination, `asm` operand, `SliceAddress`, `SliceLen` or `switch` names one |

ADR 0107's **R3** gains `Subslice` and `ConstSubslice` among the projections
of the place of a `Ref`. R1 is unchanged: `[T]` holds no reference (ADR 0108),
and a reference to a slice is still only the type of a local.

### The borrow analysis

ADR 0107's paths gain:

- `Index` of a `[T]`, `Subslice` and `ConstSubslice` are each `Elem`, whatever
  their locals or constants: they overlap every element and every subslice of
  the same base, and the base (ADR 0108). A subslice followed by `Index` is
  `Elem, Elem`, which overlaps as `Elem` does.
- The locals of `Subslice` are read, as the local of `Index` is, for liveness.

The rules of what a local may hold are unchanged and cover every new form:
`&(*s)[a..b]`, `&*s` and the conversion `&(*q)[0..N]` are `Ref`s whose place
starts with the `Deref` of a local of a reference type, so the result holds
what that local holds, plus its own site. `len(s)` is `SliceLen` of the
place `s`: a read of `s`, as ADR 0107 lists `SliceLen` among the reads.

**How a site is described** (ADR 0108). The analysis tells the sites MIR
building made by their shape, as it tells `&*q` today: a `Ref` into a
temporary that is not named, whose place is a local of a reference type
followed by `Deref`, or by `Deref` and `ConstSubslice(0, N)` with `N` the
length of the array that local refers to. Of those, a mutable one whose local
is a named variable or parameter and whose temporary is passed to a call is
"lent to this call"; any other made from a reference to an array, shared or
mutable, is "converted to a slice here"; a shared one from a `&mut T` stays
"converted to `&` here". A `&(*r)[0..N]` the program wrote is a `Ref` into a
temporary of the same shape, and is described the same way: as for `&*q`,
the description follows the MIR, and the label still points at the
expression that made the borrow. `&buf` converted directly ("What MIR
building writes", above) has a place rooted at `buf`, not at a reference, and is
an ordinary borrow. The name of a subslice in a message is `[_.._]` after its
base's.

The analysis of moves and the analysis of linearity read the new projections
as they read `Index`: a place with one is part of an element, never a `Field`
or `Payload` path, and no array or slice holds a linear value (ADR 0103, L3).
That is what ADR 0099 and 0103 already say of an element, so neither is
amended.
The check of shared statics names the static of a `Ref` or `AddressOf` of a
subslice, as of any place (ADR 0093, 0107).

## Consequences

- A borrow through a reference to a slice has a path, so the borrow check
  covers elements and sub-slices of slices as it covers arrays, with no new
  rule in the analysis beyond the mapping of the projections.
- Raw slices keep their MIR; their dumps do not change, except where a range
  replaces `slice(&raw const ...)` in a test.
- Code generation and the evaluator gain the lowering of places of type
  `[T]`, which `&raw` of a range shares with references.
- The validator gains U1 and changed S7, T1, T2, T5 and R3; every program of
  the tests passes it.

## Alternatives considered

- **`Rvalue::Slice` from a reference** — no new projection, but the analysis
  would need a rule for a write that keeps what its operand holds, and a
  sub-slice would be two statements (an element's address and a length), the
  first a `Ref` of one element whose site would not cover the rest.
- **Raw slices built with `Index` of `[T]` as well** — one lowering for both,
  but every slice dump in the tests would change for no gain: nothing is
  followed through a raw pointer.
- **Only `Subslice` with locals, no `ConstSubslice`** — one projection fewer,
  but every conversion of a reference to an array would write two constant
  locals first, as `Index` would without `ConstIndex`.
- **`len` of a `&mut [T]` operand evaluated as a value** — it would move a
  `&mut [T]` variable into a temporary, so `len(s)` would end `s`; reading
  the place is what `len` of a raw slice does in effect.
