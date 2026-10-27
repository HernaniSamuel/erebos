# 0061. Alternate registers

- Status: Accepted
- Date: 2026-10-26
- Amends: 0058 (registers in different alternate groups may share
  bytes)

## Context

ADR 0058 lets two registers share a byte only when one is `read` and the
other `write`, as the 16550's receive buffer and transmit holding
register at offset 0. The 16550 has more: with the DLAB bit of its line
control register set, offsets 0 and 1 are the divisor latch, `dll` and
`dlm`, both read-write, over `rbr`/`thr` and `ier`. A program sets DLAB,
writes the divisor, and clears DLAB. ADR 0058 rejects such a declaration
(`ty::register-overlap`), and its Consequences leave the program a
second view of the device made with `as`, which no check sees. The
roadmap left to M3.3, whose program configures the UART through its
fields, or to M3.4 whether to relax the rule.

SVD describes such registers with `alternateGroup`, a name shared by the
registers of one mode, and `alternateRegister`, which names the one
register another overlaps. Peripherals that share addresses are
`alternatePeripheral`.

## Decision

A register may name an **alternate group** after its offset and its read
effect (ADR 0060):

```
peripheral Uart16550 {
    pub read rbr: u8 at 0x00 read modifies,
    pub write thr: u8 at 0x00,
    read write ier: Ier at 0x01,
    read write lcr: Lcr at 0x03,
    read write dll: u8 at 0x00 in dlab,
    read write dlm: u8 at 0x01 in dlab,
}
```

```
register = [ "pub" ] access NAME ":" type "at" expr [ read_effect ] [ "in" NAME ]
```

- The group's name is a label of the peripheral type, as SVD's
  `alternateGroup`: it names no item, is not resolved, and is in no
  namespace; register names and group names may coincide. A register
  without `in` is in no group. `in` is recognized there by position
  only.
- **Two registers are checked for overlap only when they are in the same
  group, or both in none**; between them, ADR 0058's rule holds (a
  shared byte is an error unless one is `read` and the other `write`).
  So `dll` may share bytes with `rbr`, `thr` and `ier`, and two registers
  of `dlab` may not share one with each other.
- Nothing else changes. Accesses to a register in a group are checked
  and built as any other: the compiler does not know which mode the
  device is in, and setting DLAB before reaching `dll` is the program's
  business, as it is in C.
- Alignment, the peripheral type's extent and its alignment count every
  register, grouped or not.
- The dumps show a register's group.

`ty::register-overlap`'s help names `in GROUP` for registers of another
mode.

## Consequences

- The 16550's divisor latch, and SVD's alternate registers, are declared
  with the rest of the device, with their offsets checked within their
  group.
- A register in a group that overlaps nothing is accepted; the group then
  only documents a mode.
- Grouping can hide a real overlap between two registers that were meant
  to be in one mode; the name written at the register says which mode
  is meant.
- Peripherals that share addresses (`alternatePeripheral`) are still
  `ty::peripheral-overlap`; M3.4, checking against a real chip's SVD,
  decides whether to relax that.

## Alternatives considered

- **Naming the register overlapped, as SVD's `alternateRegister`**
  (`dll ... over rbr`) — `dll` overlaps three registers, and `dlm`
  another; a group names the mode once per register.
- **A block for each mode** (`mode dlab { ... }` inside the peripheral
  type) — the same meaning with more syntax, and registers are no longer
  in offset order.
- **Leaving the rule to M3.4** — the milestone program of M3.3 would
  reach `dll` and `dlm` through a pointer made with `as`, outside every
  check.
- **Checking the mode at each access** (DLAB set before `dll`) — the
  compiler would need to follow the device's state through the program;
  out of scope for a language without typestate (M5).
