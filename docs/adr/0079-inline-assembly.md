# 0079. Inline assembly

- Status: Accepted; amended by 0083 (`sym entry`)
- Date: 2026-11-03
- Amends: 0016 (string literals exist, as the templates of `asm` only),
  0077 (the thumbv7m board stops QEMU with inline assembly, without a C
  file; a test program holding one target's assembly, or naming an item
  of `core::arch` specific to one target (ADR 0080), lists only that
  target, riscv32 or thumbv7m)

## Context

Some instructions have no expression in the language: reading a control
and status register on RISC-V (`csrr`), a special register on ARM
(`mrs`), waiting for an interrupt (`wfi`), fences (ADR 0023), a
semihosting request (`bkpt 0xab`), masking interrupts. Some functions
have a convention no C function has, as the EABI's 64-bit division
(ADR 0074, 0076), and the reset entry runs before there is a stack
(ADR 0012). Today the compiler writes `_start` as module assembly, and
the tests reach semihosting through a C file (ADR 0077).

LLVM takes inline assembly as a template, a constraint string per
operand (`=r`, `{a0}`, `~{memory}`), and flags for side effects and
stack alignment. GCC exposes those constraint strings to the program;
Rust's `asm!` names operands by direction and register class, and
derives the constraints, with options that say what the code does not
do (`nomem`, `nostack`, `pure`). Erebos has no macros and no strings
(ADR 0016): only byte strings, which are data (ADR 0031).

## Decision

### `asm` expressions

```
let mut cause: u32 = 0;
asm("csrr {}, mcause", out(reg) cause, options(nomem, nostack));

let mut x: u32 = 5;
asm("add {x}, {x}, {y}", x = inout(reg) x, y = in(reg) 3 as u32,
    options(pure, nomem, nostack));

asm("wfi", options(nomem, nostack));
```

`asm` is a keyword and starts an expression:

```
asm_expr  = "asm" "(" STRING { "," STRING } { "," operand }
            [ "," "options" "(" option { "," option } [","] ")" ] [","] ")"
operand   = [ NAME "=" ] ( "in" "(" where ")" expr
                         | ( "out" | "lateout" ) "(" where ")" out_place
                         | "inout" "(" where ")" expr [ "=>" out_place ]
                         | "sym" path
                         | "const" expr )
          | "clobber_abi" "(" STRING ")"
where     = "reg" | STRING
out_place = place | "_"
option    = "pure" | "nomem" | "readonly" | "nostack"
          | "preserves_flags" | "noreturn"
```

- **Templates.** The strings are joined with line breaks into the
  template. A **string literal** `"..."` is a new token, with the escapes
  of byte strings (ADR 0031); it is the template of `asm` and the name
  of a register or an ABI inside `asm`, and nothing else: anywhere else
  it is `parser::string-outside-asm`, with a help pointing at `b"..."`.
  Templates are ASCII.
- **Operand order.** Operands come in three groups, in this order:
  operands without a name, then named operands, then operands with an
  explicit register and `clobber_abi`, which have no name and no number.
  An operand out of its group is `parser::unexpected-token`; two
  operands with one name are `ty::asm-template`.
- **Placeholders.** `{}` takes the next operand of the first group,
  `{N}` the `N`-th operand of the first group (from 0), `{name}` the
  named one; `{{` and `}}` are braces. A placeholder may appear any
  number of times. It becomes the register the compiler chose, the
  symbol of a
  `sym` operand, or the value of a `const` operand. An operand with an
  explicit register cannot appear in the template; every other operand
  must appear at least once (`ty::asm-template`).
- **Bytes.** A template holds ASCII bytes from `0x01` to `0x7F`; another
  byte, written with `\xNN` or `\0`, is `ty::asm-template`.
