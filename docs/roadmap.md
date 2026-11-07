# Roadmap

Each milestone runs **end to end** through every compiler layer (ADR 0003)
and ends with a program running on QEMU, verified by an automated test.

| Milestone | Delivers |
|-----------|----------|
| **M0** ✅ | `entry fn main() -> ! { loop {} }` compiles to an object file, links, and boots on QEMU. Every layer that has work to do exists, even if trivial (const evaluation waits for M2). |
| **M1** ✅ | Functions, integers, `bool`, typed `let`, `if`/`while`/`loop`, raw and volatile pointers (ADR 0013), byte literals (ADR 0016), checked division (ADR 0007), the panic function (ADR 0014). Prints "hello" over the QEMU UART via volatile pointer writes. |
| **M2** ✅ | Compound data, enums and modules, in the steps below. |
| **M3** ✅ | `memory { ... }` blocks generating the linker script; `peripheral` blocks with native MMIO. The UART is declared in the language. |
| **M4** ✅ | Interrupt handlers, vector tables, inline `asm`, compiler and hardware fences (ADR 0023), in the steps below. |
| **M5** | Moves/ownership, then references and borrow checking (ADR 0009); methods (`impl`, `self`, `&self`, `&mut self`, `x.f()`). |
| **M6** | Types as values and `comptime` parameters: generics in the Zig model, monomorphized (ADR 0027). |

## M2 steps

M2 is split into steps that each end like a milestone: ADRs, the reference
updated, and a program on QEMU.

| Step | Delivers | Program on QEMU |
|------|----------|-----------------|
| **M2.1** ✅ Structs and arrays | `struct`, `[T; N]`, fields, checked indexing, `&raw const/mut` (ADR 0021); C layout, `size_of`/`align_of`/`offset_of`, `#[packed]`, `#[align(N)]`, `bool` loads (ADR 0022); unchecked pointer indexing (ADR 0021); volatile access widths (ADR 0023); `memcpy`/`memset` in `rt/` | a ring buffer on the stack |
| **M2.2** ✅ Aggregates across calls | passing and returning structs and arrays by value with the riscv32 C ABI (small ones in registers, large ones through a hidden pointer) | calls a C function built with clang, to prove the ABI is the real one |
| **M2.3a** ✅ Compile-time execution | the MIR interpreter and on-demand checking (ADR 0027, 0028); `const` items; `comptime` expressions and blocks; array lengths and attribute arguments computed at compile time | a lookup table computed by the compiler, and `comptime` checks of struct layouts |
| **M2.3b** ✅ Statics and byte strings | `static`/`static mut` with compile-time initializers, `#[align(N)]` on statics; `_start` initializes `.data` and `.bss`; `b"..."`; slices `*const [T]` | prints with a real `print` function |
| **M2.4a** ✅ Enums and `match` | enums with explicit discriminants (`enum Mode: u8 { Off = 0, Run = 3 }`), `match` on integers, `bool` and enums, with exhaustiveness and dead arms checked | a state machine |
| **M2.4b** ✅ Enum payloads | variants carrying data, their layout and ABI, patterns nested in payloads | a command decoder: messages with payloads, decoded from bytes and dispatched with `match` |
| **M2.4c** ✅ Cost of `match` | code of a `match` on an enum with payloads tested arm by arm, linear in its patterns (ADR 0042); checks of exhaustiveness and dead arms that examine once the values the arms do not tell apart; at most four witnesses searched (ADR 0043) | matches whose code and checks grew exponentially, built and run |
| **M2.5** ✅ `Option` and `Result` | built into the compiler until generics exist; the `?` operator | a number parser returning `Result` |
| **M2.6** ✅ Modules and panic information | `mod`, `use`, `pub`, `core`; `panic fn on_panic(info: *const PanicInfo) -> !` with file, line and a `PanicReason` enum (ADR 0014) | a panic prints `division by zero at main.ers:12` |

Decisions already agreed for the later steps (their ADRs come with the step):

- **M2.2:** the earlier idea of never passing aggregates by value is
  dropped; `Result` is only useful if functions can return it.
- **M2.3b:** `b"abc"` has type `[u8; 3]`, its length counted by the compiler.
  It is a place in read-only memory, so `&raw const b"abc"` points at it
  without a copy, while `let s: [u8; 3] = b"abc";` copies it. This revisits
  ADR 0016 (no strings).
- **M2.4:** every `match` is exhaustive; on integers that means a
  catch-all arm (`_` or a `let` binding, ADR 0037).
- **M2.5:** `Option(T)` and `Result(T, E)` are compiler-known types, as
  `[T; N]` is; in M6 they become ordinary comptime functions in `core`
  returning enums (ADR 0027), with the same syntax, so programs do not
  change. They are written with parentheses, like any comptime call. `?`
  requires the same error type, with no conversion. `Option(*mut T)` takes 8
  bytes: address 0 is valid (ADR 0019), so there is no null niche for
  `None`.

## M3 steps

M3 is split into steps like M2's, each ending with ADRs, the reference
updated, and a program on QEMU.

| Step | Delivers | Program on QEMU |
|------|----------|-----------------|
| **M3.1** ✅ Memory maps | `memory { ... }` blocks: regions, where code, data and the stack go, the linker script generated from them; the stack with a declared size at the start of its region; region overflows as diagnostics (ADR 0057) | a program laid out as on a small board, checking where its code, data and stack are |
| **M3.2** ✅ Peripherals | `peripheral` blocks: registers at offsets, with widths and access modes (read-only, write-only, read-write), accessed as MMIO; a write to a read-only register is a compile error; the UART declared in the language | hello world through a UART and a test finisher declared as peripherals |
| **M3.3** ✅ Register fields | fields at bit positions of a register (representation clauses, as in Ada), enums in fields, write-1-to-clear fields, reads with side effects, reset values | the UART configured through its fields |
| **M3.4a** ✅ Register arrays and clusters | register arrays and clusters (ADR 0064), access modes of fields (ADR 0065), peripherals over others (ADR 0066), from the design checked against the SVD of three real riscv32 chips (ADR 0067) | the PLIC's priorities, enable words and contexts, and the eight virtio-mmio transports through one driver |
| **M3.4b** ✅ SVD | `erebos svd`, a subcommand writing Erebos source from an SVD file (ADR 0070), how it names items (ADR 0071) and treats what has no declaration (ADR 0072); the ESP32-C3's SVD generated and compiled | a peripheral generated from SVD, run on QEMU |
| **M3.5** ✅ Second target | ARM Cortex-M on QEMU; what depends on the target becomes a per-target description (pointer width, widest access, ABI, startup, linker script, LLVM setup, runtime functions) | the e2e programs on both targets |

