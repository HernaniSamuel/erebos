# 0102. Consuming linear values, and `end`

- Status: Accepted; amended by 0113 (a function of a type may be named `end`)
- Date: 2026-11-10

## Context

ADR 0094: a value of a linear type must be moved before every point where the
function holding it returns, and is never overwritten; the check is per
function and follows control flow. ADR 0101 says which types are linear. The
roadmap leaves to M5.3 assigning over a place holding a linear value and how
the module that defines a linear type ends a value.

Moves (ADR 0097): a non-copy value is moved out of a local, a parameter, a
temporary, their fields, and payload values bound by `match`. A `match` does
not move its scrutinee; it moves only what its non-copy bindings bind. A
discarded place (`a;`) neither moves nor uses `a`, so that a statement cannot
pass for consuming a linear value. A value that goes out of scope unmoved is
valid (affine).

Two things make "consumed" more than "moved":

- A `#[linear]` struct whose fields are all copy, the usual handle
  (`struct Transfer { channel: u32 }`), cannot be taken apart by moves: its
  fields are copied, and the struct still holds its value. Something must end
  it, and only where it may be built.
- A value of `Option(Transfer)` holds a `Transfer` only when it is `.Some`.
  In the arm `.None => {}` of a `match` on it, nothing is moved, and nothing
  needs to be.

## Decision

### Linear parts

The **linear parts** of a value are the values of `#[linear]` structs in it:
the value itself if its type is one, and those its fields and payload values
hold, at any depth, through fields and the payloads of every variant. A
linear type that is not a `#[linear]` struct (`Option(Transfer)`, a struct
with a `Transfer` field) has linear parts but is none itself. A `#[linear]`
struct with a linear field has both: itself, and the field.

A linear part is **held** by a place from when a value is written into it
until it is consumed. The check is per function and follows control flow: at
a point, a linear part is held if some path from the start of the function
reaches the point holding it.

### What gives a value, and what consumes it

A value comes into a function as a parameter, as the result of a call, or
as a struct literal of a `#[linear]` type, with fields or none (`Tok {}`).
Wherever it is then moved, by a `let`, an assignment, a binding or an
argument, the place it is moved into holds it, and it keeps where it came
in. A value that is not moved anywhere stays held where it was made:
`make();` and `Tok {};` give a temporary that holds it. Writing a copy
field of a held `#[linear]` struct changes the value; it neither gives nor
forgets one.

A linear part is **consumed** at a point when one of these happens to it, or
to a place that contains it:

- it is **moved**: passed as an argument, returned, assigned or bound to
  another place, given to a literal, a `break`, the value of a block or arm,
  or `?` (ADR 0097). What it was moved into holds it now;
- it is **ended** by `end` (below);
- a test of a tag shows that the variant whose payload holds it is not the
  value's: in the arm `.None` of a `match` on an `Option(Transfer)`, the
  payload of `.Some` holds nothing, so nothing is owed there. Every test of
  the tag counts, a `match`'s arms and the tests of `?`.

A discarded place (`t;`, ADR 0097) consumes nothing, nor does reading a copy
field (`t.channel`), nor `&raw` of the place.

### The rules

- **Returns.** At every `return`, explicit or at the end of the body, and at
  every `?` that returns, no linear part may be held by a variable, a
  parameter or a temporary. The value being returned is moved to the caller,
  so it is not held. A linear part still held there is
  `analysis::linear-not-consumed`.
- **Paths that do not return** owe nothing: a call to a function returning
  `!` (`exit`, the panic function), a check that panics, an `asm` with
  `noreturn`, a loop with no exit. The function never gets back to its caller
  along them, as ADR 0094's "every point where the function holding it
  returns" says. So `entry fn main() -> !` is checked for overwrites (below)
  but never for returns; a program checks its handles in the functions it
  calls.
- **Overwrites.** Writing a place while a linear part of it, the place or a
  part of it, is held is an error, at the write. This covers
  `t = start(...)` over an unconsumed `t` (`analysis::linear-overwritten`),
  `s.h = v` over a held field, and a `let` or a discarded call in a loop
  whose value from the previous iteration was not consumed (reported as not
  consumed, "Diagnostics" below). Writing a copy field of a held
  `#[linear]` struct is no overwrite.
- A use after a value was moved or ended is `analysis::use-after-move`, as
  for any non-copy value (ADR 0097).

### `end`

`end(e)` ends the value of a `#[linear]` struct. It is a built-in, like
`len` (ADR 0031): a call by that name is always it, and no item may be named
`end`. Locals, parameters and fields may still be named `end`, as they may
be named `len`. It is an expression of type `()`.

