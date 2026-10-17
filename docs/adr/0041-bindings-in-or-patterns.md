# 0041. Bindings in or-patterns

- Status: Accepted
- Date: 2026-10-17
- Amends: 0037 (`let` may be an alternative of an or-pattern, or appear
  inside one)

## Context

ADR 0037 forbids `let` as an alternative of an or-pattern
(`resolve::binding-in-or-pattern`), since without payloads every
alternative matches a value of one type and there is nothing for an
alternative to bind but the scrutinee itself. With payloads (ADR 0039,
0040), different variants often carry a value that the arm handles alike:

```
match m {
    Msg::Led(let on: bool) => set(on),
    Msg::Buzz(let on: bool) => set(on),
    ...
}
```

Rust allows such alternatives to share an arm when every alternative binds
the same names with the same types. Erebos writes the type of every
`let` (ADR 0008), so an alternative cannot take a variable's type from
another one.

## Decision

### Every alternative binds the same variables

An or-pattern may contain `let` bindings, as an alternative or inside one
at any depth, when **every alternative binds the same names, each with the
same type and the same mutability**:

```
enum Msg: u8 {
    Led(bool) = 1,
    Buzz(bool) = 2,
    Write(u8, u16) = 3,
    Clear(u8) = 4,
}

match m {
    Msg::Led(let on: bool) | Msg::Buzz(let on: bool) => set(on),
    Msg::Write(let r: u8, 0) | Msg::Clear(let r: u8) => clear(r),
    _ => {}
}
```

- Each alternative writes its own `let` with its type (ADR 0008).
- Together they declare **one variable** per name, in the arm's
  expression, as a `let` pattern does (ADR 0037): it holds a copy of the
  value at the position where the alternative that matched binds it.
- A name that some alternatives bind and others do not, or that some bind
  with `let mut` and others with `let`, is
  `resolve::inconsistent-binding`, reported once per name and or-pattern,
  pointing at the first alternative that binds it and the first that does
  not, or at the first `let` and the first `let` whose mutability
  differs from it.
- The first alternative's `let` gives the variable its type. Each later
  `let` of that name with another type is `ty::mismatched-types` at its
  type, whose help says that one variable has one type: in
  `A(let x: u8) | B(let x: u16) | C(let x: u16)`, both `u16`s.
- The rule applies at every or-pattern, at any depth:
  `Msg::Write(let r: u8, 0 | 1)` is fine, since the inner alternatives
  bind nothing, and `Msg::Write(let r: u8 | 3, _)` is
  `resolve::inconsistent-binding`.
- A name that an inner or-pattern binds inconsistently is reported
  there, and counts as bound by the alternative that contains it, so the
  outer or-pattern reports nothing more about it:
  `A(let x: u8 | 3) | B(let x: u8)` is one error, at the inner one.
- One name bound twice within one alternative is still
  `resolve::duplicate-binding` (ADR 0040).

`resolve::binding-in-or-pattern` disappears. A `let` alternative at the
top of an or-pattern now follows these rules: in `let x: u8 | let x: u8`
the second alternative is dead (`ty::unreachable-pattern`), and in
`let x: u8 | 3` the `3` binds no `x` (`resolve::inconsistent-binding`).
What a catch-all arm is does not change (ADR 0037).

### Code generation

Each variable is one local. Where the `match` finds that an alternative
matches, it copies the values that alternative binds into those locals,
then runs the arm's expression, which is generated once.

### Diagnostic codes

| Code | Meaning |
|------|---------|
| `resolve::inconsistent-binding` | a name not bound by every alternative of an or-pattern, or bound with `let mut` by only some |

`resolve::binding-in-or-pattern` (ADR 0037) is removed.

### Conformance tests

- **ui (resolve):** a name bound by one alternative only, in both orders;
  `let mut` in one alternative and `let` in the other; `let x: u8 | 3`;
  `Msg::Write(let r: u8 | 3, _)`; `A(let x: u8 | 3) | B(let x: u8)`
  (one error); `A(let x: u8) | B(let x: u16) | C(let x: u16)` (both
  `u16`s); the arm's expression sees one variable
  when it shadows a constant.
- **ui (typeck):** one name bound as `u8` and `u16`
  (`ty::mismatched-types`); `let x: u8 | let x: u8` (the second
  alternative is dead); accepted: the examples above, and `let mut` in
  every alternative, assigned in the arm.
- **MIR snapshot:** two alternatives binding one local from different
  payload positions, and the arm's expression generated once.
- **Differential test** (ADR 0027): or-patterns binding values from
  different variants and positions, at compile time and at run time.

## Consequences

- Variants handled alike share an arm, as variants without payloads
  already do.
- Each alternative repeats the binding's type, which keeps every `let`
  readable on its own, at the cost of some repetition.
- The arm's expression is generated once, whichever alternative matched.

## Alternatives considered

- **Keep `resolve::binding-in-or-pattern`** — simplest, but variants
  carrying the same kind of value would need one arm each, with the same
  expression.
- **Writing the type once**, in the first alternative — shorter, but the
  other `let`s would take a type from elsewhere, which no `let` does
  (ADR 0008).
- **Allowing names that only some alternatives bind**, unavailable in the
  arm — accepts code whose bindings are useless, which is a mistake more
  often than not.
