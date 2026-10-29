# 0069. Programs with an injected error are rejected

- Status: Accepted
- Date: 2026-10-29

## Context

The tests that show the compiler rejecting a program are written one by
one: a `tests/ui` program with the error and a snapshot of its
diagnostic. A rule the compiler stops checking is noticed only if
someone wrote the program for it, and a check that works in the program
written for it can still be skipped in another place (inside a `match`
arm, a nested block, a function of another module). The compiler
accepting a program it should reject is the failure the existing tests
are worst at: `tests/fuzz.rs` accepts any outcome but a panic.

`docs/reference.md` lists the checks of the language with the code of
the diagnostic each reports, so the list of what must be rejected
already exists, as text.

## Decision

`tests/rejected.rs` takes valid programs, inserts into them a snippet
that breaks one rule, and demands that the compiler reports the code of
that rule.

### 1. The catalog

A table in the test lists snippets. Each has:

- the **code** of the diagnostic it must produce (`ty::division-by-zero`),
- the **statements** of the snippet, which use only literals, items
  that belong to every program (`core`) and names of their own, so that
  they are wrong wherever they are placed,
- the **layer** whose output is needed to see the error, which is the
  stage the program is compiled to.

A snippet that needs a declaration (an item, an `enum`) carries it and
is inserted at the level of items instead of statements.

### 2. The injection

Each program of `tests/ui` and `tests/e2e` that compiles without a
diagnostic, up to the stage the snippet needs, is taken as a base. For
each snippet, from a seeded generator (the one of ADR 0068), a place is
chosen: a statement boundary inside a function body, or the end of the
file for an item. The snippet is inserted and the program compiled. The
diagnostics must contain the snippet's code. A program that compiles
without it, or that reports other diagnostics but not that one, fails.
Diagnostics other than the code do not fail the test: the base program
is valid, and a snippet in a function that already ended in `return`
may add an unreachable-code report.

The number of programs and snippets per run is fixed from a seed, as in
ADR 0068, with the same variables to run more.

### 3. The catalog is complete or says why not

A test reads the codes of the check tables in `docs/reference.md` and
demands that each is in the catalog, or in a list in the test of codes
exempted with the reason (a code that only appears together with
another, or that needs a whole program of its own, such as a
`link::region-overflow`). A code with neither fails, so a check added
to the language cannot go without a rule here.

A failing injection is fixed like a generated program (ADR 0068): by a
`tests/ui` program of its own.

## Consequences

- A check that holds in the program written for it and fails inside a
  `match` arm, a loop or another function shows up without anyone
  writing that program.
- Each new diagnostic of the language needs a snippet, which is also
  the shortest example of what it rejects.
- It finds a rule that is not checked at all, and a rule that is checked
  in some positions; it does not find a program that is wrong in a way
  no snippet describes. The generated programs of ADR 0068 and the
  mutation of `tests/fuzz.rs` look elsewhere.
- A snippet that is wrong where inserted but whose error the compiler
  hides behind an earlier one (the first error of a function stops its
  checking) is a false alarm; the fix is to place the snippet first in
  its function, which the catalog may ask for.

## Alternatives considered

- **Random edits to valid programs, and a check that "something is
  reported"** — an edit may give a valid program, so there is no answer
  to compare to. A snippet is wrong by construction.
- **Check only that the compiler does not accept** — fails to notice a
  rule that is rejected for the wrong reason (a parse error that hides
  a missing type check). The code is the oracle.
- **A diagnostic test per rule and position, written by hand** — what
  exists, and what this adds to; it does not scale with the places a
  rule can be violated.
