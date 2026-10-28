# 0064. Register arrays and clusters

- Status: Accepted
- Date: 2026-10-28
- Amends: 0048 (a step, the length of a register array or a cluster
  array, and a cluster type reaching a failed value are failed values;
  the peripheral types of a `ty::infinite-size` cycle reach one),
  0058 (a peripheral type holds register arrays and clusters besides
  registers; its extent and alignment count them; the overlap check
  covers their elements; `&raw` of a cluster gives a pointer to its
  peripheral type; an array of a peripheral type is allowed as a
  cluster's type, and nowhere else), 0060 (a register's type may be an
  array of a register type; the register grammar gains `step`), 0061 (a
  cluster may be in an alternate group)

## Context

A device often repeats a register, or a block of registers, at a fixed
distance. QEMU `virt`'s PLIC has a priority register per interrupt
source, 4 bytes apart from offset 0, and per context (a hart's privilege
mode) a block of a threshold register and a claim register, 0x1000 bytes
apart from offset 0x20_0000. ADR 0058 can declare these only one by one,
as `priority1` to `priority95`, which no loop can index, and left
register arrays and clusters to M3.4.

SVD describes a repeated register with `dim` (the count),
`dimIncrement` (the distance, in bytes) and a name holding `[%s]`, and a
repeated block with a `<cluster>`, which holds registers and may itself
have `dim`. The distance need not be the register's width: the
ESP32-C3's SVD has 19 register arrays, and in 23 of its repeated
registers the distance is larger than the register (ADR 0067). SVD's
other repetition, a name holding `%s` with a list of indices, gives
registers of distinct names, not an array (ADR 0067).

`svd2rust` makes a cluster a register block type of its own, reached
from its parent's, so a driver for one DMA channel serves every channel.
ADR 0058 gives Erebos the same for devices: one driver serves every
instance of a peripheral type through a pointer to it.

## Decision

### Register arrays

A register whose type is an array, `[T; N]` with `T` a register type
(ADR 0060), is a **register array**: `N` registers of type `T`, the
first at the register's offset and each next one `step` bytes further.

```
peripheral Plic {
    pub read write priority: [u32; 96] at 0x0000,
    pub read pending: [u32; 3] at 0x1000,
    pub enables: [PlicEnables; 2] at 0x2000 step 0x80,
    pub ctx: [PlicContext; 2] at 0x20_0000 step 0x1000,
}

peripheral PlicEnables {
    pub read write word: [u32; 3] at 0x00,
}

peripheral PlicContext {
    pub read write threshold: u32 at 0x00,
    pub read write claim: u32 at 0x04 read modifies,
}
```

- `N` is an array length, a `usize` computed at compile time (ADR 0029).
- `step`, after the offset, is a `usize` computed at compile time in the
  same scope. Left out, it is the register's width: `priority` above has
  4.
- Every element has the array's access mode, read effect and group: an
  element is read and written as a register of type `T` declared with
  them (ADR 0058, 0060, 0061).
- `T` is a register type. An array of arrays, `[[u32; 3]; 2]`, is
  `ty::register-type`, whose help names clusters: a register repeated in
  two dimensions is a register array in a cluster array, as `enables`
  above.

### Clusters

An entry of a peripheral type **without an access mode** is a
**cluster**: its type is a peripheral type, or an array of one, whose
registers it places at its offset.

```
cluster = [ "pub" ] NAME ":" type "at" expr [ "step" expr ] [ "in" NAME ]
```

- A cluster's type names a peripheral type, `PlicContext`, or an array
  of one, `[PlicContext; 2]` (a **cluster array**). Any other type is
  `resolve::not-a-peripheral-type`, as for a peripheral (ADR 0058), and
  is then the entry's only error (no `resolve::missing-step` or
  `resolve::misplaced-step` with it). When that type is one a register
  may have, or an array of one, as `rbr: u8 at 0`, the entry is most
  likely a register without its access mode, and the help says so: a
  register starts with `read`, `write` or `read write`. Conversely, a
  register whose type is a peripheral type or an array of one is
  `ty::register-type`, whose help says that a cluster has no access
  mode.
- An array of a peripheral type is a type only there: as a pointee,
  `*const [PlicContext; 2]`, or anywhere else a type is written, it is
  `ty::peripheral-by-value`, as ADR 0058 has it for an array element.
- **A cluster array needs `step`** (`resolve::missing-step`): a
  peripheral type's extent grows when a register is added at its end, and
  a step taken from it would move every element but the first without a
  word changing at the cluster. A single cluster takes no `step`, nor does
  a register that is no array (`resolve::misplaced-step`).
- A cluster has no read effect of its own: its registers have theirs. It
  may be in an alternate group (ADR 0061), as a register may.
- A peripheral type that contains itself through clusters, directly or
  through others, is `ty::infinite-size`, naming every type of the
  cycle, as for a struct or an enum that contains itself (ADR 0039),
  with a help of its own: the cycle is broken by removing a cluster,
  since a register cannot hold a pointer. Every type of the cycle has no
  layout, and so **reaches a failed value** (see "Failed values").