Decisions already agreed for the later steps:

- **M3.5:** the second target comes before M4, so that interrupts and
  vector tables, the most target-specific part of the language, are
  designed with both targets in view.

## M4 steps

M4 is split into steps like M3's, each ending with ADRs, the reference
updated, and a program on QEMU. The concurrency model they share is
ADR 0078.

| Step | Delivers | Program on QEMU |
|------|----------|-----------------|
| **M4.1** ✅ Inline assembly | `asm` expressions with operands, options and naked functions (ADR 0079); `core::arch`, a file per target (ADR 0080); `compiler_fence` and `fence` (ADR 0081); `__aeabi_uldivmod` and `__aeabi_ldivmod` in `core` | the thumbv7m board stops QEMU through `bkpt` in Erebos; C code divides 64-bit values on both targets; machine registers read through `asm` on each target |
| **M4.2** ✅ Startup in the language | `_start` moves into `core::arch`, written in Erebos and assembly (ADR 0012 superseded by 0083); `sym entry` | the e2e programs boot through `core`'s `_start` on both targets |
| **M4.3** ✅ Handlers and vector tables | `interrupt fn ... at` for exceptions and the target's interrupts, the vector table (thumbv7m) and the trap entry (riscv32) generated from them, `critical` blocks (ADR 0078), a vector without a handler calling the panic function | a timer interrupt (the CLINT on `virt`, SysTick on `mps2-an385`) counting ticks shared with the entry function; a fault reaching the panic function on both targets |
| **M4.4** ✅ External interrupts | the chip's interrupts as a list `erebos svd` writes from `<interrupt>`, the NVIC's interrupts in the thumbv7m table, the PLIC's dispatched by the program on riscv32 | a UART driven by its interrupt on both targets |
| **M4.5** ✅ Shared statics checked | the rule of ADR 0078 checked over the call graph | programs with handlers checked; rejected programs as ui tests |

Decisions already agreed for the later steps:

- **M4.5:** only `static mut` and `extern static mut` named in the
  program are checked; peripherals and raw pointers wait for M5.

Until M5, there are no methods: functions are free and take their receiver
explicitly (`fn rect_area(r: *const Rect) -> u32`), and modules (M2.6)
provide namespacing. Method receivers are references (`&self`,
`&mut self`), and taking an address implicitly at `x.f()` would be an
implicit conversion (ADR 0008), so methods wait for references instead of
being designed twice.

## Deferred ideas

Agreed as useful, not yet scheduled:

- `for i in 0..n` over integer ranges, without general iterators. With
  `n` evaluated once and `i` not assignable in the body, such a loop
  always ends, so most counting `while` loops need no termination
  argument.
- Loop labels (`'outer: loop`) to break out of nested loops.
- Warnings (unreachable code, unused variables, private types in public
  signatures, ADR 0050); today there are only errors.
  Among them, loops that certainly never end once entered: a `while`
  whose condition reads nothing the body can change (no assignment to
  its variables, no call, no write through a pointer, no volatile read),
  and a `while` from which no path leaves, as one whose body always
  enters a `loop` without `break`. A `loop` is infinite on purpose and a
  volatile read may change from outside, so neither is warned about.
- Loop variants, as SPARK Ada and Dafny have them: `while i < n
  decreases n - i { ... }`, where the variant, an unsigned integer, must
  decrease at every iteration. Checked at run time like division
  (ADR 0007): the loop ends, or calls the panic function, and never hangs
  silently; checks the compiler can prove, as `i = i + 1` with `n` fixed,
  are removed.
- Handlers with a frame: a handler that receives the interrupted state
  (the stacked `pc` on thumbv7m, `mepc` and the saved registers on
  riscv32), so that a fault handler can report where it happened, or repair
  the cause and return, and `ecall` can return past itself. ADR 0084 allows
  a return only where nothing needs repairing; this step would relax that,
  with the trap entry saving `mepc` and `mstatus` (ADR 0085).
- A compiler server for AI agents: look up a symbol by name and get its
  definition, type and uses from the compiler, and edit by symbol (a patch
  to a function or item) with the change type checked before it is
  applied, instead of reading and matching file text. Unlike LSP, queries
  are by name, not by cursor position. It builds on the on-demand queries
  of ADR 0028 and the structured diagnostics; still to be designed.
- Function pointers, as a safe, copyable type and not a raw pointer: one
  points to code that does not change and, with no closures, carries no
  environment, so the borrow checker has nothing to check in it. Not
  decided. The step that adds them must settle the rule of ADR 0078 (a call
  through a pointer reaches every function whose address is taken, unless
  its type says more), the Thumb bit 0 in their value on thumbv7m (ADR
  0074), and whether a pointer to a function that may run only in some
  context can be told from one that may run in any (the effect system
  below). Dynamic dispatch is a struct of them (M6).
- An effect system: annotations such as "never panics" or "safe in an
  interrupt handler", checked over the call graph. With "anything" as the
  default and annotations only restricting, it can be added without
  changing existing programs, so it can wait. The one effect needed soon,
  which statics an interrupt handler reaches, belongs to the concurrency
  model of M4 and M5.
- A generator of random valid programs, as Csmith does for C, feeding the
  differential tests: each program runs at compile time and on QEMU at
  `-O0` and `-O`, and the outputs must agree. Today's fuzzing only checks
  that the compiler does not panic; this is what finds miscompilations.
  It can start with a small part of the language and grow with it.
- Deferred formatting for logs, as `defmt` does in Rust: the program sends
  the index of a format string and its arguments, and the strings stay on
  the host, in a section that is not loaded. `comptime` can build the
  table.
- More concurrency than one core with one priority level, not scheduled
  but kept open on purpose: later steps must not close it. Two parts:
  - Priorities and nesting (ADR 0078, "What later steps may add"):
    handlers of several priorities, and `critical` blocks that mask only up
    to a ceiling, as RTIC's stack resource policy does. One level is the
    case where every handler has the same priority, so programs written
    for M4 keep their meaning. What each target gives has to be checked
    first: the NVIC has priorities on thumbv7m, while on riscv32 the
    CLINT and exceptions have none and the PLIC has priorities and a
    threshold for external interrupts only.
  - Several cores (harts) sharing memory. A critical section protects
    nothing from another core (ADR 0078), so this needs atomics (the `A`
    extension on riscv32, exclusive loads and stores on ARMv7-M) and a
    memory model of their own, and the rule for shared statics would gain
    a kind of context for code running on another core. On
    microcontrollers concurrency is mostly interrupts, which M4 covers.
  The M5 ADRs on ownership and references should say how they extend to
  both, as ADR 0078 says for its own rule, before they are accepted.
