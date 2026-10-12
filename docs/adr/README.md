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
| 0003 | [Layered compiler pipeline](0003-layered-compiler-pipeline.md)   | Accepted |
| 0004 | [Hand-written lexer and parser](0004-hand-written-lexer-and-parser.md) | Accepted |
| 0005 | [LLVM backend via inkwell](0005-llvm-backend-via-inkwell.md)     | Accepted |
| 0006 | [C ABI as the only ABI](0006-c-abi-as-the-only-abi.md)           | Accepted |
| 0007 | [Wrapping integer arithmetic](0007-wrapping-integer-arithmetic.md) | Proposed |
| 0008 | [Explicit types, no inference](0008-explicit-types-no-inference.md) | Accepted |
| 0009 | [Defer borrow checking](0009-defer-borrow-checking.md)           | Accepted |
| 0010 | [First target: riscv32 on QEMU `virt`](0010-first-target-riscv32-qemu-virt.md) | Accepted |
| 0011 | [Development environment: native Windows with prebuilt LLVM 22](0011-development-environment.md) | Accepted |
