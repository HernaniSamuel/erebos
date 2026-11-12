# 0101. Linear types and `#[linear]`

- Status: Accepted; amended by 0105 (no linear value written through a
  reference)
- Date: 2026-11-10

## Context

ADR 0094 makes every type with values copy, affine or linear. A **linear**
type is an affine type marked so: a value of it must be moved before every
point where the function holding it returns, and is never overwritten. A
type containing a linear value is linear, whatever it declares. It leaves to
M5.3 how a type is marked, assigning over a place holding a linear value,
statics of linear types, and how the module that defines a linear type ends a
value (roadmap, pending decisions). ADR 0102 settles assigning and ending;
this ADR settles the mark and where a linear value may be held.

Where M5.2 left things:

- `#[copy]` is written on a struct (not a bit-field struct) or an enum with
  payloads (ADR 0096). Attributes are checked in HIR lowering
  (`src/hir/lower/item.rs`), with `resolve::misplaced-attribute`,
  `resolve::attribute-argument`, `resolve::duplicate-attribute` and
  `resolve::conflicting-attributes` (`#[packed]` with `#[align]`).
- A field of a struct is private unless `pub`, and a struct literal is
  allowed only where every field is visible (ADR 0050): a struct with a
  private field is built only by its module, "the base for handles".
- No element of an array is ever moved out of (ADR 0097, 0100), nor is any
  place reached through a pointer, nor a static (ADR 0094, 0097). A value
  put in one of them stays there until the place is overwritten.

That last point decides where a linear value may go. A value that can never
be moved out of its place can never be consumed, so the only thing that can
happen to it is to be forgotten, which is what linearity forbids.

## Decision

### The attribute

`#[linear]` takes no argument and may be written on a struct that is not a
bit-field struct:

```
#[linear]
pub struct Transfer { channel: u32 }

#[linear]
pub struct Token { private: () }
```

- On any other item, enums with and without payloads included, it is
  `resolve::misplaced-attribute`, whose help says where it applies.
- With an argument it is `resolve::attribute-argument`; twice,
  `resolve::duplicate-attribute`.
- With `#[copy]` on the same struct it is `resolve::conflicting-attributes`:
  a linear value must not be duplicated.
- It goes with `#[packed]` and `#[align(N)]`, and changes neither the layout
  nor the ABI.

The mark says that a value must be consumed; it does not say who may make
or end one. That is visibility's: a struct literal is allowed only where
every field is visible (ADR 0050), and so is `end` (ADR 0102). A
`#[linear]` struct with only `pub` fields, or with none, is built and ended
by any module that sees it; one with a private field, even of type `()`,
only by its module. A handle that must not be forged needs a private field.

An enum is not marked: its variants are as visible as the enum (ADR 0050),
so any module could build its values and take them apart with `match`, and
the mark would protect nothing. An enum is linear by holding a linear value
(below).

### Which types are linear

