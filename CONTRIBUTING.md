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
  Each `tests/ui/*.ers` file is compiled up to the stage named on its first
  line (`// emit: <stage>`); review new snapshots with `cargo insta review`.
- **End-to-end tests** (`tests/e2e/*.ers`) compile an Erebos program and boot
  it on QEMU. They need `qemu-system-riscv32` on `PATH` (or `EREBOS_QEMU`).
  A program declares `// expect-output: <line>` (repeatable) and
  `// expect-exit: <code>`; it must print exactly those lines and stop QEMU
  through the test finisher with that code. A program without
  `expect-exit` must run forever. Any CPU exception fails the test.
  Every program runs both unoptimized and with `-O`: the optimizer exploits
  undefined behavior, so a difference means the generated IR is wrong.
- `erebos build <file> [-O] --emit=<stage>` prints any stage (`tokens`, `ast`,
  `hir`, `typeck`, `mir`, `llvm-ir`), which is the quickest way to debug.
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

Native Windows (ADR 0011):

1. **Visual Studio Build Tools** with the "Desktop development with C++"
   workload (MSVC linker and Windows SDK).
2. **Rust** via rustup, toolchain `stable-x86_64-pc-windows-msvc`.
3. **LLVM 22** (required by `inkwell`, ADR 0005). Do not use the LLVM
   installer; download the full archive from the LLVM GitHub releases:
   `clang+llvm-22.1.x-x86_64-pc-windows-msvc.tar.xz`, then, in **Git Bash**
   (the Windows 10 `tar.exe` cannot decompress `.xz`):

   ```sh
   mkdir -p /c/llvm-22
   tar -xJf clang+llvm-22.1.x-x86_64-pc-windows-msvc.tar.xz -C /c/llvm-22 --strip-components=1

   # The archive's llvm-config lists xml2s.lib (libxml2) as a system library but
   # does not ship it. Only LLVMWindowsManifest uses it, and inkwell never does,
   # so an empty stub library satisfies the linker.
   echo '/* empty libxml2 stub */' > xml2s_stub.c
   /c/llvm-22/bin/clang.exe --target=x86_64-pc-windows-msvc -c xml2s_stub.c -o xml2s_stub.obj
   /c/llvm-22/bin/llvm-lib.exe /out:C:/llvm-22/lib/xml2s.lib xml2s_stub.obj
   ```

   Then, in PowerShell:

   ```powershell
   [Environment]::SetEnvironmentVariable('LLVM_SYS_221_PREFIX', 'C:\llvm-22', 'User')
   ```

   Restart the terminal / VS Code afterwards so the variable is picked up.

   The archive's libraries use the static C runtime (`libcmt`), so the
   repository's `.cargo/config.toml` builds Rust with `+crt-static` too.
   Mixing runtimes makes the compiler crash on exit; do not remove it.
   `C:\llvm-22\bin\ld.lld.exe` is the linker used for Erebos programs.
4. **QEMU** for the riscv32 target (ADR 0010): the Windows installer from
   <https://qemu.weilnetz.de/w64/>, with `C:\Program Files\qemu` on `PATH`.

Verify:

```powershell
C:\llvm-22\bin\llvm-config.exe --version   # 22.x
C:\llvm-22\bin\ld.lld.exe --version
qemu-system-riscv32 --version
```

On Linux, the equivalent is LLVM 22 from <https://apt.llvm.org>
(`llvm-22-dev`, `libpolly-22-dev`, `lld-22`), `LLVM_SYS_221_PREFIX=/usr/lib/llvm-22`
and `qemu-system-misc`.