- Ada-inspired types for hardware. Representation clauses are bit-field
  structs (ADR 0059), whose fields are checked against their bits at
  run time through the panic function; what remains is range types
  checked the same way (ADR 0007, 0014), which a bit field could then
  have as its type, and possibly a `newtype` that keeps a distinct type
  with the same representation. Compatible with ADR 0008 (no implicit
  conversions) and with the no-runtime rule. Fixed point is out of scope
  for now. Needs a `Proposed` ADR reviewed before any implementation.
- Range-typed array indexes, as in Ada (`type Pin is range 0 .. 3`, an
  array indexed by `Pin`): when the index type is exactly the array's
  range, no bounds check is needed. A third way next to the checked and
  unchecked indexing of M2.1; part of the Ada-inspired types above.
- Typestate for hardware, as Rust's `Pin<Input>` / `Pin<Output>` pattern:
  a conversion consumes the old value and returns the new type, so using
  an input pin as an output is a compile error. It needs the moves of
  M5 and no new type-system feature; revisit when M5 lands.
- Affine or linear ownership, to decide with the moves of M5 (ADR 0009).
  Rust's types are affine: a value is used at most once and may be
  dropped, with `Drop` running implicitly at the end of the scope.
  Linear types, as Haskell's `LinearTypes` (`a %1 -> b`), require exactly
  one use, so forgetting a value is a compile error. That fits hardware
  tokens that must be given back, such as "interrupts disabled" for a
  critical section, or exclusive ownership of a peripheral. Questions for
  the M5 ADR: affine or linear, or both by marking some types; what
  happens to an unconsumed value; and no implicit code at scope end,
  given ADR 0008 and the no-runtime rule. Typestate (above) depends on
  this choice.
- Placing a static in a named region of the memory map, for DMA buffers
  or memory with other properties, and statics left uninitialized so that
  they survive a reset. A region other than `data`'s holding initial
  values needs `_start` to fill more than one range. Region names are
  not items (ADR 0057), so how a static names its region is open: an
  attribute on the static naming a region would make region names
  visible to code, while a placement in the `memory` block naming the
  static would not.
