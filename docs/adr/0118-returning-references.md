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
  are, and that the number overstates what the idea as written would remove:

  | Sites of reason 8 | Count | What they are |
  |-------------------|------:|---------------|
  | return types, a byte string in static memory chosen by a value | 29 | `fn reason(r: core::PanicReason) -> *const [u8]`, `name(m: Mode)`, `state_name`, ...: an enum, an integer or a `bool` by value, no reference parameter; two take no argument |
  | return types, a sub-slice of the argument | 3 | `rest` (`command_decoder`), `part` (`print`), `erebos_skip` (`slices`, called from C) |
  | `const`s and `static`s | 12 | `HEX_DIGITS`, `TOPICS`, `GREETING`, `EVENTS`, tables of slices (`MESSAGES`, `INPUTS`); 3 of them hold device pointers, not slices |
  | parameters | 4 | `part`'s and `erebos_skip`'s `s`, `widest`'s `table` (a slice of slices), `reader`'s `text` |
  | a field, a local | 2 | `Reader.text` of `panic_location`, one local |
  | operands of `slice(..)` | 3 | in `rest`, `part` and `erebos_skip` |

  Reason 2 (an analysis rejected the reference) has no case. So the idea as
  the roadmap defines it, a result tied to the one reference parameter,
  removes the 3 sub-slice functions and what goes with them, a handful of
  the 53. The 29 functions have no reference parameter to tie a result to:
  what they return is static data, which a reference tied to nothing would
  state. The 12 `const`s and `static`s and the field are references
  **stored**, which ADR 0094 leaves to written lifetimes or regions.

Where the compiler stands:

- Type checking rejects a reference type as a return type,
  `ty::reference-position` (ADR 0105, `ReferencePosition::Return`). `&f(&x)`
  borrows a temporary of `f`'s return type, since no function returns a
  reference.
- The validator's R1 forbids a reference as a body's return type: `_0` is
  never a `Ty::Ref` (ADR 0107).
- The borrow analysis (`src/analysis/borrows.rs`, ADR 0107, 0109, 0112)
  computes, for every local of a reference type, the set of borrow sites it
  may hold, and which of those locals are live. A call's argument of a
  reference type is a whole local (R1), collected per call; the write of a
  call's result to a local of a reference type "sets it to nothing", because
  no call returns a reference (`Flow::def_of`). `Terminator::Return` reads
  nothing, since `_0` is never a reference. Each site has a path rooted at a
  local, a static (byte strings are read-only statics in MIR) or a
  peripheral; a borrow through a raw pointer is no site.
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
- **Hylo, Swift**: references are not returned; projections (`subscript`,
  `yield`) lend a part of an argument to the caller for the duration of an
  access, which is a coroutine, not a value.

## Options

The question this ADR must answer first is how far to go, given that the
measured sites are mostly not the shape the idea names.

