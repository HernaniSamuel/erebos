# 0037. `match` and patterns

- Status: Accepted; amended by 0038 (which `match`es on an enum need its values)
- Date: 2026-10-16
- Amends: 0032 (the arms a `match` never takes)

## Context

With enums (ADR 0035), programs need to branch on a value among many:
the state of a state machine, a command byte, a register field. Chains of
`if x == A { } else if x == B { }` work, but say nothing about whether
every case is handled, and the compiler cannot check it.

The roadmap fixed two points for M2.4: every `match` is exhaustive, and on
integers that means an explicit `_` arm. ADR 0029 flags the classic
ambiguity of `match x { MAX => ... }`, where a name may be a constant to
compare with or a new variable, and leaves it to this step. In Rust it is
a new variable unless a constant of that name is in scope, so a misspelled
constant silently matches everything; Rust catches it with warnings, which
Erebos does not have.

This ADR covers `match` on integers, `bool` and enums without payloads
(M2.4a). Payloads, patterns nested in them and bindings inside or-patterns
come in M2.4b.

## Decision

### Syntax

```
match state {
    State::Idle => start(),
    State::Run | State::Wait => {
        step();
    }
    State::Done => return,
}

let reply: u8 = match cmd {
    CMD_RESET => reset(),
    0x10 | 0x11 => read(cmd),
    b'q' => 0,
    let other: u8 => unknown(other),
};
```

- `match scrutinee { arms }`, where an arm is `pattern => expression`.
  Arms are separated by commas, with an optional trailing comma.
- **An arm whose expression starts with a block-like expression** (`{ }`,
  `if`, `match`, `while`, `loop`) **ends with it**, as a statement does
  today, and needs no comma. So in `0 => {}` followed by `-1 => {}`, the
  `-1` starts the next arm; it is not a subtraction. To continue such an
  expression, put it in parentheses or braces.
- As in the condition of `if`, a `{` after the scrutinee opens the arms,
  so a struct literal there needs parentheses.
- `match` is an expression (ADR 0015) and block-like: as a statement
  without `;` it must have type `()`.
- `match` becomes a keyword, and `_` becomes a token of its own: the
  wildcard pattern, and nothing else. No name can be `_`: not a
  variable, parameter, item, field or variant (`let _: u32`,
  `const _: u32` and `struct S { _: u8 }` are errors); no existing
  program does this.

### What can be matched

The scrutinee is an integer, a `bool` or an enum (ADR 0035). Any other
type is `ty::match-type`: pointers and slices (their values are addresses
not known at compile time), aggregates (enums with payloads come in
M2.4b), `()` and `!`. The scrutinee has no expected type, so a literal
scrutinee (`match 3 { ... }`) is `ty::untyped-literal`, as anywhere else.

### Patterns

| Pattern | Matches |
|---------|---------|
| `_` | every value |
| `let name: T`, `let mut name: T` | every value, binding it to a new variable |
| an integer literal, `-` and an integer literal | that value |
| a byte literal (`b'q'`) | that byte, against a `u8` scrutinee |
| `true`, `false` | that value |
| `Mode::Run` | that variant |
| `NAME` | the value of the constant `NAME` |
| `p \| q \| ...` | any of the alternatives |

- **Integer literals** take the scrutinee's type, as against any expected
  type (ADR 0008): `300` against a `u8` is `ty::literal-out-of-range`, and
  `-1` against an unsigned type is `ty::negate-unsigned`, as in
  `let x: u8 = -1;`. A **byte literal** is a `u8` (ADR 0016) and matches
  only a `u8` scrutinee. Any literal against a type it does not have, such
  as `b'q'` against a `u32`, `0` against a `bool` or an enum, or `true`
  against an integer, is `ty::mismatched-types`.
- **A bare name is always a constant.** It must name a `const` item
  (ADR 0029) of the scrutinee's type (`ty::mismatched-types` otherwise),
  and matches its value, computed at compile time, whatever that value is.
  A name that names nothing is `resolve::unknown-name`; one that names a
  local variable, a parameter or an item that is not a constant (a
  static, function, struct or enum) is `resolve::not-a-constant`. A local
  variable shadows a constant of the same name here as anywhere
  (ADR 0029), so it is then `resolve::not-a-constant`, whose help
  suggests `let x: T => if x == NAME { ... }`.