- Clusters, registers and register arrays share the one namespace of the
  peripheral type's entries: two entries with one name are
  `resolve::duplicate-register`. Entries are private to the module of
  their peripheral type unless `pub`, as registers are (ADR 0058).

The grammar of ADR 0058 and 0060 becomes:

```
peripheral_type = "peripheral" NAME "{" [ entry { "," entry } [ "," ] ] "}"
entry           = register | cluster
register        = [ "pub" ] access NAME ":" type "at" expr [ "step" expr ] [ read_effect ] [ "in" NAME ]
```

`step` is recognized there by position only; elsewhere it is an
ordinary name. An entry starts with `pub`, an access mode, or a name
followed by `:` (a cluster, which may be named `read`).

### Layout

An entry's **elements** are its one register or cluster, or the `N`
elements of an array, element `i` at `offset + i * step`. An element
**occupies** `[start, start + size)`, with `size` a register's width or
a cluster's extent; an array of 0 elements occupies nothing.

- A peripheral type's **extent** is the end of the last byte any of its
  entries occupies; its **alignment** is the largest of its registers'
  widths and its clusters' alignments, 1 when it has none. Every entry
  counts toward the alignment, an array of 0 elements too, as every
  register does in ADR 0061: its offset is declared and checked.
- An extent is only ever compared with 2³² (ADR 0058's range check of a
  peripheral, and through a cluster its parent's extent), so it is
  computed exactly up to 2³² and **saturates** above, where nested
  arrays could pass 2⁶⁴; a diagnostic shows such an extent as past 2³².
- **Alignment.** An entry's offset, and its step, must be multiples of
  its element's alignment: a register's width, or a cluster type's
  alignment. A misaligned offset is `ty::misaligned-register`, as in ADR
  0058; a misaligned step is `ty::misaligned-step`. Then every register
  of every element is aligned once the peripheral's address is
  (ADR 0058).
- **The elements of one array may not overlap**: a step smaller than
  the element's size is `ty::step-too-small`. It is checked whatever the
  length, as the declaration states the step. A step of 0 is therefore
  accepted only for elements of size 0 (an empty cluster type), which
  occupy nothing.
- A step of 2 for a `u32` is both misaligned and too small, and both are
  reported: each is a fault of its own, and a step of 6 fixes only one.
  Neither is a failed value: as a misaligned offset (ADR 0058), each is
  reported and the type keeps its layout, so the bodies using it are
  still checked and built. An entry whose step is too small only takes
  part in no overlap check with other entries, which needs elements that
  do not overlap each other (and a step of 0 for elements that occupy
  bytes would be no distance to divide by).
- **Two entries may not share a byte**, under ADR 0058's and 0061's rule
  applied to the bytes their elements occupy: unless both are registers
  (or register arrays), one `read` and the other `write`, or they are of
  different alternate groups, or one is in a group and the other in
  none. Entries whose elements occupy nothing take part in no overlap
  check. A cluster element occupies its extent,
  gaps between its registers included, so a register inside a cluster's
  range is an overlap even where the cluster has no register. Inside a
  cluster type, its own registers are checked when that type is.
  `ty::register-overlap` is reported once per pair of entries, at the
  later one, naming the first byte they share.

### Accesses

- **`P.a[i]`**, with `a` a register array, is the place of element `i`,
  a register of type `T`, read and written as ADR 0058 and 0060 say.
  `i` is a `usize`. The index is **checked**: one not below `N` calls the
  panic function with `core::PanicReason::IndexOutOfBounds`, located at
  the `[`, as for arrays (ADR 0021, 0053), and one known at compile time is
  `ty::index-out-of-bounds` (ADR 0032). The index runs after the place
  it indexes and before the access, as for arrays (ADR 0018).
- **`P.c.r`** and **`P.c[i].r`**, with `c` a cluster or a cluster array,
  are the register `r` of the cluster element: `P.ctx[1].threshold`.
  Visibility is checked at each step: `ctx` where `Plic`'s entries are
  visible, `threshold` where `PlicContext`'s are.
- **`&raw const P.c` and `&raw mut P.c[i]`** give a `*const PlicContext`
  or a `*mut PlicContext` to the element's address, through which a
  driver of `PlicContext` serves every context, as it would a peripheral
  of that type (ADR 0058). Through a `*const` pointer, `&raw mut` of a
  cluster is an error as for any place reached through one.
- An entry that is an array is reached only through an index, and a
  cluster element only through a register or `&raw`:
  - `P.a`, `P.c`, or a cluster array's element `P.c[i]`, read or written
    as a whole, is `ty::peripheral-by-value`;
  - `&raw` of an entry that is an array, a register array or a cluster
    array taken whole, or of an element of a register array, is
    `ty::register-address`, as `&raw` of a register is (ADR 0058): no
    pointer type holds an array's step;
  - indexing an entry that is no array, a single cluster `P.c[0]` or a
    register `P.r[0]`, is `ty::cannot-index`, as indexing any value
    that is no array.
- Indexing a pointer to a peripheral type stays `ty::peripheral-index`
  (ADR 0058): a cluster array is indexed at the entry, where its step is
  known, not through a pointer to its element.

At compile time a register access is `eval::volatile-access`, as any
(ADR 0058). `&raw` of a cluster element in a constant or a static's
initializer is a pointer made from an integer, as `&raw` of a peripheral
is.

### Failed values

A step, the length of an entry that is an array, and a cluster type
reaching a failed value are **failed values** (ADR 0048) when they are
in error, as a register's offset is (ADR 0058), and a peripheral type of
a `ty::infinite-size` cycle has no layout and reaches one. That
`ty::infinite-size` is the cycle's explanation, as an offset's error is
(ADR 0058): a register whose type mentions a type of the cycle, as
`read write r: *mut A at 0`, reports nothing more. A peripheral type with one
reaches a failed value, the failed value's own error is the only one
reported for it, and the checks of "Layout" that need it are skipped:
a peripheral of such a type takes part in no overlap or range check,
and a body naming it is not built into MIR (ADR 0048, 0058).

### What is checked

When names are resolved:

| Code | Case |
|------|------|
| `resolve::missing-step` | a cluster array without `step` |
| `resolve::misplaced-step` | `step` on a register or a cluster that is no array |
| `resolve::not-a-peripheral-type` | a cluster whose type is not a peripheral type or an array of one; the help names the access mode when the type is a register's |
| `resolve::duplicate-register` | two entries with one name in a peripheral type |

Once the values are computed:

| Code | Case |
|------|------|
| `ty::register-type` | an array of arrays, or a peripheral type or an array of one, as a register's type, with the other cases of ADR 0060 |
| `ty::misaligned-register` | an entry's offset that is not a multiple of its element's alignment |
| `ty::misaligned-step` | a step that is not a multiple of the element's alignment |
| `ty::step-too-small` | a step smaller than the element's size |
| `ty::register-overlap` | two entries sharing a byte, under the rule of "Layout"; once per pair, at the later |
| `ty::infinite-size` | a peripheral type that contains itself through clusters |

When code is type checked and built:

| Code | Case |
|------|------|
| `ty::peripheral-by-value` | a register array, a cluster or a cluster array's element read or written as a whole; an array of a peripheral type anywhere but as a cluster's type |
| `ty::register-address` | `&raw` of a register array or a cluster array taken whole, or of an element of a register array |
| `ty::cannot-index` | an index of an entry that is no array |
| `ty::unknown-register` | a name that is no entry of the peripheral type |
| `ty::private-register` | an entry not visible where it is named |
| `ty::index-out-of-bounds` | an index known at compile time not below the array's length |

## Consequences

- The PLIC's priorities, enable words and contexts are declared at their
  datasheet offsets, as SVD's `dim` and `<cluster>` describe them, and
  indexed in loops, each access checked.
- One driver serves every channel of a DMA controller, or every context
  of the PLIC, through a pointer to the cluster's peripheral type.
- An index computed at run time costs a bounds check, removed by `-O`
  where it is proved; no unchecked access to a register array exists,
  since `&raw` of one is an error.
- A register placed inside a cluster's range but in a gap between its
  registers cannot be declared in the same mode; such a layout puts the
  register in an alternate group, or the cluster's registers in the
  parent.
- Checking two arrays for overlap costs, per pair of entries, about the
  length of the shorter: each of its elements is compared, by division,
  with the elements of the other it may meet. Two arrays of 2³¹ `u8`
  registers each would take seconds; no device has such arrays.
- Arrays of peripherals (SVD's `dim` on a peripheral) and arrays of bit
  fields are not added (ADR 0067).

## Alternatives considered

- **Clusters flattened into register arrays** (`ctx_threshold: [u32; 2]
  ... step 0x1000`) — no type for one channel, so no driver serving each
  through a pointer.
- **Clusters as anonymous blocks** (`cluster ctx[2] at ... step ... {
  ... }`) — the same layout, with no type to name for a pointer.
- **`step` required for register arrays too** — SVD always states
  `dimIncrement`, but most arrays are contiguous, and the default is the
  width an array of `T` would have anyway.
- **A default step for cluster arrays**, the extent rounded up to the
  alignment, as a struct's size — adding a register at the end of the
  cluster type would move every element but the first.
- **Arrays of arrays of registers** — one more form for what a register
  array in a cluster array says, with a step per dimension to write.
- **The exact bytes of a cluster's registers in the overlap check**,
  instead of its extent — a parent's register could sit between a
  cluster's registers, which no surveyed SVD does (ADR 0067), at the
  cost of comparing every register of every element.
- **Unchecked indexing of register arrays** — the bounds check is one
  comparison, removed when proved, and a write past the array reaches
  another register with no check of its access mode.
- **Indexing a pointer to a cluster's type to reach the next element**
  — the pointer does not know the step; the entry does.