| | What it allows | Removes, of the 53 | Cost |
|---|---|---|---|
| **(a) Elision alone** | a function with exactly one reference parameter returns a reference tied to it | 2 to 3 return types (`rest`; `part`, whose results still go into a raw table; `erebos_skip`, which may stay raw as the test's subject, reason 1), 2 parameters, 3 `slice(..)` operands: about 5 to 8 | the rule in type checking; a call's result in the borrow analysis; the check of what a body returns, with its diagnostic; R1 and a new validator rule; the ABI of a returned reference |
| **(b) Elision, and read-only static data** | (a), and a function with no reference parameter returns a `&T` that borrows only read-only static data: a `static` or `extern static` without `mut`, a byte string, or a part of one | (a) and the 29 functions returning a byte string chosen by a value: about 34 to 37 | (a), and one more case in each of its rules: which roots a returned reference may borrow (the check of (a) must decide immutable statics anyway), and a signature with no reference parameter |
| **(c) A static reference type** | a written `&static T`, first-class: in fields, arrays, `const`s, `static`s, `PanicInfo.file`, made only from read-only static data | (b), the 9 tables and slices of byte strings, `widest`'s parameter, `Reader.text` and `reader`'s parameter: about 46 to 49, by the plan's list (not counted site by site) | a new type form through every layer; constants and statics whose value is a reference, against ADR 0105 ("a value computed at compile time is never a reference"); R1 rewritten; a lifetime written, against ADR 0094 ("No lifetime is ever written") |

**Recommended: (b).** It is the idea ADR 0094 anticipated, with the one case
the measurement shows to be the common one, at almost no cost over (a): once a
body's returned borrows are checked, the immutable statics are a root to
accept or refuse, and accepting them is sound, since a borrow of a place that
is never written conflicts with nothing (ADR 0106's table: only writes, moves
and `&mut` borrows conflict with a shared borrow, and none reaches such a
place). A function without reference parameters then means, by its signature
alone, "the result borrows static data", which is what the 29 functions are.
(a) alone builds the whole mechanism for a few sites and leaves the common
shape raw. (c) removes most of the rest but writes a lifetime, which ADR 0094
rules out and the roadmap's rule for M5.7 ("if the safe form needs a written
lifetime or region, leave it raw") treats as the boundary; it would need its
own ADR superseding that part of ADR 0094, and nothing in (b) closes the way
to it (see "Open questions").

The rest of this ADR is (b).

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

The returned value of a function with a reference result (its tail, and the
value of each `return`) may hold only:

- **borrows through a reference**: a borrow whose place is reached through
  the `*` of a reference (the tied parameter, a copy of it, a `let` holding
  a reborrow of it, the result of a call tied to one of these), with what that
  reference held. Fields, elements, sub-slices and clusters through it are
  parts of the parameter: `&r.buf[i]`, `&s[a..b]`, `&mut self.ch[1]`, each
  with its checks (ADR 0105, 0108, 0110). The tied parameter itself returned
  (`self` in the chain above) is one of these;
- **borrows of read-only static data**: a `static` or an `extern static`
  without `mut`, a byte string, or a field, element or sub-slice of one;
- **references made through a raw pointer**, `&*p`, which the borrow check
  holds nothing for (ADR 0105, 0106): the program's word, as everywhere.

Anything else is `analysis::returned-borrow`, at the borrow:

- a borrow of a **local**: a variable, a parameter that is not a reference
  (`self` by value included), or a value the compiler keeps for `&f()`,
  `&CONST` or `&Config { .. }` (ADR 0105): the local is gone when the
  function returns. A `const` is a value with no address of its own; the
  help says to declare a `static` to return a reference to it;
- a borrow of a **`static mut` or `extern static mut`**: the reference would
  outlive the protection the borrow was taken in (ADR 0094, 0110), and the
  caller's check could not see that it reaches the static;
- a borrow of a **peripheral or a cluster**: each call would give a `&mut` to
  one device that the caller's check sees as holding nothing, so two of them
  could be in use together; the handle to a peripheral is a reference passed
  down (ADR 0115), and the help says to borrow it in the caller and pass it
  in.

A body checked against its signature needs no knowledge of its callers, and a
caller needs no knowledge of the body: the check stays inside one function
(ADR 0094). **Recursion** needs nothing more: a recursive call's result holds
what its argument holds, by the signature, as any call's does:

```
fn last(s: &[u8]) -> &u8 {
    if len(s) == 1 { &s[0] } else { last(&s[1..]) }   // a reborrow of `*s`
}
```

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
  rule holds for any reference.
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
  returns a `&T` promises read-only data for the whole run; a program that
  cannot promise that returns a raw pointer and writes `&*p` at the call.
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

`src/analysis/borrows.rs` changes in three places:

- **A call's result.** The write of a call's result to a local of a reference
  type sets it to what the call's reference argument held before the call
  (`Holds::Copy` of that argument), or to nothing when the call has none.
- **Liveness.** `Terminator::Return` reads `_0` when `_0` has a reference
  type, so a borrow held by the return place is in use until the function
  returns.
- **What a body returns.** After the fixed points, at each `Return` reachable
  from `bb0`, each site `_0` may hold is checked: a site whose path starts
  with the `Deref` of a local of a reference type is accepted (what that
  local held is in the set too, and checked on its own), a site rooted at a
  static without `mut` is accepted, and every other site, rooted at a local
  without a `Deref`, at a `static mut` or at a peripheral, is
  `analysis::returned-borrow`.

The analysis still runs only on a body with at least one site: a body with
none returns only its tied parameter's borrow or references made through raw
pointers. Its cost is unchanged but for one pass over the returns. The check
of shared statics, `call_borrows` and the analyses of moves and linearity are
unchanged; `_0 = move self` is a move like any other (ADR 0098).

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
  PlicContext`. A register keeps no address (`ty::register-address`,
  unchanged), so no getter returns one. No function returns a reference to a
  named peripheral (`&mut UART0`): `analysis::returned-borrow`.
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

`ty::reference-return` is at the return type, with a label at each reference
parameter, and a help by case: "a returned reference is tied to the one
reference parameter: pass one reference, or return an index or a copy" for
several; "make the parameter `&mut`, or return a `&`" for a `&mut` result of a
`&` parameter; "with no reference parameter, a returned reference borrows
read-only static data, which is never `&mut` or volatile: return a raw
pointer" for the last case.

`analysis::returned-borrow` has its primary label at the borrow ("`x` is
borrowed here"), a label at the expression that writes the return place
("returned here"), and the help of its case (above). One error per site and
return; ordered and deduplicated as the borrow analysis's errors are
(ADR 0107).

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
the call, "the result of `first` holds this borrow"; when several calls chain
it, the one nearest the later use is labelled. The texts are snapshots and may
be reworded without a new ADR; the codes and what each error labels may not.

### What stays out

- **Storing a reference**, in a field, an array, a payload, an `Option` or a
  `Result` (`-> Option(&T)` included), a `const` or a `static`: it needs
  written lifetimes or regions (ADR 0094). `PanicInfo`'s fields, tables of
  slices and the device pointers stay raw (ADR 0116, 0117).
- **Written lifetimes or regions**, `&static T` among them (option (c)).
- **A result tied to several parameters**, by Rust's `&self` rule or as the
  union of what they hold: an error now, which a later step may relax without
  breaking programs.
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
  reference parameter returning a byte string, an immutable `static` and an
  element of one; an `extern fn`, a `#[export]`, a `#[naked]` and a
  `comptime` function; chaining; recursion; `*f(&mut x) = 1`, `f(&x).field`,
  `f(&x)[i]`; `len(rest(&B, 1))` in a `const`.
- **ui, rejected:** each case of `ty::reference-return`; each case of
  `analysis::returned-borrow` (a variable, a by-value parameter and `self`,
  `&f()`, `&CONST` with its help, a `static mut`, an `extern static mut`, a
  peripheral, a cluster of a named peripheral, one branch of an `if` among
  two, through a copy into a `let`); `-> Option(&T)`, `-> &&T`, `&f(&x)`,
  a `return` of a reference inside `critical`, `comptime` of a call returning
  a reference (`ty::reference-position`).
- **Analysis snapshots:** a write of the argument's place while the result is
  used later (`analysis::borrow-conflict`, with the label at the call) and
  after its last use (accepted); a read of `*r` while a `&mut` result of `r`
  lives; a `&T` result of a `&mut self` method held while the receiver is
  read; a chain; a result lent to a call; a result holding a `&mut` of a
  `static mut` inside `critical` during a call whose callee writes it
  (`analysis::borrow-conflict-in-call`); a result of a function with no
  reference parameter held across a write of everything else (accepted).
- **Unit tests of the borrow analysis** over hand-built MIR: a call's result
  takes its argument's sites, and none without a reference argument; the
  `Return` read of `_0`; the return check for each root; the fixed points
  unchanged by the order of blocks.
- **Unit tests of the validator:** R1 accepting `_0: &T` and rejecting `_0`
  reaching a reference below the top; R5 for each of its clauses.
- **The catalog of `tests/rejected.rs`** gains a snippet for each new code
  (ADR 0069).
- **LLVM IR snapshots** at both levels: a returned `&u32` is a `ptr`, a
  returned `&[u8]` the raw slice's return on each target, with no attribute.
- **End to end**, on both targets at both levels (ADR 0077): the step's
  program, a ring buffer with `peek(&self) -> &u8` and `slot(&mut self,
  i) -> &mut u8`, a UART configured by a chain of `&mut self -> &mut Self`,
  a decoder walking a buffer with `rest(s, n) -> &[u8]`, panic reasons
  printed through `fn reason(r) -> &[u8]`; and a `#[export]` function
  returning a `&[u8]` called from C.
- **The migration** (ADR 0117): in the same step, the raw pointers of reason
  8 that this ADR lets a program state as references move to them (the 29
  functions and their callers' `&*reason(..)`, `rest`, `part`'s return and
  parameter, `erebos_skip` unless it stays under reason 1), and the counts of
  the roadmap are taken again, the same way.

### Where it lands

A roadmap step of its own, **M5.8 Returned references**, after M5.7 is
closed and before M6: it completes the ownership model of M5, and M6's
`comptime` functions that make types would otherwise meet signatures with
reference results only later. It depends on M5.4a to M5.4c (the borrow
analysis, its roots at statics and peripherals, `call_borrows`), M5.5 (methods
and receivers) and M5.7 (ADR 0117's rule and measurement, which the step
applies again); it depends on nothing of M6. It delivers the rule, the
analysis, the diagnostics, the reference updated (`docs/reference.md`,
"References" and "Functions of a type and methods"), the migration and the
program above. It is about the size of M5.5's ADR 0114 in the compiler, with
most of the work in the analysis's return check and its diagnostics.

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

**Option (c) would conflict** with ADR 0094 ("No lifetime is ever written")
and ADR 0105 ("a value computed at compile time is never a reference"). It is
not recommended here; choosing it would need its own ADR superseding those
parts.

## Decisions for the user

1. **Scope: (a), (b) or (c).** Recommended: **(b)**, for the reasons under
   "Options". (a) is the roadmap's idea as written and removes about 5 to 8
   of the 53 measured sites; (b) about 34 to 37; (c) about 46 to 49 but writes
   a lifetime.
2. **Several reference parameters: error, Rust's `&self` rule, or the
   union.** Recommended: **an error** (`ty::reference-return`), narrow and
   widened later without breaking programs. Rust's rule ties to `self` when
   there are others; the union (the result holds what every reference
   argument holds, as an `if` of two borrows does in ADR 0106) is sound and
   needs no error, but keeps every argument borrowed. Either would need the
   tied argument written on the MIR call, which R5 avoids today.
3. **A `return` of a reference inside a `critical` block: an error, or
   allowed.** Recommended: **an error**, as `break` is (ADR 0110), so that the
   argument of ADR 0094 rests on type checking alone; the analysis's refusal
   of `static mut` borrows would make it sound to allow, and it can be
   relaxed later.
4. **A reference made through a raw pointer may be returned** (`&*p`), as the
   program's word. Recommended: **yes**, as ADR 0105 lets it be made and lent;
   refusing it would need the analysis to track where references without a
   site come from, and the raw pointer already says the program vouches.
5. **`extern fn` returning a reference with no reference parameter** (C
   promising read-only data). Recommended: **allowed**, by the same table: it
   is what `-> *const [u8]` and `&*p` at the call state today, unchecked
   either way.
6. **Where it lands: M5.8, after M5.7, before M6**, with the migration of the
   measured sites in the same step. Recommended as stated; the alternative is
   to fold it into M6, which has nothing it depends on.
7. **The names of the two codes**, `ty::reference-return` and
   `analysis::returned-borrow`.

## Consequences

- Accessors, getters of non-copy parts and chaining on `&mut self` are
  written with references, checked, with no lifetime written; ADR 0094's
  stated cost of second-class references shrinks to storing them.
- The functions that return a static name or message by value read as they
  mean, `fn reason(r: core::PanicReason) -> &[u8]`, and their callers drop
  `&*`; about two thirds of the measured sites of reason 8 go.
- A function's result can keep its argument borrowed after the call; the
  borrow check's errors gain a label at the call that explains why. A `&T`
  result of a `&mut` parameter keeps the mutable borrow, as in Rust; programs
  that want both write the read first, or take the value out.
- A function with one reference parameter that returns static data keeps
  that parameter borrowed while the result is used (the signature cannot say
  otherwise); a function that only selects static data by a value should not
  take a reference.
- A function with no reference parameter whose result is a reference can
  return only read-only static data: no getter of a `static mut` or of a
  peripheral exists; those are borrowed where they are named.
- The validator gains R5 and R1 changes; the analysis gains a pass over the
  returns; MIR, the evaluator and code generation change only by allowing a
  reference where a raw pointer already works.
- Stored references, `Option(&T)` results and `split_at_mut` stay raw or
  index-based; reasons 7 and 8 of ADR 0117 keep the sites that need them.

## Alternatives considered

- **Status quo, raw pointers for returns** — no work, but 32 measured return
  types and every getter of a non-copy part stay unchecked, and `&*f(..)` at
  every call says nothing the signature could not.
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
- **Elision alone, option (a)** — the roadmap's idea exactly, but it removes a
  handful of the measured sites and leaves the common shape, static data
  chosen by a value, raw, for a difference of one case in rules (a) builds
  anyway.
- **A static reference type, option (c)** — removes most of what remains, but
  writes a lifetime and makes constants and fields of reference type; a
  separate decision (see "Open questions").
- **Projections, as Hylo's `yield`** — lends a part for the duration of an
  access without returning a value; it needs coroutines or a new form of
  call, much more than a reference result.
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
- **A `&static T`** (option (c)) for tables of byte strings, `PanicInfo.file`
  and slices of slices: if it is ever added, a signature of (b) with no
  reference parameter would read as returning one, and the two spellings
  would need one meaning.
- **LLVM attributes** on returned references (`nonnull`, `dereferenceable`),
  with those on parameters, once the check is trusted (ADR 0105).
