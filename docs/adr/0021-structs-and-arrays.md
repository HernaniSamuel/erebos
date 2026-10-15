# 0021. Structs and arrays

- Status: Accepted; amended by 0031 (indexing slice pointers), 0032 (indices known at compile time)
- Date: 2026-10-14

## Context

M2 adds compound data. Structs and arrays come first (M2.1): later steps
build on them (byte strings are arrays, `PanicInfo` is a struct, enums
reuse the layout rules). Ownership does not exist until M5 (ADR 0009), and
aggregates cannot cross function boundaries until the target ABI for them is
implemented (ADR 0006, M2.2). Their memory layout is decided separately in
ADR 0022, and volatile accesses to them in ADR 0023.

## Decision

### Structs

```
struct Point { x: i32, y: i32 }

let mut p: Point = Point { y: 2, x: 1 };
p.x = p.y + 1;
```

- A struct literal names every field exactly once, in any order; the field
  values run in the order they are written (ADR 0018).
- `s.field` is a place when `s` is a place: reading it reads only that
  field, and assigning to it writes only that field.
- A struct may contain itself only behind a pointer (`next: *mut Node`).
  Any cycle of containment by value is an error (infinite size), whether
  direct (`A` contains `A`) or indirect (`A` contains `B`, which contains
  `A`, possibly inside arrays); the diagnostic names the whole cycle.
- `struct Empty {}` is allowed; zero-sized types are described in ADR 0022.

### Arrays

```
let mut buf: [u8; 4] = [1, 2, 3, 4];
let zeros: [u32; 16] = [0; 16];
buf[i] = buf[0];
```

- `[T; N]` has `N` elements of type `T`. `N` is an integer literal (a
  constant expression once `const` exists, M2.3); `N` may be 0.
- `[a, b, c]` lists every element; `[v; N]` evaluates `v` **exactly once**
  and copies the value into every element, even when `N` is 0 or 1.
- The index has type `usize`; nothing converts implicitly (ADR 0008).
- **Every index is checked.** If `i >= N`, the program calls the panic
  function (ADR 0014), as for a zero divisor (ADR 0007). A constant index
  that is out of bounds is a compile-time error. With `-O`, LLVM removes
  checks it proves redundant. Code that must not pay for the check indexes
  through a pointer to the element type instead (below).
- Indexing an array of length 0 always panics (or is a compile-time error
  with a constant index).
- `a[i]` is a place when `a` is a place, so `(*p)[i]` reads or writes one
  element through a pointer without copying the array.

### Unchecked indexing through pointers

Indexing a pointer to the **element** type is unchecked, as in C:

```
let base: *mut u32 = &raw mut buf[0];   // or a pointer from anywhere
base[i] = 7;                            // the u32 at base + i * size_of(u32)
let next: *mut u32 = &raw mut base[1];  // pointer arithmetic
```

- `p[i]` with `p: *const T` or `*mut T` and `i: usize` is the place at
  address `p + i * size_of(T)`. Nothing is checked: it is the same as
  dereferencing a pointer made from that address (ADR 0019), with the same
  validity rules.
- The address arithmetic wraps, like `usize` arithmetic (ADR 0007), and is
  never undefined; in LLVM it is a `getelementptr` without `inbounds`.
- `p[i]` is a place: it can be read, assigned (through `*mut`) and passed to
  `&raw`, which makes `&raw const p[i]` the way to offset a pointer.
  Volatility carries over (ADR 0023).
- With `p: *mut [T; N]`, `p[i]` steps over whole arrays (`p + i * size_of([T; N])`),
  exactly as in C; `(*p)[i]` is the checked access to element `i`. The type
  of the result tells them apart.

This is the escape hatch from bounds checks: the type of the indexed value,
array or pointer, decides whether the access is checked.

### Aggregates as values

- `let`, assignment and reading a place copy the whole aggregate.
- `==` and `!=` are not defined on aggregates; compare their parts.
- Until M2.2, an aggregate cannot be a parameter or a return type; pass a
  pointer instead.

**Copying is provisional until ownership (M5, ADR 0009).** There, a move is
the same machine operation as a copy; what changes is that the compiler
forbids using the source afterwards. M5 will decide which types keep being
copied; the expected direction is Rust's:

- scalars and pointers are always copied;
- an array is copied if its element type is;
- a struct (or enum) is copied only if it is declared copyable, which
  requires all of its fields to be.

Since moves do not change the generated code, only programs that use a
struct after copying it are affected. The compiler will point at that use
and suggest declaring the type copyable, so the migration is mechanical.
Structs that represent exclusive access to hardware are the reason not to
keep copying everything: duplicating a handle to a peripheral is a bug that
ownership should catch.

### Taking addresses: `&raw`

`&raw const place` and `&raw mut place` produce a `*const T` or `*mut T` to
a place: a variable, a field, an element or a dereference. `&raw mut` needs
a mutable place (a `mut` variable, or a place reached through `*mut`). The
result is never volatile; convert it with `as` (ADR 0013).

There is no `&` without `raw` until references exist (M5), so the operator
reads the same before and after.

### Volatile aggregates

Through a volatile pointer, each field or element access is one volatile
access of that scalar's width, and reading or writing a whole aggregate is
an error. ADR 0023 states the full guarantees:

```
struct Uart { data: u8, status: u8 }
let uart: *mut volatile Uart = 0x1000_0000 as *mut volatile Uart;
(*uart).data = b'A';   // one volatile 8-bit store
```

### Conformance tests

M2.1 ships with tests for:

- **Bounds checking:** e2e programs that index in bounds, one past the end
  and on an empty array, at both optimization levels, with the panic
  observed through the exit code; ui snapshots for a constant out-of-bounds
  index.
- **Pointer indexing:** an LLVM IR snapshot showing `p[i]` without a bounds
  check and without `inbounds`; an e2e program walking a buffer through a
  pointer.
- **Containment cycles:** ui snapshots for direct and indirect cycles.
- **Single evaluation in `[v; N]`:** an e2e program whose `v` has a visible
  side effect (a counter or a UART write), for `N` of 0, 1 and several.
- **Field and element places:** writing one field or element leaves the
  others intact; `(*p)[i]` and `(*p).f` do not copy the aggregate (LLVM IR
  snapshot).
- **Evaluation order** of struct literal fields written out of declaration
  order.
- **Volatile code generation:** see ADR 0023.

## Consequences

- Structs can already describe register blocks through volatile pointers,
  ahead of `peripheral` blocks (M3).
- `(*p).field` is verbose; field access through pointers without an explicit
  dereference may come with methods or references later.
- Index checks cost a compare and a branch where LLVM cannot prove them away.
- Copies of large arrays may become `memcpy`/`memset` calls, so `rt/`
  provides both (ADR 0020).
- Code that relies on copying structs may need a copyable declaration in M5.

## Alternatives considered

- **Unchecked indexing (C)** — the most common memory bug in C firmware;
  the opt-out through pointers stays available.
- **`&place` producing a raw pointer now** — would change meaning once
  references exist.
- **A separate operator or function for pointer offsets (Rust's `p.add(i)`)**
  — clearer that no check happens, but there are no methods yet, and
  `p[i]` plus `&raw` covers both offsetting and accessing with what already
  exists. The cost is that a checked and an unchecked access look alike
  at the use site; the type of the indexed value tells them apart.
- **Deciding move semantics now** — without ownership checking there is
  nothing to enforce it; the decision belongs with M5.
