# 0110. References to statics, peripherals and volatile places

- Status: Accepted
- Date: 2026-11-12
- Amends: 0023 (volatility flows from a volatile reference too), 0058 (a
  peripheral and a cluster may be borrowed; `volatile` on a reference to a
  peripheral type is an error), 0093 ("What names a static": a borrow is a
  place), 0094 ("Contexts": no reference leaves a `critical` block by
  `break`), 0105 (`&` and `&mut` of a `static mut`, a peripheral, a cluster
  and a volatile place; volatile references; a `break` out of `critical` is
  no position of a reference), 0108 (borrows of `*s` and of sub-slices
  through a volatile raw slice and of a `static mut`, errors "until M5.4c",
  allowed; volatile references to slices)

## Context

ADR 0105 made three borrows errors until M5.4c:

- `&` or `&mut` of a `static mut` or an `extern static mut`, or of a part of
  one, is `ty::reference-to-static-mut`;
- `&` or `&mut` of a place reached through a volatile pointer or a volatile
  raw slice is `ty::reference-to-volatile`;
- `&` or `&mut` of a peripheral or a cluster is `ty::reference-to-volatile`
  too.

ADR 0094 says how references meet the contexts of ADR 0078: borrowing a
`static mut` names it at the place of the borrow, as `&raw` does, and a
reference cannot outlive the `critical` block it was taken in, because "the
block's value is never a reference, and a `let` holding one is scoped and
bound once". So every use of a reference to a static is covered by the
protection of the place that borrowed it, and the rule of ADR 0078, checked by
ADR 0093, needs nothing more.

That argument has a gap. ADR 0105 lets a reference be the value of a `loop`
through its `break` values, and a `break` may leave a `critical` block:

```
let r: &mut u32 = loop {
    critical { break &mut COUNT; }
};
*r += 1;   // after the block, with interrupts enabled
```

The ways a value leaves a block are its tail, `break`, `return` and `?`. The
tail is covered (ADR 0105), `return` and `?` never carry a reference (no
function returns one, ADR 0094), and a `let` outside cannot be assigned one
(it is bound once). `break` is the one left.

Peripherals are places of a peripheral type, whose registers are accessed
volatile because of the type (ADR 0058): a pointer to one is written
`*mut Uart16550`, never `*mut volatile Uart16550`. Volatile pointers remain for
memory that is not a register, such as memory shared with a device (ADR
0058). `&raw` of any place gives a pointer that is not volatile, and a program
converts it with `as` (ADR 0021); a cast never gives a reference (ADR 0105).

The borrow check across these new borrows (a static is named by any
function, so a callee may reach what its caller lent it) is ADR 0111's. How
MIR holds them is ADR 0112's.

## Decision

### References to a `static mut`

`&X` and `&mut X`, with `X` a `static mut` or an `extern static mut`, are
allowed, and so are borrows of its parts: a field, an element, a sub-slice
(`&mut BUF[0..n]`). The rules of ADR 0105 that hold for any place hold for
them; `&mut` of a `static` or an `extern static` without `mut` stays
`ty::mut-borrow-of-immutable`.

A borrow **names** the static at the place of the borrow, as `&raw` does (ADR
0078, 0094). The check of shared statics (ADR 0093) counts it as a place, in
"What names a static", and so:

- in the thread context, a borrow of a static that the handler context also
  reaches is inside a `critical` block, or on a path of calls that passes
  through one (rule 1, `analysis::unprotected-static`);
- a borrow in an unmaskable context of a static that another context reaches
  is an error, as any place there is (rule 2, `analysis::unmaskable-static`);
- a borrow in the handler context needs nothing (rule 3).

A **use** of the reference names nothing: `*r`, `(*r)[i]` and passing `r` on
are not places of ADR 0093. The reference was taken where the static was
protected, and the rule below keeps every use of it inside that protection:

```
static mut SAMPLES: [u32; 8] = [0; 8];
static mut FILLED: usize = 0;

fn sum(s: &[u32]) -> u32 { ... }        // names no static

critical {
    let total: u32 = sum(&SAMPLES[0..FILLED]);
    FILLED = 0;
}
```

A static that only one context reaches is not shared (ADR 0078), and its
borrows are allowed anywhere, as its reads and writes are.

**At compile time** a borrow of a `static mut` is what `&raw` of it is (ADR
0105, "At compile time"), and a read or write through the reference is what one
through that pointer is: `eval::static-mut`, or `eval::extern-static` for an
`extern static mut`, as the evaluator reports today.

### No reference leaves a `critical` block

A **`break` whose value has a reference type**, and whose loop is outside the
innermost `critical` block around the `break`, is `ty::reference-position` at
the value, with a label naming the position "the value of a `break` out of a
`critical` block" and the help of the value of a `critical` block ("a reference
must not outlive the protection it was taken in"). This holds for any
reference, whatever it borrows: type checking does not know what a reference
holds. A `break` to a loop inside the block, and a `break` out of the block
with a value that is not a reference, are unchanged.

