# 0095. MIR validation

- Status: Accepted; amended by 0098 (the operand `move` and the rules M1 to
  M4), 0103 (the rules L1 to L4), 0104 (the rule E1), 0107 (T1 and T5 for
  references; the rules R1 to R3)
- Date: 2026-11-09

## Context

MIR is the core of the compiler (ADR 0003): the evaluator runs it (ADR 0027),
the check of shared statics reads it (ADR 0093), and code generation emits it.
Each of them assumes things of it that nothing checks: that a place's
projections fit its types, that a block a terminator names exists, that a
`critical` block is left on every path (ADR 0086). When MIR building breaks one,
the symptom is far from the cause: a panic of `Body::place_ty`, an `unreachable!`
in code generation, a verdict of ADR 0093's walk that is wrong in silence, or
LLVM IR that the LLVM verifier rejects (code generation panics on it,
`src/codegen/mod.rs`).

The roadmap owes a validator before M5: the analyses of moves and borrows
(ADR 0094) read MIR with the same assumptions, and each new pass over MIR makes a
validator costlier to add. rustc validates MIR after its passes, and LLVM
verifies its IR, for the same reason.

What MIR goes through today:

- `mir::build_body` and `mir::build_expr_body` build one body, on demand
  (ADR 0028); the evaluator may run it at once.
- `Queries::into_mir` assembles the whole program: bodies, statics, data,
  handlers, the vector table.
- `Mir::leave_out` drops `core` from dumps and renumbers what is kept
  (ADR 0052): the one pass that rewrites ids.

## Decision

### What it is

`src/mir/validate.rs`, part of the `mir` layer, checks the rules below and
returns every violation it finds, each with the body's name, the block, the
statement's index (or the terminator), and the rule's id. It reads only MIR and
the target's shared constants; the tables of ADR 0063 do not change.

It computes types on its own and never panics on the MIR it checks: a place
whose projections do not fit is a violation, not a call of `Body::place_ty`.
Nor does it call anything else that panics on bad input (`StructDef::fields`,
`EnumDef::payload`, `Ty::layout` of `[T]` or a peripheral type) before the
rules that make the call safe have passed.

**No cascade.** A rule whose input another rule found invalid is skipped for
that input: a place naming a local that does not exist breaks S7 only, not
also T1, T3 or T5; a static whose type has no size breaks the rule that says
so, not also P6. So one fault is one violation. Violations are listed in a
fixed order: the program's first, then by body index, block, statement (the
terminator last), and rule id.

### When it runs

It always runs, in every build, at three points:

1. at the end of building each body, by `build_body` or `build_expr_body`,
   before the body is stored or evaluated: the rules of one body;
2. after `Queries::into_mir`: the rules of the program, which include every
   body's again;
3. after `Mir::leave_out`: the same.

A pass over MIR added later runs it after itself. Its cost is linear in the
size of MIR.

### How it fails

A violation is a bug of the compiler, never of the program. The run panics once,
listing every violation found, as code generation does for the LLVM verifier:
one `panic!` in `src/mir/validate.rs`, which ADR 0063's count gains. User errors
stay diagnostics; no user input reaches this panic unless MIR building is wrong.

### The rules of one body

The id of each rule appears in its violation.

**Shape**

| Id | Rule |
|----|------|
| S1 | `locals` holds `_0` and the parameters; `_0` has type `ret` |
| S2 | an external body has no blocks and no locals other than `_0` and the parameters; any other body has at least one block |
| S3 | a naked body has one block, with no statements, whose terminator is an `asm` with `noreturn` |
| S4 | every block has as many spans as statements |
| S5 | every block a terminator names exists, and none names `bb0` |
| S6 | a body whose `ret` is `!` has no `return` |
| S7 | every local named exists: the base of a place, the index of `Offset`, `Index` and `Element`, the local of `critical_enter` and `critical_exit`, and of `PanicInfoAt::Local` |
| S8 | a local bound to read-only data (`LocalDecl::constant`) is never written: no assignment, call destination or `asm` output has it as the base of a place without a `Deref` or `Offset`, and no `&raw mut` takes such a place |
| S9 | an `asm` statement does not have `noreturn`, and an `asm` terminator does; every placeholder names an operand that exists |

