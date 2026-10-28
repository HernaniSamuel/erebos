# Roadmap

Each milestone runs **end to end** through every compiler layer (ADR 0003)
and ends with a program running on QEMU, verified by an automated test.

| Milestone | Delivers |
|-----------|----------|
| **M0** ✅ | `entry fn main() -> ! { loop {} }` compiles to an object file, links, and boots on QEMU. Every layer that has work to do exists, even if trivial (const evaluation waits for M2). |
| **M1** ✅ | Functions, integers, `bool`, typed `let`, `if`/`while`/`loop`, raw and volatile pointers (ADR 0013), byte literals (ADR 0016), checked division (ADR 0007), the panic function (ADR 0014). Prints "hello" over the QEMU UART via volatile pointer writes. |
| **M2** ✅ | Compound data, enums and modules, in the steps below. |
| **M3** | `memory { ... }` blocks generating the linker script; `peripheral` blocks with native MMIO. The UART is declared in the language. |
| **M4** | Interrupt handlers, vector tables, inline `asm`, compiler and hardware fences (ADR 0023). |
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
| **M3.4** Register arrays and SVD | register arrays, clusters and `derivedFrom`; the design checked against the SVD of a real riscv32 chip; whether `svd2erebos` is a tool of its own | a peripheral generated from SVD, run on QEMU |
| **M3.5** Second target | ARM Cortex-M on QEMU; what depends on the target becomes a per-target description (pointer width, widest access, ABI, startup, linker script, LLVM setup, runtime functions) | the e2e programs on both targets |

Decisions already agreed for the later steps:

- **M3.5:** the second target comes before M4, so that interrupts and
  vector tables, the most target-specific part of the language, are
  designed with both targets in view.

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
- A compiler server for AI agents: look up a symbol by name and get its
  definition, type and uses from the compiler, and edit by symbol (a patch
  to a function or item) with the change type checked before it is
  applied, instead of reading and matching file text. Unlike LSP, queries
  are by name, not by cursor position. It builds on the on-demand queries
  of ADR 0028 and the structured diagnostics; still to be designed.
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
- Several cores (harts) sharing memory. On microcontrollers concurrency is
  mostly interrupts, which M4 covers; several cores need atomics (the `A`
  extension) and a memory model of their own.
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

- **M3** (M3.2 to M3.4 for peripherals, M3.5 for the target):
  - `peripheral` blocks must express what an SVD file describes, so that
    a `svd2erebos` can generate them: register arrays, clusters and
    `derivedFrom` (M3.4). Access modes, offsets and widths are settled
    (ADR 0058); fields at bit positions, enums in fields and defaults
    used as reset values (ADR 0059), the six `oneTo*`/`zeroTo*` write
    effects and read effects (ADR 0060). Left to M3.4, checking against
    the SVD of a real chip: access modes of fields (SVD's read-only
    fields in a read-write register); the write effects without a
    neutral value (`clear`, `set`, `modify`); the reset value of a
    register without fields (ADR 0060).
  - Registers sharing an address in one mode are declared in alternate
    groups (ADR 0061). Peripherals sharing addresses, SVD's
    `alternatePeripheral`, stay `ty::peripheral-overlap`; M3.4, checking
    against SVD, decides whether to relax that.
    The SVD of a real riscv32 chip (ESP32-C3, GD32VF103 or CH32V) tests
    the design. Also whether `svd2erebos` is a separate tool or a file
    read at compile time, which bears on reproducible builds.
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
- **M4:**
  - A concurrency model spanning M4 and M5, in an ADR written before M4
    starts, so that M4 decides nothing M5 must break. M4 may share a
    `static mut` with interrupt handlers without checks (critical
    sections, as in C; ADR 0030) if M5 then only adds checks and changes
    no meaning. Rust checks sharing with the `Send` and `Sync` traits, and
    Erebos has no traits (see M6). The direction discussed: interrupt
    handlers and their priorities are declared in the language, so the
    compiler can compute from the call graph which statics each handler
    reaches and require a critical section only where code of another
    priority reaches the same static, as the priority ceiling protocol of
    RTIC does, but in the compiler instead of macros. A call through a
    function pointer hides its callee; the ADR must say what that means.
  - Compiler and hardware fences (ADR 0023).
  - Moving `_start` into the language (ADR 0012, 0030).
- **M5:**
  - Constants and statics of types that must not be copied: duplicating a
    peripheral handle is a bug ownership should catch (ADR 0021), but a
    constant is copied at each use and reading a static copies it.
  - Copy versus move (ADR 0021), and borrow checking (ADR 0009).
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
- **Next: M3.4** (register arrays and SVD).
