# 0111. Borrows of statics and peripherals, across calls

- Status: Accepted
- Date: 2026-11-12
- Amends: 0106 (statics and peripherals as roots; parts of a peripheral
  overlap; the conflicts of a call with what its callee reaches)

## Context

ADR 0106 checks references inside one function: a borrow is in use until the
last use of what holds it, and an access to an overlapping place conflicts
with it by a table of kinds. Its places start at a variable, a parameter, a
value the compiler keeps or a static. Second-class references keep that
check inside one function (ADR 0094) because no other function can name a
caller's variable: a callee reaches the caller's memory only through the
references it is given, and accesses through them are accesses through the
borrow.

ADR 0110 lets a program borrow a `static mut`, a peripheral and a cluster. A
static or a peripheral is named by any function, so a callee can reach what
its caller lent it, with no reference in between:

```
static mut COUNT: u32 = 0;

fn bump(r: &mut u32) {
    COUNT += 1;     // the same memory as *r
    *r += 1;
}

bump(&mut COUNT);
```

The same happens to a borrow that is not lent but held across the call:

```
let r: &mut [u8; 16] = &mut BUF;
refill();           // writes BUF
(*r)[0] = 1;
```

The roadmap asks M5.4c to reject "a `&mut` passed for a static its callee
also names", which the check of shared statics (ADR 0093) does not see: it
counts places per context, not borrows.

Inside one function, M5.4a left statics without a path in the analysis
(roadmap, "M5.4a done"): only immutable statics could be borrowed, and a
borrow of one conflicts with nothing. With `static mut` and peripherals, `let
r: &mut u32 = &mut COUNT; COUNT = 2; *r = 1;` must be the conflict that the
same code over a variable is.

Registers of one peripheral may share bytes: a `read` and a `write` register
at one offset (ADR 0058), alternate registers (ADR 0061). Two peripherals
share bytes when one is declared `over` the other (ADR 0066).

## Decision

### Statics and peripherals are roots

The places the borrow check follows start at a variable, a parameter, a value
the compiler keeps, a **static** (with or without `mut`, `extern` or not) or a
**peripheral**. Fields, elements, sub-slices and dereferences of a reference
follow as before (ADR 0106, 0108).

- Two different statics never overlap, and a static and a peripheral never
  do. Memory the compiler did not lay out can break this (two `extern
  static`s a linker places at one address, a symbol at a peripheral's
  address); that is the program's word, as an access through a raw pointer
  is.
- Two peripherals overlap when their bytes meet, which ADR 0058 allows only
  for one declared `over` the other (ADR 0066).
- **Parts of a peripheral always overlap**, as elements of an array do: two
  registers, two clusters, two elements of a register or cluster array, and a
  register and a cluster, whatever their offsets. `&mut UART0.ch[0]` and
  `UART0.ctrl = 1` conflict while the borrow is in use. The same holds for
  parts reached through a reference to a peripheral type: `(*u).thr` and
  `(*u).lsr` are one place for the check.

Inside one function, ADR 0106's table applies to them unchanged:

```
let r: &mut u32 = &mut COUNT;
COUNT = 2;          // a write while a `&mut` borrow is in use
*r = 1;             // analysis::borrow-conflict
```

### Across calls

**During a call**, the borrows in use are those ADR 0106 says: what the
call's arguments may hold, and what is held by anything used after the call.
For each of them whose place starts at a static or a peripheral, the
**callee's accesses** to that static or peripheral are those of the functions
the call reaches: the callee and every function it reaches through calls.
A check that may call the **panic function is not followed**: the panic
function never returns, and it is given no reference (its one parameter is a
`*const core::PanicInfo`, ADR 0053), so once it runs no borrow is used again,
which is ADR 0106's rule for paths that do not return. A call of another
function returning `!` is followed as any call: it may be given the
reference and use it after its own access to the static. So is a call of the
panic function written by name in the program (`on_panic();`): only the call a
check makes is left out, in every function followed, those reached through a
`!` call included. Each access of a
function followed this way, to a place that overlaps the borrowed one,
conflicts with the borrow by ADR 0106's table:

| Access in a reached function | conflicts with a borrow in use that is |
|------------------------------|-----------------------------------------|
| read (a copy, a test of a tag, `&raw const`, `len`) | mutable |
| `&` borrow | mutable |
| write (an assignment, a call's result, an `asm` output, `&raw mut`), a `sym` operand naming a `static mut` or an `extern static mut` | shared or mutable |
| move, or `end` | shared or mutable |
| `&mut` borrow | shared or mutable |

- An access through the callee's own reference parameters is an access
  through the borrow, as inside one function: `*r += 1` in `bump` is the use
  the borrow is for.
- A `sym` operand gives the static's address to assembly, which may do
  anything with it, so for a `static mut` or an `extern static mut` it counts
  as a write of the whole static, in a reached function and in the function
  holding the borrow alike (`let r: &mut u32 = &mut COUNT; asm(..., sym
  COUNT); *r = 1;` is `analysis::borrow-conflict`). A `sym` of a static
  without `mut` is no access: assembly may not write it, and `f(&TABLE)` with
  `f` passing `sym TABLE` keeps compiling as it did.
- Why the panic function is not followed: it is mandatory, and programs print
  from it, often through the UART peripheral they also lend to drivers. A
  driver `write(u: &mut Uart16550, s: &[u8])` indexing `s` has a check, and a
  panic function writing `UART0.thr` would make every `write(&mut UART0, ...)`
  an error, though the reference is never used again once the panic function
  runs. References carry no LLVM attribute (ADR 0105), so the compiler assumes
  nothing either.
- What is reached through a raw pointer is not followed, as in one function,
  and neither is code outside the program: an `extern fn` reaches nothing,
  and `sym f` is no call (ADR 0093).
- `f(&BUF)` with `f` reading `BUF` is accepted, as `let r: &[u8; 16] =
  &BUF; let x: u8 = BUF[0];` is in one function; `f(&mut S.a)` with `f`
  touching only `S.b` is accepted, as two fields of one struct are.
- The check does not depend on contexts: it holds in the thread context, in
  handlers, and in functions nothing calls.
- A check of the function holding the borrow conflicts with nothing for the
  same reason: no borrow is in use across it.
- ADR 0093 still follows the checks for contexts: a panic function reached by
  a check runs in the context of the check, and its places count there.

### Diagnostic

| Code | Case |
|------|------|
| `analysis::borrow-conflict-in-call` | a call during which a borrow of a static or a peripheral is in use, whose callee reaches an access to it that conflicts |

Each error is one call and one borrow in use. It has:

- the **primary label** on the call, saying which static or peripheral the
  callee reaches while it is borrowed;
- a label on the **borrow**, as ADR 0106 labels a site, and, when the borrow
  is held by something used after the call, a label on that **later use**;
- a label for **each further call** on the path from the callee to the
  access, and one on the **access**, saying what it does ("written here",
  "borrowed `&mut` here"), in files of their own when they cross files (ADR
  0049), as ADR 0093 shows a path;
- a **help**: pass the reference on instead of naming the static in the
  callee, or end the borrow before the call.

The path is the shortest, with ties broken as ADR 0093's search breaks them;
of several conflicting accesses, the first found is labelled. A call with
several borrows in conflict is one error per borrow.

```
error[analysis::borrow-conflict-in-call]: `COUNT` is borrowed during this call, and the callee writes it
   ╭─[main.ers:9:5]
 3 │     COUNT += 1;
   ·     ─────┬────
   ·          ╰── `COUNT` is written here
 ...
 9 │     bump(&mut COUNT);
   ·     ──┬──────┬─────
   ·       │      ╰── `COUNT` is borrowed `&mut` here
   ·       ╰── `bump` reaches `COUNT` while it is borrowed
   ╰────
  help: pass the reference on instead of naming `COUNT` in the callee, or end the borrow before the call
```

The texts are snapshots, and may be reworded without a new ADR; the code and
what each error labels may not.

### When it runs

The conflicts inside one function are found by the borrow analysis of each
body, before evaluation (ADR 0094, 0099). The conflicts across calls need the
callees, so they are found over the program's assembled MIR, with the check
of shared statics (ADR 0093): at `--emit=mir` and every later stage, after the
analyses of one body passed. A program with an error in one body reports that
first, and these on the next run.

Compile-time evaluation runs bodies before this check has seen the whole
program, as it does for the check of shared statics. The evaluator runs a
conflict across a call as the program says it, reading and writing the one
memory in order, so nothing it computes depends on the rule; the program is
rejected afterwards.

### Conformance tests

- **ui, rejected** (`// emit: mir`, `analysis::borrow-conflict-in-call`):
  `bump(&mut COUNT)` with `bump` writing `COUNT`; with `bump` reading it; with
  the access two calls down, across modules in files of their own; `&BUF`
  lent to a function that writes `BUF`, `&raw mut BUF` and a `sym` of `BUF`;
  a borrow held across a call that writes the static, labelling the later use;
  `fail(&mut COUNT)` with `fail` returning `!`, writing `COUNT` and then
  `*r`; `&mut UART0` lent to a function that writes a register of `UART0`,
  and a peripheral `over` another; two borrows in one call, two errors; the
  same conflict in a function nothing calls and in a maskable handler.
- **ui, rejected** (`analysis::borrow-conflict`, in one function): a write of
  a `static mut` while a `&mut` of it is used later; `f(&mut BUF, &BUF[0])`;
  `&mut UART0.ch[0]` with a register of `UART0` written while it is in use;
  `let a = &mut (*u).ch[0]; (*u).ctrl = 1;` with `a` used after, `u` a
  `&mut` to a peripheral type; a `sym` of a `static mut` while a `&mut` of it
  is used later.
- **ui, accepted**: `f(&BUF)` with `f` reading `BUF`; `f(&mut S.a)` with `f`
  writing `S.b`; a borrow that ends before the call; a callee that reaches
  the static only through its reference parameter; a callee that reaches the
  static through a raw pointer from a `const`; an `extern fn` given a borrow
  of a static; a recursion over a reference to a static; `write(&mut UART0,
  s)` with `write` indexing `s` and a panic function writing `UART0`; a body
  holding `&mut S` with a check of its own and a panic function writing `S`;
  `f(&TABLE)` with `f` passing `sym TABLE`, `TABLE` a `static`.
- **`tests/rejected.rs`:** `analysis::borrow-conflict-in-call` gets a snippet
  (a `static mut`, a function writing it and taking a `&mut u32`, and a
  function lending the static to it), checked at `--emit=mir`.
- **Unit tests** of the analysis over hand-built MIR: a static and a
  peripheral as roots; parts of a peripheral overlapping; two peripherals
  whose bytes meet.

## Consequences

- A static lent to a function cannot be reached by it another way at the same
  time, so a `&mut` reference to a static means what one to a variable means.
- The check is the first borrow rule that looks across functions. It costs
  one search of the call graph per callee of a call made while a borrow of a
  static or a peripheral is in use, kept per callee (ADR 0112), over the
  accesses each body has to statics and peripherals.
- A function reached only after a check fails may touch a lent static freely:
  the reference is dead by then. A function returning `!` that is reached by
  an ordinary call is checked, which rejects a callee that writes a lent
  static on a path that then diverges without using the reference; following
  which `!` calls are given the reference would accept it later, without
  breaking programs.
- A callee that only reads a static is still rejected under a `&mut` borrow of
  it, even when the read is of a part that the caller does not touch through
  the reference, as in one function.
- Treating parts of a peripheral as one place rejects borrowing one cluster
  while writing a register of the same peripheral. Comparing offsets would
  accept it; that can be added without breaking programs.
- Peripherals are borrowed and checked here, while their sharing across
  contexts stays M5.6's (ADR 0078).

## Alternatives considered

- **Any naming of the static in a reached function conflicts** — simpler, but
  it rejects `f(&BUF)` with `f` reading `BUF`, which the same code in one
  function accepts; the table and the paths are already the borrow
  analysis's.
- **Rejecting any borrow of a static lent to a call** — no whole-program check,
  but the roadmap's program lends a static's buffer to a function in
  `critical`, which is the point of references to statics.
- **Recording in each body which statics it reaches, during building** — the
  per-body analysis runs before the callees exist, and recursion needs the
  whole graph anyway.
- **`analysis::borrow-conflict` for these too** — one code for one rule, but
  the error is reported by another check, at another stage, labels a path
  through other functions, and is fixed by changing the callee; a code of its
  own tells the reader which.
- **Registers of a peripheral compared by bytes** — more precise, but the
  analysis would read offsets that MIR's projections do not carry, and
  alternate registers make overlap more than "the same register".
- **Following the checks to the panic function, as ADR 0093 does for
  contexts** — one walk for both checks, but every driver lent the UART that
  the panic function prints through would be rejected, for a reference that
  is never used again (review of this ADR, finding 1).
- **Not following any call of a function returning `!`** — accepts more, but
  is unsound: such a function may be given the reference and use it after its
  own access to the static.
- **A `sym` of any static as a write** — would reject `f(&TABLE)` where `f`
  passes `sym TABLE` to assembly, which compiles today; assembly may not write
  a static without `mut`.
- **Peripherals not followed until M5.6, as raw pointers** — `putc(&mut
  UART0, &mut UART0)` would be accepted, two `&mut` of one device in use
  together.
