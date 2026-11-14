# 0116. References in `assert` and the panic function

- Status: Accepted
- Date: 2026-11-14
- Amends: 0031 (an `assert` message is a `&[u8]`), 0053 (the panic
  function's parameter is a `&core::PanicInfo`; `ty::panic-param`), 0014
  (through 0053), 0095 (the rule T8), 0105 (the panic function's parameter
  is a reference), 0108 ("What does not change": the message of `assert` and
  the panic function's parameter), 0111 (why the call a check makes is not
  followed)

## Context

Two built-in forms of the language take raw pointers because references did
not exist when they were designed:

- `assert(cond, msg)` takes a message of type `*const [u8]` (ADR 0031), so a
  message is written `&raw const b"..."`. ADR 0031's consequences say the
  price of `b"..."` being `[u8; N]` is this `&raw const`, and that "references
  (M5) may make it lighter". ADR 0108 kept the message a `*const [u8]`, and
  the roadmap leaves to M5.7 "whether `assert` takes a `&[u8]` message, so
  that `assert(c, &b"...")` is written without `&raw const`".
- The panic function takes no parameter or one `*const core::PanicInfo`
  (ADR 0053). Its handler reads `(*info).reason`, `(*info).line`, and passes
  `(*info).message` on; nothing in it needs a raw pointer: the handler only
  reads through `info`, which ADR 0053 says points to read-only data or to a
  frame that outlives the handler.

References are second-class (ADR 0094): parameters and `let` without `mut`,
never in fields. `&b"..."` borrows the byte string, a place in read-only
memory, with no copy, and converts to `&[u8]` where ADR 0024 converts
(ADR 0108). `&*p` makes a reference from a raw pointer that is not volatile
(ADR 0105, 0108). A reference is passed as a pointer and a reference to a
slice as a raw slice, across the C ABI too (ADR 0105, 0108).

M5.7 moves `core` and the tests to references where they can (ADR 0117);
these two forms are in every test that panics on purpose or reports a panic.

## Decision

### `assert` messages

`assert(cond, msg)` takes an optional second argument of type **`&[u8]`**,
or of a type that converts to one (a `&[u8; N]`, a `&mut [u8]`,
ADR 0024, 0105, 0108):

```
comptime {
    assert(size_of(Header) == 7, &b"Header must match the wire format");
}
assert(n < 4, &buf[0..len]);
```

- A `*const [u8]` is no longer accepted: it is `ty::mismatched-types`, as any
  other type is. When the message has a raw slice type or a raw pointer to an
  array of `u8` (`*const [u8]`, `*mut [u8]`, `*const [u8; N]`, `*mut [u8;
  N]`), not volatile, the error gains a help naming the form to write from
  what was written: `&b"..."` for `&raw const b"..."`, `&p` for `&raw const
  p` or `&raw mut p` of any other place, and `&*msg` for any other expression
  (a variable, a field, a call), which makes a reference from it (ADR 0108).
  A volatile one gets no help: no reference that is not volatile can be made
  from it (ADR 0110).
- A volatile reference (`&volatile [u8]`) is not accepted: an `assert`
  message is not read volatile (ADR 0110 gives no conversion from a volatile
  reference to one that is not).
- Everything else of ADR 0031 and 0053 stands: the message is evaluated only
  if the assertion fails; at compile time `eval::assertion-failed` shows it;
  at run time it reaches the panic function in `PanicInfo.message`.
- The message is a use of the reference for the borrow check (ADR 0106): its
  borrow is in use until the panic function is called, on the failing path
  only, where nothing else runs. A message that borrows a place a `&mut` in
  use also reaches is `analysis::borrow-conflict`, as any `&` would be; a
  message borrowing a `static mut` is a place of the rule of shared statics
  (ADR 0093, 0110). No message borrows a peripheral: a `&[u8]` is never of a
  peripheral type, and registers and register arrays keep no address
  (ADR 0110).

### The panic function

The panic function takes **no parameter or one `&core::PanicInfo`**:

```
panic fn on_panic() -> ! { loop {} }

panic fn on_panic(info: &core::PanicInfo) -> ! {
    print(&*info.message);
    ...
}
```

- The one parameter has exactly the type `&core::PanicInfo`: not `&mut`, not
  `&volatile`, and not `*const core::PanicInfo`, which is no longer accepted.
  Any other parameter list is `ty::panic-param`, whose message and help now
  name `&core::PanicInfo`.
- Like any parameter of a reference type, it may not be `mut`
  (`ty::reference-position`, ADR 0105): the sentence of ADR 0053 that it "may
  be `mut`, as any parameter" is withdrawn.
- Its fields are reached as through any reference, `info.line` (ADR 0114).
- `PanicInfo` itself does not change: `file` and `message` stay `*const [u8]`,
  since a reference is never a field (ADR 0094). A handler that passes them
  where a `&[u8]` is wanted writes `&*info.message`; its layout, which
  `tests/layout` and `core`'s `comptime` block check, is unchanged.
- What the compiler's checks pass is unchanged (ADR 0053): the address of
  read-only data, or of a `PanicInfo` in the failing function's frame. Both
  are valid while the panic function runs, which never returns, so a
  reference to them is sound. The ABI is unchanged: a reference is passed as a
  pointer (ADR 0105).
- A program that calls its panic function explicitly passes a reference, as
  `on_panic(&info)` for a `PanicInfo` it built.
- For every check but an `assert` with a message, `file` may be the name of
  a file and `message` is an empty slice "whose address is unspecified"
  (ADR 0053). `&*info.message` on it makes a reference to zero bytes, which
  reads nothing, so its address is never used as one (ADR 0105, 0108).
- ADR 0085 and 0090, whose small functions call a panic function "with
  `*const core::PanicInfo`" for an unhandled vector, are read as with
  `&core::PanicInfo`: they pass the same address of read-only data.

### The call a check makes, in the analyses

ADR 0111 does not follow the call a check makes to the panic function, since
the panic function never returns "and it is given no reference (its one
parameter is a `*const core::PanicInfo`)". The parameter is now a reference,
and the exemption still holds, for a narrower reason: the reference the
compiler's call passes is to the check's own `PanicInfo`, in read-only data or
in a local of the failing function (ADR 0053), never to a static or a
peripheral, and the message inside it is a `*const [u8]`, which the borrow
check does not follow (ADR 0106). So the panic function holds no borrow of
anything the caller lent, and once it runs no borrow is used again. A call of
the panic function written by name, as `on_panic(&INFO)` with `INFO` a
static, is followed as any call, as ADR 0111 already says for `on_panic();`.

