# 0100. Moves as built in M5.2

- Status: Accepted
- Date: 2026-11-10
- Amends: 0097 ("Places" and "Places a value is moved out of": the elements
  of a temporary's array do not move; "Uses of a moved place": one error per
  fault holds for uses that do not move), 0099 ("The analysis of moves": only
  variables and parameters are followed)

## Context

Building M5.2 (`.claude/plans/M5.2.md`, steps 3 to 5) found three places where
ADR 0097 and 0099 say something the compiler does not do, and the project
owner accepted the compiler's behaviour each time. The code, the tests
(`tests/ui/move_out_of_place.ers`, `tests/ui/use_after_move*.ers`) and
`docs/reference.md` already agree with it; the ADRs do not. This ADR records
three clarifications of what M5.2 built, one for each place, which is why it
holds more than one decision.

- ADR 0097 says that "a field or element of a temporary (`f().x`) is a field
  of a value, not of a place", so that its parts would move like the fields of
  a local. A field does. An element does not: MIR moves only out of a local
  followed by `Field` and `Payload` projections (ADR 0098, rule M3, and "type
  checking has rejected a move out of any place but a local followed by
  fields"), and a `move` through `Index` fails the validator. Type checking
  accepted `make().items[i]` for a while; the step that added M3 found it.
- ADR 0099 makes every `move` operand a move site and every use and write of a
  place an event. MIR building writes a literal, a variant, or the tags of
  `[.None; N]` into a temporary by parts, and reuses that local at each
  iteration of a loop. Applied to temporaries, the rules reject valid
  programs: in `loop { consume(Pair { a: make(), b: make() }); }`, the write
  of `_t.0` at the second iteration is "a write to a part of a place moved
  whole" by the site of the earlier `move _t`; and `[.None; 4]` in a loop
  reaches `move _t` with its own site through the exit of the fill loop,
  whose body the flow cannot know ran.
- ADR 0097 says a second use after the same move is not reported again, and
  ADR 0099 that killing at the use "is what makes one fault one error". Both
  were written with uses that read in mind. A use that is itself a `move`
  generates a site of its own after it kills the sites it is related to, so it
  is a fault for the next use.

## Decision

### Elements of a temporary's array

The fields of a temporary stay movable (`make().h`), at any depth of fields.
No element of an array moves, whatever the array is rooted in: a local, a
parameter, a static, a pointer or a temporary. `make().items[i]`, with a
non-copy element, is `ty::move-out-of-place` (message "an array element"), as
`a.items[i]` is. A copy field read through an element (`make().items[1].n`) is
read as before.

ADR 0097, "Places", is amended. Its sentence

> A field or element of a temporary (`f().x`) is a field of a value, not of a
> place.

becomes

> A field of a temporary (`f().x`) is a field of a value, not of a place, and
> may be moved out of. An element of an array is never moved out of, a
> temporary's included (`f().items[i]`).

and the bullet of "Places a value is moved out of"

> An element of an array, by `a[i]` whatever `i`: no.

becomes

> An element of an array, by `a[i]` whatever `i` and whatever the array is
> rooted in: no, the elements of a temporary's array included
> (`f().items[i]`).

The sentence that lists the roots ("whose root is a local, a parameter or a
temporary") is unchanged: with the bullet, it says that a temporary's path
goes only through fields.

### The analysis follows variables and parameters

The analysis of moves follows only the locals that have a source name:
variables and parameters. A `move` of any other local, a temporary of MIR
building, is no site, and no use or write of one is an event. No source text
names a temporary, so no error of the program is lost: a temporary is only an
intermediate, and `_t = move a; call(move _t)` still has its site on `a`,
which the later use of `a` reaches as before. The validator's rules M1 to
M4 (ADR 0098) still apply to temporaries; only the flow analysis skips them.
ADR 0098 says that M1 and M2 make every read of a non-copy value visible as a
`move`, "which the analysis of ADR 0099 relies on": that now holds for
variables and parameters only.

The exclusion of temporaries belongs to the analysis of moves. It does not
constrain the other analyses that `analysis::check_body` will run: the
linearity analysis of M5.3, which checks every path that returns, will have to
see temporaries, such as the result of a discarded call (`make_linear();`).

ADR 0099, "The analysis of moves", is amended. Its sentences

> A **move path** is a local followed by `Field` and `Payload` projections:
> the places a `move` can name (ADR 0098, M3). Each `move` operand of the
> body is a **move site**, with its path and span.

become

> A **move path** is a variable or a parameter followed by `Field` and
> `Payload` projections: the places a `move` can name (ADR 0098, M3). Each
> `move` operand of the body whose local is a variable or a parameter is a
> **move site**, with its path and span. A local with no source name, a
> temporary of MIR building, is not followed: a `move` of one is no site, and
> no use or write of one is checked.

and the last bullet

> Locals bound to read-only data, statics and places through pointers are not
> move paths and are never moved (ADR 0098, M3), so the analysis ignores them.

is followed by

> Temporaries are ignored too. MIR building writes a literal, a variant or
> the tags of `[.None; N]` into a temporary by parts and reuses the local at
> each iteration of a loop, so the rules above, applied to it, reject programs
> that move nothing twice, such as `loop { consume(Pair { a: make(), b: make() }); }`.

The rest of ADR 0098 stands. Its rule that moves appear in the order the
program evaluates them still matters for the variables, which are where the
analysis looks.

### One error per fault, for uses that do not move

ADR 0097, "Uses of a moved place", is amended. Its sentence

> One error per fault: once the use is reported, the place counts as valid on
> the paths through it, so a second use after the same move is not reported
> again.

becomes

> One error per fault: once the use is reported, the place counts as valid on
> the paths through it, so a second use that does not move, after the same
> move, is not reported again. A use that is itself a `move` makes its own
> move site (ADR 0099), and is a fault for the uses after it:
> `consume(a);` three times in a row, the first one being the move, gives two
> errors, at the second and at the third.

ADR 0099, "The analysis of moves". Its sentence

> Killing at the use is what makes one fault one error: after an error on
> `a.f` where `a` was moved, the site moving `a` is killed on that path, so a
> later use of `a.g` is not reported either.

is followed by

> This holds for the uses that do not move. A use that is a `move` kills the
> sites related to its place and then generates its own, so `consume(a);`
> three times in a row, the first one being the move, gives two errors, at the
> second and at the third: the second use is reached by the first move and the
> third by the second move only, each labelled with the move that reaches it.

## Consequences

- The three ADRs say what the compiler does. `docs/reference.md` and the
  existing tests need no change; the reference already states the three
  rulings.
- An element of an array of non-copy values cannot be taken out of a
  function's result, as it cannot be out of a variable: such an array is
  moved whole only (ADR 0097).
- The analysis of moves never checks a temporary for a use after a move. The
  cost is nil for the programs written in the language, which cannot name
  one.
- Tests owed, to be added before acceptance, in a separate commit
  (`.claude/plans/M5.2.md`, "Left open"): `consume(a);` several times in a
  row in `tests/ui/use_after_move_once.ers`, which uses reads on purpose
  today, and `arr()[i]` and `match make().items[i]` with a non-copy binding.
- On acceptance, the status lines of 0097 and 0099 become `Accepted; amended
  by 0100 (...)`, and the index gains the amendments.

## Alternatives considered

- **Relax rule M3 so that a `move` through `Index` of a temporary's array is
  valid** — for affine values it would be sound: the other elements stay in
  a temporary that nothing uses again, and the analysis does not follow
  temporaries (above). It would not be for the linear values of M5.3, whose
  other elements would never be consumed, so M5.3 would have to forbid it for
  them. M3 would also gain an exception that depends on whether the local is
  a temporary, which the validator does not otherwise ask, and MIR building
  would have to emit such a `move` for an element chosen by a run-time index.
  That buys a rare program (`make().items[i]` with a non-copy element), and
  the same element of an array in a variable stays unmovable, so the
  exception would be one more difference between a value and a place.
  Relaxing the rule later breaks no program, so it is not worth doing now.
- **Follow temporaries, with a rule for the parts MIR building writes** — the
  analysis would have to know that a write of a literal's parts starts a new
  value, and that the tags of `[.None; N]` need no earlier write; that is
  MIR building's knowledge, and it would have to say so in MIR (a marker per
  temporary at its construction), for no error that a program can have.
- **Make a use that moves generate no site after a fault, so that the
  later uses are not reported** — it would give one error for any number of
  moves after the first. But each `consume(a)` after the first moves a value
  that is gone, and the program must change each; the transfer function would
  also differ for a use that faulted, where today it is the same for every
  event (ADR 0099), and the fixed point would depend on where the errors
  are.
