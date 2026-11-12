# 0108. Slices behind references

- Status: Accepted; amended by 0110 (borrows through a volatile raw slice and of a `static mut` allowed; volatile references to slices)
- Date: 2026-11-12
- Amends: 0008 (a reference to an array converts to a reference to a slice),
  0021 (an index may be a range), 0031 ("Slices": a place of type `[T]` can be
  borrowed and sub-sliced; a range in an index; `len` of a reference), 0032
  (a range known at compile time is checked while building), 0105 ("What may be borrowed": a
  place of type `[T]`; "Signatures and the ABI": a reference to a slice is
  passed as a slice), 0106 ("Places and overlap": elements and sub-slices of a
  slice)

## Context

M5.4 is split in three (roadmap, "M5 steps"): M5.4a gave references to sized
places (ADR 0105 to 0107), M5.4b gives slices behind them, M5.4c references to
statics shared with handlers and to volatile places.

Where the language stands:

- A slice is `*const [T]` or `*mut [T]` (ADR 0031): the address of the first
  element and a length, two words, passed as the C struct
  `struct { const T *ptr; size_t len; }` (ADR 0025 on riscv32, ADR 0075 on
  thumbv7m). `[T]` is unsized: `*s` can only be indexed, `(*s)[i]` checked
  against the length, or passed to `&raw`, which gives the slice back.
- A pointer to an array converts implicitly to a slice of the same elements,
  `*const [T; N]` to `*const [T]` (ADR 0031, amending ADR 0008), in the
  positions of ADR 0024.
- `len(s)` takes a slice or a pointer to an array. `slice(p, n)` makes a slice
  from a thin pointer, unchecked. A sub-slice is written
  `slice(&raw const (*s)[2], 3)`: unchecked, since nothing ties `3` to the
  length of `s`.
