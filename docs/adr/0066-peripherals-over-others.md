# 0066. Peripherals over others

- Status: Accepted
- Date: 2026-10-28
- Amends: 0058 (a peripheral may name another it overlaps; the two are
  not checked for overlap)

## Context

ADR 0058 makes two peripherals sharing a byte `ty::peripheral-overlap`,
and ADR 0061 left to M3.4 whether to relax that for devices that share
addresses, which SVD marks with `alternatePeripheral`.

A real chip has such a case. The ESP32-C3's SVD declares the APB
controller, `APB_CTRL`, at `0x6002_6000`, with a register `RND_DATA` at
offset `0xB0`, and the random number generator, `RNG`, at the same
address, with its one register `DATA` at `0xB0`: two views of one
register, with no `alternatePeripheral` (ADR 0067). A program declaring
both gets `ty::peripheral-overlap`; ADR 0058 leaves it a second view
made with `as`, which no check sees. The same need appears with a
cluster's type declared as a peripheral of its own, a second view of a
part of a larger device: a PLIC context declared alone at the address it
has inside the PLIC.

ADR 0061 settled the same question for registers with alternate groups.
For peripherals, SVD names the one peripheral another overlaps.

## Decision

A peripheral may name, after its address, the peripheral it overlaps:

```
peripheral APB_CTRL: ApbCtrl at 0x6002_6000;
peripheral RNG: Rng at 0x6002_6000 over APB_CTRL;
```

```
peripheral = "peripheral" NAME ":" type "at" expr [ "over" path ] ";"
```

- `over` names a peripheral by a path (ADR 0049), resolved and checked
  for visibility as any path. A name of anything else is
  `resolve::not-a-peripheral`; a peripheral naming itself is
  `resolve::over-itself`.
- **Two peripherals are not checked for overlap when one names the
  other, or both name the same one**: the second view and the first, and
  two alternative views of one device. Any other pair is checked as in
  ADR 0058, so `C over B` with `B over A` checks `C` against `A`.
- A peripheral `over` another that it does not overlap is accepted; the
  word then documents a view.
- Nothing else changes: each peripheral's alignment and range are
  checked, and accesses are built as to any peripheral. The compiler
  does not know which view the device shows, as for alternate groups
  (ADR 0061).
- `over` is recognized there by position only.
- The dumps show the peripheral named.

`ty::peripheral-overlap`'s help names `over NAME` for a second view of a
device.

### What is checked

When names are resolved:

| Code | Case |
|------|------|
| `resolve::not-a-peripheral` | `over` naming something that is no peripheral |
| `resolve::over-itself` | a peripheral naming itself after `over` |

A name of nothing is `resolve::unknown-name` or `resolve::unknown-path`,
and a peripheral not visible `resolve::private-item`, as for any path.

## Consequences

- SVD's `alternatePeripheral` maps to `over`; the ESP32-C3's `RNG` is
  declared next to `APB_CTRL`, with `over` written where the SVD does not
  say it.
- A cluster's type can also be a peripheral of its own over the device
  holding it, as one PLIC context reached without the PLIC.
- An `over` written by mistake hides a real overlap with the named
  peripheral; the name written at the peripheral says which one.

## Alternatives considered

- **Keeping `ty::peripheral-overlap`** — the second view stays a pointer
  made with `as`, outside every check of its type, and an SVD like the
  ESP32-C3's cannot be declared whole.
- **Alternate groups of peripherals** (`in NAME`, as ADR 0061) — SVD names
  the overlapped peripheral, not a group, and a group would be a new
  label at item level, where names are items.
- **A peripheral occupying only its registers' bytes**, not its extent —
  `RNG`'s `DATA` and `APB_CTRL`'s `RND_DATA` share their bytes, so the
  ESP32-C3's case stays an overlap, and the check costs a comparison per
  pair of registers.
- **`over` with several names** — no surveyed SVD needs it; two views of
  a device both name the first.