- **Directions.** `in(where) e` passes the value of `e` in a register;
  `out(where) p` stores into the place `p` (or nowhere, with `_`) what
  the register holds when the code ends; `lateout` is `out` whose
  register may be one an input used, since it is written only after
  every input has been read; `inout(where) e` passes `e` and stores the
  result back into `e`, which must then be a place, or into the place
  after `=>`. The place of an `out` follows the rules of an assignment
  (`ty::assign-immutable`), so a local it writes is `let mut`. The
  words `in`, `out`, `lateout`, `inout`, `sym`, `const`, `clobber_abi`,
  `options`, `reg` and the option names are recognized only in these
  positions; elsewhere they are ordinary names, as the words of the
  `memory` block are (ADR 0057). `const` is a keyword already.
- **Registers.** `reg` lets the compiler choose a general-purpose
  register; a string names one (`"a0"`, `"x10"`, `"r0"`, `"lr"`). The
  compiler knows each target's registers and their other names; an
  unknown name is `ty::asm-register`, and so is a register the compiler
  reserves:
  - riscv32: `x0` (`zero`), `x2` (`sp`), `x3` (`gp`), `x4` (`tp`),
    `x8` (`s0`, `fp`) and `x9` (`s1`, LLVM's base pointer);
  - thumbv7m: `r6` (LLVM's base pointer), `r7` (the frame pointer in
    Thumb code), `r13` (`sp`) and `r15` (`pc`).
  The same register named twice among the inputs, or twice among the
  outputs, is `ty::asm-register`, and so is a register named by an
  `in` and by an `out` (which promises a register no input uses):
  write `inout`, or `lateout` when the output may reuse the input's
  register.
- **`clobber_abi` and explicit registers.** A register `clobber_abi`
  covers may also be an explicit operand: as an output it is that
  output, not a clobber; as an input only, its value is lost when the
  code ends, as with `inout(...) e => _`. With `clobber_abi`, every
  output names an explicit register (`ty::asm-register` for an
  `out(reg)`, `lateout(reg)` or `inout(reg)`), since the compiler could
  otherwise choose a register the call overwrites.
- **Types.** An operand of `in`, `out`, `lateout` and `inout` is an
  integer of at most 32 bits (`usize` and `isize` included) or a thin
  raw pointer (to a sized type, not a slice); anything else, `bool`,
  enums, bit-field structs and pointers to slices included, is
  `ty::asm-operand-type` (convert with `as`). The value has no
  expected type, so a literal needs `as` (`in(reg) 3 as u32`). The two
  places of an `inout` with `=>` have one type.
- **Widths.** An input narrower than 32 bits fills its register
  extended by its signedness: zeros above a `u8` or `u16`, copies of
  the sign bit above an `i8` or `i16`. An output narrower than 32 bits
  takes the low bits of its register.
- **Evaluation order.** The operand expressions and the output places
  (their indices, with their checks, and the calls in them) are
  evaluated left to right, before the code runs (ADR 0018); a check
  that fails there calls the panic function and the code does not run.
  After the code, the outputs are stored left to right, so of two
  outputs to one place the right one stays. The place of an `inout`
  without `=>` is evaluated once: read before the code and written
  after it, so `inout(reg) A[f()]` calls `f` and checks the index once.
- **`sym path`** names a function, an external function, a static or an
  external static, and is replaced by its symbol (ADR 0051); anything
  else is `ty::asm-sym`. The item counts as used, so it is kept in the
  object.
- **`const e`** is a value of any integer type, `u64` and `i64`
  included, computed at compile time (ADR 0027); like an input, it has
  no expected type, so a literal needs `as`. It is replaced by its value
  in decimal, with a `-` when negative.
- **`clobber_abi("C")`** marks every register a C call may change as
  overwritten (riscv32: `ra`, `t0`–`t6`, `a0`–`a7`; thumbv7m: `r0`–`r3`,
  `r12`, `lr` and the flags), so that the code may call a C function.
  No other ABI exists (ADR 0006).

### Meaning

The template is copied into the object as written, with the
placeholders replaced; the compiler does not read it. Without options,
an `asm` expression:

- runs every time it is evaluated, in program order relative to calls,
  volatile accesses (ADR 0023) and other `asm` expressions;
- may read and write any memory the program can reach, so it is a
  compiler fence: no access to statics or memory reached through
  pointers moves across it;
- may push to the stack, so the compiler aligns the stack pointer as the
  C ABI wants before it;
- may change the ARM condition flags.

The code must leave every register it does not list as an output or
through `clobber_abi` as it found it, the stack pointer balanced, and
must not jump out of the template, unless `noreturn` says so. Breaking
that is not defined. The compiler may copy the template, by inlining or
by unrolling a loop, so labels inside it must be local (`1:` and `1b`,
`1f`).

The **options** take guarantees away from the compiler's assumptions:

| Option | Promise | Effect |
|--------|---------|--------|
| `nomem` | reads and writes no memory | memory accesses may move across it; not a compiler fence |
| `readonly` | reads memory, writes none | writes may not move across it, reads may |
| `pure` | has no effect but its outputs, which depend only on its inputs and, with `readonly`, memory | may be removed when its outputs are unused, or evaluated once for equal inputs; needs `nomem` or `readonly` and an output |
| `nostack` | does not touch the stack | the stack is not aligned for it |
| `preserves_flags` | leaves the condition flags as they were | no effect on riscv32 |
| `noreturn` | never ends | the expression has type `!`; no output operands |

Without `noreturn` the expression has type `()`. Options that
contradict each other (`nomem` with `readonly`, `pure` without an
output) are `ty::asm-options`.

### Errors from the assembler

The compiler checks operands, registers, types and placeholders; LLVM's
assembler reads the instructions when the object is built. An
instruction it rejects (an unknown mnemonic, a register that does not
exist, an immediate out of range) is `codegen::asm`, at the span of the
`asm` expression, with the assembler's message. LLVM's default reaction
to such an error ends the process, so code generation installs its own
handler; no template ends the compiler.

Some errors exist only in the whole object: a non-local label that
inlining or unrolling defined twice, or an instruction that fails only
with the register chosen in context, or no register left for the
operands (more `reg` operands than free registers). When the compiler
cannot tell which `asm` LLVM rejected, it reports `codegen::asm`
without a label, with LLVM's message; an error LLVM reports for
anything other than inline assembly, its operands' registers included,
is `codegen::llvm`, as today. Each `asm` is
reported once however many copies of it the optimizer made, and the
reports come in the order of the program's source.

### Compile-time evaluation

`asm` cannot run at compile time: an evaluation that reaches one is
`eval::asm`. A function containing `asm` may still be type checked and
called at run time like any other.

### Naked functions

```
#[naked]
fn __aeabi_uldivmod() {
    asm(
        "push {{r4, lr}}",
        "sub sp, sp, #16",
        "add r4, sp, #8",
        "str r4, [sp]",
        "bl {div}",
        "ldrd r2, r3, [sp, #8]",
        "add sp, sp, #16",
        "pop {{r4, pc}}",
        div = sym crate::builtins::__erebos_udivmoddi4,
        options(noreturn),
    );
}
```

`#[naked]` makes a function with **no code of the compiler's**: no
prologue, no epilogue, no stack frame. Its body holds exactly one
`asm` expression, as a statement (`asm(...);`) or as the tail of the
block, with `noreturn` and only `sym` and `const` operands, which does
everything, returning included (`ty::naked-body` otherwise). Its
parameters and return type say how C calls it, by the C ABI, and the
template finds them where the ABI puts them; the body cannot name a
parameter. A naked function is called like
any other, and is the one way to write a function with a convention no
signature has. `#[naked]` applies to functions with a body; on an
`extern fn`, the entry function or the panic function it is
`resolve::misplaced-attribute`. Calling one at compile time is
`eval::asm`.

### Where it is checked and generated

The parser builds the expression; type checking checks the operands,
registers, places, types, options and placeholders, with the target of
the build (ADR 0073); MIR has an `asm` statement, or a terminator with
`noreturn`, holding the template and its operands; code generation
derives the constraint string, escapes `$` in the template, and emits
LLVM's inline assembly, with `sideeffect` unless `pure`,
`alignstack` unless `nostack`, `~{memory}` unless `nomem` or
`readonly`, and the clobbers of the flags (`~{cc}` on thumbv7m) unless
`preserves_flags`. A naked function is an LLVM function with the
`naked` and `noinline` attributes whose body is the assembly and
`unreachable`.

### The thumbv7m board

The board of the tests on thumbv7m (ADR 0077) makes its semihosting
request in Erebos, with `asm("bkpt 0xab", ...)`, and has no C file.

### Diagnostic codes

| Code | Meaning |
|------|---------|
| `parser::string-outside-asm` | a string literal outside an `asm` expression |
| `ty::asm-template` | a placeholder naming no operand, an unused operand, an explicit register in the template, an unmatched brace, two operands with one name, a byte outside `0x01`–`0x7F` |
| `ty::asm-register` | an unknown or reserved register; one named twice among the inputs or the outputs, or by an `in` and an `out`; an output without an explicit register beside `clobber_abi` |
| `ty::asm-operand-type` | an operand of a type no register holds, a `const` that is not an integer |
| `ty::asm-sym` | `sym` naming something other than a function or a static |
| `ty::asm-options` | options that contradict each other or the operands, an option or `clobber_abi` given twice, an ABI other than `"C"` |
| `ty::naked-body` | a naked function whose body is not one `asm` with `noreturn` and only `sym` and `const` operands |
| `codegen::asm` | an instruction the assembler rejects |
| `eval::asm` | `asm` reached at compile time |

### Conformance tests

- **ui snapshots** for each code above; the AST of every operand form
  and option; string literals outside `asm`; each reserved register of
  each target (`// target:`); the operand groups out of order.
- **MIR snapshots** of the evaluation order: two outputs to one place,
  and an output place whose index check fails before the code runs.
- **LLVM IR snapshots** on both targets showing the constraint string
  of each direction, an explicit register, `clobber_abi("C")`, each
  option, `sym` and `const`, a `$` escaped, and a naked function.
- **`codegen::asm`**: a template with an unknown instruction is
  reported at its span, and the compiler exits with an error, not by
  LLVM's handler.
- **End to end** on both targets: operands of each direction carry
  values in and out; narrow inputs arrive extended; `sym` calls a
  function; a `noreturn` asm ends a
  program; the thumbv7m board stops QEMU through `bkpt`.

## Consequences

- Programs can reach every instruction of their target, and `core` can
  define functions with conventions C cannot write.
- `asm` is a keyword. String literals exist only for it; text is still
  bytes elsewhere (ADR 0016).
- A template is not portable: `core::arch` (ADR 0080) is where the
  compiler's portable wrappers live, and a program's own assembly is
  for one target (`// targets:` in the tests, ADR 0077).
- The compiler trusts the template to keep the promises of its options
  and registers. A wrong promise is a bug the compiler cannot see.
- Mistakes in instructions are found when the object is built, after
  type checking: `erebos build --emit=typeck` accepts a template the
  assembler will reject.
- The startup code (ADR 0012) can move into `core` as a naked function,
  which is M4.2's.

## Alternatives considered

- **GCC's constraint strings** (`asm("..." : "=r"(x) : "r"(y) :
  "memory")`) — exposes LLVM's constraint letters, which differ per
  target and mean nothing to a reader; the defaults are unsafe (no
  clobber of memory unless asked).
- **Only module-level assembly and naked functions** — every single
  instruction (`wfi`, `csrr`, a fence) becomes a call, and values move
  through the ABI instead of the registers the code wants.
- **Templates as byte strings (`b"wfi"`)** — no new token, but a
  template is not data, and a byte string has a type and an address;
  a token that only `asm` accepts is plainer.
- **Intrinsics per instruction** (`core::arch::csrr(MCAUSE)`) — each
  needs compiler support; inline assembly lets `core` write them.
- **Defaults that promise less (no memory clobber)** — faster code by
  default, at the price of silent miscompilations when the code does
  touch memory, as with GCC's `asm` without `"memory"`.