With this rule ADR 0094's argument is complete: the tail of a `critical`
block is never a reference (ADR 0105), no `break` carries one out of it, and
`return`, `?` and a `let` outside it cannot. Converting a reference to a raw
pointer with `as` and using the pointer outside the block stays allowed: a
raw pointer is the program's word, as a pointer taken with `&raw` inside a
block and used outside is (ADR 0093).

### References to peripherals and clusters

`&P` and `&mut P`, with `P` a peripheral, and `&P.c`, `&mut P.c`,
`&mut P.cs[i]`, with `c` a cluster and `cs` a cluster array (ADR 0064), are
allowed. Their type is `&T` or `&mut T`, with `T` the peripheral type, and
through them a program reaches registers as through a pointer to `T`:

```
fn putc(u: &mut Uart16550, byte: u8) {
    while (*u).lsr & 0x20 == 0 {}
    (*u).thr = byte;
}

putc(&mut UART0, b'h');
```

- A peripheral is not `mut`, and `&mut` of one is allowed, as `&raw mut` is:
  what may be written is the registers' business (ADR 0058).
- A register reached through a reference to a peripheral type is accessed
  volatile, as through a pointer to one (ADR 0058), with ADR 0023's guarantee.
  Writing one through a `&T` is `ty::write-through-const`, as through a
  `*const T`; a register's access mode is checked as through a pointer.
- `*r` read or written whole, a peripheral type as the type of a value, and
  `(*r)[i]` are the errors they are through a pointer (ADR 0058).
- A register, a register array, a cluster array as a whole and a bit field
  stay without an address (`ty::register-address`, `ty::bit-field-address`,
  ADR 0105): a reference to one would reach it without its access mode.
- A reference to a peripheral type is written without `volatile`, as a
  pointer to one is: `&volatile Uart16550` and `&mut volatile Uart16550` are
  `ty::volatile-peripheral`.

Peripherals stay unchecked **across contexts** (ADR 0078): a borrow of one
names no static, and M5.6 decides how a context comes to own a peripheral. The
borrow check follows them inside a function and across calls (ADR 0111).

**At compile time** a borrow of a peripheral is what `&raw` of it is, a pointer
made from its address (ADR 0058), and a register access through it is
`eval::volatile-access`.

### Volatile references

A reference may be **volatile**, as a pointer may (ADR 0013, 0023):

```
type = ... | "&" [ "mut" ] [ "volatile" ] type
```

`&volatile T` and `&mut volatile T` are the shared and the mutable volatile
reference to `T`.

- **Making one.** `&place` and `&mut place` give a volatile reference exactly
  when the place is volatile: when the last pointer or reference on its path,
  through `*`, an index of a pointer or of a slice, is a volatile one (ADR
  0023's "Places"). `&*p` and `&mut (*p).desc[i]` with `p: *mut volatile Ring`
  give `&volatile Ring` and `&mut volatile Desc`; `&(*r).len` with
  `r: &volatile Ring` gives `&volatile u32`. A variable, a parameter, a static
  and a peripheral are never volatile places, so their borrows never give a
  volatile reference. This differs from `&raw`, whose pointer is never
  volatile (ADR 0021): a program converts that pointer with `as`, and nothing
  converts to a reference (ADR 0105), so the borrow keeps the volatility or it
  would be lost with nothing at the use to show it.
- **Using one.** An access through a volatile reference is volatile, with ADR
  0023's guarantee and its errors (`ty::volatile-aggregate`,
  `ty::volatile-too-wide`, `ty::volatile-packed-field`): `*r` of a struct is
  `ty::volatile-aggregate`, `(*r).len` one access of its width.
- **Slices.** `&volatile [T]` and `&mut volatile [T]` are laid out and passed
  as `*const volatile [T]` and `*mut volatile [T]` (ADR 0108); `(*s)[i]` is
  checked against the length and is one volatile access, `len(s)` reads
  nothing.
- **Conversions.** `&mut volatile T` converts to `&volatile T` where
  `&mut T` converts to `&T`, and `&volatile [T; N]` and
  `&mut volatile [T; N]` to the volatile references to `[T]` where ADR 0108
  converts arrays to slices. A volatile reference never converts to one that
  is not, nor the other way, for the reason ADR 0024 gives for pointers: it
  would hide whether accesses are volatile.
- **Casts.** A volatile reference casts where the volatile raw pointer of its
  mutability casts (ADR 0105, "Casts"): `r as *mut volatile T`, and on to any
  pointer `as` reaches from there.
- **Kinds and positions.** `&volatile T` is copy and `&mut volatile T` is not,
  as for references that are not volatile (ADR 0105); they may be where any
  reference may be, `extern fn` and `#[export]` signatures included, passed as
  the pointer of their mutability.
- **At compile time** an access through one is `eval::volatile-access`, as
  through a volatile pointer.

The borrow check treats a volatile reference as any other (ADR 0106): what it
borrows through a raw pointer it does not follow, and what it borrows through
a reference it follows from that reference.

### Diagnostic codes

