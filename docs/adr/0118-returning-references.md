# 0118. Returning references

- Status: Proposed
- Date: 2026-11-14
- Amends: 0094 ("References": a return type is a position of a reference
  where this ADR allows it; "Contexts": what a `return` may carry),
  0095 (through 0107: R1 lets a body return a reference; the rule R5),
  0105 ("Where a reference may be": the return type and the returned value;
  `&f(..)` of a call returning a reference; "Signatures and the ABI"),
  0106 ("Borrows and how long they last": a call's result holds a borrow),
  0107 ("What each local may hold": a call's result; "Liveness": a `return`
  reads the return place), 0108 ("Signatures and the ABI": a reference to a
  slice may be returned), 0110 ("No reference leaves a `critical` block": a
  `return` neither), 0113 (Consequences, "Getters"), 0117 (reason 8 narrows
  to what this ADR cannot return)

## Context

ADR 0094 makes references second-class, a parameter or a `let` without `mut`
and never a field, an element, a payload, a static or a return type, so that
no lifetime is ever written and the borrow check stays inside one function.
Among its alternatives it names "references returned when tied to one
parameter", which "gives accessors and chaining with no lifetime written, and
can be added later without breaking programs; left out of M5 to keep the
first checker small", and its section on references says the same: returning
a reference tied to the one reference parameter, as Rust's elision does, "is a
later addition that would break no program". Its consequences list what this
costs: accessors that return a reference, and chaining on `&mut self`, are not
expressible, and raw pointers are what such code uses.

The evidence the roadmap waited for:

- **M5.5.** A method `&self -> &T` is the same shape; ADR 0113 says a getter
  returns a copy until this exists, and a part of a non-copy type has no
  getter at all.
- **M5.7.** ADR 0117 counted what stays a raw pointer in `rt/`,
  `tests/boards/` and `tests/e2e/`. Of 59 candidate sites (reasons 2, 6, 7
  and 8), 53 are reason 8 without the two fields of `core::PanicInfo`, 89.8
  percent, above the 60 percent at which ADR 0117 opens this ADR. The plan of
  M5.7 (step 7, Decisions) and the roadmap ("After M5") record what the 53
  are:

  | Sites of reason 8 | Count | What they are |
  |-------------------|------:|---------------|
  | return types, a byte string in static memory chosen by a value | 29 | `fn reason(r: core::PanicReason) -> *const [u8]`, `name(m: Mode)`, `state_name`, ...: an enum, an integer or a `bool` by value, no reference parameter; two take no argument |
  | return types, a sub-slice of the argument | 3 | `rest` (`command_decoder`), `part` (`print`), `erebos_skip` (`slices`, called from C) |
  | `const`s and `static`s | 12 | `HEX_DIGITS`, `TOPICS`, `GREETING`, `EVENTS`, tables of slices (`MESSAGES`, `INPUTS`); 3 of them hold device pointers, not slices |
  | parameters | 4 | `part`'s and `erebos_skip`'s `s`, `widest`'s `table` (a slice of slices), `reader`'s `text` |
  | a field, a local | 2 | `Reader.text` of `panic_location`, one local |
  | operands of `slice(..)` | 3 | in `rest`, `part` and `erebos_skip` |

  Reason 2 (an analysis rejected the reference) has no case.

**What the 89.8 percent measures.** ADR 0117's share counts every site of
reason 8, but the idea it was measured for, a result tied to the one
reference parameter, removes only a few of them: `rest`'s return type and
its `slice(..)` operand (`&s[start..]`, which ADR 0108 allows at `start ==
len(s)`), 2 sites; and, if its callers, which store the results in a raw
static table, cast them back (`part(&*TOPICS, 0, 7) as *const [u8]`),
`part`'s return type, its parameter and its `slice(..)` operand, 3 more;
`erebos_skip` belongs to a program whose subject is raw slices returned to C,
and stays raw under reason 1. About **2 to 5 of the 53**.
Measured against that idea alone, the share would not have opened this ADR.
The 29 functions have no reference parameter to tie a result to: what they
return is static data. The 12 `const`s and `static`s and the field are
references **stored**, which ADR 0094 leaves to written lifetimes or regions.

So choosing more than the tied form, as this ADR does (see "Options"), is a
decision to build a larger feature than the one measured, not something the
measurement asked for. And what the larger feature gives those 29 sites is
readability and a checked signature, not safety: `&raw const b"off"` is the
address of read-only data and always valid, and `&*reason(..)` at the call is
sound today. The gain is that the signature says what the result is, the
callers drop `&*`, and a body that returned something else would be rejected.

Where the compiler stands:

- Type checking rejects a reference type as a return type,
  `ty::reference-position` (ADR 0105, `ReferencePosition::Return`). `&f(&x)`
  borrows a temporary of `f`'s return type, since no function returns a
  reference.
- The validator's R1 forbids a reference as a body's return type: `_0` is
  never a `Ty::Ref` (ADR 0107).
- The borrow analysis (`src/analysis/borrows.rs`, ADR 0107, 0109, 0112)
  computes, for every local of a reference type, the set of borrow sites it
  may hold (`State`, one set per such local, with no record of where a site
  came from), and which of those locals are live. A call's argument of a
  reference type is a whole local (R1), collected per call; the write of a
  call's result to a local of a reference type "sets it to nothing", because
  no call returns a reference (`Flow::def_of`). `Terminator::Return` reads
  nothing, since `_0` is never a reference. Each site has a path rooted at a
  local, a static (byte strings are read-only statics in MIR) or a
  peripheral; a borrow through a raw pointer is no site. Parameters hold no
  site at the start of the body.
- The analysis runs on one body before evaluation (ADR 0094, 0099); the
  conflicts across calls (`src/analysis/call_borrows.rs`, ADR 0111, 0112)
  read the per-body fixed points at each call, over the assembled program.
  Analyses read only MIR (ADR 0093, 0063).
- A reference is passed as a pointer and a reference to a slice as the raw
  slice of its mutability (ADR 0105, 0108); raw slices are already returned,
  in two registers on riscv32 (ADR 0025) and through `sret` on thumbv7m
  (ADR 0075).

What other languages do:

- **Rust's elision**: for a function with exactly one reference parameter
  (or input lifetime), every elided output lifetime is that one; with
  several, `&self` or `&mut self` wins; otherwise a lifetime must be written.
  With none, a returned reference needs `'static` written. A `&T` result of a
  `&mut self` method keeps `self` mutably borrowed while it lives.
- **Languages with second-class references** (Hylo, Swift's `inout`) do not
  return references as values; what they offer in their place for accessors
  is a different mechanism, outside the scope of this comparison.

## Options

| | What it allows | Removes, of the 53 | Cost |
|---|---|---|---|
| **(a) Elision alone** | a function with exactly one reference parameter returns a reference tied to it | about 2 to 5 (above) | the rule in type checking; a call's result in the borrow analysis; the check of what a body returns, with its diagnostic; R1 and the rule R5; the ABI of a returned reference |
| **(b) Elision, and read-only static data** | (a), and a function with no reference parameter returns a `&T` that borrows only read-only static data: a `static` or `extern static` without `mut`, a byte string, or a part of one | (a) and the 29 functions returning a byte string chosen by a value: about 31 to 34 | (a), and: a row of the signature rule in type checking; a clause of R5; the classification of a returned site's root (statics without `mut` accepted, others refused); the promise an `extern fn` without reference parameters makes; and a permanent meaning (below) |
| **(c) A static reference type** | a written `&static T`, first-class: in fields, arrays, `const`s, `static`s, `PanicInfo.file`, made only from read-only static data | (b), the 9 slices and tables of byte strings, `widest`'s parameter, `Reader.text` and `reader`'s parameter: roughly 43 to 46, by the plan's list (not counted site by site) | a new type form through every layer; constants and statics whose value is a reference, against ADR 0105 ("a value computed at compile time is never a reference"); R1 rewritten; a lifetime written, against ADR 0094 ("No lifetime is ever written") |
| **(d) A return-only `&static T`** | (a), and a function may declare `-> &static T`, a spelling allowed only at the top of a return type and converting to `&T` at the call; no first-class type, no constant of reference type | as (b): about 31 to 34 | as (b), and a new spelling in the parser and type checking; it writes a lifetime, against ADR 0094 |

**Decided: (b).** It is the one option that removes the common measured shape
without writing a lifetime. Its costs over (a), stated plainly:

- **A permanent meaning.** Once programs rely on it, "a reference result with
  no reference parameter borrows read-only static data" cannot be
  reinterpreted (as tied to something else, or as needing a written
  `&static`) without breaking them. Every later design of stored or static
  references inherits it.
- **The extra pieces**: the row of the signature rule, the clause of R5, the
  classification of roots in the return check (which (a) must also decide,
  since a tied function may return a byte string too), and the promise an
  `extern fn` makes (below). Each is small; together they are a second
  feature beside elision, not a free addition to it.
- **What it buys**: about 29 more sites, for readability and checked
  signatures rather than safety (see "Context").

(d) removes the same sites with an explicit spelling, and would let a
function with a reference parameter return static data without keeping its
argument borrowed. It is not chosen because it writes a lifetime, which ADR
0094 rules out, and because if (c) ever came, `&static T` at a return would be
a second spelling with a narrower meaning than the first-class type. (c)
removes most of the rest, at the cost of a lifetime written and constants of
reference type; it needs its own ADR superseding those parts of ADR 0094 and
0105. Both stay open questions.

## Decision

### The rule, read off the signature

A function may have a **reference type as its return type** (`&T`, `&mut T`,
`&[T]`, `&mut [T]`, and their volatile forms), at the top of the return type
only. The parameters of a reference type, `self` written `&self` or
`&mut self` among them, decide what the result may borrow:

| Reference parameters | Result | The result borrows |
|----------------------|--------|--------------------|
| exactly one, `&U` or `&mut U` | `&T` | through that parameter, or read-only static data |
| exactly one, `&mut U` | `&mut T` | through that parameter |
| exactly one, `&U` | `&mut T` | error: `ty::reference-return` |
| none | `&T`, not volatile | read-only static data only |
| none | `&mut T`, or volatile | error: `ty::reference-return` |
| two or more | any | error: `ty::reference-return` |

The one reference parameter is the result's **tied parameter**. Parameters
that are not references (`self` by value, integers, raw pointers, structs)
count for nothing. The rule is the signature's, written in full in the
source: the caller's check reads only the signature, and the callee's body is
checked against it. Nothing is inferred (ADR 0008): the return type is
written, and which parameter it is tied to is fixed by a rule on the written
parameter types, as the receiver's type is fixed by its `impl` (ADR 0113).

**Read-only static data** is a `static` or an `extern static` without `mut`,
a byte string, or a field, element or sub-slice of one. A `static` without
`mut` and a byte string are never written; an `extern static` without `mut`
is written by nothing in the program, and whether C writes it is C's to vouch
for, as for every `extern` item (ADR 0026, 0030).

```
fn first(s: &[u8]) -> &u8 { &s[0] }                     // tied to `s`
fn rest(s: &[u8], at: usize) -> &[u8] { &s[at..] }       // tied to `s`
fn slot(r: &mut Ring, i: usize) -> &mut u8 { &mut r.buf[i] }

impl Ring {
    pub fn peek(&self) -> &u8 { &self.buf[self.head] }  // tied to `self`
}

impl Config {
    pub fn baud(&mut self, rate: u32) -> &mut Self { self.rate = rate; self }
    pub fn parity(&mut self, p: Parity) -> &mut Self { self.parity = p; self }
}
cfg.baud(115200).parity(.None);                          // chaining

fn reason(r: core::PanicReason) -> &[u8] {              // no reference parameter:
    match r {                                            // static data only
        .DivisionByZero => &b"division by zero",
        .IndexOutOfBounds => &b"index out of bounds",
        ...
    }
}
```

A volatile result follows the same table: `&mut volatile Desc` tied to a
`&mut volatile Ring` parameter is a reborrow of part of it. The body's type
checking decides what it can produce: no reference converts between volatile
and not (ADR 0110), and a `&mut` is reached only through a `&mut` (ADR 0105),
so the table's errors are the cases no body could satisfy except through a
raw pointer.

**Several reference parameters** are an error, not Rust's `&self` rule and
not the union of what they hold: the narrow rule can be widened later without
breaking programs, and either widening would need the tied argument written
on the MIR call, which R5 avoids today.

### What a call's result borrows

At a call of such a function, the result **holds what its tied argument
holds**: the borrow sites the argument may hold at the call (ADR 0107), and,
since an argument of a reference type is lent or copied, the site of that loan
too. A call with no reference argument gives a result that holds nothing. A
borrow held by a result lasts until the last use of what holds it, as every
borrow does (ADR 0106):

```
let b: &u8 = first(&buf);
buf[0] = 1;        // analysis::borrow-conflict: `b` is used below
print_u8(*b);

let b: &u8 = first(&buf);
print_u8(*b);
buf[0] = 1;        // accepted: `b` is not used after its read
```

- **A `&mut` argument** variable or parameter is lent by a reborrow `&mut *r`
  (ADR 0106); the result holds that reborrow, so `r` and `*r` are not used
  while the result is: `let x: &mut u8 = slot(r, 0); r.head = 1; *x = 2;`
  is a conflict, and `*x = 2; r.head = 1;` is not.
- **A `&T` result of a `&mut` parameter** holds the mutable reborrow, as in
  Rust: with `fn top(&mut self) -> &u8`, `let p: &u8 = ring.top();` then a
  read of `ring.count` while `p` is used later is a conflict. The result is
  not downgraded to a shared borrow; doing so later breaks no program (see
  "Open questions").
- **A result as an argument, a receiver or a `let`.** A result is a value of
  a reference type, in the positions where one may be (ADR 0105, 0114); a
  `&mut` result is moved where it goes, and a `let` holding one is lent to
  calls as any `&mut` variable is. In a chain, `cfg.baud(1).parity(.None)`,
  each receiver is a reborrow of the previous result (ADR 0114), which holds
  what it held: `cfg` is borrowed until the last call of the chain returns.
- **Across calls** (ADR 0111): a result holding a borrow of a static or a
  peripheral keeps it in use during the calls made while the result is live,
  and a callee reaching the static against the table is
  `analysis::borrow-conflict-in-call`, with no change to that check: the
  sites in use at each call come from the per-body fixed points, which now
  carry what results hold.
- `&f(&x)` with `f` returning a reference is a reference to a reference,
  `ty::reference-position`, as `&r` is (ADR 0105). `*f(&x)`, `f(&x).field`,
  `f(&x)[i]` and `f(&x).g()` are places and calls through the result.

### What a body may return

The check of a body with a reference result follows from what the borrow
analysis already computes, with one invariant:

1. **Parameters hold nothing** at the start of the body: what the tied
   parameter refers to was borrowed by the caller, and the caller's check
   holds it.
2. **Every borrow the body makes is a site** with a path rooted where it
   borrows: a local (a variable, a parameter that is not a reference, a value
   the compiler keeps), a static, a peripheral, or the `Deref` of a local of
   a reference type. The only borrows without a site are those through a raw
   pointer (ADR 0106).
3. **Sets are closed under what a reference was made from**: a copy or move
   of a reference takes its set, a reborrow `&(*q).f` takes `q`'s set and its
   own site, and a call's result takes its tied argument's set (above). So the
   set a local holds is the transitive union of every site on every chain of
   copies, reborrows and tied calls that produced its value.

At each `return` (each `Terminator::Return` that `bb0` reaches), every site
in the set of `_0` is classified by its root:

- rooted at the **`Deref` of a local of a reference type**: accepted. It
  borrows through a reference, and whatever that reference holds is itself in
  the set, by 3, and classified on its own;
- rooted at a **static without `mut`** (byte strings included): accepted;
- rooted at a **local** without a `Deref`, at a **`static mut` or `extern
  static mut`**, or at a **peripheral** (a cluster included):
  `analysis::returned-borrow`.

What a program may return is the consequence, not a second rule:

- **parts of the tied parameter**: `&r.buf[i]`, `&s[a..b]`, `&mut
  self.ch[1]`, the parameter itself (`self` in the chain above), a copy of it,
  a reborrow of it held in a `let`, a call's result tied to it; each part with
  its checks (ADR 0105, 0108, 0110). Their sites are rooted at a `Deref`, and
  the parameter holds nothing (1);
- **read-only static data**;
- **references made through a raw pointer**, `&*p`, which have no site (2).

And what it may not: a value whose chain reaches a borrow of the body's own
memory, of a `static mut`, or of a peripheral, however it gets there. `let t:
&[u8; 4] = &local; &t[1..]` is rejected (the reborrow's set holds the site of
`&local`), and so is `first(&local)` returned (the result holds that site).

- **A local** (a variable, a parameter that is not a reference, `self` by
  value, a value the compiler keeps for `&f()`, `&CONST` or `&Config { .. }`,
  ADR 0105) is gone when the function returns. A `const` is a value with no
  address of its own; the help says to declare a `static` to return a
  reference to it.
- **A `static mut` or `extern static mut`**: the caller's check holds
  nothing for the result (or only the tied argument's borrows), so it cannot
  see which static the result reaches. Two results of such a function could
  be in use together, one `&mut`, with no conflict found, and a direct access
  of the static beside the result would conflict with nothing; in a static
  shared with handlers, the reference would also outlive the protection it
  was taken in (ADR 0094, 0110).
- **A peripheral or a cluster of a named peripheral**, for the same reason:
  each call would give a `&mut` to one device that the caller's check sees as
  independent of the others. The handle to a peripheral is a reference passed
  down (ADR 0115), and the help says to borrow it in the caller and pass it
  in.

A body checked against its signature needs no knowledge of its callers, and a
caller needs no knowledge of the body: the check stays inside one function
(ADR 0094). **Recursion** needs nothing more: a recursive call's result holds
what its argument holds, by the signature, as any call's does:

```
fn last(s: &[u8]) -> &u8 {
    if len(s) <= 1 { &s[0] } else { last(&s[1..]) }   // `&s[0]` panics on an empty `s`
}
```

**A reference made through a raw pointer may be returned** (`&*p`), as the
program's word, as ADR 0105 lets it be made and lent: refusing it would need
the analysis to track where references without a site come from, and the raw
pointer already says the program vouches. Its reach is wider than inside one
body: the signature grants its meaning once for every caller. `fn counter()
-> &u32 { &*(&raw const COUNT) }`, with `COUNT` a `static mut`, has no
reference parameter, so every call's result is "read-only static data that
holds nothing". As written it compiles only when one context owns `COUNT`
(ADR 0115): if a handler also reaches it, naming it there outside `critical`
is `analysis::unprotected-static` (ADR 0093). The shared case names it inside
a block and leaves with the raw pointer, since no reference leaves `critical`:

```
fn counter() -> &u32 {
    let p: *const u32 = critical { &raw const COUNT };
    &*p
}
```

Then every caller reads `*counter()` outside any protection while a handler
may write `COUNT`. In both forms that is the program's word, given once in
`counter`, in its signature, for every caller; the rule of shared statics sees
the `&raw const COUNT`, but not the uses of the result.

### `critical` blocks and contexts

ADR 0094's argument that no reference outlives the protection it was taken in
rests on the ways a value leaves a `critical` block (ADR 0110): the tail is
never a reference (ADR 0105), no `break` carries one out (ADR 0110), and a
`let` outside cannot be assigned one. It also said that `return` and `?` never
carry a reference. `?` still cannot: `Option` and `Result` hold no reference.
`return` now can, so:

- **A `return` whose value has a reference type, inside a `critical` block,**
  is `ty::reference-position` at the value, with the label "the value of a
  `return` out of a `critical` block" and the help of the `break` case. As
  for `break`, type checking does not know what the reference holds, and the
  rule holds for any reference, so that ADR 0094's argument rests on type
  checking alone.
- Independently, no returned reference holds a borrow of a `static mut`
  (above), so a reference to a static never leaves the function that
  borrowed it by a return either.

The rule of shared statics and peripherals (ADR 0078, 0093, 0115) is
unchanged: a borrow names its static or peripheral where it is made, a use of
a reference names nothing, and the borrows a result may hold were made by the
caller, under the caller's protection, or are of read-only data, which the rule
does not concern.

### Functions with fixed callers

- **`extern fn`** may return a reference under the same table; C vouches for
  what it returns, as it vouches for reference parameters (ADR 0105) and for
  linear values (ADR 0101). An `extern fn` with no reference parameter that
  returns a `&T` promises read-only data for the whole run, as an `extern
  static` without `mut` does; a program that cannot promise that returns a
  raw pointer and writes `&*p` at the call.
- **`#[export]`** functions may return one; their body is checked as any
  body, and C receives a pointer or the C struct of a slice (ADR 0108).
- **`#[naked]`** functions may declare one; their body is the assembly,
  which vouches for it, and has no site to check.
- **The entry function, handlers and the panic function** are unchanged: the
  first two take no parameters and return nothing or `!` (ADR 0084, 0053),
  the panic function returns `!` (ADR 0053, 0116). ADR 0111's exemption of
  the call a check makes stands: that call returns nothing.

### At compile time

`comptime` functions and functions called at compile time may return
references; the evaluator runs a `Ref` as `AddressOf` (ADR 0107) and returns
the pointer, with no change. A value computed at compile time is still never
a reference (ADR 0105): `comptime reason(.DivisionByZero)` and a `const` of a
reference type stay `ty::reference-position`, while `const N: usize =
len(rest(&B, 1));` is a `usize`. A reference made through a raw pointer to a
local of a call that has returned is `eval::dangling-pointer` when used
(ADR 0030); a borrow of such a local is rejected before evaluation, since the
analyses run first (ADR 0094).

### In MIR and the validator

Nothing new is added to MIR: `_0` may have a reference type, and a call's
destination may be a local of one. The tied argument is read off the call:
it is the call's one argument of a reference type, since arguments fit their
parameters exactly (P3, T5).

The validator (ADR 0095, 0107) changes:

- **R1**: a body's return type may be a reference, at the top only: `_0` of
  type `&T` is allowed, and, as for every local, its type reaches no other
  reference.
- **R5** (new): a body whose return type is a reference has at most one
  parameter of a reference type; when the return type is a `&mut`, that
  parameter exists and is a `&mut`; when there is none, the return type is a
  shared reference that is not volatile.

Every call to such a body then has at most one reference argument (P3), so the
analysis's reading is never ambiguous. R5 is the signature rule of type
checking, stated for MIR so that a body built otherwise fails the validator
rather than misleading the analysis.

### The borrow analysis

`src/analysis/borrows.rs` changes in four places:

1. **A call's result.** The write of a call's result to a local of a
   reference type sets it to what the call's reference argument held before
   the call (`Holds::Copy` of that argument), or to nothing when the call has
   none.
2. **Liveness.** `Terminator::Return` reads `_0` when `_0` has a reference
   type, so a borrow held by the return place is in use until the function
   returns. This is mainly for completeness: MIR building writes `_0` last,
   so the locals that held its value are what keep a borrow in use up to that
   write, and the read of `_0` covers what may stand between the write and
   the `Return` (a `critical_exit`, an `End`), and any later MIR that moves
   the write earlier. A unit test over hand-built MIR covers it.
3. **What a body returns**, the classification above, after the fixed
   points, at each reachable `Return`.
4. **The call that holds a borrow**, for the extra label of
   `analysis::borrow-conflict` (below). `State` keeps a set of sites per local
   with no provenance, so the label is found after the fixed points, only for
   an error: from the later use, the analysis walks backwards over the
   definitions of the local it reads (`Def`), following only definitions
   whose value holds the site at the fixed point: a copy to the local copied,
   a reborrow to the local it was made through, a call's result to the call's
   reference argument, recording that call. The walk goes from the later use
   backwards, so the first call met is the nearest one, and it is labelled:
   when one block defines the local twice, the later definition is the one
   that reaches the use. It keeps a set of the definitions visited, since
   loops make the graph of definitions cyclic, and visits each once. Where
   the walk forks (a join, or several definitions reaching one read), each
   branch is followed, and of the calls met first on them the one with the
   lowest block, then statement index, is labelled, so that the label does
   not depend on the order of visits (ADR 0028). In `let b: &u8 = if c {
   first(&buf) } else { other(&buf) };` the two `&buf` are two sites, each
   held only through its own call, so an error on the site of the first
   labels `first` and one on the second labels `other`. A walk that reaches
   the site's own `Ref` without a call adds no label.

The analysis still runs only on a body with at least one site: a body with
none returns only its tied parameter's borrow or references made through raw
pointers. Its cost is unchanged but for one pass over the returns, and the
walk of item 4 only on error. The check of shared statics, `call_borrows` and
the analyses of moves and linearity are unchanged; `_0 = move self` is a move
like any other (ADR 0098).

### Code generation and the ABI

A returned reference is returned exactly as a raw pointer of its mutability
and volatility: one register, or, for a reference to a slice, the raw slice,
in two registers on riscv32 (ADR 0025) and through `sret` on thumbv7m
(ADR 0075), as `-> *const [u8]` is today. ADR 0107's temporary slot for the
result of a call with a reference argument holds as written. No LLVM attribute
is given to a returned reference that a raw pointer lacks (`nonnull`,
`noalias`, `dereferenceable`), as for parameters (ADR 0105).

### Methods and peripherals

- **Getters** return a reference to a part of `self` (`fn buf(&self) ->
  &[u8; 8]`), and a part of a non-copy type has one; ADR 0113's "a getter
  returns a copy" holds only for copy values a program wants copied.
- **Chaining** on `&mut self -> &mut Self` configures a driver in one
  expression; the receiver is borrowed for the whole chain.
- **Peripherals** (ADR 0110, 0115): a method of a peripheral type may return a
  reference to one of its clusters, `fn ctx(&mut self, i: usize) -> &mut
  PlicContext { &mut self.ctx[i] }`: a reborrow through the receiver, which
  the caller borrowed where it named the peripheral. A register keeps no
  address (`ty::register-address`, unchanged), so no getter returns one. No
  function returns a reference to a named peripheral (`&mut UART0`):
  `analysis::returned-borrow`.
- `x.get().set(1)` with `get(&mut self) -> &mut Part` is a call on the
  result, borrowed as any receiver (ADR 0114); there is still no two-phase
  borrow, so `v.slot(v.len())` stays a conflict.

### Diagnostics

| Code | Case |
|------|------|
| `ty::reference-return` (new) | a reference return type the table rejects: two or more reference parameters; a `&mut` result with a `&` parameter; a `&mut` or volatile result with no reference parameter |
| `analysis::returned-borrow` (new) | a returned value that may hold a borrow of a local, of a `static mut` or `extern static mut`, or of a peripheral or a cluster |
| `ty::reference-position` (changed) | the return type is no longer a case of its own; a reference below the top of a return type (`-> Option(&T)`, `-> [&T; 2]`, `-> *const &T`, `-> &&T`) is the case of its inner position; new cases: `&` of a call returning a reference, and a `return` with a reference out of a `critical` block |
| `analysis::borrow-conflict` (changed) | when a site in conflict is held through a call's result, an extra label at that call |

`ty::reference-return` applies to every function that declares a return
type, `extern fn` and `#[export]` functions included. It is at the return
type, with a label at each reference parameter, and a help by case: "a returned reference is tied to the one
reference parameter: pass one reference, or return an index or a copy" for
several; "make the parameter `&mut`, or return a `&`" for a `&mut` result of a
`&` parameter; "with no reference parameter, a returned reference borrows
read-only static data, which is never `&mut` or volatile: return a raw
pointer" for the last case.

`analysis::returned-borrow` has its primary label at the borrow ("`x` is
borrowed here"), a label at the expression that writes the return place
("returned here"), and the help of its case (above). There is one error per
site and return: a site reaching two `return`s is two errors, at the same
borrow with different "returned here" labels. They are ordered by the span
of the borrow, then of the return, and deduplicated as the borrow analysis's
errors are (ADR 0107), so that one site reaching one return through two paths
is one error.

```
error[analysis::returned-borrow]: cannot return a reference to `total`, a local of this function
   ╭─[main.ers:4:19]
 4 │     let t: &u32 = &total;
   ·                   ───┬──
   ·                      ╰── `total` is borrowed here
 5 │     t
   ·     ┬
   ·     ╰── returned here
   ╰────
  help: return the value, or a part of the reference parameter
```

In `analysis::borrow-conflict`, a site held through a result gains a label at
the call found by item 4, "the result of `first` holds this borrow". The texts
are snapshots and may be reworded without a new ADR; the codes and what each
error labels may not.

### What stays out

- **Storing a reference**, in a field, an array, a payload, an `Option` or a
  `Result` (`-> Option(&T)` included), a `const` or a `static`: it needs
  written lifetimes or regions (ADR 0094). `PanicInfo`'s fields, tables of
  slices and the device pointers stay raw (ADR 0116, 0117).
- **Written lifetimes or regions**, `&static T` among them (options (c) and
  (d)).
- **A result tied to several parameters**, by Rust's `&self` rule or as the
  union of what they hold.
- **`split_at_mut`**: two `&mut` halves in use together need two results, and
  the language has no tuples and no binding of two references at once
  (ADR 0108). A program takes the halves one after the other.
- **A reference moved forward in a loop**: a `let` of a reference is still
  bound once (ADR 0094); a loop over a buffer keeps an index, or recurses.
- Two-phase borrows (ADR 0114) and downgrading a `&` result of a `&mut`
  parameter.

### Conformance tests

- **ui, accepted** (`--emit=typeck`, with MIR twins): each accepted row of the
  table: `&T` and `&mut T` tied to a parameter, to `&self` and `&mut self`,
  to the one reference beside `self` by value or a raw pointer; slices and
  sub-slices; a volatile result of a volatile parameter; a `&T` with no
  reference parameter returning a byte string, an immutable `static`, an
  element of one, and an `extern static` without `mut`; an `extern fn`, a
  `#[export]`, a `#[naked]` and a `comptime` function; chaining; recursion;
  a peripheral type's method returning a cluster (`fn ctx(&mut self, i:
  usize) -> &mut PlicContext`); `*f(&mut x) = 1`, `f(&x).field`, `f(&x)[i]`;
  `len(rest(&B, 1))` in a `const`; `counter` returning `&*(&raw const
  COUNT)` for a `static mut` one context owns, and the form of a shared one
  that takes the pointer inside `critical` (the program's word), with
  `&*(&raw const COUNT)` outside `critical` for a shared `COUNT` rejected as
  `analysis::unprotected-static`.
- **ui, rejected:** each case of `ty::reference-return`, on a function with a
  body, an `extern fn` and a `#[export]` function; each case of
  `analysis::returned-borrow` (a variable, a by-value parameter and `self`,
  `&f()`, `&CONST` with its help, a `static mut`, an `extern static mut`, a
  peripheral, a cluster of a named peripheral, one branch of an `if` among
  two, through a copy into a `let`, `let t: &[u8; 4] = &local; &t[1..]`,
  `first(&local)`); one site reaching two `return`s (two errors, in order)
  and one return reached by two paths (one error); `-> Option(&T)`, `-> &&T`,
  `-> *const &T`, `&f(&x)`, a `return` of a reference inside `critical`,
  `comptime` of a call returning a reference (`ty::reference-position`).
- **Analysis snapshots:** a write of the argument's place while the result is
  used later (`analysis::borrow-conflict`, with the label at the call) and
  after its last use (accepted); the join of two calls, each site labelled at
  its own call; a read of `*r` while a `&mut` result of `r` lives; a `&T`
  result of a `&mut self` method held while the receiver is read; a callee
  writing what it then returns (`let x: &mut u32 = &mut self.a; self.a = 1;
  x` rejected, because `x` is read afterwards, and `self.b = 1` accepted, two
  fields; the read of `_0` at the `Return` is covered by the unit tests); a
  chain; a result lent to a call; a
  result holding a `&mut` of a `static mut` inside `critical` during a call
  whose callee writes it (`analysis::borrow-conflict-in-call`); a result of a
  function with no reference parameter held across a write of everything
  else (accepted).
- **Unit tests of the borrow analysis** over hand-built MIR: a call's result
  takes its argument's sites, and none without a reference argument; the
  `Return` read of `_0`; the return check for each root, a reborrow of a
  local's borrow and a call result tied to one; the walk of item 4, through a
  copy, a reborrow and a join, with the tie broken by block and statement;
  the fixed points unchanged by the order of blocks.
- **Unit tests of the validator:** R1 accepting `_0: &T` and rejecting `_0`
  reaching a reference below the top; R5 for each of its clauses.
- **At compile time:** a reference made through a raw pointer to a local of a
  call that has returned, returned and read in a `const`
  (`eval::dangling-pointer`).
- **The catalog of `tests/rejected.rs`** gains a snippet for each new code
  (ADR 0069).
- **LLVM IR snapshots** at both levels: a returned `&u32` is a `ptr`, a
  returned `&[u8]` the raw slice's return on each target, with no attribute.
- **End to end**, on both targets at both levels (ADR 0077): the step's
  program, a ring buffer with `peek(&self) -> &u8` and `slot(&mut self,
  i) -> &mut u8`, a UART configured by a chain of `&mut self -> &mut Self`,
  a decoder walking a buffer with `rest(s, n) -> &[u8]`, panic reasons
  printed through `fn reason(r) -> &[u8]`; and a `#[export]` function
  returning a `&[u8]` called from C. The peripheral getter needs a device
  with a cluster, which only the riscv32 programs declare today (the PLIC's
  contexts): the step adds one on thumbv7m too, as a device the program
  declares under ADR 0077's reason for its own devices or in the thumbv7m
  board module (the plan chooses, the NVIC's register groups being the
  candidate), so that a driver reaching a cluster through a getter runs on
  both targets.
- **The migration** (ADR 0117): in the same step, the raw pointers of reason
  8 that this ADR lets a program state as references move to them (the 29
  functions and their callers' `&*reason(..)`, `rest`, `part`'s return and
  parameter if its table allows it; `erebos_skip` stays under reason 1), and
  the counts of the roadmap are taken again, the same way.

### Where it lands

A roadmap step of its own, **M5.8 Returned references**, after M5.7 is
closed and before M6: it completes the ownership model of M5, and M6's
`comptime` functions that make types would otherwise meet signatures with
reference results only later. It depends on M5.4a to M5.4c (the borrow
analysis, its roots at statics and peripherals, `call_borrows`), M5.5 (methods
and receivers) and M5.7 (ADR 0117's rule and measurement, which the step
applies again); it depends on nothing of M6. It delivers the rule, the
analysis, the diagnostics, the migration with its recount, the program above,
and `docs/reference.md` updated where it says references are not returned:

- "Where a reference may be": the example `fn returns(r: &u32) -> &u32 { r }`
  marked `ty::reference-position`, the list of positions naming "a return
  type", and "`&f(&x)` is not one of these: no function returns a
  reference";
- "Slices behind references": "A reference to a slice is never returned";
- "Signatures and the ABI" of references: "No function returns a reference";
- "The borrow check": "no call returns a reference, so nothing holds it
  afterwards";
- "References in MIR": R1's "nor a body's return type", and R5 added;
- "Functions of a type and methods", "Getters": "References are not
  returned";
- the table of diagnostic codes: `ty::reference-position`'s "a return type",
  and the two new codes.

It is about the size of M5.5's ADR 0114 in the compiler, with most of the work
in the analysis's return check and its diagnostics.

## Amends / conflicts

Nothing here contradicts the principles of an accepted ADR: no lifetime is
written, the check stays inside one function, nothing is inferred (ADR 0008),
and the analysis reads only MIR (ADR 0003, 0028, 0063, 0093). The texts it
changes, each named in the status lines when accepted:

- **ADR 0094** lists "a return type" among the positions a reference may not
  be, and says `return` and `?` never carry a reference. ADR 0094 anticipates
  the tied form as "a later addition that would break no program"; the form
  with no reference parameter (read-only static data) is **beyond what ADR
  0094 named**, and this is an amendment of it, not the addition it
  announced. Its consequence that accessors and chaining are not expressible
  ends.
- **ADR 0105**: a return type and the returned value become positions of a
  reference; `&f(&x)` is no longer "a temporary of `f`'s return type" when
  `f` returns a reference; "No function returns a reference" in "Signatures
  and the ABI".
- **ADR 0106**: "no call returns a reference, so nothing holds it
  afterwards".
- **ADR 0107** (and so **ADR 0095**): R1's "nor a body's return type"; "no
  call returns a reference" in "What each local may hold"; a `return` reads
  `_0`. The rule R5 is added.
- **ADR 0108**: "A reference to a slice is never returned".
- **ADR 0110**: the completed argument says a `return` cannot carry a
  reference out of `critical`; it now cannot because of the rule above.
- **ADR 0113**, consequences: the getter returning a copy.
- **ADR 0117**: reason 8 covers what this ADR cannot return, and the counts
  change with the migration.

**Options (c) and (d) would conflict** with ADR 0094 ("No lifetime is ever
written"), and (c) also with ADR 0105 ("a value computed at compile time is
never a reference"). Neither is chosen; either would need its own ADR
superseding those parts.

## Decided with the user

The user decided, on the draft's questions: scope (b); several reference
parameters an error, `ty::reference-return`; a `return` of a reference inside
`critical` an error, as `break`; `&*p` may be returned, as the program's word;
an `extern fn` with no reference parameter may return a `&T`, C vouching for
read-only data, as for an `extern static` without `mut`; the step is M5.8,
after M5.7 and before M6, with the migration and a recount; the codes are
`ty::reference-return` and `analysis::returned-borrow`. Each is stated in the
Decision above.

## Consequences

- Accessors, getters of non-copy parts and chaining on `&mut self` are
  written with references, checked, with no lifetime written; ADR 0094's
  stated cost of second-class references shrinks to storing them.
- The functions that return a static name or message by value read as they
  mean, `fn reason(r: core::PanicReason) -> &[u8]`, and their callers drop
  `&*`; about 29 of the measured sites of reason 8 go, for readability and a
  checked signature, not safety, since their raw form was already sound.
- "A reference result with no reference parameter borrows read-only static
  data" is a meaning fixed for good; later designs of static or stored
  references must keep it.
- A function's result can keep its argument borrowed after the call; the
  borrow check's errors gain a label at the call that explains why. A `&T`
  result of a `&mut` parameter keeps the mutable borrow, as in Rust; programs
  that want both write the read first, or take the value out.
- A function with one reference parameter that returns static data keeps
  that parameter borrowed while the result is used (the signature cannot say
  otherwise); a function that only selects static data by a value should not
  take a reference.
- A function with no reference parameter whose result is a reference can
  return only read-only static data, or what a raw pointer vouches for: no
  getter of a `static mut` or of a peripheral exists unless the program
  writes the raw pointer that grants it, once, for every caller.
- The rule on `critical` forbids even a harmless `return first(&self.buf)`
  inside a `critical` block, whose result holds only the caller's borrow; the
  program computes the value inside the block and returns after it.
- The validator gains R5 and R1 changes; the analysis gains a pass over the
  returns and, on error, a walk for the call's label; MIR, the evaluator and
  code generation change only by allowing a reference where a raw pointer
  already works.
- Stored references, `Option(&T)` results and `split_at_mut` stay raw or
  index-based; reasons 7 and 8 of ADR 0117 keep the sites that need them.

## Alternatives considered

- **Status quo, raw pointers for returns** — no work, but 32 measured return
  types and every getter of a non-copy part stay unchecked, and `&*f(..)` at
  every call says nothing the signature could not.
- **Elision alone, option (a)** — the roadmap's idea exactly, but it removes
  about 2 to 5 of the measured sites and leaves the common shape, static data
  chosen by a value, raw.
- **A static reference type, option (c)** — removes most of what remains, but
  writes a lifetime and makes constants and fields of reference type, against
  ADR 0094 and 0105.
- **A return-only `&static T`, option (d)** — the sites of (b), with the
  meaning written at the return and no tie to a parameter that is present;
  but it writes a lifetime, against ADR 0094, and would be a second spelling
  beside a first-class `&static T` if (c) ever came.
- **Returning indices or handles instead** — `fn find(&self, k) -> usize`
  and `self.buf[i]` at the use is safe today and stays the answer where a
  result would be stored; but it does not express a static name, a sub-slice
  of a parameter, or chaining, and every use repeats the indexing and its
  check.
- **Written lifetimes, as Rust** — the general solution, for stored
  references too, at the cost ADR 0094 rejected: lifetimes in signatures, a
  checker over regions, and a dependency or a large checker of its own.
- **Inferring the tie from the body** — no error for several parameters, but
  the caller's check would depend on the callee's body (the check would leave
  one function, and recursion would need a fixed point over the call graph),
  and the signature would no longer say what a result borrows, against
  ADR 0008's intent.
- **Rust's `&self` rule, or the union, for several reference parameters** —
  each accepts more programs; both can come later without breaking any, and
  both need the tied argument recorded on the MIR call.
- **Allowing a `return` of a reference out of `critical`** — sound, since no
  returned reference holds a `static mut` borrow, but the argument of ADR 0094
  would then rest on the analysis instead of type checking; relaxing the rule
  later breaks no program.
- **Allowing `&mut` results with no `&mut` parameter, unchecked** — a getter of
  a singleton (`fn uart() -> &mut Uart`) would give two `&mut` of one device
  that the check sees as independent; a raw pointer, with `&mut *p` written
  where it is used, is the honest form.

## Open questions

- **`Option(&T)` results**, the usual shape of a lookup (`find(&self, k) ->
  Option(&V)`): a reference as a type argument is storing it in a payload.
  Allowing it at the top of a return type only, with the same tie, is a
  possible next step; until then a lookup returns an index or panics.
- **Downgrading** a `&T` result of a `&mut` parameter to a shared borrow
  after the call: sound for plain data, but a shared access to a peripheral's
  registers can have effects (a read that pops a FIFO), so it is left out.
- **A `&static T`**, return-only (d) or first-class (c), for a function with a
  reference parameter that returns static data, tables of byte strings,
  `PanicInfo.file` and slices of slices. Either writes a lifetime (ADR 0094).
  If one is ever added, a signature of (b) with no reference parameter must
  keep its meaning, and the two spellings need one.
- **LLVM attributes** on returned references (`nonnull`, `dereferenceable`),
  with those on parameters, once the check is trusted (ADR 0105).
