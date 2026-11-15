# 0119. `unsafe`

- Status: Accepted
- Date: 2026-11-15
- Amends: 0020 (the Erebos signatures of `memcpy`, `memmove`, `memset` and
  the `__aeabi_mem*` functions are `unsafe fn`), 0026 (a call to an
  `extern fn` and an access to an `extern static` need `unsafe`), 0079
  ("`asm` expressions": an `asm` outside a naked function needs `unsafe`;
  "Naked functions": a call to one needs `unsafe`, and an `unsafe` block is
  no naked body), 0086 (`enable_interrupts` is an `unsafe fn`), 0105 ("What
  may be borrowed": a borrow through a raw pointer needs `unsafe`), 0118
  (its rules are unchanged; its examples that return `&*p`, as `counter()`,
  now need an `unsafe` block around the borrow)

## Context

Erebos checks moves (ADR 0097), linear values (ADR 0101), references and
borrows (ADR 0105 to 0112, 0118) and shared statics (ADR 0093). Each check
holds only for what it can see, and each ends at the same few operations
whose correctness the program vouches for:

- an access through a raw pointer: the pointer may come from an integer
  (ADR 0019), from `slice(p, n)` with any `n`, or from a reference that has
  ended, and "a reference made through a raw pointer is the program's word"
  (ADR 0105): the borrow check follows nothing through it;
- `asm` (ADR 0079), which can write any register or any memory, and a
  `#[naked]` function, whose whole body is one `asm`;
- a call to an `extern fn` and an access to an `extern static` (ADR 0026),
  whose definitions the compiler does not see and which "do not match their
  definitions" is not defined (ADR 0030);
- `core::arch::enable_interrupts()` (ADR 0086), which, called inside a
  `critical` block or a handler, directly or through a callee, unmasks
  interrupts while the check of shared statics (ADR 0093) still treats the
  rest of the block, or the handler, as masked. ADR 0086 says so; nothing
  rejects that call.

None of these is marked. A reader cannot tell from a function's text whether
it relies on the checks or steps outside them, and a review cannot find the
places where the program, not the compiler, carries the proof. ADR 0009
accepted that "early programs are memory-unsafe in the same way C is"; M5
built the checks, and the question left is where they stop.

ADR 0030 rejected `unsafe` for `static mut` because "Erebos has no
`unsafe`"; ADR 0117 deferred marking the dereference of a raw pointer "until
it is clear what remains", and its count after M5.8 is 351 sites of raw
pointers in 77 files of `rt/`, `tests/boards/` and `tests/e2e/`, 201 of them
in programs whose subject is raw pointers (reasons 1 and 9) and 123 in
`core`'s memory functions, the boards' registers and `asm` (reasons 3 to 5).
What remains is not rare, but it is local: it is in `core`, the boards and
the tests of raw pointers, and the user decided (2026-11-15) to mark it
before M6.

The cost of marking grows with time in two ways. The migration grows with
the code that writes these operations. More, each feature built before the
mark is designed without knowing where the boundary is: M6's generic bodies
(ADR 0027) and function pointers (deferred ideas) would each have to be
reopened to learn about `unsafe`, and every safe signature in `core` that is
not sound for all arguments (`memcpy`, `enable_interrupts`) gains callers
that a later `unsafe fn` breaks. Rust went through this twice: its 2024
edition made `std::env::set_var` an `unsafe fn` and made the lint
`unsafe_op_in_unsafe_fn` warn by default, both as edition migrations.

## Decision

### The rule

An **unsafe operation** is one of the five below. Each is allowed only
directly inside an `unsafe` block; anywhere else it is
`ty::unsafe-required`. An `unsafe` block allows unsafe operations and
changes nothing else: every check of the language (types, moves, linear
values, the borrow check, shared statics, `critical`) holds inside it as
outside, and it generates the same MIR as a plain block.

1. **An access through a raw pointer.** Reading, writing or borrowing (`&`,
   `&mut`) a place reached through a raw pointer: `*p`, `p[i]`, `(*p).f`,
   `(*s)[i]` and a range of `*s`, through a thin pointer, a slice, a volatile
   pointer or a pointer to a peripheral type (`(*u).thr`), and a method call
   whose receiver borrows such a place. A place reached through a reference
   is not one: `*r` is safe.
2. **`asm`**, except the body of a `#[naked]` function (below).
3. **A call to a function whose body the compiler does not check**: an
   `extern fn`, whatever its signature, or a `#[naked]` function.
4. **A read, write or borrow of an `extern static`** or `extern static mut`,
   or of a part of one.
5. **A call to an `unsafe fn`**, as a free function, a function of a type or
   a method (`x.f()`), at run time or at compile time.

Not unsafe, because none of them reads or writes memory the program has not
proven it may: `&raw const` and `&raw mut` of any place, a place through a
raw pointer and an `extern static` included (the offset of `&raw const p[i]`
is a GEP without `inbounds`, ADR 0021, so it wraps, as ADR 0007's
arithmetic does); casts between pointers and integers (ADR 0019) and from
references to raw pointers; comparing pointers; `slice(p, n)` and `len(s)`;
a `static mut` (ADR 0093 checks it); peripherals and their registers
(ADR 0058); calls to an `#[export]` function that is neither `unsafe` nor
`#[naked]`. Returning `&*p` from a function, which ADR 0118 allows, borrows
through a raw pointer and so needs `unsafe`; ADR 0118's rules are unchanged.

**Where the rule applies.** The same in every body of HIR, each checked on
its own with no `unsafe` block around it: a function, a function of a type, a
handler, the entry and panic functions, and every body that runs at compile
time (a `const` or `static` initializer, a `comptime` item, an enum's
discriminant, a bit-field struct's bits and defaults, and each value HIR
lowers on its own: an array's length, a repeat count, `#[align]`, a value of
the `memory` block, a register's offset, a peripheral's address). At compile
time the evaluator still checks every access it makes and stops on one it
cannot make (ADR 0027); `unsafe` is required there too, so that a function
means the same at both times. **Coverage is lexical**: an operation is
covered by the `unsafe` blocks around it in the source, wherever it is
evaluated. A `comptime(..)` operand, an `asm` `const` operand included, is
an expression of the body it is written in, typed with it and evaluated from
it; HIR makes no body of it, so it is checked once, with that body, under the
`unsafe` blocks around it.

**Expressions already in error.** An operation whose operand or callee has
an erroneous type (an error was already reported for it) counts as unsafe
when deciding whether a block is used, and is never reported as
`ty::unsafe-required`: an error is not followed by others it caused.

### `unsafe { }`

```
let x: u32 = unsafe { *p };
unsafe {
    (*regs).ctrl = 1;
}
```

`unsafe` followed by a block is an expression, typed and evaluated as the
block is (ADR 0015). As a statement it needs no `;`, as `critical` does not.
It may hold `return`, `break` and `continue`, and may be nested in, or hold,
`critical`, `loop` and any other block. Like any block it is a value, not a
place: `unsafe { *p } = 1` is rejected as an assignment to a block is, and
`unsafe { *p = 1; }` is the form that writes.

**Every `unsafe` block is used.** A block is used when an unsafe operation
is directly inside it: inside it and not inside an `unsafe` block nested in
it. Each block that is not used is `ty::unused-unsafe`, at its `unsafe`
keyword; a nested one also gets a label at the `unsafe` block around it. Two
nested blocks with no operation are two errors. There are no warnings in the
compiler; the error keeps every `unsafe` in a program a mark of something
real.

### `unsafe fn`

```
// Writes `n` bytes of `c` from `dest`.
// Safety: `dest` points to `n` bytes the caller may write.
pub unsafe fn fill(dest: *mut u8, c: u8, n: usize) {
    let mut i: usize = 0;
    while i < n {
        // Sound: `i < n`, and the caller vouches for `n` bytes at `dest`.
        unsafe { dest[i] = c; }
        i += 1;
    }
}
```

`unsafe fn` declares a function whose callers must uphold a condition the
compiler does not check; a call to it is unsafe operation 5. The keyword
goes after `pub` and before `fn`: `#[export] pub unsafe fn`. It does **not**
make the body an `unsafe` block: an unsafe operation inside an `unsafe fn`
needs its own `unsafe` block. An `unsafe fn` whose body has no unsafe
operation is not an error: its contract may be about state the compiler does
not see. The two meanings stay apart: `unsafe fn` is a contract with the
caller, `unsafe { }` the place where the body relies on its own reasoning.

`unsafe` is allowed on a function with a body, at module level or in an
`impl`, `#[export]` and `#[naked]` functions included (a C caller is not
affected; an Erebos caller needs `unsafe`, which it needs for a naked
function anyway). Elsewhere it is `parser::misplaced-unsafe`: before
`extern fn` (every call to one is already unsafe), `entry fn`, `panic fn` and
`interrupt fn` (no Erebos code calls them, so no caller can uphold a
contract), `impl`, and any item that is not a function.

### Naked functions

The body of a `#[naked]` function stays exactly one `asm` expression, with
no `unsafe` block around it: an `unsafe` block there is `ty::naked-body`,
with a help saying that `#[naked]` is the mark. The mark is at the calls
instead: a call to a naked function from Erebos code is unsafe operation 3,
as a call to an `extern fn` is, since in both the compiler checks nothing of
what the callee does. Calls that are not Erebos calls do not change: LLVM's
calls to `__aeabi_uldivmod`, the hardware's jump to `_start`, a `sym`
operand inside another `asm`. Rust marks the attribute instead
(`#[unsafe(naked)]`); Erebos marks the calls, so that every place where safe
code reaches unchecked code is found by searching for `unsafe`.

### `core`

- `core::arch::enable_interrupts()` becomes `pub unsafe fn` on both targets,
  so the portable item keeps one signature (ADR 0080). Its contract: it is
  not called inside a `critical` block or an `interrupt fn`, directly or
  through any depth of callers. That is a property of every path to the
  call, which a function can establish only for itself when nothing calls
  it: **the entry function**, outside every `critical` block in it. So in
  `rt/`, `tests/boards/` and `tests/e2e/`, `enable_interrupts` is called in
  an `unsafe` block only from the entry function, outside `critical`; any
  other function that calls it is itself an `unsafe fn` with the same
  contract, which its callers carry up to the entry. Like ADR 0117's rule,
  this is checked when a change is reviewed. A check through the call graph
  can come later and make the function safe again without breaking a
  program. (Setting up what the handlers use before enabling them, which
  ADR 0086 advises, is not part of the contract: a static is initialized at
  startup, ADR 0030, so a handler that runs early reads defined values.)
- `memcpy`, `memmove`, `memset` and the twelve `__aeabi_mem*` functions of
  `rt/builtins.ers` become `#[export] unsafe fn` (with `pub` where they have
  it today): each writes through pointers it cannot check. C and the code
  LLVM generates call them as before; where they call one another, the call
  is in an `unsafe` block. They are the only functions of `rt/` that take a
  raw pointer.
- Every other function of `rt/` keeps a safe signature, with its `asm` and
  accesses in `unsafe` blocks inside: `fence`, `compiler_fence`,
  `wait_for_interrupt` and `interrupts_enabled` are sound in every context;
  the `nvic_*` functions are sound for every argument (`nvic_address`
  asserts `irq < 496`) and in every context (inside `critical`, PRIMASK
  still masks a line `nvic_enable` enables); `__critical_enter` and
  `__critical_exit` are called by the code `critical` generates, not by
  Erebos source; `__erebos_udivmoddi4` takes a reference.

In `rt/` and `tests/boards/`, each `unsafe fn` says its contract in a
comment above it (`Safety: ...`), and each `unsafe` block is preceded by a
comment saying why it is sound. This is checked when a change is reviewed,
not by the compiler; the test programs are not under it.

### Where it is checked

`unsafe` is checked in type checking, after a body is typed: whether `*p`
reaches through a raw pointer or a reference depends on the type of `p`, and
the blocks are the structure of HIR, which MIR does not keep. A pass of its
own, `src/ty/unsafety.rs`, walks each typed body with the stack of `unsafe`
blocks around the current expression, marks the innermost one used at each
unsafe operation or reports `ty::unsafe-required`, and reports the blocks
left unused. It reads the typed body only; it does not change it, and
nothing after type checking sees `unsafe`: MIR building lowers an `unsafe`
block as a block, and the validator, the analyses, the evaluator and code
generation do not change. Whether a function is `unsafe` or `#[naked]` is
part of its signature in HIR, which the pass reads at each call.

### Diagnostic codes

| Code | When |
|------|------|
| `parser::misplaced-unsafe` | `unsafe` before anything but `fn` with a body, or `{` in an expression |
| `ty::unsafe-required` | an unsafe operation outside an `unsafe` block; the label names the operation ("this dereferences a raw pointer", "this calls an `extern fn`", "this calls a naked function", "this `asm`", "this reads an `extern static`", "this calls `unsafe fn f`"); in an `unsafe fn`, a help says its body is not an `unsafe` block |
| `ty::unused-unsafe` | an `unsafe` block with no unsafe operation directly inside it; for a nested block, a label at the one around it |
| `ty::naked-body` (existing) | gains the case of an `unsafe` block as the body, with a help saying `#[naked]` is the mark |

One error per operation, at the operation: an expression that is two unsafe
operations (`*f_extern()` with a raw result) gives two.

### Conformance tests

- ui tests, per layer:
  - parsing: `unsafe` blocks as expressions and statements, `unsafe fn` at
    module level, in an `impl`, with `#[export]` and `#[naked]`; each
    misplaced position;
  - type checking: each of the five operations rejected outside a block and
    accepted inside one, a call to an `#[export] unsafe fn` and to a naked
    function among them; each safe operation listed above accepted outside
    one; an `unsafe fn` body without a block rejected; an unused block, a
    nested unused block, two nested empty blocks (two errors); a block in a
    `const` initializer, an item-level `comptime` block, an enum's
    discriminant and an array's length; a `comptime(..)` operand and an
    `asm` `const` operand holding an unsafe operation, inside an `unsafe`
    block (covered, checked once) and outside one (one error); a `sym`
    operand naming a naked function, which is no call; an operation on an
    expression in
    error (one error, no `ty::unused-unsafe`); `return`, `break` and
    `continue` out of an `unsafe` block; `unsafe { *p } = 1` rejected;
    `asm` as a naked body accepted without a block and `ty::naked-body` with
    one;
  - MIR: a body with and without an `unsafe` block around the same
    operations gives the same MIR.
- `tests/rejected.rs`: a snippet for each new code; and, mechanically, for
  every `unsafe` block of the programs of `tests/e2e/` that is not inside
  another `unsafe` block, the program with that one `unsafe` removed (its
  block kept) is rejected with `ty::unsafe-required`. Such a block is used,
  so an operation directly in it is left uncovered. A nested block is left
  out: removing its `unsafe` leaves its operations covered by the block
  around it, and the program is rightly accepted.
- `tests/e2e/`: every program migrated, with the same output; a new program
  that builds a safe interface (a ring over static memory, a fill through an
  `unsafe fn`) on `unsafe fn` and `unsafe` blocks, on both targets at both
  levels.

### What stays out

- Unsafe attributes (Rust's `#[unsafe(no_mangle)]`): an `#[export]` name
  that clashes is a link error today, not undefined behavior in Erebos.
- `unsafe` on types, traits or `impl` blocks: Erebos has none of what Rust
  marks that way (`Send`, `Sync`, unsafe traits).
- `extern fn` declarations that state a call is safe (Rust 2024's `safe fn`
  in `unsafe extern` blocks): every call stays unsafe; it can come later
  without breaking programs.
- Checking `enable_interrupts` against `critical` and handlers: a later
  analysis, which would let it be safe again.
- Generic bodies (M6): see Consequences.

## Consequences

- With the convention for `enable_interrupts` kept, a function without
  `unsafe` relies only on the checks of the compiler and on the contracts of
  the `unsafe fn`s it calls inside its own `unsafe` blocks; a review of a
  program's soundness starts from its `unsafe` blocks and `unsafe fn`s,
  which a search for `unsafe` finds, naked functions and C included.
- Every program changes: `rt/`, the boards, the programs of `tests/e2e/`,
  the ui tests that write these operations (about 250 to 290 of 709 files),
  the snippets of `tests/rejected.rs`, the programs embedded in the Rust
  tests (`tests/elf.rs`, `tests/e2e.rs`, `tests/svd.rs` and the chip program
  under `tests/svd/chips/`), and the generated programs of ADR 0068, whose
  calls to `extern fn opaque_*` go into `unsafe` blocks. 38 programs of
  `tests/e2e/`, 2 ui tests and one program of `tests/e2e.rs` call
  `enable_interrupts`. The 72 `#[naked]` bodies do not change; the Erebos
  calls to them gain blocks.
- `unsafe` becomes a keyword. No program uses it as a name today; the SVD
  generator escapes the lexer's keywords (ADR 0071).
- ADR 0080 is unchanged: `enable_interrupts` changes the same way on both
  targets, and its `// portable:` lines say `unsafe fn`.
- **M6 owes**: generic bodies are checked per instantiation (ADR 0027);
  whether `*x` is unsafe can depend on the type argument. M6 decides whether
  a block is `ty::unused-unsafe` only when no instantiation uses it, and
  whether a function's `unsafe` can depend on a `comptime` parameter.
- **Function pointers owe**: when they come (deferred ideas), their type
  says whether the function is `unsafe`, and a call through an `unsafe` one
  is unsafe operation 5.
- ADR 0030's alternative "requiring `unsafe` for `static mut`" stays
  rejected for a new reason: ADR 0093 checks `static mut`.
- ADR 0117's count of raw pointers is unchanged by this ADR: `unsafe` marks
  where a raw pointer is used, not where one is declared. The roadmap gains
  the count of `unsafe` blocks and `unsafe fn`s per directory.

## Alternatives considered

- **No `unsafe`, as today**: the checks of M5 then hold "except where they
  do not", with no mark of where; the migration and the redesign of later
  features grow until it is added.
- **Only dereferences of raw pointers**: leaves `asm`, which writes any
  memory and any register, and C, which the compiler does not see, unmarked:
  the operations that break the most stay the quietest.
- **Dereferences and `asm`, not `extern`**: less migration in the `*_c`
  programs, but a C function can do anything a raw pointer can.
- **`unsafe` on `static mut`, as Rust**: ADR 0093 already rejects the races
  Rust's mark guards against.
- **Address computation through a raw pointer as unsafe, as Rust**
  (`&raw const (*p).f`): a simpler rule, "`*` of a raw pointer is unsafe",
  but it marks code that cannot be undefined: Erebos's offsets wrap and emit
  no `inbounds`. Linker symbols are named with `&raw const` of an `extern
  static`, and stay safe.
- **The body of an `unsafe fn` as an `unsafe` block** (Rust before 2024):
  joins the caller's contract and the body's reasoning in one keyword; Rust
  spent an edition separating them.
- **An unused `unsafe` block accepted, or a warning**: accepted, stale
  blocks pile up after changes and dilute the mark; a warning needs a
  mechanism for warnings in the compiler, which is a decision of its own.
- **`enable_interrupts` left safe, with the hole recorded**: `unsafe` would
  be born with an exception known to break ADR 0093's check.
- **A check that only the entry function calls `enable_interrupts`, outside
  `critical`**: fully checked, and the function could stay safe, but it is a
  new rule in type checking and forbids enabling from a helper; the
  convention above gives the same programs with a review instead.
- **The contract of `enable_interrupts` with no convention**: discharging
  it would take reasoning over the whole program, and a safe function
  calling it could be unsound with nothing marking it.
- **Calls to a naked function safe, with the limit recorded**: a safe
  `#[naked] fn poke(addr: usize, v: u32)` would let safe code write any
  memory, and the search for `unsafe` would miss it.
- **`unsafe fn` required on every naked function**: the same effect at the
  calls, but it marks the declarations that only LLVM or the hardware call,
  where no caller reads the mark.
- **`unsafe { asm(...) }` as the body of a naked function**: a second mark
  of what `#[naked]` already says, in 72 functions.
- **The check in MIR, as an analysis**: MIR has no blocks, so it would need
  `unsafe` regions recorded in it for this one check; Rust moved its check
  from MIR to the typed tree for the same reason.
