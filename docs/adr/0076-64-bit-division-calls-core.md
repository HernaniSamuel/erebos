# 0076. 64-bit division calls `core` where LLVM's call cannot be written

- Status: Accepted
- Date: 2026-11-03
- Amends: 0020 (on a target whose 64-bit division functions LLVM would
  call with a convention no C function has, 64-bit `/` and `%` become
  calls that code generation emits)

## Context

Neither target divides 64-bit integers in one instruction. Code
generation emits LLVM's `udiv`, `sdiv`, `urem` and `srem` on `i64`.
Where LLVM cannot expand one inline, its backend calls a run-time
function, which `core::builtins` defines in Erebos (ADR 0020). On
riscv32 those are libgcc's `__udivdi3`, `__umoddi3`, `__divdi3` and
`__moddi3`, ordinary C functions.

At `-O`, LLVM expands a 64-bit division by most constants inline, on
both targets: by a power of two into a shift or a mask, by others such
as 3 or 10 into multiplications. Compiling `x / 10`, `x / 3`, `x / 8`
and `x % 16` on `u64` with clang at `-O2` for riscv32 and for thumbv7m
emits no call. At `-O0`, and for divisors that are not constants, it
calls.

On ARM EABI targets LLVM calls `__aeabi_uldivmod` and `__aeabi_ldivmod`
instead (ADR 0074). Each returns the quotient in `r0`–`r1` and the
remainder in `r2`–`r3`. Under the AAPCS a 16-byte struct is returned
through memory, so no function with the C ABI, in Erebos or in C, has
that convention; compiler-rt and Rust's `compiler_builtins` write them
in assembly, which Erebos gets in M4.

MIR already guarantees that a division never sees a zero divisor or
`MIN / -1` (ADR 0007): those are checked before the operation.

## Decision

The target description's `div64` field (ADR 0073) says whether **LLVM's
64-bit division may be emitted**: whether every call its backend may
make for it is to a function with the C ABI, which `core` defines. It
is true on riscv32 and false on thumbv7m.

**Where `div64` is true**, nothing changes: code generation emits LLVM's
division, and LLVM expands it or calls libgcc's names, as ADR 0020 says.

**Where `div64` is false**, code generation lowers `/` and `%` on `u64`
and `i64` to a **call** to `core`'s function instead of an LLVM
division:

| Operation | Called |
|-----------|--------|
| `u64 / u64` | `__udivdi3` |
| `u64 % u64` | `__umoddi3` |
| `i64 / i64` | `__divdi3` |
| `i64 % i64` | `__moddi3` |

- The functions are the ones `core::builtins` already defines, on every
  target, found by their symbols, which `#[export]` makes those names
  (ADR 0051). Their meaning is unchanged, and they still never divide
  64-bit values themselves.
- The call is an ordinary C ABI call (`u64` in a register pair), placed
  where the division was, after the checks MIR makes. It appears in
  `--emit=llvm-ir`.
- Divisions of 32 bits and narrower stay LLVM instructions on both
  targets: riscv32 has `M`, ARMv7-M has `UDIV` and `SDIV`.
- The compile-time interpreter is unaffected: it divides 64-bit values
  itself.

LLVM's backend thus never calls `__aeabi_uldivmod` or `__aeabi_ldivmod`.

## Consequences

- riscv32 keeps what it has: inline expansion of constant divisors at
  `-O`, and libgcc's names for the rest, which C code linked into a
  riscv32 program also calls. A test with a C file dividing `u64` on
  riscv32 pins that the names stay.
- On thumbv7m every 64-bit division is `core`'s loop of 64 steps, also
  by a constant and by a power of two, since LLVM sees a call. Division
  by 10 of a `u64`, as in printing, costs the loop. Emitting LLVM's
  division when the divisor is a constant LLVM expands without a call
  is an optimization for later; it must hold at `-O0` too, where LLVM
  expands nothing.
- Two mechanisms, chosen by one field: a third target states which one
  it needs.
- C code linked into a thumbv7m program cannot divide 64-bit values
  (ADR 0074).

## Alternatives considered

- **Calling `core` on every target** — one mechanism, but riscv32 would
  lose the inline expansion of constant divisors at `-O`, every power of
  two included, for nothing: its libcalls are ordinary C functions.
- **A convention attribute accepted only in `core`**, returning a struct
  of two `u64` in `r0`–`r3`, so that `core` defines `__aeabi_uldivmod`
  and `__aeabi_ldivmod` — LLVM would keep choosing the calls and
  expanding constant divisors on ARM too, and C code would link; but the
  language gains a calling convention for two functions, and ADR 0006
  keeps special conventions for interrupt handlers and naked functions.
- **Waiting for inline assembly (M4)** — `core` could then wrap its
  functions in the EABI's convention; until then no thumbv7m program
  could divide 64-bit values, and M3.5's tests could not all run on
  both targets.
- **A triple without `eabi`**, for which LLVM uses libgcc's names — see
  ADR 0074.
- **Lowering the division in MIR** — MIR is target-independent
  (ADR 0025); whether a target's division may be emitted is code
  generation's knowledge.
