# 0075. Aggregates across calls on ARMv7-M

- Status: Accepted
- Date: 2026-11-03

## Context

Every function uses the target's C ABI (ADR 0006), and aggregates cross
calls by value (ADR 0025 for riscv32). LLVM does not lower aggregates to
the C ABI: the front end must give each one the IR form that clang
gives its C twin, so that the backend puts the same bytes in the same
registers and stack slots.

On `thumbv7m-none-eabi` (ADR 0074) the C ABI is the AAPCS, base
standard (soft float). Its rules for composite types, as clang 22
implements them for this triple, checked by compiling C declarations
with `clang --target=thumbv7m-none-eabi -S -emit-llvm`:

| C type | Argument | Return |
|--------|----------|--------|
| `struct {}` | not passed | nothing |
| 1 byte | `[1 x i32]` | `i8` |
| 2 bytes | `[1 x i32]` | `i16` |
| 3 or 4 bytes | `[1 x i32]` | `i32` |
| 5 bytes, alignment 1 | `[2 x i32]` | `sret` pointer |
| `{ uint64_t }` | `[1 x i64]` | `sret` pointer |
| 64 bytes, alignment 4 | `[16 x i32]` | `sret` pointer |
| 68 bytes, alignment 4 | `ptr byval align 4` | `sret` pointer |
| 72 bytes, alignment 8 | `ptr byval align 8` | `sret` pointer |
| 16 bytes, `aligned(16)`, fields of alignment 4 | `[4 x i32]` | `sret` pointer |
| 16 bytes holding a field of an `aligned(16)` type | `[2 x i64]` | `sret` pointer |
| 80 bytes, `aligned(16)`, fields of alignment 4 | `ptr byval align 4` | `sret` pointer |
| 9 bytes, `packed` | `[3 x i32]` | `sret` pointer |

The `aligned(16)` rows show that clang classifies by the alignment of
the fields, each with its own alignment, without the record's own
`aligned` attribute (its "unadjusted alignment").

## Decision

Every function, Erebos or C, passes and returns aggregates (structs,
arrays, enums with payloads) on `thumbv7m-none-eabi` as follows. The
**natural alignment** of an aggregate is the largest alignment of its
fields, elements or variants, each counted with its own `#[align(N)]`,
without the aggregate's own `#[align(N)]` (ADR 0022); a `#[packed]`
struct's is 1.

| Aggregate | As an argument | As a return value |
|-----------|----------------|-------------------|
| size 0 | not passed at all | nothing is returned |
| size 1 to 64 bytes, natural alignment up to 4 | its bytes as `[N x i32]`, `N` = size / 4 rounded up | size 1: `i8`; size 2: `i16`; size 3 or 4: `i32`, in `r0`; larger: into memory the caller provides, whose address is a hidden first argument (`sret`) |
| size 1 to 64 bytes, natural alignment 8 or more | its bytes as `[N x i64]`, `N` = size / 8 rounded up | as above |
| larger than 64 bytes | `byval`: the caller copies it into the argument area, aligned to its natural alignment clamped to 4..8 | into memory the caller provides (`sret`) |

What the backend does with these forms is the AAPCS's rules, which the
compiler does not restate: an argument takes the core registers `r0` to
`r3` in order, then the stack; an `[N x i64]` starts at an even
register and at an 8-aligned stack slot; an argument whose words do not
all fit in the registers left is split, its first words in registers
and the rest on the stack, unless an argument already went on the
stack; a `byval` argument is copied the same way, so its first words
may also travel in registers.

- An argument is a copy: the callee may write to it without the caller
  seeing the change, as on riscv32 (ADR 0025).
- Arrays pass like a struct of the same size and alignment; a C twin
  wraps the array in a struct.
- Slices (`*const [T]`, ADR 0031), eight bytes of alignment 4, pass as
  `[2 x i32]` and return through `sret`, as the C struct
  `{ const T *ptr; size_t len; }` does.
- Zero-sized types take no register and no stack space, and C twins
  leave them out, as on riscv32.
- Bit-field structs are scalars in calls (ADR 0059), not aggregates.
- Bytes of padding, of a register beyond the aggregate, and of an
  `i8`/`i16`/`i32` return beyond the aggregate's size are unspecified.

The classification lives in `codegen::abi`, next to riscv32's, chosen by
the target's `arch` (ADR 0073). Its pass modes grow by two: an array of
`i32` or `i64` of a given length, and `byval` with an alignment; the
return of a small aggregate as an integer of its size, which riscv32's
`Cast` already expresses. MIR is unchanged.

## Consequences

- Erebos and C call each other with aggregates on both targets. The
  existing programs with C twins cover riscv32's rows; a program of its
  own, with a C twin, runs every row of this table on QEMU, placed at
  the AAPCS's register boundaries (ADR 0077).
- The rules differ from riscv32's: on riscv32 an aggregate above 8 bytes
  goes by pointer, here up to 64 bytes go in registers and on the stack
  and larger ones by copy. A program's meaning does not change; its code
  and stack use do.
- `#[align(N)]` on a struct does not change how it is passed on this
  target, only where it is stored. That program pins it, at top level
  and nested, since it is the least obvious rule.
- The forms follow clang 22. A clang that changed them would break C
  interoperation; the tests compare against the clang that builds the C
  files.

## Alternatives considered

- **Restating the AAPCS's register allocation in the front end, as
  explicit register and stack assignments** — LLVM's ARM backend already
  allocates `[N x i32]` and `byval` arguments as the AAPCS says, for
  clang; doing it twice risks a disagreement with the C side.
- **The aggregate's full alignment, `#[align(N)]` included** — simpler to
  state, but a `#[align(16)]` struct of words would start at an even
  register when clang's twin does not, and calls across the two would
  read the wrong registers.
