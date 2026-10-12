# 0010. First target: riscv32 on QEMU `virt`

- Status: Accepted
- Date: 2026-10-12

## Context

Milestones run end to end on an emulator (see `docs/roadmap.md`), so one
concrete target must be chosen before M0. Candidates were ARM Cortex-M
(`lm3s6965evb`) and RISC-V (`virt`).

## Decision

The first and, for now, only target is:

- LLVM triple `riscv32-unknown-none-elf`, CPU `generic-rv32`,
  features `+m,+a,+c` (RV32IMAC), soft-float ABI `ilp32`.
- Machine: QEMU `qemu-system-riscv32 -machine virt -nographic -bios none`.
  - RAM starts at `0x8000_0000`; with `-bios none`, execution starts there.
  - NS16550A-compatible UART at `0x1000_0000` (used by M1 for output).
- Tests exit QEMU through the `virt` test device (SiFive test finisher at
  `0x0010_0000`) so end-to-end tests terminate with a status code.

## Consequences

- RISC-V has a small, regular ISA and a simple psABI, which keeps the first
  C ABI work (ADR 0006) manageable.
- The UART and test finisher give M0/M1 everything needed for automated tests.
- Other targets (Cortex-M, RV64) are added later behind a target abstraction;
  until then, target-specific values may live in one `target` module.

## Alternatives considered

- **ARM Cortex-M (`lm3s6965evb`)** — attractive for real boards, but needs a
  vector table and Thumb specifics from M0. Can be added later.
