# Agent guide

Instructions for AI coding agents working on Erebos. Humans: see
[`CONTRIBUTING.md`](CONTRIBUTING.md), which also applies to agents.

## What Erebos is

A small, Rust-inspired systems language for **bare metal only**: no OS, no
std, no runtime, no heap. Hardware concepts (memory maps, MMIO, interrupts)
become language constructs. The compiler is written in Rust and uses LLVM via
`inkwell`.

## Before you change anything

1. Read the ADR index in [`docs/adr/README.md`](docs/adr/README.md). The
   accepted ADRs are binding.
2. Check [`docs/roadmap.md`](docs/roadmap.md) to see which milestone is
   current. Do not implement features from later milestones unless asked.
3. If a task requires contradicting an ADR, **stop and say so**. Do not
   silently work around it. Propose a new ADR instead.

## Hard rules

- Everything in the repository is in **English**, even when the conversation
  is in another language.
- Respect layer boundaries (ADR 0003, 0028): a compiler layer only consumes
  the output of the layer directly before it. The one edge back: type
  checking and MIR building ask the compile-time evaluator for values, and
  get values, never MIR. Codegen sees only MIR and evaluated data.
- No type inference and no implicit conversions (ADR 0008).
- Integer `+ - *` wrap; never emit `nsw`/`nuw` (ADR 0007).
- Only the target C ABI exists (ADR 0006). Aggregates cross calls by value,
  with the riscv32 rules of ADR 0025.
- Hand-written lexer and parser; do not add parser generators (ADR 0004).
- User errors produce `miette` diagnostics with spans, never panics.
- Do not add dependencies without stating why in the commit body.

## Workflow

- Work in small steps that each build and pass
  `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test`.
- Add or update tests with every change (snapshot tests per layer,
  end-to-end tests on QEMU).
- Commit messages follow Conventional Commits with layer scopes
  (see `CONTRIBUTING.md`). Only commit when asked.
- When you make a design decision that is not covered by an ADR, write it
  down as a `Proposed` ADR and flag it to the user.

## Project layout

```
docs/adr/       Architecture Decision Records (binding)
docs/roadmap.md Milestones and pending decisions
docs/reference.md The language as implemented (keep in sync with the compiler)
src/            Compiler (single crate; modules per layer, see ADR 0003)
rt/             Runtime support functions in Erebos, built into every program (ADR 0020)
tests/ui/       Snapshot tests per compiler stage
tests/e2e/      Programs run on QEMU, with expected output and exit code
tests/layout/   Structs whose layout is checked against clang
```
