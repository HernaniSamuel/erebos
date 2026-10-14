# Architecture Decision Records

This directory records the decisions that shape Erebos: what was decided, why,
and what it costs. Chats and memory fade; these files do not.

## When to write an ADR

Write one when a decision:

- changes what the language means (syntax, semantics, types, ABI), or
- changes how the compiler is structured (layers, dependencies, backends), or
- would surprise a new contributor who only reads the code.

Do not write one for local implementation details that the code already explains.

## How

1. Copy [`0000-template.md`](0000-template.md) to `NNNN-short-kebab-title.md`
   using the next free number.
2. Start with `Status: Proposed`. Change to `Accepted` once decided.
3. ADRs are immutable once accepted. To change a decision, write a new ADR
   and mark the old one `Superseded by NNNN`.
4. Add it to the index below in the same commit (`docs(adr): ...`).

## Index

| #    | Title                                                            | Status   |
|------|------------------------------------------------------------------|----------|
| 0001 | [Record architecture decisions](0001-record-architecture-decisions.md) | Accepted |
| 0002 | [Bare-metal-only scope](0002-bare-metal-only-scope.md)           | Accepted |
| 0003 | [Layered compiler pipeline](0003-layered-compiler-pipeline.md)   | Accepted, amended by 0028 |
| 0004 | [Hand-written lexer and parser](0004-hand-written-lexer-and-parser.md) | Accepted |
| 0005 | [LLVM backend via inkwell](0005-llvm-backend-via-inkwell.md)     | Accepted |
| 0006 | [C ABI as the only ABI](0006-c-abi-as-the-only-abi.md)           | Accepted |
| 0007 | [Wrapping integer arithmetic](0007-wrapping-integer-arithmetic.md) | Accepted |
| 0008 | [Explicit types, no inference](0008-explicit-types-no-inference.md) | Accepted, amended by 0024 |
| 0009 | [Defer borrow checking](0009-defer-borrow-checking.md)           | Accepted |
| 0010 | [First target: riscv32 on QEMU `virt`](0010-first-target-riscv32-qemu-virt.md) | Accepted |
| 0011 | [Development environment: native Windows with prebuilt LLVM 22](0011-development-environment.md) | Accepted |
| 0012 | [Provisional compiler-generated startup code](0012-provisional-compiler-generated-startup.md) | Accepted |
| 0013 | [Provisional volatile pointers](0013-provisional-volatile-pointers.md) | Accepted |
| 0014 | [Panic handler](0014-panic-handler.md)                          | Accepted |
| 0015 | [Expression-oriented blocks and implicit unit return](0015-expression-oriented-blocks.md) | Accepted |
| 0016 | [Literals: integers and bytes; no chars, strings or floats](0016-literals-bytes-no-floats.md) | Accepted |
| 0017 | [Literal operands of `as` take the target type](0017-literal-cast-operands.md) | Accepted |
| 0018 | [Evaluation order](0018-evaluation-order.md)                   | Accepted |
| 0019 | [Conversions, pointer representation and pointer validity](0019-integer-pointer-conversions-and-operator-details.md) | Accepted |
| 0020 | [Runtime support functions written in Erebos](0020-runtime-support-functions-in-erebos.md) | Accepted |
| 0021 | [Structs and arrays](0021-structs-and-arrays.md)               | Accepted |
| 0022 | [Data layout and alignment control](0022-data-layout-and-alignment.md) | Accepted |
| 0023 | [Volatile access guarantees](0023-volatile-access-guarantees.md) | Accepted |
| 0024 | [Implicit conversion from `*mut T` to `*const T`](0024-mut-to-const-pointer-conversion.md) | Accepted |
| 0025 | [Aggregates across calls on riscv32](0025-aggregates-across-calls.md) | Accepted |
| 0026 | [External functions](0026-external-functions.md) | Accepted |
| 0027 | [Compile-time execution (comptime)](0027-compile-time-execution.md) | Accepted |
| 0028 | [On-demand type checking and evaluation](0028-on-demand-compilation.md) | Accepted |
