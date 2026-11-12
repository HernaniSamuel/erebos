# 0097. Moves

- Status: Accepted; amended by 0100 (the elements of a temporary's array do not
  move; one error per fault holds for uses that do not move), 0103 (`end`
  may use a partly moved place), 0105 (no move out of a place through a
  reference)
- Date: 2026-11-09

## Context

ADR 0094 makes non-copy values affine: using one moves it, and the source
cannot be used again until it is assigned a new value; the check is per
function and follows control flow; nothing runs when a value is moved from,
goes out of scope or is overwritten; a static of a non-copy type is never
moved out of. It leaves to M5.2 which other places a value may be moved out
of (a field, an array element, a payload bound by `match`, a place through a
raw pointer) and the diagnostics. ADR 0096 says which types are copy.

Places today (ADR 0021, 0031, 0039): a local or parameter, a field of a
place, an element of an array place, `*p` and `p[i]` through a raw pointer,
an element of a slice through `(*s)[i]`, a static, and the payload values a
`match` binds. A field or element of a temporary (`f().x`) is a field of a
value, not of a place.

## Decision

### What moves

A **use** of a place reads it, takes its address, or names a part of it. A
use that reads a whole value of a non-copy type **moves** it: `let`,
assignment, an argument, a returned value, a field or payload value of a
literal, a `break` value, the value of a block or arm, `?`'s operand. A use of
a copy type copies, as today. Moving and copying are the same machine
operation (ADR 0021).

A **discarded place** is the expression of an expression statement when it
is a place of a non-copy type, or a block (`{ ... }` or `critical { ... }`)
whose tail is a discarded place, recursively: `a;`, `a.f;`, `{ a };`,
`{ { a } };` and `critical { a };`. It computes the place (an index in it is
evaluated and checked) but neither moves nor uses it, and is valid after `a`
is moved. So a statement cannot pass for consuming a value, which matters for
the linear values of M5.3. Any other discarded expression computes its value
as today: a call, a literal, and an `if` or a `match`, which choose a value
and so move what their arms give; `if c { a } else { b };` moves `a` or `b`.
A place of a copy type is read as today, so a volatile read written as a
statement, as `UART0.lsr;` (ADR 0058, 0060), still happens.

### Places a value is moved out of

A non-copy value may be moved out of a place whose path from its root goes
only through fields, and whose root is a local, a parameter or a temporary:

- `a`, `a.f`, `a.f.g` with `a` a local or parameter: yes. Moving `a.f` moves
  that field only; `a` is then **partly moved**.
- A payload value bound by a pattern of `match` (ADR 0040), at any depth of
  payloads and fields: yes, out of that part of the scrutinee, which is then
  partly moved. A `let` arm binds the whole scrutinee and moves it whole.
- An element of an array, by `a[i]` whatever `i`: no.
- A place through a pointer, `*p`, `p[i]`, `(*p).f`, `(*s)[i]`: no. The
  compiler cannot follow what the pointer points to, so moving out of it would
  duplicate the value in silence.
- A static, or a part of one: no (ADR 0094).

Moving out of a place that is not allowed is `ty::move-out-of-place`, at the
use, with a message naming what the place is (an array element, a place
through a pointer, a static), and a help suggesting `#[copy]` when the type is
a struct or an enum with payloads of the program whose fields are all copy.
Fields of a copy type are read from any of those places as today: a static
holding a driver's state still gives its integer fields.

An array of a non-copy type is moved whole only. A pointer to a non-copy type
is valid, and a non-copy value is written through it (`*p = v` moves `v`);
reading the whole value back through the pointer is the error above.

### `match` does not move its scrutinee

A `match` on a place reads the tags and the values its patterns test, which
are copy, and moves only what its bindings of non-copy types bind. So
`match state { .Idle => ..., .Busy(let n: u32) => ... }` leaves `state`
usable, and `match msg { .Data(let buf: Buffer) => ... }` moves the payload
value `buf` out of `msg` in that arm only. A scrutinee that is not a place is
a temporary, as today.

A binding moves its value only once the whole alternative it belongs to has
matched, so an alternative that binds a value and then fails a later test
moves nothing (ADR 0098 says how MIR does it). For that, a binding of a
non-copy type inside an or-pattern nested in a payload, as `b` in
`.Pair(.A(let b: Buf) | .B(let b: Buf), 0)`, is
`ty::non-copy-binding-in-or-pattern`, at the binding: the positions after
such an or-pattern are tested once for all its alternatives (ADR 0042), so
at the end of the alternative the place to move from is no longer known. The alternatives of an arm
(`.A(let b: Buf) | .B(let b: Buf) => ...`) are not nested, and may bind
non-copy values. Bindings of copy types are unrestricted.

