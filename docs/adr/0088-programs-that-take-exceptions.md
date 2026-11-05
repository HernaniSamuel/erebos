# 0088. Programs that take exceptions on purpose

- Status: Accepted
- Date: 2026-11-05
- Amends: 0077 (the event log must stay empty, except for a program that
  declares in a header that it takes exceptions)

## Context

ADR 0077 starts QEMU with the event log `-d int,guest_errors` and fails a
run whose log holds anything but three kinds of line of a correct run on
`mps2-an385`: "Any other line fails the run, including any other `Taking
exception`". The rule exists because a miscompiled program faults almost
at once and floods the log; before M4.3 no correct program took an exception.

ADR 0084 and 0085 make handlers run, and a fault reach the panic function.
Every end-to-end program they ask for takes exceptions by design: a timer
interrupt, `udf`, `svc`, a pended PendSV or NMI, a trap on riscv32. QEMU logs
about eight lines per exception on `mps2-an385` (`Taking exception 5 [IRQ]`,
`...taking pending nonsecure exception 15`, the vector read, `Taking exception
8 [QEMU v7M exception exit]`, ...), and `riscv_cpu_do_interrupt` lines on
`virt`, so none of them can pass ADR 0077's rule.

## Decision

A program of `tests/e2e` may declare, in the header comments of its root
file, the line

```
// takes-exceptions: yes
```

and it must then also declare `// expect-exit:`. The harness reads the header
as it reads the others, scanning the lines of the root file in `expectation`
(`tests/common/qemu.rs`): a repeated `yes` changes nothing, any other value
after the colon is an error of the harness that names it, and the header
without `expect-exit` is an error of the harness too.

For such a program the harness starts QEMU with the event log `-d
guest_errors` alone, on every target. The rest is checked as ADR 0077 says:
the program must stop QEMU with the exit code it expects, print exactly the
lines it expects, and leave the log empty, which now holds guest errors only.
ADR 0077's table of lines still applies, and removes nothing under `-d
guest_errors` alone. A program without the header keeps `-d int,guest_errors`
and ADR 0077's table, unchanged; the generated programs of ADR 0068 never
have it.

**What the log still catches.** On `mps2-an385` very little: an access to
unassigned memory is not a guest error there (it is a BusFault, which only
`-d int` shows), and what QEMU does log is a device refusing a setting, such
as the CMSDK UART enabled with a divisor it rejects, which ADR 0077 already
lists. The check of the log is close to void for these programs, and the
program's own checks carry the weight: its output, and its exit code.

For that reason the exit code is required, and the panic function of such a
program must not produce the expected output and exit code. The tests of ADR
0085 use `exit(99)` or a distinct code in it. A fault that the author did not
intend then reaches the panic function (ADR 0085), and the run fails by its
output or its exit code; without that, the guarantee of ADR 0085 that a
fault reaches the panic function would not help.

*A choice the user may strike when accepting this ADR:* requiring
`expect-exit`. The alternative is to leave it to the author, as ADR 0077
does, where a program without it must run forever. All the programs of M4.3
declare it, so nothing is lost by the rule, and a program that takes
exceptions and never exits would be checked by nothing at all.

M4.3 uses the header on `mps2-an385` (step 5) and on `virt` (step 6). The
harness tests in `tests/e2e.rs` cover the header: it is read, a program that
takes an exception passes with it and fails without it, showing the `Taking
exception` lines, and a program with it and a real guest error (the UART
enabled with a divisor of 1) still fails. These run on thumbv7m only; step 6's
first riscv32 program with the header is that target's test.

## Consequences

- Every program that takes no exception is still checked for a stray one, as
  before; the loss is limited to the programs that declare the header.
- In a program with the header, an exception the author did not intend (a
  miscompiled access that faults, a second fault inside a handler) is not seen
  in the log. What happens next depends on the fault, and only the program's
  output and exit code show it:
  - a fault in a handler other than HardFault's escalates to HardFault, which
    reaches the panic function (or the program's own HardFault handler);
  - a fault inside the HardFault handler, or inside a default handler of
    HardFault, is a lockup that QEMU ends with `qemu: fatal: Lockup: can't
    escalate 3 to HardFault`, exit status 3, and the same text in the log
    file; the panic function does not run, and the exit code differs from the
    expected one;
  - on riscv32 a second trap enters the trap entry again (ADR 0085), and
    reaches the panic function or recurses until the harness's timeout.
- A new header to maintain in `tests/common/qemu.rs`, the one place that
  parses headers.

## Alternatives considered

- **A header listing the line prefixes to remove** — as exact as ADR 0077's
  table, but about eight prefixes per exception on thumbv7m and a different
  set on riscv32, which change with QEMU's version, and each program would
  repeat them. The precision is for exceptions whose vector a program already
  prints or counts.
- **`-d guest_errors` for every program** — the simplest, but every program
  that takes no exception, which is nearly all of them, would lose the
  detection of a stray one, the reason for the rule of ADR 0077.
- **The header without requiring `expect-exit`** — see the choice above.
