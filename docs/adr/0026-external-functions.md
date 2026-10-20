# 0026. External functions

- Status: Accepted; amended by 0051 (which functions C calls by their name)
- Date: 2026-10-14

## Context

ADR 0006 promises that any C function can be declared and called from
Erebos, but there is no way to declare a function defined elsewhere: a call
must name a function with a body. Vendor code, assembly routines and the
C functions that test the ABI (ADR 0025) all need it. Erebos functions are
already callable from C, since every function uses the C ABI under its own
name.

## Decision

A function without a body, marked `extern`, is defined outside the
program and found by the linker:

```
extern fn c_checksum(data: *const u8, n: usize) -> u32;

entry fn main() -> ! {
    let sum: u32 = c_checksum(buffer, 16);
    ...
}
```

- It is called like any function, with the same type checking.
- Its parameters have types but no `mut`, since there is no body.
- It cannot be an `entry` or `panic` function, and its name follows the
  same rules as other functions (no duplicates, no reserved names).
- Nothing checks that the definition agrees with the declaration; as in C,
  a mismatch is undefined. The declaration is a promise by the programmer.
- `erebos build` takes object files to link with: `--link file.o`, which
  may be repeated.

The keyword is `extern` and the ABI is not written: there is only the C
ABI (ADR 0006).

## Consequences

- Erebos programs can call C, assembly and vendor libraries, and the ABI of
  ADR 0025 can be tested against code clang compiled.
- A wrong declaration is not caught; headers cannot be imported, so
  declarations are copied by hand until that becomes a need.
- `extern` becomes a keyword. Later, `extern` may also apply to statics
  (M2.3).

## Alternatives considered

- **`extern "C" { ... }` blocks, as in Rust** — the ABI string has one
  possible value, and a block adds nesting for nothing.
- **An attribute, `#[extern] fn`** — attributes change layout and placement
  (ADR 0022); whether a function has a body here changes what it is.