- Reset code of the program, for boards that must run code before
  `.bss` and `.data` are initialized, such as setting up an external
  memory controller that holds them (ADR 0083 gives every program
  `core`'s `_start`). The open variant is a hook in assembly: a naked
  function of the program that `_start` calls before the loops, as
  `cortex-m-rt`'s `__pre_init`. A hook of compiled code would read
  statics not yet set, which is why `cortex-m-rt` deprecated its Rust
  `#[pre_init]`; a program-defined `_start` repeats the loops.
- Reading the values of the memory map from code (a region's origin and
  size, the stack's bounds), for example to check stack use. Until then a
  value both need is a `const` used in the block and in the code
  (ADR 0057).
- Effects in function types ("reads MMIO", "disables interrupts", "may
  panic"), for example to forbid some calls inside interrupt handlers.
  A type-system feature of its own; not scheduled. Revisit after M6.

## Notes for M2

- Struct and array literals are built in a temporary and then copied into
  their variable. Building them in place would save a copy at `-O0`; `-O`
  already removes it.
- A large argument is copied by the caller and, if it is a variable,
  that copy is all there is; a temporary argument could be passed without
  the copy. `-O` removes most of it already.

## Pending decisions

Questions accepted ADRs leave to a later step, to settle in that step's
ADRs:

- **M3** (M3.2 to M3.4b for peripherals, M3.5 for the target):
  - `peripheral` blocks must express what an SVD file describes, so that
    a `svd2erebos` can generate them. Access modes, offsets and widths
    are settled (ADR 0058); fields at bit positions, enums in fields and
    defaults used as reset values (ADR 0059), the six `oneTo*`/`zeroTo*`
    write effects and read effects (ADR 0060); alternate registers
    (ADR 0061); register arrays and clusters (ADR 0064), field access
    modes (ADR 0065) and peripherals over others (ADR 0066). The SVD
    files of the ESP32-C3, GD32VF103 and CH32V003 were surveyed
    (ADR 0067): `derivedFrom` is several peripherals of one type, and
    the write effects without a neutral value (`clear`, `set`,
    `modify`), the reset value of a register without fields, arrays of
    bit fields and arrays of peripherals are not added, none of the
    three using them.
  - M3.4b: settled by ADR 0070 (`erebos svd`, a subcommand writing
    Erebos source, the compiler never reading SVD; XML read with
    `roxmltree`; the ESP32-C3's file and a hand-written one of QEMU
    `virt` in the tests), 0071 (names, keywords and names that meet) and
    0072 (each construct; what has no declaration declared as close as
    possible, with a comment and a warning; access values that are no
    SVD value read as absent; `over` computed from the file).
  - A second target, so that riscv32 assumptions are found before they
    spread: ARM Cortex-M on QEMU (`mps2-an385` or `lm3s6965evb`) is
    32-bit like riscv32, so `usize` keeps its width, and is where most
    microcontrollers are. Today the target is fixed in several places:
    `POINTER_BITS` and `WIDEST_ACCESS_BITS` are constants
    (`src/target.rs`), the ABI is riscv32's alone (`src/codegen/abi.rs`,
    ADR 0025), and `_start`, the linker script and LLVM's initialization
    are written for RISC-V (ADR 0012, 0030); the 64-bit division of
    `core` exists because RV32 lacks it (ADR 0020). Decided: the second
    target is M3.5, which makes those places a per-target description;
    M3.1 took the board out of the target (ADR 0057). A
    64-bit target, such as x86-64 bare metal, would also make the
    pointer width a parameter of layout, types and the compile-time
    interpreter; it is not planned (ADR 0002, 0010).
  - M3.5: settled by ADR 0073 (one description per target, chosen with
    `--target`, riscv32 by default; pointer width and widest access
    stay shared constants, checked per target), 0074 (`thumbv7m-none-eabi`
    on QEMU `mps2-an385`: a two-word vector table before `_start`, ARM
    unwind tables discarded, the EABI's memory functions in `core`),
    0075 (aggregates across calls in the AAPCS, as clang lowers them),
    0076 (64-bit division a call to `core` that code generation emits
    where LLVM would call the EABI's functions; riscv32 unchanged) and
    0077 (test programs reach their machine through a board module per
    target, which the harness supplies; semihosting on ARM).
- **M4** (M4.1 to M4.5):
  - The concurrency model spanning M4 and M5 is ADR 0078: contexts
    (thread, handler, unmaskable), one priority level, `critical`
    blocks, and the rule for shared statics, checked from M4.5; M5 and
    priorities only add to it. Calls through function pointers are left
    to the step that adds them, with the constraint the ADR states.
  - M4.1: settled by ADR 0079 (inline assembly), 0080 (`core::arch`)
    and 0081 (fences, amending ADR 0023).
  - M4.2: settled by ADR 0083 (`_start` a naked function of
    `core::arch`, found by path; `sym entry`; no reset code of the
    program; the thumbv7m vector table's words emitted by the compiler
    in `.vector_table`), which supersedes 0012 and amends 0030, 0052,
    0057, 0074 and 0079.
  - M4.3: settled by ADR 0084 (handlers: `core::arch::Vector`, which are
    unmaskable, which must return `!`), 0085 (the table on each target,
    the riscv32 trap entry with `mtvec` direct, default handlers, which
    are not contexts of ADR 0078), 0086 (`_start` masks, the portable
    items, how `critical` lowers, the wait) and 0087 (`#[align(N)]` on
    functions).
  - M4.4: settled by ADR 0089 (`#[interrupts]` on one enum; vectors on
    thumbv7m, a list for the program on riscv32), 0090 (the table's length,
    a word with no variant, default handlers for lines, the origin's
    alignment per program), 0091 (`core::arch::nvic_*` on thumbv7m; the PLIC
    and its dispatch are the program's) and 0092 (`erebos svd` writes the
    enum; the milestone programs).
  - M4.5: settled by ADR 0093 (a new layer, `analysis`, over the whole
    program's MIR before any dump of it; one error per place, with the path of
    calls from the context; only what a body does at run time names a static,
    so a pointer from a `const` is a raw pointer, and the way out).
  - Not in M4: priorities and nesting (ADR 0078, "What later steps may
    add"); function pointers.
- **M5:**
  - Constants and statics of types that must not be copied: duplicating a
    peripheral handle is a bug ownership should catch (ADR 0021), but a
    constant is copied at each use and reading a static copies it.
  - Copy versus move (ADR 0021), and borrow checking (ADR 0009). A direction
    to weigh, not decided: references as second-class values, allowed as
    parameters (and perhaps locals) but not stored in struct fields and not
    returned, so that no lifetime is ever written and the check stays inside
    one function, an in-house analysis in place of Polonius (ADR 0009). It
    costs accessors that return references, structs that hold views and
    chaining on `&mut self`; raw pointers and slices (ADR 0013, 0031) stay
    the unchecked way out for those. The aim behind it is that the compiler
    checks everything that can be checked, and what cannot (raw pointers,
    what a register does) stays visibly marked. Open: a `&mut` parameter that
    aliases a `static mut` its callee also names (M4.5's check sees only
    statics), references in locals, and how this meets the choice between
    affine and linear values below.
  - Fewer raw pointers, a goal to measure once references exist: parameters
    by address and slices move to references; a static placed in a named
    region (deferred ideas) replaces a computed address for a DMA buffer;
    handles into static arrays, with range-typed indexes (deferred ideas),
    replace pointers in linked data. What stays raw is `extern`, `asm`
    operands, fixed addresses outside peripherals, reinterpreting memory and
    the inside of allocators. Rule of thumb: if the safe form needs a written
    lifetime or region, leave it raw. Whether to mark the dereference of a
    raw pointer (the language has no `unsafe`, ADR 0030) waits until it is
    clear what remains.
  - The `(*s)[i]` syntax for slice elements (ADR 0031).
  - Whether the MIR analyses run before compile-time evaluation, which
    today runs MIR they have not checked (ADR 0033).
  - Affine or linear values. Affine values, used at most once but free
    to drop, are enough for typestate (a pin taken from `Disabled` to
    `Output`, written `Pin(comptime state: PinState)` once M6 exists).
    Linear values must be used, which also catches a DMA transfer whose
    handle is dropped without waiting for it; Rust cannot, since
    forgetting a value is safe there. Erebos has no destructors, so
    requiring a value to be used costs less than it would in Rust.
- **M6:**
  - `compile_error` (ADR 0031).
  - `!` as a type argument, such as `Result(T, !)` for generic code
    (ADR 0045).
  - Whether a pointer to a type that reaches a failed length or
    alignment stays usable where its pointee is not reached (ADR 0048).
    Today it makes the code that mentions it erroneous, so a length of
    `S` computed by a `comptime` function that only takes a `*const S`
    is a false `ty::cycle`; `comptime` functions over self-referential
    types make this common.
  - Requirements on type arguments (ADR 0027). The direction discussed:
    a requirement is a `comptime` function returning `bool`, named in the
    signature (as `where is_bus(B)`) and checked at each instantiation,
    so that a failure is reported at the call with the requirement's
    name, as C++20 concepts do. It needs comptime queries on types (which
    functions and fields a type has, their types). No trait system: the
    body is still checked only when instantiated, dynamic dispatch is a
    struct of function pointers, and properties such as what may be
    shared with interrupt handlers (M4, M5) are not requirements.
  - ADR 0027 writes return types Zig-style (`fn max(comptime T: type,
    a: T, b: T) T`); by ADR 0015 a function without `->` returns `()`, so
    the M6 ADR writes `-> T`.

Work owed:

- A MIR validator, before M5: a pass that checks the invariants of MIR
  (the types of places, a well-formed control flow graph) after MIR
  building and after each pass over MIR, as rustc's MIR validation and
  the LLVM verifier do. The borrow checker of M5 relies on those
  invariants, and each new pass makes the validator costlier to add.

Documentation owed:

- The informal grammar reference ADR 0004 asks for in `docs/`; with
  `comptime` precedence and expressions inside types, it is overdue.

## Status

- Environment ready and verified (ADR 0011): an `inkwell` + LLVM 22 smoke test
  emitted a valid `riscv32` object file, and a hand-written assembly program
  linked with `ld.lld` booted on QEMU `virt`, printed over the UART at
  `0x1000_0000` and exited through the test finisher at `0x0010_0000`
  (ADR 0010).
- **M0 done.** `erebos build` runs lexer, parser, HIR lowering, type
  checking, MIR building, LLVM codegen and `ld.lld`; the result boots on QEMU
  through the provisional `_start` (ADR 0012) and is checked by `tests/e2e.rs`.
- **M1 done.** Programs with functions, recursion, loops, branches, integer
  arithmetic and volatile pointers print over the UART and exit QEMU through
  the test finisher; end-to-end tests compare their output and exit code.
  The language is Turing-complete.
- **M2.1 done.** Structs, arrays, checked and unchecked indexing, `&raw`,
  C layout with `#[packed]` and `#[align(N)]`, layout queries, and the
  volatile guarantees of ADR 0023. The milestone program, a ring buffer on
  the stack, runs on QEMU; layouts are checked against clang, and volatile
  accesses in LLVM IR with and without `-O`. `rt/` provides `memcpy`,
  `memmove` and `memset`.
- **M2.2 done.** Structs and arrays cross calls by value with the riscv32 C
  ABI (ADR 0025), and `extern fn` declares functions defined elsewhere
  (ADR 0026). The milestone program exchanges aggregates of every size
  class with C code compiled by clang, in both directions.
- **M2.3a done.** Erebos runs code at compile time in the Zig model
  (ADR 0027): a MIR interpreter with the target's semantics computes
  constants, `comptime` expressions and checks, array lengths and
  `#[align]` arguments, on demand and with cycles reported (ADR 0028,
  0029). The milestone program computes a CRC-32 table at compile time and
  checks a message on QEMU; a differential test compares 21 functions run
  by the compiler and on the target, and builds are reproducible.
- **M2.3b done.** Statics (`static`, `static mut`, `extern static`) get
  their initial values at compile time and may point at each other through
  relocations; `_start` zeroes `.bss` and copies `.data` from an image laid
  out as on a board with flash (ADR 0030). Byte strings and slices, with
  checked element access, `len`, `slice` and messages in `assert`
  (ADR 0031). Shift amounts, divisors and indices known at compile time are
  checked, not only literals (ADR 0032); `!` is no longer a type of memory
  (ADR 0034); pointer comparisons at compile time give the run-time answer
  or an error. The milestone program prints byte strings and numbers on
  QEMU through a `print` function taking a slice, with its messages in a
  static table; an ELF test checks where statics go and the symbols
  `_start` relies on.
- **M2.4a done.** Enums with an explicit integer type and explicit values
  (ADR 0035), stored, passed and accessed through volatile pointers as
  that integer, checked against C23 enums in layout and across calls.
  `match` on integers, `bool` and enums (ADR 0037), with literal, constant,
  variant, `let` and or-patterns, compiled to a switch; a differential
  test checks that it runs alike at compile time and on the target. Every
  `match` is exhaustive, and dead arms are errors. An enum value that is
  no variant is defined, and a `match` without a catch-all arm calls the
  panic function on one (ADR 0036). The milestone program runs a state
  machine over a byte string of events on QEMU, printing each transition,
  and checks the same machine in a `comptime` block.
- **M2.4b done.** Enum variants carry payloads of one or more positional
  values (ADR 0039), laid out as a C union of structs that each start with
  the tag, checked against clang in layout and across calls, where they
  pass as aggregates; `!` is no payload type. `match` reaches payloads
  through nested variant patterns (ADR 0040), with exhaustiveness and dead
  arms checked over every position by the usefulness of patterns, at most
  three missing values listed, and compiles to a decision tree of
  switches; or-patterns may bind variables when every alternative binds
  the same ones (ADR 0041). A differential test runs nested matches alike
  at compile time and on the target. The milestone program decodes a
  stream of command bytes into messages with payloads on QEMU, dispatches
  them with nested and or-patterns, printing each action, and checks the
  decoder in a `comptime` block.
- **M2.4c done.** A `match` on an enum with payloads is tested arm by
  arm (ADR 0042): a switch on the tag, then per variant a chain of the
  alternatives naming it, in arm order, ending in the first catch-all arm
  or a call to the panic function, so that its code is linear in its
  patterns; a `let` is copied where its position is reached, and the first
  matching alternative binds. Exhaustiveness and dead arms examine once
  the values the remaining arms do not tell apart, and the search for
  missing values stops at the fourth: three are listed, then "and more"
  (ADR 0043). A test generates matches over 32 positions, whose checks and
  code grew exponentially, and bounds their MIR; a differential test runs
  smaller ones alike at compile time and on the target. The milestone
  program runs such matches on QEMU, at both optimization levels, printing
  the arm each input takes, and checks the same arms in a `comptime` block.
- **M2.5 done.** A variant may be named by the expected type, `.Run` or
  `.Write(3, 4)`, in expressions and patterns, wherever a literal takes its
  type but in the operand of `as` (ADR 0044). `Option(T)` and `Result(T, E)`
  are built-in enums with payloads, one per set of type arguments, with a
  `u8` tag and no niches, checked against clang in layout and across calls
  (ADR 0045); `!` is not a type argument until M6. The `?` operator returns
  a `.None` or an `.Err(x)` from the function, with the same error type and
  no conversion, and calls the panic function on a tag that is no variant
  (ADR 0046). A differential test runs options, results and `?` alike at
  compile time and on the target. The milestone program parses decimal
  numbers from a static table of byte strings into
  `Result(u32, ParseError)`, combining checked operations with `?` and
  reporting overflow, empty input and the bad digit, on QEMU at both
  optimization levels, and checks the same results in a `comptime` block.
  Its follow-ups record that a zero-sized payload value has an offset, as
  any other (ADR 0047), and make a type that reaches a failed array length
  or struct alignment erroneous, so that the failure is reported once
  instead of a cascade computed from a wrong layout (ADR 0048).
- **M2.6 done.** Programs are split into modules, inline or in files found
  by a fixed rule, each with its own namespace; items are named by paths
  from `crate`, `super`, `core` or a name in scope, and brought in with
  `use` (ADR 0049). Items and fields are private to their module unless
  `pub`, variants are visible where their enum is reachable, and `pub use`
  offers items of private children (ADR 0050, 0054). Functions and statics
  outside the root module have their paths as symbols, `uart.init`, and
  `#[export]` keeps a name for C (ADR 0051). The runtime support functions
  became the module `core`, written in Erebos and shipped inside the
  compiler (ADR 0052). The panic function may take a
  `*const core::PanicInfo` giving the reason, file, line and column of the
  failed check and the message of an `assert`, read-only data for every
  check but an `assert` with a message; a handler without it pays nothing
  (ADR 0053). Diagnostics may point into several files, and the test
  harnesses run programs of several files. The milestone program, in five
  files, averages groups of readings on QEMU, at both optimization levels,
  until a division by zero makes its handler print
  `division by zero at main.ers:12`. With it, M2 is done.
- **M3.1 done.** A program declares its board's memory in a `memory`
  block (ADR 0057): regions, whose origins and sizes are computed at
  compile time, and the regions that hold the code, the data and the
  stack, which has a declared size at the start of its region, so that an
  overflow leaves the region instead of running over the statics. The
  compiler checks the map (empty, overlapping or out-of-range regions, a
  misaligned code origin, a stack that does not fit) and generates the
  linker script from it, which `--emit=linker-script` prints; `_start`
  sits alone at the origin of the code's region and takes the stack's top
  from the script, so code generation does not depend on the board. A
  region too small for what it holds is reported at the region in the
  source, from the linker's error. The target description no longer holds
  QEMU's RAM, and every program built, the test programs included,
  declares its map. The milestone program, laid out as on a small board
  with 64 KiB of flash and 16 KiB of RAM, checks on QEMU at both
  optimization levels that its stack, statics and read-only data are
  where its map puts them; a second one runs from RAM, its code and data
  in one region.
- **M3.2 done.** Devices are declared in the language (ADR 0058): a
  peripheral type gives a device's registers at offsets computed at
  compile time, each a `u8`, `u16` or `u32` with an access mode (`read`,
  `write` or `read write`), and a peripheral is an instance of one at an
  address, so that two instances of one design share a type and one
  driver serves both through a pointer to it. Each register access is one
  volatile load or store of the register's width, from a peripheral with
  no pointer on its path or through a pointer written without
  `volatile`; reading a write-only register and writing a read-only one
  are compile errors, as are `&raw` of a register and a device used as a
  whole. The compiler checks register types, misaligned and overlapping
  registers, and misaligned, overlapping or out-of-range peripherals, and
  treats failed offsets, register types and addresses as failed values
  (ADR 0048). Volatile pointers stay for memory that is not a register.
  The milestone program prints hello world on QEMU at both optimization
  levels through a 16550 UART and the test finisher declared as
  peripherals, its driver reaching the UART through a pointer held in a
  static; the hello world test writes them through peripherals too.
- **M3.3 done.** Registers have fields (ADR 0059 to 0062). A bit-field
  struct, `struct Lcr: u8 { ... }`, is a value stored as its integer
  type (`u8`, `u16`, `u32` or `u64`), whose fields sit at bit positions
  written as a datasheet gives them (`dlab: bool at 7`, `word_length:
  WordLength at 0..=1`); fields are `bool`, integers or enums, may have
  defaults, and a literal ends with `..` to take them. A value written to
  a field must fit its bits, checked at compile time when known and
  otherwise by a panic (`FieldOutOfRange`). `as` converts a bit-field
  struct to integers and back from its own integer type, and it is a
  scalar in calls and volatile accesses. A register may have such a
  struct, or an enum, as its type; a field assignment is one read and one
  write of the register. A field may declare a write effect (`write 1
  clears`, six combinations), and then a field assignment to a register
  writes the other effect fields with the value that has no effect, and
  a compound assignment to it is an error; a register or a field may
  declare a read effect (`read clears`, `read sets`, `read modifies`),
  and then a read-modify-write of it is an error, the program reading
  into a local instead. A field's default is its reset value. Registers
  of another mode of the device, such as the 16550's divisor latch, go
  in an alternate group (`in dlab`) and are checked for overlap against
  their group only. The milestone program configures the 16550 on QEMU
  through its fields at both optimization levels: it sets DLAB and writes
  the divisor, builds the line control and FIFO control registers with
  literals, turns on loopback, and reads a received byte and an overrun
  that a read clears. Bit-field structs were also run against clang's
  calling convention and layout. Left to M3.4: access modes of fields,
  the write effects without a neutral value, a register's reset value
  without fields, and peripherals sharing addresses.
- **M3.4a done.** A peripheral type holds register arrays and clusters
  (ADR 0064, 0067). A register whose type is an array, `priority: [u32;
  96] at 0x0`, is `N` registers, `step` bytes apart (the width by
  default); an entry without an access mode is a cluster, whose type is a
  peripheral type or an array of one (which needs its `step`), so one
  driver serves every context of the PLIC through a pointer to the
  cluster's type, `&raw mut PLIC.ctx[1]`. An index into either is checked
  against the length, by a panic at run time and a compile error when
  known, and costs one comparison that `-O` removes where it is proved.
  The compiler checks steps (too small, misaligned), the overlap of
  entries element by element, and a type that contains itself through
  clusters, and treats failed steps, lengths and cluster types as failed
  values (ADR 0048). A bit field may declare an access mode, `read` or
  `write`, checked in volatile places beside its register's (ADR 0065).
  A peripheral may name another it overlaps, `peripheral RNG: Rng at ...
  over APB_CTRL;`, and the two are not checked for overlap (ADR 0066).
  The milestone program runs on QEMU at both optimization levels: it
  writes and reads the PLIC's priorities in a loop with a run-time index,
  sets an enable word and the thresholds of both contexts through one
  driver, reads a context a second time through a peripheral `over` the
  PLIC, and prints the magic value, version, device and vendor of the
  eight virtio-mmio transports through one driver; a second program
  checks that an index past a register array calls the panic function.
  Left to M3.4b: `svd2erebos`, and what it does with what has no
  declaration (ADR 0067).
- **M3.4b done.** `erebos svd <input.svd> [-o <output.ers>]` writes an
  Erebos module from an SVD file (ADR 0070 to 0072); the compiler never
  reads SVD, so a build is still a function of its `.ers` files. The
  generator is the module `svd`, reading the XML with `roxmltree`, and
  reports through `diag`, which gained warnings (the compiler emits none).
  It writes a module per peripheral type, then the peripherals, with
  registers, register arrays and lists, clusters, bit-field structs,
  fields with their modes, effects and defaults, and enums; names are
  converted to Erebos's (keywords and names that meet get a `_`, with a
  warning when a name changed); `derivedFrom`, `dim` and the properties
  SVD inherits are resolved; what has no declaration is declared as close
  as the language allows, with a `// svd:` comment and a warning; an
  `access` value that is no SVD value is read as absent and warned; and
  peripherals that share bytes are written `over` the first candidate, with
  a comment, or warned (`svd::overlap`) when no `over` can say it. The
  output is a function of the file's bytes and name. The ESP32-C3's SVD
  (version 18, 1.37 MB, Apache 2.0, in `tests/svd/chips/`) is generated
  whole and built into executables at both optimization levels, not run
  since the official QEMU has no such machine: 34 peripheral types, 37
  peripherals, 1 429 registers, 3 507 fields, no error, two warnings; small
  hand-written files of `tests/svd/` cover the constructs it lacks, each
  generated, type checked and snapshotted. The milestone program runs on
  QEMU at both optimization levels: the devices of QEMU `virt`, described
  in a hand-written SVD (`tests/e2e/svd_virt/virt.svd`) and generated into
  `virt.ers`, kept and compared by a test, are configured and used through
  their fields, a register array with a run-time index, a cluster array
  served by a driver, a peripheral found `over` the PLIC, and eight
  transports through one driver. The reader limits what a hostile file can
  cost (nesting, `derivedFrom` chains and copies, lists), reported as
  `svd::too-large`. Interrupts, which the generator does not write, are
  M4's.
- **M3.5 done.** There are two targets, chosen with `erebos build --target
  <name>`: `riscv32imac-unknown-none-elf` (the default, so every earlier
  command builds as before) and `thumbv7m-none-eabi` (ADR 0073, 0074). What
  depended on the target is one description per target (`target::Target`,
  `TARGETS`): LLVM's machine, the stack alignment (16 and 8), the code
  alignment (4 and 128), and whether LLVM's 64-bit division may be emitted;
  the pointer width and the widest access stay shared constants, checked
  against every description. The memory map checks (`ty::misaligned-code`,
  `ty::stack-size`) read the build's target. On thumbv7m the reset entry is
  a two-word vector table (the stack's end and `_start`) before `_start`, the
  linker script says `OUTPUT_ARCH(arm)`, places `.ARM.attributes` and
  discards the ARM unwind tables, and aggregates cross calls as the AAPCS
  says, as clang lowers them: `[N x i32]` and `[N x i64]` up to 64 bytes,
  `byval` above, `i8`/`i16`/`i32` returns, classified by the natural
  alignment, without the aggregate's own `#[align(N)]` (ADR 0075). 64-bit
  `/` and `%` are calls to `core`'s `__udivdi3`, `__umoddi3`, `__divdi3` and
  `__moddi3` where LLVM would call the EABI's functions, which no function
  with the C ABI can be (ADR 0076), and `core` defines the EABI's twelve
  memory functions. The milestone program is the e2e programs on both
  targets: every portable program of `tests/e2e/` builds for both and runs
  on QEMU `virt` and on `mps2-an385`, at both optimization levels, with the
  same output and exit code, each reaching its machine through a board
  module the harness supplies (`tests/boards/`), and stopping QEMU through
  the test finisher or through semihosting; the generated programs, the
  layout tests, the ELF checks and the reproducible builds run on both too,
  and `aapcs_c` calls C and is called by C with every row of ADR 0075's
  table, at the AAPCS's register boundaries (ADR 0077). Left to M4:
  exception handlers and the rest of the vector table (a fault on thumbv7m
  locks the processor up), and inline assembly, with which `core` can define
  `__aeabi_uldivmod` and `__aeabi_ldivmod`, so that C code linked into a
  thumbv7m program can divide 64-bit values. Not planned: Cortex-M0,
  floating-point units, big-endian and 64-bit targets.
- **M4.1 done.** `asm` is an expression (ADR 0079): a template of string
  literals, which exist only there, and operands (`in`, `out`, `lateout`,
  `inout`, `sym`, `const`, `clobber_abi`) with `reg` or a named register,
  and options (`pure`, `nomem`, `readonly`, `nostack`, `preserves_flags`,
  `noreturn`). Type checking reads each target's table of registers
  (`target::Target`) and checks the placeholders, the registers, the types
  and the options; MIR has an `asm` statement, or a terminator with
  `noreturn`; code generation derives LLVM's constraint string and emits
  inline assembly, with a handler for LLVM's diagnostics, so that an
  instruction the assembler rejects is `codegen::asm` at the `asm`
  expression and never ends the compiler (ADR 0082); an `asm` cannot run at
  compile time (`eval::asm`). `#[naked]` functions have a body that is one
  `asm` and no code of the compiler's. `core::arch` is a module of `core`
  whose file depends on the target, `rt/arch/riscv32.ers` or
  `rt/arch/armv7m.ers` (ADR 0080); its portable items are `compiler_fence`
  and `fence` (ADR 0081, `fence iorw, iorw` and `dmb sy`), which lift the
  limitation of ADR 0023 for handing memory to a device, and `armv7m.ers`
  defines the naked `__aeabi_uldivmod` and `__aeabi_ldivmod`, so that C code
  on thumbv7m divides 64-bit values (a program cannot define either name:
  `resolve::reserved-name` on every target, ADR 0080 corrected). The
  thumbv7m board stops QEMU through `bkpt` in Erebos and has no C file. The
  milestone programs run on QEMU at both optimization levels: every program
  on `mps2-an385` stops through `asm`; `asm_riscv32` and `asm_armv7m` read
  machine registers (`csrr`, `mrs`) and move values through every kind of
  operand, `sym` with `clobber_abi("C")` and a `noreturn` jump; the naked
  functions are called from Erebos and from C; `fence_handoff` hands a
  descriptor to a "device" across `core::arch::fence()` on both targets.
  Left to later steps: `_start` is still generated by the compiler (M4.2),
  and nothing masks interrupts or waits for one (M4.3); finer fences
  (`fence w,o`, `dmb st`, `dsb`, `isb`) wait for a step that needs one.
- **M4.2 done.** `_start` is a private `#[export]` `#[naked]` function of
  `core::arch`, one per target (`rt/arch/riscv32.ers`, `rt/arch/armv7m.ers`),
  with the four steps of ADR 0030 in assembly: the stack, `.bss`, `.data`, a
  jump to the entry function (ADR 0083, which supersedes ADR 0012). The
  jump is `sym entry`, a new operand of `asm` that names the entry function's
  symbol whatever its module and name, so no startup stub is left in code
  generation; on riscv32 it is a `tail`, which reaches the whole address
  space where the old `j` reached 1 MiB each way, and on thumbv7m a
  `movw`/`movt` pair and `bx`. The compiler names `core::arch::_start` by
  path and puts it in `.text.start`; a program that defines `_start` is still
  one `resolve::reserved-name`, whose help now says that `core::arch` defines
  it, and no `resolve::duplicate-symbol`. `--emit=llvm-ir` leaves `core` out,
  so `_start` is read in `rt/arch/`; the program's IR has no `module asm` on
  riscv32, and on thumbv7m only the two words of the vector table, which the
  compiler still emits, now in a section of their own, `.vector_table`,
  placed by the linker script before `.text.start`. The milestone program is
  every e2e program on both targets at both levels, booting through `core`'s
  `_start`, with the startup tests of ADR 0030, a program whose entry function
  is in a module, a program per target that jumps back to its entry function
  through `sym entry`, and one whose entry function is more than 1 MiB from
  `_start`; `tests/elf.rs` checks that `_start` is a function with a size on
  both targets at both levels. Left to M4.3: handlers and the rest of the
  vector table, the riscv32 trap entry, `critical` blocks, whether `_start`
  masks interrupts on thumbv7m, and the table's size and `code_align`; a
  program cannot run code before `_start`, and reset code of the program is a
  deferred idea.
- **M4.3 done.** A handler is `interrupt fn name() at Vector::X` (ADR 0084),
  with a path to a variant of `core::arch::Vector`, an enum in each file of
  `core::arch` (`rt/arch/riscv32.ers`, `rt/arch/armv7m.ers`) whose values are
  `mcause` and the exception number. It takes no parameters, returns `()` or
  `!`, must return `!` where returning would run the trapping instruction
  again (the faults, and on riscv32 every exception), cannot be called, and
  is a root; `src/target.rs` holds what the compiler needs of each vector, and
  a test keeps it equal to the enums. Five codes are new:
  `resolve::call-interrupt`, `resolve::not-a-vector`,
  `resolve::duplicate-vector`, `ty::interrupt-params` and
  `ty::interrupt-return`. `#[align(N)]` applies to functions (ADR 0087), set on
  the LLVM function and shown in the MIR dump. `critical { ... }` is an
  expression (ADR 0078, 0086): MIR has `critical_enter` and `critical_exit`,
  emitted on every way out of the block, innermost first, and code generation
  calls `core::arch`'s private `__critical_enter` and `__critical_exit`; the
  evaluator does nothing for them. `_start` masks interrupts first on both
  targets, and `core::arch` gains the portable `enable_interrupts`,
  `wait_for_interrupt` and `interrupts_enabled`. The compiler generates the
  table (ADR 0085): on thumbv7m `.vector_table` has 16 words and `_start` is
  at the origin plus 64; on riscv32 `__erebos_trap_table` has 33 words, and
  `_start` sets `mtvec` to `__erebos_trap`, a naked `#[align(4)]` function of
  `core::arch` that saves the 16 registers the C ABI does not preserve. A
  vector without a handler names a default handler that calls the panic
  function, with the new `PanicReason::UnhandledVector` and a `PanicInfo` of
  an empty file, line 0 and the vector's name as message (`unknown` for a
  cause of riscv32 `Vector` lacks); the table and the defaults are left out of
  the dump of a program without handlers. The milestone programs run on both
  targets at both levels: a timer (SysTick on `mps2-an385`, the CLINT on
  `virt`) counting five ticks that the entry function reads in a `critical`
  block while waiting with `wait_for_interrupt`; a fault reaching the panic
  function (`udf` as `HardFault`, an illegal instruction as
  `IllegalInstruction`); handlers that keep every register and the stack's
  alignment; and a race on a counter that is exact only with `critical`. The
  harness gained the header `// takes-exceptions: yes`, which needs
  `// expect-exit:` and reduces the QEMU event log to guest errors (ADR 0088).
  Left to M4.4: the chip's interrupts and the length of the thumbv7m table
  beyond the system exceptions, `code_align`, and enabling interrupts in the
  NVIC and the PLIC; the check of shared statics is M4.5.
- **M4.4 done.** The chip's interrupts are an enum with `#[interrupts]`, one
  per program, `u32` with no payloads (ADR 0089); `erebos svd` writes it last in
  the generated file from the SVD's `<interrupt>` elements, merging a line
  shared by several peripherals and leaving out a second name for a value
  (ADR 0092), and a hand-written one works the same. Four codes are new:
  `resolve::duplicate-interrupts`, `ty::interrupts-enum`,
  `ty::interrupt-number` and `ty::interrupt-not-a-vector`. On thumbv7m the list's
  variants are vectors: `interrupt fn ... at Interrupt::Uart0Tx`, maskable, told
  apart from `Vector`'s by enum and variant, with a value of 496 or more
  rejected (ADR 0089). `.vector_table` has `16 + m + 1` words for a list whose
  largest value is `m`; a hole holds `__erebos_unhandled_other`, a line without a
  handler `__erebos_unhandled_irq_<variant>`, and `ty::misaligned-code` asks the
  origin a multiple of the power of two that holds the table (ADR 0090).
  `core::arch` gains `nvic_enable`, `nvic_disable`, `nvic_pend`, `nvic_unpend`
  and `nvic_is_pending` on thumbv7m, taking the line's number and failing an
  `assert` at 496 (ADR 0091). On riscv32 the compiler reads the list for
  nothing: a handler at its variant is `ty::interrupt-not-a-vector`, and the
  program enables the PLIC and dispatches from `Vector::MachineExternal`, a
  `claim` loop over the sources (ADR 0091). The milestone programs run on both
  targets at both levels: a UART driven by its interrupt, the CMSDK UART0's
  transmit line through the NVIC on `mps2-an385` and the 16550 through the PLIC
  on `virt`, each printing a message through the handler alone from a ring
  buffer; and, on riscv32, the 16550's loopback giving a received byte and a
  transmit interrupt of one source. Reception on thumbv7m is not tested end to
  end (the CMSDK UART has no loopback, and a byte through QEMU's standard input
  did not reach it on Windows; ADR 0092). Left to M4.5: the check of shared
  statics; priorities and nesting stay out of M4 (ADR 0078).
- **M4.5 done.** The rule of ADR 0078 for shared statics is checked by a new
  layer, `src/analysis/`, the first MIR analysis (ADR 0093): after MIR is
  built for the whole program, `core` included, and before any dump of it, it
  finds the functions each context reaches by calls and by the checks that
  reach the panic function. `analysis::unprotected-static` reports a place in
  the thread context, outside `critical`, naming a `static mut` or
  `extern static mut` that the handlers of maskable vectors also reach;
  `analysis::unmaskable-static` one that an unmaskable handler reaches and
  another context reaches too, `critical` or not. Each error is one place,
  with the path of calls from the context, a place of the other context and a
  help. Only what a body does at run time names a static: a pointer made at
  compile time (`const P: *mut u32 = &raw mut X;`) is a raw pointer, the way
  out, which waits for M5 like peripherals. The calls the compiler inserts
  reach no check and no static, which a unit test keeps. The nine e2e programs
  that shared a static without the rule were rewritten with the same output
  (a read in `critical`; a `const` pointer for `nmi_returns` and
  `svcall_returns`), and `critical_race` and `uart_interrupt` on both
  targets are the programs that share one. Left open: accepting accesses before
  `enable_interrupts` or after the last interrupt (ADR 0078), function
  pointers, handlers in the generator of ADR 0068, and whether the analyses of
  M5 run before compile-time evaluation.
- **Next: M5** (moves, ownership, references and methods), the milestone after M4 in
  the table above.
