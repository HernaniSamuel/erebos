# Contributing to Erebos

## Language

Everything in the repository is written in **English**: code, identifiers,
comments, diagnostics, documentation, commit messages, issues and PRs.

## Design decisions

Read [`docs/adr/`](docs/adr/README.md) before changing language semantics or
compiler structure. If your change contradicts or extends an accepted ADR,
write a new ADR in the same PR.

## Commits

Follow [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <imperative summary, lowercase, no period>

<optional body: what and why, wrapped at 72 columns>
```

**Types:** `feat`, `fix`, `refactor`, `perf`, `test`, `docs`, `build`, `ci`, `chore`.

**Scopes** follow the compiler layers (ADR 0003):
`span`, `diag`, `lexer`, `parser`, `ast`, `resolve`, `hir`, `ty`, `const`,
`mir`, `codegen`, `link`, `driver`, `adr`, `spec`.

Examples:

```
feat(parser): parse while loops
fix(lexer): reject unterminated block comments
docs(adr): add 0010 shift semantics
```

Keep commits small and focused: one logical change per commit. Every commit
should build and pass tests.

## Branches

`main` is always green. Work on short-lived branches named
`<type>/<short-description>` (e.g. `feat/pratt-expressions`) and merge via PR.

## Code quality gates

Before committing:

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
```

## Tests

- **Per-layer snapshot tests** (`insta`) for tokens, AST, HIR, MIR and
  diagnostics. Snapshot changes must be reviewed, not blindly accepted.
- **End-to-end tests** compile an Erebos program, run it on QEMU, and compare
  its UART output.
- Every bug fix comes with a test that fails without the fix.
- Every new diagnostic comes with a snapshot of its rendered output.

## Code style

- No `unwrap()`/`expect()` on user-controlled input: report a diagnostic.
  Internal invariants may use `expect("why this cannot fail")`.
- No panics for user errors. The compiler reports and recovers.
- Comments explain *why*, not *what*.
- Prefer plain data and functions over traits and generics until there is a
  second concrete use case.

## Development environment

- Rust: stable toolchain (edition 2024).
- LLVM: a development build providing `llvm-config` and static libraries is
  required by `inkwell` (ADR 0005). *Version and setup instructions: TBD.*
- `ld.lld` for linking.
- QEMU (`qemu-system-arm` or `qemu-system-riscv32`, depending on the first
  target) for end-to-end tests. *TBD.*
