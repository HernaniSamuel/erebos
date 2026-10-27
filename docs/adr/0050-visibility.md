# 0050. Visibility

- Status: Accepted; amended by 0054 (only `_` covers an enum that is not reachable), 0059 (`..` may leave out an invisible field that has a default or a write effect)
- Date: 2026-10-20

## Context

With modules (ADR 0049), a module has items meant for others and items
that are its own business. A driver's register helpers should not be
callable from the rest of the program, and a peripheral handle should
not be buildable by anyone but its driver: the ownership of M5 and
typestate (roadmap, M5 pending decisions) rely on values that only
their module can create.

Rust makes items and struct fields private by default, visible in their
module and its descendants, and `pub` opens them. C and Zig have no
private fields. Choosing the default for fields now matters: making
public fields private later would break every program that uses them.

## Decision

### Items

- An item is **private** unless declared `pub`. A private item is
  visible in the module that declares it and in that module's
  descendants. A `pub` item is visible wherever its module is.
- A path is usable where **each of its segments is visible**: a `pub`
  item inside a private module is reachable only where that module is.
- An item is **reachable** from a module when some path written there
  would name it and be usable, `pub use` names included: an enum of a
  private module re-exported with `pub use` is reachable wherever the
  re-export is. Uses that write no path to the item, `.V` and fields
  (below), are judged by reachability.
- `pub` comes after the attributes and before the item's keywords:
  `pub fn`, `pub extern fn`, `pub static mut`, `pub struct`,
  `pub enum`, `pub const`, `pub mod`, `pub use`, `pub panic fn`,
  `#[export] pub fn` (ADR 0051). It does not apply to `comptime` blocks,
  which have no name, nor to the entry function, which nothing may call
  (`parser::misplaced-pub`). Before fields it is allowed (below); before
  anything else, such as a variant, a parameter, a `let` or another
  `pub`, it is `parser::unexpected-token`.
- `pub use` makes the name it brings in a `pub` name of its module, so a
  module can offer items of its private children:
  `pub use regs::Block;`. The item must itself be `pub`
  (`resolve::private-reexport`): re-exporting cannot widen what an item
  declared.
- Naming an item that is not visible is `resolve::private-item`,
  pointing at the use and at the item.
- The compiler calls the entry and panic functions whatever their
  visibility.

### Fields

```
pub struct Config {
    pub baud: u32,
    divisor: u16,
}
```

- A **field is private** unless declared `pub`, visible where a private
  item of the struct's module would be. A `pub` field is visible
  wherever its struct is reachable, through a `pub use` too.
- Where a private field is not visible, **no place may name it**:
  reading or writing `c.divisor`, `&raw const c.divisor`, and
  `offset_of(Config, divisor)` are `ty::private-field`, found where
  field names are resolved against the type.
- A **struct literal** outside the field's visibility is
  `resolve::private-field`, whether it gives the private field or
  leaves it out: a literal gives every field (ADR 0021), so it cannot
  be completed there, and the error names the private field rather than
  reporting `resolve::missing-fields`. A struct with a private field can
  be built outside its module only by a function of that module.
- Visibility changes no layout: private fields are laid out, copied and
  passed like any other (ADR 0022, 0025), and `size_of` counts them.

### Variants

- Enum variants and their payloads are visible wherever the enum is
  reachable, and **only there**: `uart::Mode::Run` checks `Mode` as
  every path checks its segments, and `.Run` (ADR 0044), which does not
  write the enum, is `ty::private-variant` in an expression or a
  pattern where the enum it names is not reachable. So `set(.Run)` and
  `match get() { .Run => ... }` in another module are rejected when
  `Mode` is private, even if `set` and `get` are `pub`: only the
  module of a private enum, and its descendants, create its values or
  name its variants. `_` and `let` patterns name no variant and stay
  allowed. An enum offered with `pub use` is reachable through it, so
  `.Run` is accepted wherever `a::Mode::Run` would be.
- A `match` on an enum that is not reachable, without a catch-all arm,
  is reported as non-exhaustive (ADR 0037, 0043) with its witnesses as
  usual; since they cannot be written there, its help says that a `_`
  or `let` arm is the way to cover them.

### What visibility is not

- It controls **names**, not memory: a private static may still be
  reached through a pointer the module hands out.
- It is checked where a name is used, never in signatures: a `pub fn`
  may take or return a private type. Outside the module the type cannot
  be named, so, with types always written (ADR 0008), such a value can
  only be passed on.
- It does not change symbols (ADR 0051): a private function is still a
  symbol of the object file.
- Evaluation at compile time is unaffected.

### Diagnostic codes

| Code | Meaning |
|------|---------|
| `parser::misplaced-pub` | `pub` on a `comptime` block or on the entry function |
| `resolve::private-item` | naming an item that is not visible here |
| `resolve::private-reexport` | `pub use` of an item that is not `pub` |
| `resolve::private-field` | a struct literal of a struct with a field that is not visible here |
| `ty::private-field` | a place or `offset_of` naming a field that is not visible here |
| `ty::private-variant` | `.V` naming a variant of an enum that is not visible here |

### Conformance tests

- **ui snapshots** for each code above; a private item used from its
  module, from a descendant and, rejected, from a sibling and from the
  parent; a `pub` item in a private module, reachable from the parent
  and not from outside; `pub use` of an item of a private child; a
  private field read, written, its address taken, given in a literal,
  left out of a literal and named in `offset_of`, from inside and
  outside its module; `.V` of a private enum in an argument and in a
  pattern, from its module and from another; a non-exhaustive `match`
  on a private enum in another module, with its help; `.V` of an enum
  and a `pub` field of a struct, both of a private module and
  re-exported with `pub use`, from outside; a `pub fn` returning a
  private type, its result passed on from another module; `pub` on
  every kind of item, and before a variant, a parameter and a `let`.
- **Layout:** a struct with private fields checked against clang
  (`tests/layout`).

## Consequences

- A module can keep helpers and statics to itself, and a struct with a
  private field can only be built by its module: the base for handles
  and typestate in M5.
- Structs shared with other modules or with C need `pub` on each field.
- A private type in a public signature is not reported; a warning can
  come with warnings (roadmap, deferred ideas).
- Finer visibilities, such as Rust's `pub(crate)` and `pub(super)`, can
  be added later without breaking programs.

## Alternatives considered

- **No privacy, every item and field visible** — modules would only be
  namespaces, and nothing could guarantee that a handle was built by
  its driver.
- **Private items visible only in their own module** — a child module
  of helpers could not use its parent's private items, and the parent
  would have to make them `pub` to everyone.
- **Fields as visible as their struct, as in C and Zig** — less `pub`
  to write, but no encapsulation, and private fields could only come
  later by breaking programs.
- **Letting `.V` name a variant of a private enum, since it does not
  write the enum's name** — a module could then not stop others from
  making values of its private enum through a `pub fn` that takes one.
- **Rejecting private types in public signatures, as Rust once did** —
  rules about where a type may appear, for a value that can only be
  passed on anyway.