- **A new variable is always written with `let` and its type**, as every
  `let` is (ADR 0008): `let n: u8` matches every value and binds a copy
  of it in the arm's expression; `let mut n: u8` makes it mutable. The
  type must be the scrutinee's, with no conversion
  (`ty::mismatched-types`); without it, the pattern is
  `parser::let-without-type`. The variable shadows any item or variable
  of the same name in that arm only.
- So neither reading depends on what else is in scope: a misspelled
  constant is an error, never a catch-all, and a new variable is visible
  where it is introduced, with its type. This settles the question
  ADR 0029 left open.
- **Or-patterns** list alternatives of any pattern except `let`
  (`resolve::binding-in-or-pattern`): which value an alternative binds
  needs payloads to mean anything, and M2.4b decides it.
- A **catch-all arm** is one whose pattern is `_`, a `let`, or an
  or-pattern with a `_` alternative.

There are no range patterns (`b'0'..=b'9'`), guards (`pat if cond`) or
nested patterns in this step.

### Choosing an arm

- The scrutinee runs **once, before any arm**, and is read where it is
  written (ADR 0018): through a volatile pointer, `match (*uart).status`
  is exactly one volatile read.
- The arms are tried in order; the first whose pattern matches runs, and
  its value is the value of the `match`. Patterns run no code: constants
  are already values.
- A `match` on an enum without a catch-all arm calls the panic function
  for a value that no arm matches, which can only be a value that is no
  variant (ADR 0036).

### Typing

As for `if`: with an expected type, each arm's expression is checked
against it, with the implicit conversions of ADR 0024 and 0031; without
one, the first arm whose type is not `!` gives the type and the others
are checked against it. Arms of type `!` fit any type, and a `match`
whose arms all have type `!` has type `!`.

### Exhaustiveness

Every `match` is exhaustive: its arms together match every value of the
scrutinee's type, or the `match` is `ty::non-exhaustive-match`, which
lists what is missing and suggests the arms to add.

- A `bool` needs `true` and `false`, or a catch-all.
- An enum needs every variant, or a catch-all. Values that are no variant
  are not required (ADR 0036). Missing variants are listed in declaration
  order.
- An integer needs every value of its type, which in practice means a
  catch-all arm. This refines what the roadmap fixed, "an explicit `_`
  arm": a `let`
  arm is a catch-all too, and listing all 256 values of a `u8` or `i8`
  also counts, so that a `_` after them would be a dead arm, not a
  requirement.

### Dead arms

An arm that matches no value the arms before it leave unmatched is
`ty::unreachable-pattern`, reported once for the arm, pointing at it and
at the first earlier arm that covers it alone, if there is one. When only
some alternatives of an or-pattern are dead, each of those is reported.
Examples: `0` twice; `Mode::Run` after a constant whose value is
`Mode::Run`; anything after a catch-all; `_ | 3`; a `_` after `true` and
`false`.

For an enum, the values are all those of its integer type, so a `_`
after every variant is **not** dead while values that are no variant
remain (ADR 0036); only arms that already cover every value of the
integer type, through variants and constants, make it dead.

The values of constants in patterns come from evaluating them (ADR 0028);
a constant that needs, to be evaluated, the function whose `match` uses
it as a pattern is a `ty::cycle`. A constant that fails to evaluate is
reported once (ADR 0029), and the `match` that uses it is not checked for
exhaustiveness or dead arms, so nothing more is reported. The same holds
for a `match` on an enum one of whose values failed to evaluate or
duplicates another (ADR 0035).

### When the checks run

Exhaustiveness and dead arms are part of **type checking**: their errors
use `ty::`, appear with `--emit=typeck`, and a function with one of them
does not type check, so it is not lowered to MIR and ADR 0032's checks do
not run on it.

### Values known at compile time

A `match` whose scrutinee is known at compile time (ADR 0032) runs at most
one arm. **The arms it never takes are code that cannot run** and are not
checked by ADR 0032's rules, like the branch an `if` never takes (amends
ADR 0032). When no arm matches, which only an enum value that is no
variant can cause, no arm is checked, and the `match` panics where it
runs (ADR 0036). Type checking, exhaustiveness and dead arms still cover
every arm. The `match` itself is not a known expression, as `if` is not.