### In MIR

- The rule T8 of the validator (ADR 0095) changes: the message of an `assert`
  is an operand of type `&[u8]`, not volatile, where it was `*const [u8]`.
  MIR building converts what the message converts from, as for any argument
  (ADR 0109: the conversion is a `Ref`).
- Where the `PanicInfo` of an `assert` with a message is built (ADR 0053), its
  field `message` is assigned the message cast to `*const [u8]`, with the
  cast that `as` writes from a reference to a raw pointer (ADR 0105). Nothing
  else in MIR changes; the evaluator reads the message's address and length
  as before, since a `&[u8]` is laid out as a `*const [u8]` (ADR 0108).
- The panic function's parameter is a local of type `&core::PanicInfo`; the
  call of a check (`Terminator::Panic`) passes the same address as before.

### Diagnostic codes

No new code.

| Code | Change |
|------|--------|
| `ty::panic-param` | a panic function whose parameters are not none or one `&core::PanicInfo` |
| `ty::mismatched-types` | an `assert` message of a raw slice type gains the help described above |

### Conformance tests

- **ui snapshots:** `ty::panic-param` for `*const core::PanicInfo`, `&mut
  core::PanicInfo`, `&volatile core::PanicInfo` and the cases of ADR 0053;
  `mut info: &core::PanicInfo` as `ty::reference-position`; an explicit call
  passing `&info`; an `assert` message of each type that converts
  (`&[u8; N]`, `&mut [u8; N]`, `&[u8]`, `&mut [u8]`, a sub-slice, a temporary
  as `&f()`), and the rejected ones (a `*const [u8]` variable, `&raw const
  b"..."` and `&raw const buf`, each with its help; `&volatile [u8]`,
  `&[u32]`); a failing `assert` with a `&b"..."` message at compile time.
- **Analysis snapshots:** a message borrowing a buffer while a `&mut` of it
  is in use (`analysis::borrow-conflict`) and one where the `&mut` is no
  longer used (accepted); a message borrowing a `static mut` in the thread
  context outside `critical` while a handler writes it
  (`analysis::unprotected-static`); an explicit call `on_panic(&INFO)`
  followed by the check of calls (ADR 0111).
- **MIR snapshots:** an `assert` with a message, showing the operand of type
  `&[u8]` and the cast into `PanicInfo.message`; a panic function with the
  parameter.
- **Validator unit tests:** T8 rejects a `*const [u8]` message, a `&mut
  [u8]` (MIR building converts it to a `&[u8]` first) and a volatile
  reference, and accepts a `&[u8]`.
- **End to end:** every program that reports a panic takes `&core::PanicInfo`,
  and every `assert` message is a `&[u8]`, with the same output at both
  levels on both targets (ADR 0117 migrates them).

## Consequences

- An `assert` message and a handler are written without `&raw` or `(*info)`;
  `&b"..."` is the one way to pass a byte string, to a function or to
  `assert`.
- Every existing `assert` with a message and every panic function with the
  parameter changes, in the repository's tests and documentation. There are
  no programs outside the repository to keep compiling, and ADR 0053's
  promise that "every existing program keeps compiling" is about the panic
  function without the parameter, which is unchanged.
- A handler reaching the message or the file still writes `&*info.message`:
  the fields of `PanicInfo` stay raw until references can be stored, which
  ADR 0094 leaves out of M5. ADR 0117 counts them among what stays raw.
- Writing through `info` stays `ty::write-through-const`, as it was through
  the `*const`; what the reference adds is that `info` cannot be null or
  dangling, which the compiler's calls already guaranteed and an explicit
  call now has to.

## Alternatives considered

- **Accepting both `*const [u8]` and `&[u8]` in `assert`, and both parameter
  types in the panic function** — nothing breaks, but a built-in would take
  two types where no other form of the language does, against the one way of
  ADR 0008, and the raw form would stay in use with nothing to tell it is the
  old one.
- **Keeping the raw forms** — `&raw const` stays in every `assert` message,
  the cost ADR 0031 said references might remove, and handlers keep `(*info)`.
- **Making `PanicInfo`'s fields references too** — a reference in a field
  needs a lifetime or a region (ADR 0094); the information is read-only data
  or a frame that outlives the handler, which no type of the language can
  state yet.
- **`assert` taking `&[u8]` with an automatic borrow of a byte string**
  (`assert(c, b"...")`) — an implicit conversion from an array value to a
  reference, against ADR 0008; `&b"..."` is one character more.
