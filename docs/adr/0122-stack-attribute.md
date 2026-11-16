# 0122. `#[stack(N)]`: stack the compiler cannot measure

- Status: Accepted
- Date: 2026-11-16
- Amends: 0079 (an `asm` without `nostack` in a function needs the function's
  `#[stack(N)]`), 0026 (an `extern fn` that run-time code calls declares its
  stack)

## Context

ADR 0123 computes the worst-case stack depth of a program from the frame of
each function, which LLVM measures after register allocation. Three kinds of
code use stack that LLVM does not measure:

- **A naked function** (ADR 0079) has no prologue: LLVM reports a frame of 0,
  and whatever its assembly pushes is unknown to it. `core` has four whose
  assembly runs on the target: `_start` on both targets (it pushes nothing),
  the riscv32 trap entry (64 bytes, ADR 0085) and thumbv7m's
  `__aeabi_uldivmod` and `__aeabi_ldivmod` (8 bytes pushed and a 16-byte
  frame).
- **An `asm` expression without `options(nostack)`** may push below the stack
  pointer (ADR 0079: without `nostack`, LLVM aligns the stack for it). LLVM
  counts the frame of the function around it, not what the template does.
  A text search counts 94 `asm` expressions in `tests/e2e/` and
  `tests/boards/`, of which 59 name `nostack`, so about 35 lack it (some in
  naked functions); those of `rt/` that are not naked all have it.
- **An `extern fn`** (ADR 0026) has no code the compiler sees. A text search
  counts 208 declarations in 24 e2e programs, all defined in C files that
  the tests compile with clang at the program's own level (`-O0` or `-O2`,
  `tests/common/qemu.rs`). A C function may also call an Erebos function back
  by its symbol.

The compiler cannot know these numbers; the program must say them.

## Decision

### The attribute

```
#[naked]
#[stack(64)]
fn __erebos_trap() -> ! { asm(...) }

#[stack(16)]
extern fn crc32(data: *const u8, len: usize) -> u32;
```

- `#[stack(N)]` applies to every function: a function with a body (plain,
  naked, the entry function, the panic function, a handler, a function of a
  type) and an `extern fn`. On any other item it is
  `resolve::misplaced-attribute`, whose help lists functions. It may be given
  once (`resolve::duplicate-attribute`).
- `N` is a `usize` computed at compile time, as the argument of `#[align(N)]`
  is (ADR 0087, 0029): a constant expression resolved in the function's
  module. Any value is valid, 0 included; it is a number of bytes, and need not
  be a multiple of the stack alignment. A failed computation is the error of
  that computation, and the bound then skips the function (ADR 0048's rule).
- On a function run only at compile time it has no effect and no diagnostic,
  as `#[align]` (ADR 0087).
- MIR keeps it: `Body::stack: Option<u64>`, shown by `--emit=mir` next to the
  alignment.

### What `N` counts

`N` is the stack the function uses **that the compiler does not measure**, in
bytes, at its worst:

- **On a function with a body**, `N` is added to the frame LLVM measures. For a
  naked function, whose measured frame is 0, it is all the stack its assembly
  takes below the stack pointer it was entered with. For a function with an
  `asm` without `nostack`, it is the most that any one execution of those
  `asm` takes below the stack pointer it starts with. An `asm` that is not
  `noreturn` must leave the stack pointer as it found it, as the C ABI and
  LLVM's inline assembly expect, so an `asm` inside a loop takes the same
  bytes on each pass, not more; one that does not restore it is wrong
  whatever `N` says. In both cases, a function that the
  assembly calls through a `sym` operand is **not** in `N`: ADR 0123 counts it
  as a callee, with its own frame.
- **On an `extern fn`**, `N` is the whole depth of the external function: its
  frame and everything it calls, Erebos functions that it calls back
  included, since the compiler sees none of these calls.

On a function with a body that needs none (no `naked`, every `asm` with
`nostack`), `N` is still added to its frame: the program may reserve stack for
a reason the compiler does not see. Nothing requires it there.

### Where it is required

The stack bound (ADR 0123) needs `N` for every function it reaches that is:

- a naked function;
- a function with a body holding an `asm` expression without `nostack`, in a
  block reachable from its start block;
- an `extern fn`.

Such a function without `#[stack(N)]` is `analysis::missing-stack`, reported
by the bound, so only where the bound runs: `erebos build` and
`--emit=stack` (ADR 0123). The other stages do not need it. A function the
bound does not reach (an `extern fn` nothing calls, a naked function only C
calls) needs nothing.

### Not under `unsafe`

`N` is the program's word, which nothing checks, as is the signature of an
`extern fn`. It is a plain attribute, not an unsafe one, for three reasons:

- calling an `extern fn` already needs `unsafe` (ADR 0119), and so does writing
  an `asm` expression, so the code whose stack `N` describes is already code
  the program vouches for;
- ADR 0119 leaves unsafe attributes unscheduled, and this one alone would not
  justify the syntax;
- a wrong `N` makes the bound wrong, the same failure as a wrong declared
  stack size in `memory`, which is no `unsafe` either.

### Programs migrated

- `rt/`: `#[stack(0)]` on both `_start`s, `#[stack(64)]` on the riscv32 trap
  entry, `#[stack(24)]` on `__aeabi_uldivmod` and `__aeabi_ldivmod`. A comment
  above each says where the number comes from.
- `tests/e2e/` and `tests/boards/`: every `extern fn` gets the depth that
  `clang -fstack-usage` reports for its C definition at `-O0` and at `-O2`
  (the tests build the C at the program's level, so one number serves both
  only if it is the larger), plus the functions it calls, Erebos ones
  included, whose depth `--emit=stack` reports even when no part reaches
  them (ADR 0123, "The report"). Every `asm` without
  `nostack` whose template does not touch the stack gains `nostack`, which says
  what is true; one that does gets `#[stack(N)]` on its function.
- `tests/ui/`: only the programs that reach the bound, which are none today
  (no ui test builds an executable).

## Consequences

- The bound covers C and assembly, with the program's numbers. A wrong number
  gives a wrong bound, as a wrong `extern fn` signature gives a wrong call.
- The e2e programs gain about 210 attributes. A C file changed later must have
  its numbers updated; the bound's own oracle (ADR 0123, "Tests") catches a
  number that is too small on the paths the e2e programs run.
- `nostack` matters now: an `asm` that lacks it costs an attribute.
- The help of `resolve::misplaced-attribute` for `#[stack]` lists functions.

## Alternatives considered

- **A missing `#[stack(N)]` is 0, with or without a warning.** Less to migrate,
  but the bound silently ignores C and assembly, and a bound that is wrong
  without a word is worse than none.
- **`#[unsafe(stack(N))]`.** Opens unsafe attributes for one case; see "Not
  under `unsafe`".
- **An option on the `asm` expression, `options(stack(N))`.** It would sit on
  the code it describes, but an `extern fn` needs an attribute anyway, and a
  naked function's `asm` is its whole body. One form serves all three.
- **On an `extern fn`, `N` as its own frame, with the Erebos functions it calls
  back listed (`#[stack(N, calls(f, g))]`).** More precise, but C's own callees
  would still be in `N`; one number that covers everything is simpler to state
  and to check by hand.
- **Measure C with clang from the compiler.** The compiler does not compile C
  (ADR 0026); objects come from `--link`, built by whatever the program uses.
- **Require it where the attribute is written, at type checking.** The check
  would have to know which `extern fn` run-time code calls, which is the
  bound's graph; and every ui test with a naked or `extern fn` would change
  for a number no stage before the bound reads.
