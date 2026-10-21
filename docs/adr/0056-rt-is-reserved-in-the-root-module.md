# 0056. `rt` is reserved in the root module

- Status: Accepted
- Date: 2026-10-21
- Amends: 0052

## Context

ADR 0052 names the files of `core` from the repository's root:
`rt/core.ers` and `rt/builtins.ers`, in diagnostics and in panic
locations (ADR 0053). Program files are named from the root file's
directory (ADR 0049). A program with `mod rt;` whose `rt.ers` declares
`mod builtins;` reads its own `rt/builtins.ers`, and that file then has
the same name as a file of `core`. Its diagnostics and panic locations
read like `core`'s, which the reference says only a compiler bug
reports, and a panic location no longer tells which file failed.

Only the root module's modules can collide: every other program path has
a directory before the module's own name, and the root file's name has no
`/`.

## Decision

`rt` is a reserved module name in the root module
(`resolve::reserved-name`), as `llvm` is (ADR 0051): `mod rt;` and
`mod rt { ... }` there are rejected. A `use` may still bring in a name
`rt`, which reads no file, and modules named `rt` elsewhere stay allowed
(`drivers::rt`), since their files are below another directory. The
help says that `rt/` holds the files of `core`.

## Consequences

- No program file can be named like a file of `core`, so a diagnostic or
  panic location in `rt/` is always `core`'s.
- Programs lose one name for a top-level module. `rt` is a common
  abbreviation (runtime, real time), so the help must say why.
- The rule depends on the names ADR 0052 chose. If `core`'s files move,
  the reserved name moves with them.

## Alternatives considered

- **Mark `core`'s file names** with a form no program path can take,
  such as `<core>/builtins.ers`: no name is taken from programs, but
  diagnostics and panic locations would stop naming the files as they
  are in the repository, which is where a compiler developer looks for
  them.
- **Leave it**: the collision needs a program to choose `rt` for a
  top-level module with a child named like a file of `core`, but when it
  happens, panic locations are wrong without any error.

## Conformance tests

- **ui (resolve):** `resolve_symbol_errors.ers`: `mod rt { }` in the root
  module is `resolve::reserved-name`; a module `rt` inside another
  module, and a `use` taking the name `rt` there, are accepted.
  `resolve_use.ers`: `use chain as rt;` in the root module is accepted.