A type is **linear** when it is a struct declared `#[linear]`, or when it
holds a linear type: a struct with a field of a linear type, an enum with
payloads with a payload value of a linear type, `Option(T)` with `T` linear,
`Result(T, E)` with `T` or `E` linear, an array or a slice whose element type
is linear. The slice here is the unsized type `[T]`, which is only ever a
pointee (ADR 0031); a pointer to it, `*const [T]`, is a pointer, copy and
not linear, like every pointer. (Arrays and slices of linear types are
rejected below; they are linear so that the rule can name them.) Unlike
copy (ADR 0096), this is computed from the parts, as ADR 0094 says ("one
containing a linear value is linear, whatever it declares"). A struct
cannot hold itself by value (its layout would be a cycle, ADR 0048), so the
computation ends.

A linear type is never copy: `#[copy]` on a type with a field or payload
value of a linear type is `ty::copy-field-not-copy`, as for any non-copy
field (ADR 0096). Every linear type is therefore affine as well, and every
rule of moves (ADR 0097) applies to it.

A raw pointer to a linear type, `*mut Transfer`, is copy (ADR 0096) and not
linear: it holds an address, not the value. A pointer to an array or slice
of a linear type is rejected below with the array or slice.

### Where a linear value may not be held

- **Statics.** A static whose type is linear is `ty::linear-static`, at its
  type. The value a static holds is never moved out of (ADR 0094), so it
  could only be overwritten, which forgets it. How a context comes to own a
  value a static holds is M5.6's; that step may relax this rule.
- **Arrays and slices.** An array or slice type whose element type is
  linear is `ty::linear-array`, once per type, at the first of:
  - the type, wherever it is written: a field, a payload value, a parameter,
    a return type, a `let`, a static, a type argument, the pointee of a
    pointer (`*const [Transfer; 4]`, `*const [Transfer]`), a cast, a layout
    query;
  - an expression whose type reaches it (through arrays, fields, payload
    values and pointees) while the types of its operands do not, and where
    the type was not written: an array literal (`[start(1), start(2)];`,
    `([start(1), h])[0].channel`), a repeat expression (`[.None; N]` of
    `Option(Transfer)`), `slice(p, n)` with `p: *const Transfer`, and any
    other built-in or expression that makes such a type from parts that are
    not.

  No element is ever moved out of (ADR 0100), and nothing is moved out of a
  place through a slice (ADR 0097), so no element could be consumed. The
  rule on expressions is general rather than a list: an expression whose
  type has such an array or slice either got it from an operand, which is
  then reported, from a written type, or made it, and is reported. So type
  checking rejects every one before MIR is built (ADR 0103, rule L3).
- **Through a pointer.** Moving a value of a linear type into a place
  reached through a pointer (`*p = v`, `p[i] = v`, `(*p).f = v`) is
  `ty::linear-through-pointer`, at the assignment. The value could never be
  moved back out (ADR 0097).

Taking the address of a place holding a linear value with `&raw const` or
`&raw mut` stays valid, and so does reading or writing its copy fields
through that pointer: raw pointers are unchecked (ADR 0094). A write of a
whole linear value through it is the error above; a write of a copy field
changes the value without forgetting it.

### Functions outside the program

The parameters and results of `extern fn` and `#[export]` functions may be of
linear types. C code is outside every check (ADR 0026): a value passed to an
`extern fn` counts as consumed, and one returned by it, or passed by C to an
`#[export]` function, is a new value the program must consume.

## Consequences

- A handle to hardware that must be given back is a struct with a private
  field and `#[linear]`: only its module builds it (ADR 0050), and only its
  module ends it (ADR 0102). Without a private field, any module can make
  and end one.
- `Option(Transfer)` and enums of the program that hold a `Transfer` are
  linear without a mark; ADR 0102 says when their values are consumed.
- A table of transfers cannot be an array, and a pending transfer cannot
  wait in a static, until M5.6 (ownership of what statics hold) or a later
  step relaxes these rules. Relaxing them breaks no program.
- A linear value handed to C is trusted to be consumed there.
- The new codes join the catalog of ADR 0069.

## Alternatives considered

- **`#[linear]` on enums with payloads too, as `#[copy]`** — the same places
  as `#[copy]`, but the mark would bind nobody: every module that sees the
  enum can build a value and end it with a `match`. Allowing it later breaks
  no program.
- **A keyword, `linear struct`** — more visible, but a new token for what
  `#[copy]` already writes as an attribute, and two styles for the two
  kinds.
- **Requiring `#[linear]` on every type that holds a linear value, as
  `#[copy]` is declared** — the declaration would say what the type is, but
  ADR 0094 decided that holding a linear value makes a type linear whatever
  it declares, and a forgotten mark would be an error with nothing to choose.
- **Statics of linear types that are never assigned** — the initializer would
  be the only value, read through its copy fields; of little use before
  M5.6, which decides how a static's value is owned.
- **Arrays and writes through pointers allowed, unchecked** — the escape
  raw pointers are elsewhere (ADR 0094), but a handle put there would be
  forgotten with no mark at the place that forgets it, which is what the
  linear type exists to prevent.
- **`extern fn` and `#[export]` signatures without linear types** — C could
  not forge or forget a handle, but drivers that hand a transfer to C code
  could not be written; C is unchecked anyway (ADR 0026).
