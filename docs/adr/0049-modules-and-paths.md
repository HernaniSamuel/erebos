# 0049. Modules and paths

- Status: Accepted
- Date: 2026-10-20
- Amends: 0029 (the one namespace of items becomes one namespace per
  module), 0035 (the code for an unknown name before `::`)

## Context

A program is one file, and its items share one namespace (ADR 0029).
Programs that drive several peripherals outgrow one file, and two
drivers that each want an `init` or a `Config` cannot coexist. Until M5
there are no methods, so namespacing is all that keeps free functions
such as `uart_init` and `timer_init` apart (roadmap). Several ADRs left
their part of this to M2.6: ADR 0029 (`assert` works in any module),
ADR 0030 (the names of statics), ADR 0035 (`::` reused for paths).

Rust's module system (2018 edition) is the model most readers know:
modules declared with `mod`, either inline or in a file found by a
fixed rule, names brought in with `use`, and paths starting from
`crate`, `super` or a name in scope. Zig makes every file a struct
imported with `@import`, which needs types as values (M6).

Visibility is ADR 0050, symbol names ADR 0051 and the `core` module
ADR 0052.

## Decision

### Modules

A module is declared with `mod`, inline or in its own file:

```
mod uart;            // the items of uart.ers

mod regs {           // the items between the braces
    const BASE: usize = 0x1000_0000;
}
```

- A module is an item of the module that declares it, its **parent**.
  The file given to `erebos build` is the **root module**; the modules
  form a tree under it.
- **Files.** In the root file, `mod uart;` reads `uart.ers` in the same
  directory. In any other file, `dir/uart.ers`, `mod regs;` reads
  `dir/uart/regs.ers`: the children of a file module live in a
  directory named after it. An inline module adds its name to that
  directory: in the root file, `mod a { mod b; }` reads `a/b.ers`. There
  is no `mod.ers` and no attribute to choose another path, so each
  module has exactly one file. A file that does not exist or cannot be
  read is `resolve::module-file`, at the `mod` declaration, naming the
  path tried, relative to the root file's directory with `/` between
  directories. Every directory and file name on that path must match
  the module names exactly, also on file systems that ignore case:
  `mod Uart;` does not read `uart.ers`, nor `mod a { mod b; }` read
  `A/b.ers`. A `mod` in the root file whose file would be the root file
  itself, such as `mod main;` in `main.ers` or `mod prog;` in
  `prog.ers`, would make the root its own child; it is
  `resolve::module-file` too, saying so.
- **Order.** Files are numbered in the order their `mod` declarations
  are met, depth first, starting from the root file; `core` (ADR 0052)
  comes after every file of the program. Diagnostics, sorted by file and
  offset, and dumps follow that order, so they do not depend on how the
  compiler loads the files.
- Every kind of item may appear in any module, `comptime` blocks
  included. The program still has exactly one entry function and one
  panic function, in any module (ADR 0014).
- `mod` and `use` are items: inside a function body they are
  `parser::local-mod` and `parser::local-use`, as `const` and `static`
  are (ADR 0029, 0030).

### Namespaces

- **Each module has one namespace**, holding its items, its child
  modules and the names its `use` declarations bring in. Two of these
  with one name are `resolve::duplicate-item`, as ADR 0029 asks of one
  file today. The same name in two modules is two items.
- The **built-in names** are in every module: the types `bool`, the
  integer types, `Option` and `Result`, and the functions `assert`,
  `len` and `slice`. No item, module or `use` may take one, as today
  (`resolve::duplicate-type`, `resolve::reserved-name`). They stay in
  every module when M6 moves the definition of `Option` and `Result`
  into `core` (ADR 0045), so programs do not change then.
- A single name, as in `x` or `Config`, is looked up among the locals
  (in a body), then in the namespace of the module where it is written.
  It is **never looked up in a parent**: a child module writes
  `super::x`, or brings the name in with `use super::x;`.

### Paths

A **path** is names separated by `::`: `uart::init`, `uart::Mode::Run`.

- Its first segment is one of:
  - `crate`: the root of the module tree the path is written in;
  - `super`: the parent of the current module, and `super::super` its
    parent's parent; `super` appears only at the start of a path;
  - `core`: the root of `core` (ADR 0052);
  - any other name: looked up in the namespace of the current module.
- Each following segment is looked up in the namespace of the module
  named so far. When the path so far names an enum, the next segment is
  one of its variants and ends the path: `uart::Mode::Run`,
  `uart::Mode::Write(3, 4)`.
- Errors, saying in which module each name was looked up:
  - a **last segment** that names nothing is the error of its position,
    as today: `resolve::unknown-type`, `resolve::unknown-function`,
    `resolve::unknown-name`, `resolve::unknown-variant`;
  - a segment **followed by `::`** that names nothing is
    `resolve::unknown-path` (ADR 0035 reported `X::V` with an unknown
    `X` as `resolve::unknown-type`; with modules, `X` may have been
    meant as either);
  - a segment followed by `::` that names something other than a module
    or an enum is `resolve::not-an-enum`, as ADR 0035 has it, now
    saying "neither a module nor an enum"; `Option::None` keeps the help
    of ADR 0045;
  - a path whose last segment is a module, where a type, a value or a
    function is expected (`let x: uart`, `uart()`, `uart { }`), is
    `resolve::misused-module`;
  - `super` in the root module is `resolve::super-at-root`.
