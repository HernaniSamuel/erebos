# 0106. The borrow check

- Status: Accepted; amended on 2026-11-11 (which later use is labelled); amended by 0108 (elements and sub-slices of a slice), 0111 (statics and peripherals as roots; parts of a peripheral overlap; conflicts across calls)
- Date: 2026-11-11

## Context

ADR 0094 states the rule references must keep: while a `&mut` borrow of a
place is in use, the place is used only through it; while a `&` borrow is in
use, the place is not written, moved or borrowed `&mut`; two references passed
to one call do not overlap if one of them is `&mut`. References are
second-class, so the check stays inside one function, with no lifetime
written, and runs as an analysis of one body before evaluation (ADR 0094,
0099). ADR 0105 is the language of references in M5.4a.

ADR 0094 leaves to M5.4 how long the borrow held by a `let` lasts and when two
places overlap. This ADR settles both for M5.4a and says what the check
accepts and rejects; ADR 0107 says how MIR holds borrows and how the analysis
computes this.

## Decision

### Borrows and how long they last

Each `&place` or `&mut place` that runs makes a **borrow** of the place,
shared or mutable. A borrow is **in use** at a point of the function when a
variable, a parameter or a value the compiler keeps that may hold the
reference, or a reference made through it, is used after that point: the
borrow lasts until the **last use** of what holds it, not to the end of a
scope.

- `let r: &mut u32 = &mut x; *r = 1; x = 2;` is accepted: `r` is not used
  after `*r = 1`.
- `let r: &mut u32 = &mut x; x = 2; *r = 1;` is rejected: `r` is used after
  the write of `x`.
- A borrow passed to a call, `f(&mut x)`, is in use until the call returns;
  no call returns a reference (ADR 0094), so nothing holds it afterwards.
- A reference copied (`let b: &u32 = a;`) or moved keeps the borrows it may
  hold in use while either copy is used. A reference that may hold one of
  several borrows, as `let r: &u32 = if c { &a } else { &b };`, keeps all of
  them in use: the check does not know which branch ran.
- A borrow of a place through a reference, `&mut *r` or `&(*r).f`, keeps in
  use the borrows `r` may hold, for as long as it is itself in use: the new
  reference reaches what `r` reaches.
- Uses follow control flow: a use in a later iteration of a loop keeps a
  borrow in use across the back edge.

### Places and overlap

The check follows places made of a variable, a parameter, a value the
compiler keeps or a static, then fields, elements, and dereferences of a
reference. Two such places **overlap** when one contains the other or both
are the same, compared part by part from their root:

- two different fields of one struct do not overlap: `&mut s.a` and `&s.b`
  may be in use together;
- two elements of one array always overlap, whatever their indexes, even
  constant ones: `&mut a[0]` and `&a[1]` may not be in use together;
- `*r` and the places through it are places of their own, rooted at `r`: a
  borrow of `x` and a use of `*r` never overlap, even if `r` refers to `x`,
  since `r` is then itself one of `x`'s borrows and using it is using that
  borrow.

A place through a **raw pointer** (`*p`, `p[i]`, `(*p).f`, at any depth) is
not followed: borrowing it makes a reference the check holds nothing for, and
accessing it conflicts with no borrow (ADR 0105, "What may be borrowed").

### Conflicts

An access to a place conflicts with a borrow in use of an overlapping place:

| Access | conflicts with a borrow in use that is |
|--------|-----------------------------------------|
| read (a copy, a test of a tag, `&raw const`, `len`) | mutable |
| `&` borrow | mutable |
| write (an assignment, a call's result, an `asm` output, `&raw mut`) | shared or mutable |
| move, or `end` (ADR 0102) | shared or mutable |
| `&mut` borrow | shared or mutable |

The accesses of a statement are taken in the order the program evaluates them
(ADR 0018). A conflict is `analysis::borrow-conflict`.

The access through the reference itself is never a conflict: `*r = 1` while
`r` holds a `&mut x` writes through the borrow. A borrow of `*r` (a
**reborrow**) is in conflict, by the table, with uses of `r` and `*r` while it
is in use: `let q: &mut u32 = &mut *r; *r = 1; *q = 2;` is rejected.

### Calls

The arguments of a call are evaluated left to right, each before the call
starts. So, as ADR 0094 requires:

- `f(&mut x, &x)` and `f(&mut x, &mut x)` are rejected: the first borrow is
  in use, held for the call, when the second is made;
- `f(&mut x, x.a)` is rejected: `x.a` is read while the `&mut x` held for the
  call is in use; `f(x.a, &mut x)` is accepted, since `x.a` is read first;
- `f(&mut s.a, &mut s.b)` is accepted: the places do not overlap.

**Lending.** An argument that is a variable or a parameter of type `&mut T`
is lent for the call, not moved (ADR 0094): the call receives a reborrow
`&mut *r`, which is in use until it returns, so `r` cannot be used by a later
argument of the same call and is usable again after it. The conversion of a
`&mut T` to a `&T` (ADR 0105) is a shared reborrow `&*r` in the same way,
wherever it happens. A `&mut` that is not a variable or a parameter, such as
the value of an `if`, is moved, as any value is.

### What the check does not change

- A use after a move stays the analysis of moves' (ADR 0097, 0099): borrowing
  a moved place is `analysis::use-after-move`, and the borrow check runs only
  on a body the analysis of moves accepted. The analysis of linearity (ADR
  0102) runs on the same bodies; a borrow neither gives nor consumes a linear
  value.
- Paths that do not return (a `panic`, `unreachable`, a call that returns `!`)
  use nothing after them, so no borrow is in use there.
- `critical` blocks change nothing in M5.4a: a reference cannot be the block's
  value (ADR 0105), and a borrow taken inside it is checked like any other.
  What a borrow of a `static mut` inside `critical` means is M5.4c's.

### Diagnostics

`analysis::borrow-conflict`, one error per access in conflict, at the span of
the access, with:

- a message naming the access and the place, as the analysis of moves names
  places (`x`, `s.f`, `a[_]`, `*r`, `(*r).f`): "cannot assign to `x` while it
  is borrowed", "cannot move out of `x` while it is borrowed", "cannot end `x`
  while it is borrowed", "cannot borrow `x` as mutable while it is borrowed",
  "cannot use `x` while it is borrowed as mutable"; for a borrow the compiler
  made for an argument or a conversion, the message says "lent to this call"
  or "converted to `&` here" instead of the place's own name where it helps;
- a label at each borrow in use that the access conflicts with ("`x` is
  borrowed as mutable here");
- for each, a label at one later use that keeps it in use ("the borrow is used
  later here"): a use that comes after the access in the program, the first
  of those in source order, or, when the access reaches every one only round a
  loop, the first of those, labelled "in the next iteration of the loop" (ADR
  0107 orders them exactly);
- a help on how to end the borrow first: use the reference before, or take the
  value out into a `let` before borrowing.

A place the compiler keeps is "this value" in labels, as ADR 0103 names
temporaries. Errors are ordered by the span of the access, and an error with
the same span, message and labels as another is dropped, as for moves.

**Note, 2026-11-11:** the text said the later use is the first in source order
after the access, and "in the next iteration of the loop" otherwise. Source
order cannot tell a use that comes after the access in the program from one
that comes first in the source: a call reads its arguments when it runs, after
all of them are evaluated, but its span starts at the callee, before them. So
`f(&mut x, &x)`, with no loop in the function, would have said "in the next
iteration of the loop". A use in the access's own block after it, and every
use by the call the access is an argument of, come after the access whatever
their spans; the label of the loop is for a use reached only through a back
edge. ADR 0107, "Errors", has the exact rule.

## Consequences

- Programs that write `let r = &mut x; ... ; x = 1;` after the last use of `r`
  need no block around the borrow.
- Elements of one array cannot be borrowed `&mut` together; code that needs
  two of them borrows the array whole, or copies one out. Comparing constant
  indexes, or a built-in that splits a `&mut` to an array or a slice in two,
  can come later (M5.4b decides sub-slices) without breaking programs.
- The analysis runs on every body with a borrow, `core` included; a body with
  none does no work for it (ADR 0107).
- Raw pointers stay unchecked; `&*p` gives a reference the check trusts
  without seeing where it points.

## Alternatives considered

- **A borrow held by a `let` lasts to the end of its block** — simpler to
  state, and relaxing it later breaks nothing; but `let r = &mut x; f(r);
  x = 1;` would need a block, and the analysis of ADR 0107 computes the last
  use anyway for borrows held by the compiler's own values.
- **Comparing constant indexes** — `&mut a[0]` and `&mut a[1]` together; the
  check runs before evaluation (ADR 0094), so only literals could be compared,
  and a `const` index would behave unlike a literal one.
- **Every part of a variable overlapping every other** — simplest, but
  `f(&mut s.a, &s.b)` is the common case of handing out two parts of one
  driver's state.
- **Following places through raw pointers** — the check would have to guess
  what a pointer from `extern`, an integer or another pointer's arithmetic
  points to; raw pointers are the unchecked way out (ADR 0094).
- **Moving a `&mut` argument, with an explicit `&mut *r` to keep it** — Rust
  reborrows implicitly at calls for the same reason; ADR 0094 already says a
  `&mut` passed to a call is lent.