- Removed: `ty::reference-to-static-mut` and `ty::reference-to-volatile`;
  their cases are allowed. Their snippets leave the catalog of
  `tests/rejected.rs` (ADR 0069), and their ui tests become accepted ones.
- `ty::reference-position` gains the case of a `break` out of a `critical`
  block.
- `ty::volatile-peripheral` gains the case of a volatile reference to a
  peripheral type.
- `ty::mismatched-types` gains the cases where a volatile reference meets one
  that is not, with a help that says neither converts to the other.

ADR 0111 adds `analysis::borrow-conflict-in-call`.

### Conformance tests

- **ui, accepted** (`// emit: mir`): `&` and `&mut` of a `static mut`, a field,
  an element and a sub-slice of one, and of an `extern static mut`; a borrow of
  a shared static inside `critical`, and of one reached from one context only,
  outside; a borrow of a shared static in a function called only inside
  `critical`; a borrow in a maskable handler, with no `critical` (rule 3); a
  reference taken in `critical` and lent to a function that names
  no static; the wait of ADR 0086 with a borrow inside the block; a `break`
  with a reference to a loop inside a `critical` block; a reference cast to a
  raw pointer that leaves the block; `&` and `&mut` of a peripheral, a cluster
  and an element of a cluster array, and register accesses through them;
  `&*p` and `&mut (*p).f` through a volatile pointer, a volatile raw slice and
  a volatile reference, each conversion and cast above.
- **ui, rejected:** a borrow of a shared static outside `critical` in the
  thread context (`analysis::unprotected-static`), directly and in a function
  called once outside `critical`, with its path; in an unmaskable handler
  (`analysis::unmaskable-static`); a read through a reference to a `static
  mut` at compile time (`eval::static-mut`); `loop { critical { break &mut COUNT; } }`
  and the same with a reference to a local (`ty::reference-position`);
  `&mut` of a `static` (`ty::mut-borrow-of-immutable`); a write of a register
  through `&T` (`ty::write-through-const`), of a `read` register through
  `&mut T`; `*r` of a peripheral type whole; `&mut UART0.reg` and `&UART0.regs`
  (`ty::register-address`); `&volatile Uart16550`
  (`ty::volatile-peripheral`); `*r` of a struct through a volatile reference
  (`ty::volatile-aggregate`); a `&volatile u32` passed for a `&u32` and the
  other way (`ty::mismatched-types`); an access through a volatile reference
  at compile time (`eval::volatile-access`).
- **ui, LLVM IR**, at both optimization levels: accesses through a volatile
  reference and through a reference to a peripheral type are volatile loads
  and stores of the scalar's width, none merged or removed at `-O`.
- **End to end**, on both targets at both optimization levels (ADR 0077): a
  buffer in a `static mut` filled by a handler and read by the thread through
  a reference taken in `critical` (the roadmap's program); a driver taking
  `&mut` of the board's UART peripheral type; a function taking a
  `&mut volatile` reference to memory reached through a volatile pointer.

## Consequences

- A `static mut` can be lent to functions as a reference, checked: the borrow
  is a place of ADR 0093, and the reference stays inside the protection it was
  taken in. Functions over buffers no longer need `&raw` and raw slices to
  reach a static.
- A driver takes `&mut Uart16550` instead of `*mut Uart16550`, and the borrow
  check sees two `&mut` of one peripheral in use together (ADR 0111). Sharing
  peripherals between contexts stays unchecked until M5.6.
- Memory shared with a device that is not a register can be lent as a
  volatile reference; whole reads of aggregates through it stay errors, as
  through a volatile pointer.
- `loop { critical { break &x; } }` was accepted for a reference to a local
  since M5.4a and is now rejected. No test or program of the repository
  writes it.
- Two diagnostic codes disappear. Programs that got them are now accepted,
  so nothing that compiled stops compiling except the `break` above.
- Volatile references add a qualifier to every place that matches on
  reference types: the type checker, MIR, code generation, the evaluator
  (ADR 0112).

## Alternatives considered

- **Borrows of a `static mut` allowed only inside `critical`, as a rule of
  type checking** — simpler to state, but it rejects borrows of statics that
  one context reaches, which ADR 0078 lets the program read and write freely,
  and duplicates what ADR 0093 already decides over the call graph.
- **The `break` checked by the analysis, for references to statics only** —
  accepts `loop { critical { break &x; } }` for a local, but needs the depth
  of `critical` tracked per borrow in a second place that explains the rule;
  rejecting every such `break` costs no program of the repository and can be
  relaxed later.
- **A borrow of a volatile place giving a reference that is not volatile, as
  `&raw` does** — one kind of reference, but the volatility would be lost
  with no cast at the use to show it, since nothing converts to a reference.
- **Volatile references left an error until M5.7** — less to build now, but
  the roadmap lists them here, and drivers of memory shared with a device
  (xv6's virtio rings) want them; adding them later breaks no program either
  way.
- **References to peripherals left to M5.6, with handles** — M5.6 decides how
  a context gets a peripheral; a reference to one is how a function borrows
  it, whatever owns it, and ADR 0058's pointers already allow the same
  accesses unchecked.
