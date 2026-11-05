# 0086. Masking and waiting for interrupts

- Status: Accepted
- Date: 2026-11-05
- Amends: 0030 and 0083 (`_start` masks interrupts first), 0052 (the
  compiler knows two more items of `core::arch` by path)

## Context

ADR 0078 defines `critical` blocks, and leaves to this step how they lower
on each target, how a program enables interrupts, and how it waits for one
without losing a wake-up. The roadmap also asks whether `_start` masks
interrupts on thumbv7m so that both targets enter the entry function alike.

At reset the two targets differ: on QEMU `virt`, `mstatus.MIE` is 0, so
riscv32 starts masked; on `mps2-an385`, `PRIMASK` is 0, so thumbv7m starts
enabled, though no source of interrupts is on yet.

A wait must not lose a wake-up: if a program tests a flag and then runs
`wfi`, an interrupt between the two sets the flag and the core sleeps
anyway. `wfi` wakes for an interrupt that is pending and enabled at its
source even when masking would keep it from being taken: on both machines,
a probe in assembly on QEMU 11.1 found that `wfi` under `PRIMASK` or with
`MIE` clear returns with the handler not yet run, and that the handler
runs when the mask is lifted. So testing and waiting inside one critical
section loses nothing.

`core::arch` is where instructions of the target are written (ADR 0080),
which rejected intrinsics in favour of `asm` in `core`. But a `critical`
block must also run at compile time (ADR 0078), where `asm` is `eval::asm`
(ADR 0079).

## Decision

### Interrupts are masked in the entry function

`_start` masks interrupts as its **first** instruction, on both targets:
`csrci mstatus, 8` on riscv32, `cpsid i` on thumbv7m. The other steps follow
as ADR 0083 and 0085 list them. The entry function starts masked, also when
a program jumps back to `_start`, and enables interrupts when it is ready.
Nothing else in `_start` depends on it: `.bss` and `.data` are filled with
no handler able to see them half done.

On thumbv7m, `PRIMASK` also turns `svc` into a HardFault, so a program uses
`svc` only after enabling interrupts and outside `critical` blocks. Faults
and NMI are not masked by it, as ADR 0078 says.

### Portable items of `core::arch`

Three functions are added to each file of `core::arch`, as portable items
(ADR 0080):

```
pub fn enable_interrupts()
pub fn wait_for_interrupt()
pub fn interrupts_enabled() -> bool
```

- **`enable_interrupts`** lets maskable handlers run (`csrsi mstatus, 8`,
  `cpsie i`). It is a compiler fence, so statics written before it are in
  memory. A program calls it once it has set up what handlers use. Each
  source of interrupts has its own enable, which is the program's: `mie` on
  riscv32, the peripheral's control register on thumbv7m (SysTick's
  `TICKINT`), and for external interrupts M4.4. Called inside a critical
  section or a handler it ends that protection, as ADR 0078 says; nothing
  checks it.
- **`wait_for_interrupt`** runs `wfi`, a compiler fence too. It may return
  for no reason (the architectures allow it), so it belongs in a loop.
