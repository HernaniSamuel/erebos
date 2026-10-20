# 0014. Panic handler

- Status: Accepted; amended by 0053 (the panic function may take a `*const core::PanicInfo`)
- Date: 2026-10-12

## Context

Some operations cannot produce a meaningful result and must stop the program:
division by zero (ADR 0007) now, array bounds checks (M2) and others later.
On bare metal there is no one to report to and no process to kill, so the
language cannot decide what "stop" means. ADR 0002 forbids hidden runtime,
and there is no unwinding.

Rust's `no_std` faces the same problem and requires exactly one
`#[panic_handler]` in every binary, whether or not it can panic.

## Decision

Every program declares **exactly one panic function**, like the entry
function:

```
panic fn on_panic() -> ! {
    loop {}
}
```

- It is **always required**, even if nothing in the program can panic.
- It takes no parameters in M1 and must return `!`.
- Operations that panic call it directly; nothing else runs first.

## Consequences

- Every program has two fixed, visible points: where it starts and what
  happens when something goes wrong. Nothing is implied by the compiler.
- Adding an operation that can panic never makes a distant part of the
  program fail to compile.
- Each program pays about three lines of boilerplate.
- **To revisit (M2):** once structs and byte strings exist, the handler
  should receive the panic location and reason. Changing the signature
  breaks existing handlers, so it should happen before much code exists.

## Alternatives considered

- **Required only if the program can panic** — no boilerplate for tiny
  programs, but whether a program "can panic" is a whole-program property, so
  adding a division in one file could demand a handler elsewhere.
- **Default handler supplied by the compiler** — hidden runtime (ADR 0002).
- **A trap instruction** — without a trap vector the CPU jumps nowhere and
  loops on faults; undefined behavior in disguise.
