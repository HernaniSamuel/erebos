# 0032. Checks on values known at compile time

- Status: Accepted; amended by 0035 (enum variants are known), 0037 (arms a `match` never takes), 0039 (no value of an enum with payloads is known), 0059 (a known value that does not fit its field is an error; no value of a bit-field struct is known)
- Date: 2026-10-15
- Amends: 0007 (shift amounts and divisors), 0021 (array indices)

## Context

ADR 0007 rejects a "constant" shift amount not below the width, catching
`(1 << 32) - 1`, and ADR 0021 rejects a "constant" index out of bounds;
the compiler also rejects a zero divisor. When they were written, a
constant meant a literal, and the checker looks only at literals.

Since ADR 0029, constants have names, and writing them is the normal style:

```
const BITS: u32 = 32;
let mask: u32 = (1 << BITS) - 1;   // compiles, and is 0
let x: u8 = buf[IDX];              // compiles, panics at run time
let y: u32 = total / ZERO;         // compiles, panics at run time
```

The bug ADR 0007 meant to catch is back as soon as the amount has a name.

## Decision

### Values known at compile time

An expression is **known at compile time** when it is built only from
literals, `const` items, `comptime` expressions, `size_of`, `align_of`,
`offset_of`, unary `-` and `!`, binary operators, `as` and parentheses.
Its value is the one it would have at run time, computed as in ADR 0027.

Nothing that reads memory is known: not a dereference `*p`, even of a
constant pointer (it may be a device register, ADR 0029), not a read of a
`static` (ADR 0030), and not `&raw`.

### The checks

Each of these is an error when the operand is known at compile time, not
only when it is a literal:

| Check | Code |
|-------|------|
| a shift amount outside 0 to the width of the shifted type minus 1, taken with its own type's sign | `ty::shift-out-of-range` |
| a divisor of `/` or `%` that is zero | `ty::division-by-zero` |
| an array index not below the array's length | `ty::index-out-of-bounds` |

The checks apply to the compound assignments too (`<<=`, `>>=`, `/=`,
`%=`), as today for literals. The diagnostic shows the value when the
operand is not a literal (`BITS is 32`).

### Branches never taken

Code that cannot run under a condition known at compile time is not
checked by these rules; type checking still covers it:

- the branch an `if` never takes;
- the right operand of `&&` when the left one is known to be `false`, and
  of `||` when it is known to be `true`;
- the body of a `while` whose condition is known to be `false`.

```
let mask: u32 = if BITS < 32 { (1 << BITS) - 1 } else { 0xFFFF_FFFF };
let fits: bool = BITS >= 32 || (x >> BITS) == 0;
```

Otherwise configuration constants could not guard code that is only valid
for some of their values. A literal condition (`if false`) is a condition
known at compile time too.

### Where they run

Checking `comptime` expressions needs their values, which needs the
function they appear in to be type checked first (ADR 0028). These checks
therefore run after type checking, when the function is lowered to MIR,
and keep their `ty::` codes.

So that the errors found do not depend on errors elsewhere, nor on the
order in which items are computed:

- Every function that type checks is lowered to MIR and checked, whatever
  errors other functions have. The compiler stops after this phase, not
  before it; `--emit=typeck` still stops before it.
- A body in which these checks found an error is not evaluated: an
  evaluation that needs it reports nothing more, as for a body that does
  not type check (ADR 0028).
- A failure is reported once, where it happens: a known operand whose own
  computation fails, such as `A / ZERO` inside `x / (A / ZERO)`, reports
  that failure and nothing about the outer operation.

### Conformance tests

- **ui snapshots** for each check with a constant, a `comptime`
  expression, an expression of constants (`BITS - 1 + 1`) and an `as` cast
  (`IDX as usize`); a negative shift amount; each compound assignment;
  each form of code that cannot run (`if`, `&&`, `||`, `while`) with a
  known condition, not reported, and the same with an unknown condition,
  reported; `x / (A / ZERO)`, reported once; a shift by `*CTRL`, a
  constant pointer to a volatile register, accepted; a check failing in one
  function while another has a type error, both reported; a compile-time
  call to a function with such an error, reported once; and the existing
  literal cases, with the same codes and messages, moved from `typeck` to
  `mir` snapshots where they also hold type errors.

## Consequences

- `(1 << BITS) - 1` is caught whatever the amount is called.
- Code under a known condition is checked only where it can run, a first
  step towards what `comptime` parameters need in M6.
- Code that compiled in M2.3a with a named constant out of range no longer
  compiles; at run time it was wrong or panicked.
- Every function that type checks is lowered to MIR even when the program
  has errors, so a broken program costs a little more to reject.

## Alternatives considered

- **Literals only, as now** — misses the bug in the style ADR 0029
  encourages.
- **Any expression whose value the optimizer can find** — the errors would
  depend on optimization and change with unrelated code; the rule above can
  be read from the source.
- **Checking inside branches never taken** — rejects valid code guarded by
  a configuration constant, which is the reason to have one.