### Uses of a moved place

The check is per function and follows control flow (ADR 0094): at each
point, a part of a place is **moved** if a path from the start of the
function reaches the point through a move of that part, or of a place
containing it, and through no assignment to it since. Then:

- Reading, moving or taking the address (`&raw const`, `&raw mut`) of a place
  is an error when the place, a place containing it, or a part of it is
  moved. Reading a copy field that was not moved out of a partly moved struct
  is valid; reading the tag of an enum whose payload value was moved is valid,
  the tag being untouched.
- Assigning a whole place (`a = v;`, `a.f = v;`) gives it a value again, with
  every part of it. An assignment to a part of a place that is moved whole is
  an error: a struct is not rebuilt field by field. So after `a.f` is moved,
  `a.f = v;` makes `a` whole again; after `a` is moved, `a.f = v;` is an
  error.
- A move inside a loop reaches the next iteration through the loop's back
  edge, so a value moved in a loop without being assigned again in it is an
  error at its use in the next iteration.
- A value that goes out of scope moved, partly moved, or not, is valid
  (affine).

Each is `analysis::use-after-move`, at the use, labeling every move that
reaches it, in the order of the source: "moved here", and "moved here, in an
earlier iteration of the loop" for a move after the use in the source. The
help suggests `#[copy]` as for `ty::move-out-of-place`. One error per fault:
once the use is reported, the place counts as valid on the paths through it,
so a second use after the same move is not reported again.

### Statics

A static of a non-copy type may be assigned whole (`static mut`, moving the
value in), its copy fields may be read, and its address taken with `&raw`;
moving out of it is `ty::move-out-of-place` above. Statics are not checked by
the flow analysis: what they hold is always valid.

## Consequences

- A handle passed to a function that consumes it cannot be used again, which
  makes typestate expressible with one struct per state (roadmap, M5.2's
  program).
- Taking a value apart, field by field, is possible, as M5.3 needs for linear
  values.
- An array of handles, a handle behind a raw pointer and a handle in a static
  are reached only through their copy fields until references (M5.4) and the
  ownership of peripherals (M5.6). Relaxing a rule of "Places a value is moved
  out of" later breaks no program.
- `match` on a place and the bindings of a pattern change how MIR is built
  (ADR 0098).
- The three diagnostics join the catalog of ADR 0069.
- Which or-patterns may bind non-copy values is left open, to the step that
  needs one (roadmap, pending decisions).

## Alternatives considered

- **Moves out of whole locals and parameters only** — the simplest check,
  but a non-copy field could leave its struct only with the whole struct, and
  a value could never be taken apart.
- **Moving out of a place through a raw pointer, unchecked, as Rust's
  `ptr::read`** — raw pointers are the unchecked way out (ADR 0094), but a
  plain `*p` would duplicate a handle with no mark at the place that does it;
  forbidding it now and relaxing later breaks nothing, the reverse breaks
  programs.
- **`match` moving its scrutinee whole**, as MIR building copies a named
  scrutinee into a temporary today — every `match` on a state would consume
  it, even one with no binding.
- **A struct rebuilt by assigning each of its fields** — more permissive, but
  the check would track which fields have been written since the whole was
  moved; Rust rejects it too.
- **`&raw` of a moved place allowed, as it reads nothing** — a write through
  that pointer would make the place valid again where the analysis cannot see
  it.
- **Non-copy bindings in nested or-patterns, through a hidden local** — each
  alternative of the or-pattern would record which one passed, and a switch
  on it at the end would move from the right position. Linear and complete,
  but more code in MIR building and in the validator for a rare pattern;
  relaxing the error later breaks no program.
- **Non-copy bindings in nested or-patterns, with the positions after the
  or-pattern generated per alternative** — simple, but exponential in a
  sequence of such or-patterns, which ADR 0042 exists to avoid.
- **Every use after a move reported** — repeats one fault at each later use.
- **`a;` moving `a`, as in Rust** — an explicit way to discard a value, but
  M5.3 would have to forbid it for linear values, which must be consumed by
  something that does.
- **`a;` an error, doing nothing** — one more code for a statement that is
  harmless.
- **One code per kind of place for `ty::move-out-of-place`** — the message
  already names the place; more codes for the catalog of ADR 0069 with no
  difference in what the program must change.
