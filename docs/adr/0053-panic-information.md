# 0053. Panic information

- Status: Accepted; amended by 0059 (the panic reason `FieldOutOfRange`)
- Date: 2026-10-20
- Amends: 0014 (the panic function may take a `*const core::PanicInfo`),
  0031 (an `assert` message reaches the panic function), 0036 (the cause
  "enum value is no variant" is `PanicReason::NoVariant`)

## Context

The panic function takes no parameters (ADR 0014), so a handler cannot
say what failed or where: every division by zero, index out of bounds,
failed `assert` and enum value that is no variant looks the same. ADR
0014 planned to pass the location and reason once structs and byte
strings existed, before much code depends on the old signature; ADR 0031
evaluates an `assert` message at run time and drops it; ADR 0036 leaves
its cause a `PanicReason` for M2.6. The roadmap fixes the form,
`panic fn on_panic(info: *const PanicInfo) -> !`, with the file, the
line and a `PanicReason` enum.

The information costs memory: a file name, a line and a reason for each
check in the program. Rust pays it for every panic site and offers
`-Zlocation-detail=none` to drop it; on a microcontroller with a few
kilobytes of flash, that matters.

## Decision

### The panic function

The panic function takes **no parameter or one**:

```
panic fn on_panic() -> ! { loop {} }

panic fn on_panic(info: *const core::PanicInfo) -> ! { ... }
```

- The one parameter has exactly the type `*const core::PanicInfo`
  (ADR 0052), not `*mut` nor volatile; it may be `mut`, as any
  parameter. Any other parameter list is `ty::panic-param`.
- **Without the parameter, no panic information is built or emitted**:
  a program pays nothing for what its handler ignores. An `assert`
  message is still evaluated when the assertion fails, and dropped, as
  ADR 0031 has it: what a program does before the panic function runs
  does not depend on that function's signature.
- The compiler's checks call it directly, as before (ADR 0014). It may
  still be called explicitly, with the arguments its signature asks
  for: a caller of a handler with the parameter builds the
  `PanicInfo` itself. Code that panics on purpose writes
  `assert(false, msg)`, and the compiler fills in the information.

### `core::PanicInfo`

```
pub struct PanicInfo {
    pub file: *const [u8],
    pub line: u32,
    pub column: u32,
    pub reason: PanicReason,
    pub message: *const [u8],
}

pub enum PanicReason: u8 {
    DivisionByZero = 0,
    IndexOutOfBounds = 1,
    AssertionFailed = 2,
    NoVariant = 3,
}
```

`PanicInfo` takes 28 bytes, aligned to 4: `reason` at offset 16,
`message` at 20 (ADR 0022).

- **`file`** is the name of the source file holding the check, relative
  to the directory of the root file, with `/` between directories:
  `main.ers`, `uart/regs.ers`; in `core`, `rt/core.ers` or
  `rt/builtins.ers` (ADR 0052). It does not depend on the directory the
  compiler runs in nor on how the root file was named on the command
  line, so builds stay reproducible.
- **`line`** and **`column`** locate the first byte of the token that
  performs the check, both counted from 1, the column in bytes, so that
  nested checks, as in `a[i][j]` or `a / b / c`, each have their own:

  | Reason | Checked by | Location |
  |--------|------------|----------|
  | `DivisionByZero` | `a / b`, `a % b`, `a /= b`, `a %= b` (ADR 0007) | the operator |
  | `IndexOutOfBounds` | `a[i]`, `(*s)[i]` (ADR 0021, 0031) | the `[` |
  | `AssertionFailed` | `assert(c)`, `assert(c, msg)` (ADR 0029, 0031) | `assert` |
  | `NoVariant` | a `match` without a catch-all arm (ADR 0036, 0042) | `match` |
  | `NoVariant` | `e?` (ADR 0046) | the `?` |

  Parentheses change nothing: `(a / b)` is located at its `/`.

- **`reason`** is the cause of the check.
- **`message`** is the message of `assert(c, msg)`, computed only when
  the assertion fails (ADR 0031); for every other check, and for an
  `assert` without one, it is an empty slice, whose address is
  unspecified.

### Where the information lives

- When every field is known at compile time, for every check except an
  `assert` with a message, the `PanicInfo` is **read-only data**
  computed by the compiler: one per check, possibly shared between
  identical ones, and one byte string per file name. The call passes
  its address.
- An `assert` with a message builds its `PanicInfo` in the frame of the
  failing function, on the failing path only, where the message is
  computed. The panic function never returns, so the pointer stays
  valid while it runs.
- The handler must not write through `info`: it points at read-only
  memory or at another function's frame.

### At compile time

Nothing changes: a failed check fails the evaluation as before, with
`eval::panic` and the cause, or `eval::assertion-failed` for an
`assert` (ADR 0029); the panic function's body never runs, and no
`PanicInfo` is read.

### Diagnostic codes

| Code | Meaning |
|------|---------|
| `ty::panic-param` | a panic function whose parameters are not none or one `*const core::PanicInfo` |

The entry function keeps `ty::special-fn-params`.

### Conformance tests

- **ui snapshots:** `ty::panic-param` for two parameters, `*mut`,
  `*const volatile`, another struct and `PanicInfo` by value; a handler
  in a module with `use core::PanicInfo;`; an explicit call to a
  handler with the parameter.
- **MIR and LLVM IR snapshots:** a call to a handler with the parameter
  passing the address of read-only data, for each reason; the location
  of each check of the table, nested and parenthesized ones included;
  an `assert` with a message building its `PanicInfo` on the failing
  path; a handler without the parameter emitting no information, called
  without arguments, with an `assert` message still evaluated.
- **Layout:** the size, alignment and offsets of `PanicInfo` checked
  against clang's for an equivalent C struct (`tests/layout`), and
  asserted by a `comptime` block in `core`.
- **End to end**, at both optimization levels: since a panic ends the
  program, one program per reason, whose handler prints the reason, the
  message, the file, the line and the column; one of them reaches its
  check in a file of a subdirectory. The milestone program, of several
  files, prints `division by zero at main.ers:12`.
- The existing handlers without a parameter keep running unchanged.

## Consequences

- A handler can report what failed and where, from any file, with the
  `assert` message.
- Each check costs about 28 bytes of read-only data when the handler
  takes the information, nothing when it does not.
- Every existing program keeps compiling.
- A new kind of check adds a variant to `PanicReason`: a handler that
  matches the reason without a catch-all arm then fails to compile, and
  must handle it.
- The column counts bytes, which is the character for ASCII sources.

## Alternatives considered

- **Always one parameter, as the roadmap writes it** — one form, but
  every program pays for the information, and every existing handler
  changes.
- **`PanicInfo` by value** — 28 bytes pass through a hidden pointer
  anyway (ADR 0025); an explicit pointer says so and lets the data stay
  read-only.
- **Separate parameters (`file`, `line`, `reason`, `message`)** — a
  longer signature that every new field would break.
- **`message: Option(*const [u8])`** — tells an empty message from none,
  which no handler needs, and makes printing it a `match`.
- **No column** — saves 4 bytes per check, but two checks on one line
  could not be told apart.
- **A `PanicReason::Explicit` for explicit calls** — a caller that
  builds the information can choose any reason; `assert(false, msg)`
  is the way to panic on purpose.
- **The start of the checked expression, as one rule for every check**
  — simpler to state, but `a[i][j]` and `a / b / c` would give two
  checks the same line and column, the case the column is there for.
- **Absolute file paths** — builds would depend on where the sources
  are.