- `&place` of a place of type `[T]` is an error in M5.4a (`ty::unsized`, "is
  borrowed"): the reference would need the length. Every other place that is
  not volatile and not a `static mut` can be borrowed (ADR 0105).
- References are second-class (ADR 0094): parameters and `let` without `mut`,
  lent to calls, never returned or stored. The borrow check (ADR 0106) follows
  places through fields, elements and the dereference of a reference; two
  elements of one array always overlap.

The roadmap leaves to M5.4b: sub-slices of a `&[T]` (a built-in or a range
syntax); whether a `&mut` to an array or a slice can be split in two disjoint
halves; `(*s)[i]` through a reference to a slice; the conversion of `&[T; N]`
to `&[T]`; `len` of a reference; `&*s` and `&(*s)[i]` from a raw slice; slices
behind references across the C ABI; whether elements of a slice overlap as
array elements do. ADR 0109 is the MIR of this ADR and the borrow analysis's
part of it.

## Decision

### Types

`&[T]` is a **shared reference to a slice**, `&mut [T]` a **mutable reference
to a slice**: `[T]` may be the referent of a reference, as it may be the
pointee of a raw pointer (ADR 0031). Everything ADR 0105 says of references
holds for them: the positions they may be written and made in
(`ty::reference-position` elsewhere), `&[T]` copy and `&mut [T]` not, a
`&mut [T]` variable lent to a call, no comparison, no move out of and no
linear write through one. `[T]` follows ADR 0031's rules for its element type
(`[&u8]`, `[Uart]` and `[!]` are the errors they are behind a pointer).

**Representation.** A reference to a slice is a slice: the address of the
first element and the length in elements, `{ ptr: *const T, len: usize }`,
two words, with the layout ADR 0031 gives `*const [T]`. Wherever ADR 0031
treats a slice as an aggregate of two scalars (ADR 0019, 0023, 0025, 0075),
a reference to a slice is one too. This is the layout ADR 0031 gave slices
when it amended ADR 0022, reused, not a new rule of ADR 0022. As for every
reference (ADR 0107), its debug type is not emitted yet.

### Making one

- **From a place of type `[T]`.** `&*s` and `&mut *s` borrow the slice `s`
  points to or refers to, whole: through a raw slice, the reference is the
  program's word, as `&*p` is for a thin pointer (ADR 0105), and through a
  reference it is a reborrow (ADR 0106). `&mut *s` needs a `*mut [T]` or a
  `&mut [T]` (`ty::mut-borrow-of-immutable` otherwise, as for any place).
- **Conversion.** A reference to an array converts **implicitly** to a
  reference to a slice of the same elements, with length `N`: `&[T; N]` to
  `&[T]`, `&mut [T; N]` to `&mut [T]` or to `&[T]`, in the positions where ADR
  0024 applies, the outermost reference only. It is ADR 0031's conversion for
  pointers, as ADR 0105 gave `&mut T` to `&T` the positions of ADR 0024, and
  amends ADR 0008 the same way: nothing is lost or reinterpreted. Every
  `print(&b"hello\n")` with `print(s: &[u8])` is one. A `&mut [T]` converts to
  a `&[T]` as any `&mut T` does (ADR 0105), and a reference to an array
  converted is borrowed as that conversion is: the `&mut` it came from is
  lent, not moved (ADR 0106).
- **A sub-slice**, below.
- **Casts** follow ADR 0105: a reference to a slice casts where the slice
  `*const [T]` (from `&[T]`) or `*mut [T]` (from `&mut [T]`) casts by ADR
  0031's table, so `s as *const [T]`, `s as *const T` (the length dropped) and
  `s as *mut [T]` are allowed and `s as usize` is not. A reference to an
  array casts as the pointer to an array does, so `&[T; N] as *const [T]` is
  allowed, with length `N` (ADR 0031's table). Nothing casts to a reference
  (`ty::invalid-cast`, with the help `&*p`).

`slice(p, n)` is unchanged: it takes a thin raw pointer and gives a raw slice.
A reference given to it is `ty::mismatched-types`, with a help naming
`&(*s)[a..b]`.

### Using one

- **Elements.** `(*s)[i]` is the place of element `i`, checked against the
  length as through a raw slice (ADR 0031): an index not below `len(s)` calls
  the panic function, cause `IndexOutOfBounds`. It can be read, written
  through a `&mut [T]` (`ty::write-through-const` through a `&[T]`), borrowed,
  and passed to `&raw`. `s[i]` is `ty::cannot-index`, with the help `(*s)[i]`
  (ADR 0105: no automatic dereference).
- **The whole.** `*s` can be indexed, sub-sliced, borrowed (`&*s`,
  `&mut *s`) and passed to `&raw`; reading or copying all of it is
  `ty::unsized` (ADR 0031).
- **Length.** `len(s)` takes a reference to a slice, as it takes a raw one,
  and a reference to an array (`N`). It reads the reference, never the
  elements, and moves nothing: `len(s)` with `s: &mut [T]` leaves `s` usable.
- `&(*s)[i]` and `&*s` through a raw slice that is not volatile are allowed;
  through a volatile one they are `ty::reference-to-volatile` until M5.4c
  (ADR 0105).

### Sub-slices

A **range in an index** makes a place of type `[T]` that is part of an array
or a slice:

```
fn write(bytes: &[u8]) { ... }

let line: [u8; 8] = ...;
let n: usize = ...;
write(&line[0..n]);
fill(&mut (*buf)[2..], b' ');
let raw: *const [u8] = &raw const (*s)[..4];
```

- **Forms.** `base[a..b]` is the elements from `a` up to but not including
  `b`; `base[a..]` is `base[a..L]` and `base[..b]` is `base[0..b]`, with `L`
  the length of `base`. The range is half-open, as `..` is wherever the
  language reads it (the bit fields of ADR 0059 write a closed range `..=` and
  reject `..`). `base[a..=b]` and `base[..]` are `parser::unexpected-token`,
  the first with a help giving `a..b + 1`, the second with a help giving
  `&*s` for a slice and the conversion for an array. `a` and `b` are `usize`
  expressions; a literal takes `usize` as an index does.
- **Grammar.** Inside `[ ]`, `..` binds more loosely than every operator and
  `as`: `base[a + 1..b as usize]` is `base[(a + 1)..(b as usize)]`. A range
  exists only there: `a..b` is no expression, and `..` is not an operator
  anywhere else.
- **Bases.** `base` is a place of an array type (a variable, a field, an
  element, `*p` or `*r` with `p` or `r` pointing to or referring to an array, a
  `static`, a byte string) or of type `[T]` (`*s` for a slice `s`, raw or
  reference, or another sub-slice: `(*s)[1..][..2]`). A part of a value that
  is not a place, of an array type (`MSG[1..3]` with `MSG` a `const`, a
  call's result, an array literal), is a base too, under `&` as ADR 0105
  says of such parts: the whole value is written to a new local and the
  sub-slice of it is borrowed; `&mut` of it is `ty::mut-borrow-of-immutable`.
  `&raw` of it follows what `&raw` of an element of the same value does
  today (ADR 0021, 0029, 0105): of a part of a `const`, `ty::not-a-place`; of
  a part of a call's result or an array literal, `&raw const` points into a
  temporary holding the value and `&raw mut` is `ty::address-of-immutable`. A range on a slice not
  dereferenced is the error an index there is: `ty::slice-pointer-index` on a
  raw slice (ADR 0031), `ty::cannot-index` on a reference (ADR 0105), each
  with the help `(*s)[a..b]`. A range on anything else (a thin pointer, a
  reference to an array, an integer) is `ty::cannot-index`, with a label
  saying a range needs the length of an array or a slice, and the help
  `(*p)[a..b]` for a pointer or reference to an array or `slice(p, n)` for a
  thin pointer to anything else.
- **What a sub-slice is.** A place of type `[T]`, like `*s`: it can be
  indexed, sub-sliced again, borrowed with `&` or `&mut` (giving `&[T]` or
  `&mut [T]`) and passed to `&raw` (giving `*const [T]` or `*mut [T]`, not
  volatile, as `&raw` of any place, ADR 0021). Reading, writing or copying it
  whole (an operand, an assignment's left side, a `match` scrutinee) is
  `ty::unsized`. Whether `&mut` and `&raw mut` are allowed is decided by its
  base, as for an element: not through a `&[T]`, a `*const [T]`, a variable
  without `mut`, a `static`, a byte string (`ty::mut-borrow-of-immutable`,
  `ty::address-of-immutable`).
- **Addresses.** A sub-slice has an address only where its elements do, by
  the rules ADR 0105 shares between `&raw` and references, with the same
  codes: a range on a register array or a cluster array is
  `ty::register-address`, on an array field of a `#[packed]` struct that may
  be misaligned `ty::packed-field-address`, on an array of `!`
  `ty::never-in-memory`; `&` or `&mut` of a range on an array behind a
  volatile pointer, a volatile raw slice, a peripheral or a `static mut` is
  `ty::reference-to-volatile` or `ty::reference-to-static-mut` until M5.4c,
  while `&raw` of it is allowed.
- **Checks.** The base is evaluated first, and with it the length it has
  (the slice's length is read then, so code in the bounds that assigns the
  variable holding a raw slice does not change it, ADR 0018); then `a`, then
  `b`. The place exists when `a <= b` and `b <= len`, with `len` the length
  of the base: `N` for an array, the slice's for `*s`, and `b - a` of the
  inner range for a sub-slice, so `(*s)[2..4][3]` panics whatever `len(s)`
  is. Otherwise the panic function is called, with the cause
  `IndexOutOfBounds` located at the `[` (ADR 0053), as for an index. No new
  variant is added to `core::PanicReason`: a panic function that matches its
  variants keeps compiling. An empty sub-slice (`a == b`, `a == len`
  included) is valid.
- **Bounds known at compile time** (ADR 0032, as an index is: a literal, a
  `const`, a `comptime` expression). On an array, each bound known is
  checked while building, on its own: `a > N` or `b > N` is
  `ty::index-out-of-bounds`, and so is `a > b` when both are known. A range
  whose two bounds are integer literals inside the array costs nothing at run
  time; any other is checked at run time, as an index that is not a literal
  is (ADR 0021). An omitted bound counts as a literal (`0`, or `N`), so
  `arr[2..]` and `arr[..4]` cost nothing either. On a slice, nothing is known
  of the length, and every range is checked at run time.
- **Through a raw slice or pointer**, a sub-slice is unchecked against
  anything but the length the raw slice carries: `&raw const (*s)[2..5]` is
  the checked form of `slice(&raw const (*s)[2], 3)`. `&(*s)[a..b]` through a
  raw slice that is not volatile gives a reference the program vouches for, as
  `&*s` does.
- **At compile time** a sub-slice is evaluated as at run time, and the rules
  of ADR 0031 on slices of byte strings and statics hold for what `&raw` of
  one gives.

### Splitting

A `&mut [T]` or a `&mut [T; N]` is **not split** into two disjoint `&mut`
halves in use together in M5.4b. Two sub-slices of one array or slice overlap
(below), so `f(&mut buf[0..4], &mut buf[4..8])` is
`analysis::borrow-conflict`. A built-in that gives two references, as Rust's
`split_at_mut`, needs to return them or bind them, and references are never
returned (ADR 0094); comparing literal bounds in the borrow check is what ADR
0106 rejected for literal indexes. A program that needs both halves borrows
them one after the other, or splits through raw pointers, as it does today.

### The borrow check

ADR 0106's places gain the parts of a slice, by the rule that already holds
for arrays:

- two elements of one slice always overlap, whatever their indexes;
- a sub-slice overlaps every element and every sub-slice of the same base,
  whatever its bounds, and the base itself: `&mut (*s)[0..2]` and
  `&(*s)[3]` are not in use together;
- `*s` and the places through it are rooted at `s`, as for any reference: an
  access through `s`, or a reborrow of a part of `*s`, is an access through
  the borrows `s` holds (ADR 0106), and `len(s)` is a read of `s`.

A reborrow of a slice through a reference keeps in use the borrows the
reference holds, as any reborrow does: `let t: &mut [u8] = &mut (*s)[1..];`
then `len(s)` while `t` is used later is a conflict, as `*s` is in use through
`t`. Through a raw slice nothing is followed (ADR 0106).

By ADR 0106's rule for arguments, which this ADR does not change, `fill(s,
len(s))` with `s: &mut [u8]` is a conflict: `s` is lent to the call by the
first argument, and the second reads `s` while that loan is in use.
`fill(len(s), s)`, or the length taken into a `let` first, is accepted. It is
the case of `f(&mut x, x.a)` (ADR 0106), and the help of the error says to
take the value into a `let` before.

Diagnostics name places as ADR 0106 does, with `[_.._]` for a sub-slice:
"cannot borrow `buf[_.._]` as mutable while it is borrowed". A borrow written
by the program keeps its own description: `print(&buf)` borrows `buf` once,
labelled as any `&buf` is ("`buf` is borrowed here"). When the conversion
reborrows a reference to an array that is already held (a variable, a
parameter, or another value of a reference type), that reborrow is described
as the conversions of ADR 0106 are: "lent to this call" when a `&mut [T; N]`
variable or parameter is an argument for a `&mut [T]` parameter (ADR 0106's
lending), and "converted to a slice here" in every other case.

### Signatures and the ABI

A reference to a slice is passed, at every call and on every target, exactly
as the raw slice of its mutability is: the C struct of ADR 0031, by ADR 0025
on riscv32 and ADR 0075 on thumbv7m. `extern fn`, `#[export]` and `#[naked]`
functions may take `&[T]` and `&mut [T]`, as ADR 0105 lets them take
references; the C side vouches for the address and the length. A reference to
a slice is never returned (ADR 0094). As for any reference, code generation
gives it no attribute a raw slice lacks.

### What does not change

- Raw slices, `slice`, and `len` of a raw slice are as ADR 0031 says. `&raw
  const *s` is still the slice `s`.
- `assert`'s message is still a `*const [u8]` (ADR 0031): `assert(c,
  &b"...")` is a type error (`&[u8; N]` does not convert to a raw slice).
  Accepting a `&[u8]` there is an amendment of ADR 0031 for M5.7, which moves
  `core` and the tests to references (roadmap, pending decisions).
- The panic function keeps its `*const core::PanicInfo` (ADR 0053).

### Diagnostic codes

No new code. Existing codes gain cases: `ty::cannot-index` (a range on a base
without a length; `s[i]` and `s[a..b]` on a reference to a slice, with the
help `(*s)`), `ty::slice-pointer-index` (`s[a..b]` on a raw slice),
`ty::unsized` (a sub-slice read whole; the M5.4a case "borrowed"
goes), `ty::index-out-of-bounds` (a range known at compile time),
`ty::mut-borrow-of-immutable` and `ty::address-of-immutable` (a sub-slice
through something that cannot be written), `ty::mismatched-types` (a range
bound that is not a `usize`; a reference given to `slice`),
`ty::reference-to-volatile` and `ty::reference-to-static-mut` (`&*s` and
`&(*s)[a..b]` through a volatile raw slice; a range on an array behind a
volatile pointer, a peripheral or a `static mut`), `ty::register-address`,
`ty::packed-field-address` and `ty::never-in-memory` (a range on what has no
address), `ty::not-a-place` (`&raw` of a range on a part of a `const`),
`ty::address-of-immutable` (`&raw mut` of a range on a part of a call's result
or an array literal),
`parser::unexpected-token` (`..=` and `..` alone in an index),
`analysis::borrow-conflict` (the places above).

### Conformance tests

- ui tests for each case of each code above, and for each form of range on
  each base: an array variable, a field, `*p` to an array, `*s` for a raw
  slice, `*r` for a `&[T]`, a `static`, a byte string, a sub-slice of a
  sub-slice; the conversion in every position of ADR 0024, from `&` and
  `&mut`, lending a `&mut [T; N]` variable; `len` of each kind.
- ui tests of the borrow check: two sub-slices of one base, a sub-slice and
  an element, a reborrow of a sub-slice with the base used after it, `len(s)`
  while a reborrow of `*s` is in use, `fill(s, len(s))`, accepted orders of
  the same.
- A program reading and writing through `&[T]` and `&mut [T]`, with
  sub-slices of arrays and of slices, with elements of one byte and of four
  (a `u32` and a struct), runs on QEMU on both targets at `-O0` and `-O`.
- Each way a range fails reaches the panic function on QEMU: `a > b` with
  `b <= len`, `b > len`, `a > len`, and an index or a range past the end of a
  sub-slice but inside its base.
- Empty cases: `&[T; 0]` and `&b""` converted, an empty sub-slice at the
  length at run time and at compile time, a slice of a zero-sized element
  type.
- An `extern fn` and a `#[export]` function taking `&[u8]` and `&mut [u8]`,
  called across the C ABI, run on both targets.

## Consequences

- A function that reads or fills a buffer of any length takes `&[u8]` or
  `&mut [u8]`, checked by the borrow check, and its callers pass `&buf` or
  `&b"text"`: lighter than `&raw const b"..."`, which ADR 0031 named as the
  price of byte strings being arrays.
- A sub-slice is checked against the length, where `slice(&raw const ...,
  n)` was not; `slice` stays for memory the program knows of from elsewhere.
- Two halves of one buffer cannot be lent `&mut` together. A later step may
  add a split once references can be returned or bound in pairs (roadmap,
  deferred ideas), or compare constant bounds if ADR 0106's reason against
  comparing indexes is reconsidered; either breaks no program.
- `core` and the tests keep their raw slices until M5.7.
- A range is a new piece of syntax, but only inside `[ ]`: there is no range
  value, type or expression elsewhere.
- `fill(s, len(s))` must be written with the length first or in a `let`, as
  `f(&mut x, x.a)` must since ADR 0106.

## Alternatives considered

- **A built-in, `sub(s, a, b)`** — no new syntax, but one more reserved name,
  and it would take references only: `&raw` of a part of an array would still
  be `slice(&raw const a[2], 3)`, unchecked. A range is a place, so it
  composes with `&`, `&mut` and `&raw` alike.
- **No sub-slices, only `&*slice(...)`** — the smallest step, but every
  driver writing part of a buffer would leave the borrow check and the length
  check for the raw way.
- **`..` alone and `..=`** — `base[..]` adds a second spelling of `&*s` and of
  the conversion; `..=` would read differently from the bit fields' `..=`,
  which names the last bit, while an element range is half-open as lengths
  are. Both can be added later without breaking programs.
- **A new panic cause, `RangeOutOfBounds`** — a more precise message, but a
  new variant of `core::PanicReason` breaks every panic function that matches
  the variants without a catch-all.
- **Splitting with literal bounds** (`&mut a[0..4]` and `&mut a[4..8]` in use
  together) — the case drivers meet, but ADR 0106 rejected comparing literal
  indexes because a `const` bound would then behave unlike a literal one; the
  same holds for ranges.
- **A split built-in binding two names** (`let (lo, hi) = split(s, 4);`) —
  needs a form of binding the language does not have (no tuples); left for
  when references can be returned (roadmap, deferred ideas).
- **Elements of a slice not overlapping when their indexes differ** — the
  check runs before evaluation (ADR 0094) and indexes are run-time values; it
  is the array rule of ADR 0106.
- **An explicit conversion, `&buf as &[u8]`** — nothing casts to a reference
  (ADR 0105), and every call with a byte string would carry it, the noise ADR
  0031 removed for pointers.