**Types**

A type **fits** another when it is equal to it, or is `*mut T` where the other
is `*const T` with the same `T` and volatility: ADR 0024's conversion, which
MIR leaves implicit. ADR 0031's conversion to a slice is explicit in MIR
(`Rvalue::Slice`). A cast from a slice to a slice of the same elements, which
may change mutability and volatility (ADR 0031), is built as `Use` of the
operand into a place of the target type, and a cast from a pointer to an array
to a slice as `Slice` (`Builder::cast`); T5 accepts both as they are.

A place is **volatile** as code generation makes it (`Builder::is_volatile`,
and `address` in code generation): when the last pointer on its path that is
dereferenced (`Deref` or `Offset`) is volatile, or when a projection after it
reaches a register of a peripheral type. A `Deref` after a register starts
over from that pointer.

| Id | Rule |
|----|------|
| T1 | every projection of a place applies to the type before it: `Deref` to a pointer (thin or slice), giving its pointee; `Offset` to a thin pointer to a sized type; `Index` and `ConstIndex` to an array, a `ConstIndex` below its length; `Field` to a struct with that field, or to a peripheral type whose entry it names is no array; `Element` and `ConstElement` to a peripheral type whose entry is an array, a `ConstElement` below its length; `Tag` and `Payload` to an enum with payloads, with that variant and payload value |
| T2 | the index local of `Offset`, `Index` and `Element` is a `usize`; the local of `critical_enter` and `critical_exit` is a `u32` |
| T3 | a place read or written whole (by `copy`, as the destination of an assignment or a call, or as an `asm` output) has a type of values: not `!`, `[T]` or a peripheral type |
| T4 | a volatile place read or written whole has no aggregate type: no array, struct, slice or enum with payloads (ADR 0023); `()` is allowed, and accesses nothing |
| T5 | an assignment's rvalue has a type that fits the place's, the rvalue's type being: `Use`, its operand's, except that a slice operand may go to a slice place of the same element type whatever their mutability and volatility; `Unary`, its operand's, a signed integer for `Neg` and an integer or `bool` for `Not`; `Binary` arithmetic and bitwise, both operands' one type, an integer (or `bool` for bitwise); `Shl` and `Shr`, the left operand's, both integers; comparisons, `bool`, from operands of one type up to fitting, integers, `bool` or thin pointers, `bool` only with `==` and `!=`; `Cast`, its target, neither side an array, struct, slice or enum with payloads; `AddressOf`, a pointer to the place's type (a slice for `[T]`), `mut` as written, not volatile; `Slice`, assigned to a slice place of elements `T`, of any mutability and volatility, from a thin pointer to `T` or to `[T; N]` and a `usize` length; `SliceAddress`, the thin pointer to the elements of the slice in the place, with its mutability and volatility; `SliceLen`, `usize`, from a slice |
| T6 | an integer constant has no bit set above its type's width |
| T7 | `if` tests a `bool`; `switch` tests an integer or a `bool`, with distinct values that fit its width, 0 and 1 for `bool` |
| T8 | the message of a failed `assert` is a `*const [u8]` |
| T9 | an `asm` input, and the place of an output, is an integer of at most 32 bits or a thin pointer (ADR 0079) |

**`critical` blocks**

The **open blocks** at a point of a body are a stack of locals: on a path from
the start of `bb0` to that point, each `critical_enter` pushes its local and
each `critical_exit` pops one. The **depth** is the stack's height.

| Id | Rule |
|----|------|
| C1 | every block reached from `bb0` has one stack of open blocks at its start, whatever the path: two paths into it with equal depths but different locals break the rule |
| C2 | a `critical_exit` pops the local it names, which is on top of the stack; at depth 0 it breaks the rule |
| C3 | a `return` is at depth 0 |

A block nothing reaches is not checked against C1 to C3; MIR building may leave
such blocks (ADR 0093).