- **`interrupts_enabled`** reads the mask state and nothing else: on
  riscv32, `mstatus.MIE`; on thumbv7m, that `PRIMASK` is 0 and `IPSR` is 0, so
  that a handler finds it false on both targets. It is false in a critical
  section. It says nothing of whether any source is enabled (`mie`, a
  peripheral's control register), and on thumbv7m it does not read `BASEPRI`
  or `FAULTMASK`, which a program of this model leaves at 0 (ADR 0078). It
  serves `assert`s and tests.

There is **no function that masks**: that is `critical`, the only way the
language masks with a guarantee (ADR 0078). A program that must stay masked
past a block writes `asm`.

**The wait.** This is the form ADR 0078 left to this step to show:

```
loop {
    critical {
        if DONE { break; }
        core::arch::wait_for_interrupt();
    }
}
```

An interrupt after the test stays pending: `wfi` returns at once, and the
handler runs at the end of the block, before the next test.

### How `critical` lowers

Each block has a hidden local of type `u32`, which holds what it recorded.
MIR has two new statements:

```
critical_enter _3   // `_3 = core::arch::__critical_enter()`
critical_exit _3    // `core::arch::__critical_exit(_3)`
```

- **The MIR builder** emits `critical_enter` where the block starts and
  `critical_exit` on every path that leaves it: at its end, and before the
  jump of a `break`, `continue`, `return` or `?` that crosses it, innermost
  block first, each with its own local. A value leaving (`return x`, `break
  x`, the block's own value) is computed before the exit, so it is read
  under the mask. The panic call of a check is not an exit (ADR 0078). A
  block whose end cannot be reached has no exit there.
- **The evaluator** does nothing for either statement, so at compile time
  the block runs as a block (ADR 0078), with no `eval::asm`.
- **Code generation** calls two private functions of `core::arch`, which
  the compiler names by path, as it names `_start` (ADR 0052, amended):

  ```
  // riscv32
  fn __critical_enter() -> u32 {
      let mut was: u32 = 0;
      asm("csrrci {}, mstatus, 8", out(reg) was,
          options(nostack, preserves_flags));
      was & 8
  }
  fn __critical_exit(was: u32) {
      asm("csrs mstatus, {}", in(reg) was,
          options(nostack, preserves_flags));
  }

  // thumbv7m
  fn __critical_enter() -> u32 {
      let mut was: u32 = 0;
      asm("mrs {}, primask", "cpsid i", out(reg) was,
          options(nostack, preserves_flags));
      was
  }
  fn __critical_exit(was: u32) {
      asm("msr primask, {}", in(reg) was,
          options(nostack, preserves_flags));
  }
  ```

  Neither `asm` has `nomem`, so both are compiler fences (ADR 0079), which
  is what ADR 0078 asks of the block's entry and exit, inlined or not. At
  `-O` LLVM inlines them; at `-O0` each is a call. Being private and named
  with `__`, no program can call them, so `critical` stays the only form.

### Syntax and types

`critical` is a keyword (ADR 0078). `critical BLOCK` is an expression, whose
value and type are the block's, usable wherever an expression is, a `while`
condition included (`while !critical { DONE } {}`). As a statement it follows
the rule of other block expressions (ADR 0015): no `;` is needed after it,
and its type must then be `()` or `!`. `break` and `continue` to a loop
outside it, `return` and `?` are allowed inside it. A value of any type may
leave it, as from any block.

`--emit=mir` shows the two statements. `--emit=llvm-ir` shows calls to the two
functions, declared, since dumps leave `core` out.

### Conformance tests

- **ui snapshots:** the AST of `critical` as a statement, as a `let` value
  and in a `while` condition; `critical` as a name; the types, including a
  block of type `!` and a mismatch as a statement; MIR of each exit: the
  end, `break`, `continue`, `return` with a value, `?`, and nested blocks
  crossed by one jump, in the right order with the right locals; a block
  that diverges has no exit; LLVM IR on both targets, showing the calls.
- **Compile time:** a `const` and a `comptime` block containing `critical`
  with `return`, `break` and `?` evaluate, with no `eval::asm`.
- **End to end,** one program for both targets at both levels, through
  `interrupts_enabled`: false at the entry function (also after a jump back
  to `_start`, on each target, with a `// targets:` program); true after
  `enable_interrupts`; false in a block and true after each kind of exit;
  a nested block leaves it false until the outer one ends; a block entered
  masked leaves it masked.
- **The fence, end to end:** at `-O`, `while !critical { DONE } {}` ends when
  a handler sets `DONE`, on each target: the loop reads the static each
  time.
- **The mask, end to end:** on each target a thread loop runs `critical {
  COUNT += 1; }` many times while the timer's handler also increments
  `COUNT`, and the total is exact.
- **The wait, end to end:** the form above, on each target, with a timer.
- **Generated programs (ADR 0068):** `critical` blocks as one more feature
  of the generator, with the exits it already generates, against both
  oracles.
- A test using every portable item on every target gains the three
  functions (ADR 0080).

## Consequences

- Both targets enter the entry function alike, masked, and every program
  that uses interrupts says when they start.
- A `svc` before `enable_interrupts` on thumbv7m is a HardFault, where a
  chip's reset state would have let it through.
- `critical` costs a call at `-O0` and two or three instructions at `-O`.
- Generated code shows a call for each entry and exit; `rt/arch/` shows what
  they do, as with `_start`.
- `interrupts_enabled` reads two registers on thumbv7m, so that its meaning
  is portable.

## Alternatives considered

- **Leaving each target as its reset leaves it** — one instruction less,
  but one program starts in two states, and a jump back to `_start` could
  fill `.bss` under a running handler on thumbv7m.
- **`disable_interrupts()`** — symmetrical, but it is the pair ADR 0078
  rejected: any early exit leaves the unmask out.
- **No `interrupts_enabled`** — two functions fewer in the portable list,
  but a test of `critical`'s exits would need a copy per target.
- **`wfi` before enabling, with the flag tested outside** — loses a wake-up,
  the case this ADR exists to avoid.

Defaults assumed while writing this ADR, for correction when accepting it:

- **`interrupts_enabled` reading only `PRIMASK` on thumbv7m** — simpler, but
  true inside a handler on thumbv7m and false on riscv32.
- **The two calls as ordinary MIR `Call` statements** — no new statements,
  but the evaluator would reach their `asm` (`eval::asm`) unless it recognized
  the two functions by id.
- **Code generation writing the two instructions itself** — no functions in
  `core`, and no call at `-O0`, but intrinsics are what ADR 0080 rejected:
  the instructions would be hidden in the compiler, and the two functions
  already read like what they do.
- **A `dsb` before `wfi` on thumbv7m** — Arm recommends it for portability
  across architecture versions; a Cortex-M3 does not need it, and a later
  target can add it inside `wait_for_interrupt`.
