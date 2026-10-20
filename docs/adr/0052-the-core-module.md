# 0052. The `core` module

- Status: Accepted
- Date: 2026-10-20
- Amends: 0020 (the runtime support functions move into `core`; the
  check against defining one becomes a check of symbols)

## Context

The runtime support functions (`memcpy`, `memmove`, `memset`, 64-bit
division) are in `rt/builtins.ers`, compiled with every program in the
program's own namespace (ADR 0020). ADR 0020 says that with modules this
file becomes part of `core`. The panic function receives a
`core::PanicInfo` (roadmap, ADR 0053), so `core` must exist in M2.6.
ADR 0031 adds that the built-in functions may move into `core`, and
ADR 0045 that `Option` and `Result` become comptime functions in `core`
in M6.

## Decision

- `core` is a **module tree written in Erebos and shipped inside the
  compiler**: its root is `rt/core.ers`, and its file modules are found
  as the root's are (ADR 0049), so `mod builtins;` there is
  `rt/builtins.ers`. Its files are embedded in the compiler binary and
  compiled and checked with every program, as `rt/builtins.ers` is
  today (ADR 0020).
- A path in **any module of the program** may start with `core`:
  `core::PanicInfo`, `use core::PanicReason;`. `core` is a reserved
  name: no item, module or `use` of the program may take it
  (`resolve::reserved-name`). Inside `core`, `crate` is `core`'s root;
  `core` cannot name the program.
- In M2.6, `core` holds:
  - `PanicInfo` and `PanicReason` (ADR 0053), `pub` in its root;
  - the runtime support functions, in the private module
    `core::builtins`, each `#[export]` so that LLVM finds it by its name
    (ADR 0051). `memcpy`, `memmove` and `memset` are `pub` and
    re-exported from the root, so a program calls `core::memcpy`. The
    64-bit division functions are private: LLVM calls them, programs
    write `/` and `%`. Helpers such as `__erebos_udivmod64` are private
    and not exported (`core.builtins.__erebos_udivmod64`).
- **Not moved:** `Option`, `Result`, `assert`, `len` and `slice` stay
  built-in names in every module until M6. Moving them would need a
  prelude or a `core::` at every use, and M6 turns `Option` and
  `Result` into functions anyway (ADR 0045).
- **Names the runtime needs.** An item of the program whose name starts
  with `__` is still `resolve::reserved-name` (ADR 0020), keeping the
  names LLVM may call free. An item whose symbol equals one of `core`,
  such as a root `fn memcpy` or an `#[export] fn memset` anywhere, is
  `resolve::duplicate-symbol` (ADR 0051), with a help naming the `core`
  function to call. This replaces the `resolve::reserved-name` error of
  ADR 0020 for a program defining a runtime function.
- `core` has no entry or panic function. The compiler knows two of its
  items by path, `core::PanicInfo` and `core::PanicReason`.
- `--emit` dumps leave `core` out, as they leave out the runtime today.
  Its files are named `rt/core.ers` and `rt/builtins.ers` in
  diagnostics and panic locations; an error in `core` is a compiler
  bug.

### Conformance tests

- **ui snapshots:** `core` as the name of an item, a module and a
  `use`; a root `fn memcpy` and an `#[export] fn memset` in a module,
  each `resolve::duplicate-symbol` with the help; a path to a private
  item of `core` (`core::builtins::__udivdi3`),
  `resolve::private-item`; `use core::PanicInfo;`.
- **End to end:** `memory_functions.ers` calls `core::memcpy`,
  `core::memmove` and `core::memset`; 64-bit division keeps working
  through the exported division functions (`division64.ers`).
- **ELF test:** the runtime functions keep their C names.

## Consequences

- The runtime support functions are an ordinary Erebos module: they
  get the rules of every other module, and ADR 0020's promise is kept.
- `core` gives future library code (deferred formatting, M3's
  peripherals support) a place, and M6 a home for `Option` and
  `Result`.
- Programs that called `memcpy` directly write `core::memcpy`.
- Programs still pay for the runtime functions they do not call, until
  unused sections are removed (ADR 0020).

## Alternatives considered

- **`core` with only `PanicInfo` and `PanicReason`, the runtime left in
  the program's namespace** — less to change now, but two mechanisms
  for compiler-shipped code, and ADR 0020's reserved-name rule stays a
  special case.
- **A prelude, bringing `core` items into every module** — saves
  `core::`, but hides where names come from, the reason glob imports
  are left out (ADR 0049).
- **Moving `assert`, `len`, `slice`, `Option` and `Result` into `core`
  now** — every use would need `core::` or a prelude, and M6 changes
  them again.