### The rules of the program

| Id | Rule |
|----|------|
| P1 | every body follows the rules of one body |
| P2 | every body named exists: a callee, a `sym`, `entry`, `panic`, and, where they are not `None` (after `leave_out` they may be), `reset`, `critical_enter`, `critical_exit`, a handler and a vector table word; every static named is that static, compared by the address of its leaked definition, not by id, so that a reference left from before `leave_out` renumbered it is caught: an operand, a `sym`, a panic site's file (`Mir::statics[id].def`); likewise every peripheral an operand names is in `Mir::peripherals`; every relocation's target and every read-only data named exists: a local's, a panic site's, a default handler's |
| P3 | a call passes as many arguments as the callee has parameters, each fitting its parameter's type, and a destination, if any, that the callee's return type fits |
| P4 | a body that is not `comptime` calls no `comptime` body and names none with `sym`; `entry`, `panic`, `reset` and the handlers are not `comptime` |
| P5 | `entry` and `panic` return `!`; `entry` and every handler take no parameter; `panic` takes none, or one thin pointer; `critical_enter` takes none and returns `u32`; `critical_exit` takes one `u32` |
| P6 | every static's `def.id` is its index in `Mir::statics`, and its type is a type of values (as in T3); an external static has no bytes; any other has as many bytes as its type's size |
| P7 | every relocation, of a static or of read-only data, lies inside its bytes (its offset plus the pointer size at most their length), and its addend is at most its target's size |
| P8 | in a body that is not `comptime`, a `panic` terminator carries a site exactly when the panic function takes a parameter; in a `comptime` body it carries none, since code run at compile time never calls the panic function (ADR 0053); a site in a local (`PanicInfoAt::Local`) names a local of the type that parameter points to |

Types name their structs, enums and peripheral types by leaked definitions
shared by reference; the lists of `Mir` that hold them are for dumps, and are
not checked against the types.

These rules state what MIR building does today. A rule today's MIR breaks is a
bug of MIR building, fixed with a test in the step that adds the rule, or a
rule stated wrong here, corrected by an amending ADR before that step goes on.

### How the rules grow

The rules above are the list this ADR starts with, as ADR 0063's tables are.
A step that adds to MIR (a kind of operand, statement, projection or type)
adds the rules its additions must follow, with new ids, in its own ADR, which
says `Amends: 0095`; M5.2's moves and M5.4's references are the first.

### Tests

- A unit test per rule builds MIR that breaks it and only it, and checks that
  the violation names that rule; the valid MIR beside it reports nothing.
- A test per point of "When it runs" shows that the validator runs there,
  through the code that runs it, not by calling it directly: since valid MIR
  passes silently, the other tests would pass as well if it never ran.
- Every program the tests compile, ui, e2e, `core`, the generated programs of
  ADR 0068 and the snippets of ADR 0069, passes the three points: the validator
  runs in every build, so the existing tests are its tests on valid MIR.

## Consequences

- A bug of MIR building is reported where MIR is made, with the rule it breaks,
  instead of as a panic in a later layer or a wrong answer of an analysis.
- The analyses of M5 may rely on the rules without checking them again.
- A new kind of statement, operand or projection must be added to the validator,
  which matches every variant so that the compiler points at it.
- A compiler with a bug of MIR building panics where it once might have produced
  a working program by chance.
- The count of panics of ADR 0063 grows by one.

## Alternatives considered

- **Only in debug builds or tests** (`cfg(debug_assertions)`) — the tests would
  still catch bugs, but a release build would emit code from MIR known to be
  wrong; the cost of always running it is small.
- **An internal-error diagnostic** (`internal::invalid-mir`) instead of a
  panic — would keep ADR 0063's count, but would present a bug of the compiler
  as an error of the program.
- **Validating only the whole program after `into_mir`** — simpler, but the
  evaluator runs bodies before that point, and analyses of one body (ADR 0094)
  run on them before it.
- **Inside `src/analysis/`** — the analyses judge programs, and report to the
  user; the validator judges the compiler, as LLVM's verifier belongs to LLVM.