- `e` must be of a `#[linear]` struct type, else `ty::end-type`.
- Every field of that struct must be visible where `end` is written, as for
  a struct literal (ADR 0050), else `ty::end-private-field`, labeling a field
  that is not visible. Whoever may build a value may end it; a struct with a
  private field is built and ended only by its module.
- `e` must be a place a value may be moved out of (ADR 0097, 0100), or a
  temporary (`end(start(...))`), else `ty::move-out-of-place`.
- `end` consumes the value whole. Its linear fields must have been consumed
  before, by moving them out (`let inner: Token = t.inner;` and then
  `give_back(inner)`): a linear field still held when `end` runs is
  `analysis::linear-not-consumed`, at `end`. Its fields that are only affine
  are let go, as any affine value.
- `e` may be partly moved: moving its linear fields out first is how a
  struct with them is ended. A use of `e` after `end` is
  `analysis::use-after-move`.
- `end` does nothing at run time.

A typical driver:

```
pub fn wait(t: Transfer) {
    while !done(t.channel) {}
    end(t);
}
```

### Diagnostics

One error per value, whatever the number of returns and writes it reaches
(ADR 0103 says how values are told apart):

- `analysis::linear-not-consumed`, at the point where the value came into
  the function (the call, the literal, or the function for a parameter),
  naming the place that holds it (`t`, `s.h`, a payload value of `o`, "this
  value" for a temporary) and its linear type, with a label at each return
  it reaches unconsumed. A value that reaches the statement that made it
  again, through a loop, without being consumed (`start(1);` or `let t:
  Transfer = start(1);` in a loop) is this error too, labelled at that
  statement. At `end`, for a linear field not consumed before it. The help
  says how to consume it: pass it to a function that does, return it, or
  end it with `end` where its fields are visible.
- `analysis::linear-overwritten`, at a write over a held value made
  elsewhere (`t = start(2);` over the `t` of `start(1)`), labeling where the
  value it would overwrite came in. A value reported as not consumed is not
  reported again as overwritten.
- `ty::end-type`, `ty::end-private-field` above, and the codes of ADR 0101.

## Consequences

- A transfer that is started and never waited for, a token never given
  back, a call result of a linear type that is discarded, are errors, at the
  point that made them.
- `Option(Transfer)` is used by matching on it: the `.None` arm owes
  nothing, the `.Some(let t: Transfer)` arm owes `t`. `.Some(_)` and a `_`
  arm that covers `.Some` leave the payload held, which is an error at the
  return.
- A value of `Option(Transfer)` that is known to be `.None` but was never
  tested, such as the result of a call, must still be matched or moved
  before a return: only a test of the tag tells the analysis which variant
  it is. A value written as `.None` in the function is known (ADR 0103).
- Code after a call that does not return is not checked for returns, so a
  handle dropped before `exit` is not reported. Since the entry function
  and the handlers that return `!` never return, a value they make is
  checked only for overwrites: "the check on every path that returns"
  (roadmap) applies to the functions they call. A program keeps its
  handles in functions that return, and its tests of this check are in
  such functions.
- `end` is a reserved name: an item named `end` must be renamed. Whether a
  function of a type (M5.5) may be named `end` is M5.5's, with how `impl`
  items are named.
- The new codes join the catalog of ADR 0069.

## Alternatives considered

- **Destructuring with `let`, as `let Transfer { channel: let c: u32 } = t;`**
  — gives the fields back as it ends the value, and needs no reserved name,
  but struct patterns do not exist (a `match` on a struct is `ty::match-type`,
  ADR 0037), and they would be new syntax and new MIR for what a read of the
  copy fields and `end` already do.
- **Linear only outside the defining module** — no construct at all, but the
  module that handles the value, the driver, is where it is most likely to be
  dropped by mistake.
- **`match` on a linear value consuming it**, each pattern binding every
  linear payload value — no tag tracking, but it amends "`match` does not
  move its scrutinee" (ADR 0097), forbids `_` where a linear payload is,
  and a `match` that only looks at a value would consume it.
- **No enum holding a linear value in M5.3** — the simplest analysis, but no
  optional transfer, and no `?` over a result holding a handle.
- **Paths that do not return owe their linear values too** — catches a handle
  dropped before `exit`, but every hidden check that can panic (a division,
  an index) would need every linear value consumed before it, which no
  program can do; and ADR 0094 says "returns".
- **Every linear type ended by `end`, not only `#[linear]` structs** — `end`
  on an `Option(Transfer)` would forget the transfer in it.
- **`end` on any non-copy struct** — harmless, but it would end affine
  values that need no ending, and hide that the type is not linear.