### Code generation

A `match` becomes a switch on the scrutinee's integer (an LLVM `switch`),
whose default is the catch-all arm, the panic call of ADR 0036 for an
enum without one, or unreachable for a `bool` or an integer whose every
value is listed. It costs what a C `switch` costs.

### Diagnostic codes

| Code | Meaning |
|------|---------|
| `resolve::not-a-constant` | a bare name in a pattern that is not a constant |
| `resolve::binding-in-or-pattern` | `let` as an alternative of an or-pattern |
| `ty::match-type` | a `match` on a type other than an integer, `bool` or enum |
| `ty::non-exhaustive-match` | a `match` whose arms leave values unmatched |
| `ty::unreachable-pattern` | an arm, or an alternative, that matches nothing new |

### Conformance tests

- **ui (parser):** arms with and without commas after blocks, a trailing
  comma, `0 => {}` followed by `-1 => {}`, a `match` as a statement, a
  struct literal as scrutinee with and without parentheses, `let` without
  a name and without a type, `_` as a variable or constant name.
- **ui (resolve):** a bare name that is a static, a function, a struct,
  an enum, a local variable shadowing a constant, and nothing at all;
  `let` in an or-pattern; a `let` binding that shadows a constant inside
  its arm only.
- **ui (typeck):** each rejected scrutinee type; `match 3`; a constant of
  another type; a `let` of another type; a literal out of range; `-1`
  against a `u8`; `b'q'` against a `u32`; a non-exhaustive `match` on a
  `bool`, an enum (listing two missing variants) and an integer; dead
  arms: a repeated literal, a variant repeated through a constant, an arm
  after `let`, `_ | 3`, a whole or-pattern arm dead (reported once), `_`
  after `true` and `false`; a cycle through a constant used as a pattern;
  accepted: `_` after every variant, all 256 `u8` values without `_`,
  `let mut`, arms of type `!`, a `match` of type `!`, arms converting
  `*mut T` to `*const T` and an array pointer to a slice.
- **ui (MIR):** ADR 0032 checks skipped in the arms a known scrutinee
  never takes, and run in the one it takes.
- **MIR and LLVM IR snapshots:** the switch with each kind of default; a
  scrutinee read once through a volatile pointer.
- **ui (eval) and differential test** (ADR 0027): `match` on integers of
  every width including `u64` and `i64`, with negative patterns such as
  `-128` on an `i8`, on `bool` and on enums, including an `i8` enum with
  a negative variant, with constants and or-patterns, at compile time and
  at run time.
- **The milestone program:** a state machine on QEMU.

## Consequences

- Programs branch on states and codes with the compiler checking that
  every case is handled, and that no arm is dead.
- Every binding costs `let` and a type, and a constant can be a pattern
  without any marker; both are visible at the use site.
- A misspelled constant and a duplicate arm are errors rather than silent
  catch-alls, without warnings.
- Matches on integers need a catch-all arm; matches on enums listing every
  variant pay the check of ADR 0036.
- Range patterns and guards are absent: ranges are written as a `let`
  arm with an `if`.
- Since a bare name is always a constant, variants are always written
  with their enum's name. `Option` and `Result` (M2.5) and `use` (M2.6)
  must keep to this, or amend this ADR to let `None` or an imported
  variant stand alone.

## Alternatives considered

- **Rust's rule: a name binds unless it names a constant** — familiar,
  but a misspelled constant silently becomes a catch-all, and Erebos has
  no warnings to catch it.
- **Names always bind, and constants are never patterns** — no ambiguity,
  but command codes declared as constants, common in firmware, could not
  be matched.
- **`let n` taking the scrutinee's type** — shorter, especially inside
  payloads (M2.4b), but every other `let` writes its type (ADR 0008).
- **`_` a wildcard in patterns only, an identifier elsewhere** — keeps
  `let _: u32` valid, but `const _` would then be unreachable from
  patterns, and `_` would mean two things.
- **Dead arms allowed**, like unreachable code after `return` — but a dead
  arm is almost always a mistake, and without warnings it would go
  unnoticed.
- **Requiring `_` on enums as well** — would force a default arm on every
  enum `match`; ADR 0036 handles values that are no variant without it.
- **Range patterns** — useful for parsers; left out of this step.