- Paths go wherever an item is named: types (`uart::Config`,
  `Option(uart::Config)`, `*const uart::regs::Block`), constants,
  statics, calls, struct literals (`uart::Config { baud: 115200 }`),
  variants and patterns (`uart::Mode::Run`), and the type operand of
  `size_of`, `align_of` and `offset_of`.
- `crate`, `super`, `mod`, `use` and `pub` become keywords. `core` is
  a reserved name (ADR 0052).

### `use`

```
use uart::putc;
use uart::regs::BASE as UART_BASE;
use crate::timer::{init, Tick, regs::CONTROL,};
pub use regs::Block;
```

- `use path;` brings the item or module the path names into the
  current module's namespace under the path's last name; `use path as
  name;` under another name. Braces after `::` list several paths from
  one prefix, each with an optional `as`, with a trailing comma
  allowed; they do not nest.
- A `use` path resolves as every path does, from the current module's
  namespace, `crate`, `super` or `core`; it may go through a name
  another `use` brought in.
- A `use` names a module or an item, never a variant
  (`resolve::use-variant`): variants are written `E::V`, or `.V` where
  the type is known (ADR 0044). A path made only of `crate`, `super` or
  `core` brings in no name and is `parser::unexpected-token`.
- There are **no glob imports** (`use uart::*`): every name a module
  uses is written in that module, or in the `use` that brings it in.
- `pub use` makes the name visible outside the module (ADR 0050).
- `use` declarations, like items, apply whatever their order. A `use`
  whose target can only be found through itself, as `use b::x;` in
  module `a` and `use a::x;` in module `b`, is `resolve::use-cycle`,
  reported once, at the first `use` of the cycle in file order, then
  offset, and naming the others.

### Names in diagnostics and dumps

An item outside the root module is shown by its path from the root,
`uart::init`, `uart::Config`, `core::PanicInfo`, in diagnostics and in
the `hir` and `typeck` dumps; items of the root module keep their own
names, so one-file programs show what they show today. The `mir` and
`llvm-ir` dumps name functions and statics by their symbols
(ADR 0051), `uart.init`.

### Diagnostic codes

| Code | Meaning |
|------|---------|
| `parser::local-mod` | `mod` inside a function body |
| `parser::local-use` | `use` inside a function body |
| `resolve::module-file` | the file of `mod name;` does not exist, cannot be read, or is the root file |
| `resolve::unknown-path` | a segment followed by `::` names nothing |
| `resolve::misused-module` | a module where a type, a value or a function is expected |
| `resolve::super-at-root` | `super` in the root module |
| `resolve::use-variant` | a `use` naming an enum variant |
| `resolve::use-cycle` | `use` declarations that can only be resolved through themselves |

`resolve::not-an-enum` (ADR 0035) now also covers a segment followed by
`::` that names neither a module nor an enum.

### Conformance tests

- **ui snapshots** for each code above; inline modules holding every
  kind of item; a path in every position listed above, through nested
  modules, `crate`, `super` and `super::super`; a name found only in the
  parent, rejected; the same name in two modules; `use` with `as`,
  braces and a trailing comma; a `use` through another `use`; items and
  `use` declarations referring to each other across modules in any
  order; an unknown segment at each position, naming its module; a
  struct, a constant and `Option` before `::`; a module used as a type,
  a value, a callee and a struct literal.
- **Multi-file programs**: a test program may be a directory whose root
  file is `main.ers`, with file modules beside it and in
  subdirectories, in `tests/ui` and `tests/e2e`; the file of a module
  declared in the root file, in another file, and inside an inline
  module; a missing file, a file whose name differs only in case, and
  `mod main;` in `main.ers`; a diagnostic with labels in two files.
- **End to end:** the milestone program (ADR 0053) is split into files.

## Consequences

- Programs can be split into files, and modules namespace their items;
  a driver's `init` no longer collides with another's.
- `mod`, `use`, `pub`, `crate` and `super` can no longer name anything.
- Without glob imports, a module using many names of another lists
  them, or writes paths.
- There is no prelude: an item of `core` is written `core::x` or
  brought in with `use`.
- The snapshot and end-to-end harnesses learn programs made of several
  files.

## Alternatives considered

- **Only file modules** — one form, but name resolution could then only
  be tested with several files, and grouping a few items needs a file.
- **`mod.ers` files, as Rust 2015** — two places for the same module.
- **Looking names up in parent modules too** — saves `super::`, but a
  name added to a parent would silently change what a child's name
  means.
- **`self::`** — the current module's namespace is already where a
  plain name is looked up.
- **Glob imports** — shorter, but where a name comes from is no longer
  written, and two globs can collide.
- **Files as structs, as Zig's `@import`** — needs types as values
  (M6), and a module would not be declared where it is used.
