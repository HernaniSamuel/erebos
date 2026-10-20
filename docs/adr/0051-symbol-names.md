# 0051. Symbol names

- Status: Accepted
- Date: 2026-10-20
- Amends: 0026 (which functions C can call by their name), 0030 (the
  symbols of statics)

## Context

Every function and static is emitted with its own name as its symbol
(ADR 0006, 0026, 0030), so C code can call an Erebos function or
declare `extern uint32_t TICKS;`. With modules (ADR 0049), two modules
may each have an `fn init`, and one symbol cannot name both. The
roadmap left the choice to M2.6: names unique across the program, or
mangled, with an attribute to export them.

Some symbols must keep their exact names whatever module defines them:
LLVM calls `memcpy` and `__udivdi3` by name, and these move into `core`
(ADR 0052); interrupt handlers (M4) will be named by vector tables and
vendor startup code.

Rust mangles every name into an encoded form (`_RNv...`) and
`#[no_mangle]` keeps a name; Zig names symbols by their full path with
dots (`uart.init`) and `export` keeps a name.

## Decision

- The symbol of a **function with a body or a static** (`static`,
  `static mut`) is its **path joined by dots**: the names of the
  modules from the root of its tree down to it, then its own name.
  `fn init` in `uart` is `uart.init`; `static BASE` in `uart::regs`
  is `uart.regs.BASE`. The program's root module contributes no name,
  so **items of the root module keep their own names**, and every
  program of one file keeps its symbols. The root of `core`
  contributes `core` (ADR 0052): `core.f`, `core.builtins.f`.
- **Reserved prefixes.** LLVM gives names starting with `llvm.` a
  meaning of their own (intrinsics, `llvm.used`), so a module of the
  root named `llvm` would turn valid items into them: `llvm` is a
  reserved module name in the root module (`resolve::reserved-name`).
  `_start` stays a reserved name in every module (ADR 0012), as names
  starting with `__` are (ADR 0052): a rule on names, simpler to read
  than one on symbols, and it keeps `#[export] fn _start` from
  replacing the startup code.
- **`#[export]`** on a function with a body or on a static gives it its
  **own name** as its symbol, whatever its module:

  ```
  #[export]
  pub fn uart_rx_ready() -> bool { ... }    // symbol `uart_rx_ready`
  ```

  It takes no argument (`resolve::attribute-argument`). On an item of
  the root module it changes nothing, and is allowed, so that an item
  can move between modules without changing its symbol. On any other
  item (`extern fn`, `extern static`, `struct`, `enum`, `const`, `mod`,
  `use`, `comptime`) it is `resolve::misplaced-attribute`. It is an
  attribute, not a keyword, by the rule of ADR 0022: it changes how the
  item is placed in the object file, its symbol, not what the item is;
  `extern` (ADR 0026) is a keyword because it decides whether the item
  has a body.
- An **`extern fn` or `extern static` is always its own name**: it
  names a symbol defined outside the program.
- The entry and panic functions follow the same rule; `_start` jumps to
  the entry function's symbol, whatever it is.
- A dot occurs in no Erebos or C identifier, so a path symbol never
  equals a symbol that is a plain name, and two path symbols are equal
  only if their items are the same.
- **Every symbol of the program is unique.** Two items with one symbol,
  in any modules, `core` included, are `resolve::duplicate-symbol`,
  pointing at both: a root `fn putc` and `#[export] fn putc` in `uart`,
  or one `extern fn c_checksum` declared in two modules. The fix for the
  latter is to declare it once and `use` it; the compiler does not check
  that two declarations of one C function agree. Two items with one name
  in one module are `resolve::duplicate-item` (ADR 0049) and are not
  reported again.
- Symbols are not visibility (ADR 0050): every function and static,
  private or not, is still emitted and still an external symbol of the
  object file, as ADR 0030 has it.
- Byte strings and constant data keep their private names; bodies run
  only at compile time are not emitted (unchanged).

### Diagnostic codes

| Code | Meaning |
|------|---------|
| `resolve::duplicate-symbol` | two items of the program with one symbol |

### Conformance tests

- **LLVM IR snapshots:** functions and statics of nested modules named
  by their paths, root items by their names, `#[export]` items by their
  names, `extern` declarations by their names, and a call between
  modules.
- **ui snapshots:** `resolve::duplicate-symbol` for a root item and an
  exported one, two exported ones, two `extern` declarations in two
  modules, and a program item against a `core` symbol (ADR 0052);
  `#[export]` on every item it does not apply to, and with an argument;
  `mod llvm` in the root (rejected) and in a submodule (accepted);
  `fn _start` in a submodule, with and without `#[export]`.
- **ELF test:** the symbol table of a program with modules holds the
  path symbols and the exported ones.
- **End to end:** a C function calls an `#[export]` function and reads
  an `#[export]` static of a submodule.

## Consequences

- Modules may reuse names freely; symbols say where an item comes from,
  readable in `objdump`, `nm` and gdb (`break 'uart.init'`).
- C sees the root module's items and the exported ones; to call any
  other from C, mark it `#[export]`.
- Programs of one file keep every symbol they have today.
- C cannot name a symbol with a dot at all, short of an `asm` label on
  its declaration; those are the items C was not meant to name. Linker
  scripts accept dots in symbol names.
- Removing unused functions, or giving private items internal linkage,
  stays a separate decision (ADR 0030).

## Alternatives considered

- **Names unique across the program** — no mangling, but modules would
  stop being namespaces exactly where it matters: two drivers with an
  `init`.
- **An encoded mangling, as Rust's** — unambiguous for generics, but
  unreadable without a demangler, and nothing in M2.6 needs more than a
  path. Generics (M6) may revisit this.
- **`::` or `__` as the separator** — `uart::init` needs quoting in
  assembly and confuses tools that parse C++ names; `uart__init` is a
  valid C identifier and could collide with a plain name.
- **An `export fn` keyword, as Zig's** — reads well, but the symbol is
  placement, which attributes express (ADR 0022), and statics would
  need the keyword too.
- **Path symbols for root items too** — uniform, but every function C
  calls today would need `#[export]`, for no gain in a module whose
  names are already unique.
