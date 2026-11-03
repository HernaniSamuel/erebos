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

**Scopes** follow the compiler layers (ADR 0003, 0033):
`span`, `diag`, `lexer`, `parser`, `ast`, `resolve`, `hir`, `typeck`,
`layout`, `mir`, `eval`, `codegen`, `link`, `driver`, `svd` (the SVD
generator, ADR 0070), `adr`, `spec`.

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

`sh scripts/check.sh` runs the same gates, checking the formatting
instead of applying it; agents are held to it when they finish
(`.claude/settings.json`).

## Tests

- **Per-layer snapshot tests** (`insta`) for tokens, AST, HIR, MIR and
  diagnostics. Snapshot changes must be reviewed, not blindly accepted.
  Each `tests/ui/*.ers` file is compiled up to the stage named in its header
  (`// emit: <stage>`); with `// opt: both` it is also compiled with `-O`,
  and both results go in the snapshot. A directory `tests/ui/<name>/`
  with a `main.ers` is a program of several files: `main.ers` is its root
  file, with the headers, and its module files sit beside it and below
  (ADR 0049); it is snapshotted as `<name>`. Review new snapshots with
  `cargo insta review`.
- **End-to-end tests** (`tests/e2e/*.ers`) compile an Erebos program and boot
  it on QEMU. They need `qemu-system-riscv32` on `PATH` (or `EREBOS_QEMU`).
  A program declares `// expect-output: <line>` (repeatable) and
  `// expect-exit: <code>`; it must print exactly those lines and stop QEMU
  through `board::exit` with that code. A program without
  `expect-exit` must run forever. Any CPU exception fails the test.
  A program reaches its machine through `mod board;` (ADR 0077): the
  harness copies it into a work directory with the target's board module,
  `tests/boards/<target>.ers`, as `board.ers`, which holds the `memory`
  block (ADR 0057) and offers `board::putc`, `board::exit` and
  `board::SCRATCH`. A program that has a `board.ers` of its own is an
  error; one that must declare its own devices or `memory` block says
  `// targets: <target name>` and which of ADR 0077's three reasons
  applies.
  `// link-c: file.c` (repeatable) compiles a C file next to the program
  with clang for the target, at the matching optimization level, and links
  it in; the program declares its functions with `extern fn` and its
  globals with `extern static`. A directory `tests/e2e/<name>/` with a
  `main.ers` is a program of several files, its headers in `main.ers` and
  its `link-c` files beside it. `// c-flags: <flags>` (repeatable) passes
  extra flags to clang for those C files, such as
  `-msmall-data-limit=8` or `-g`.
  Every program runs both unoptimized and with `-O`: the optimizer exploits
  undefined behavior, so a difference means the generated IR is wrong.
- `erebos build <file> [-O] --emit=<stage>` prints any stage (`tokens`, `ast`,
  `hir`, `typeck`, `mir`, `linker-script`, `llvm-ir`), which is the quickest
  way to debug. Of these, only `linker-script` needs a `memory` block, so a
  ui test needs one only at that stage.
- **Layout against C** (`tests/c_layout.rs`) compiles the structs of
  `tests/layout/structs.ers` and their C twins in `tests/layout/structs.h`,
  and has clang check that every size, alignment and offset agrees
  (ADR 0022). It needs clang: `EREBOS_CLANG`, or the one in the LLVM
  archive (`C:\llvm-22\bin\clang.exe`). A struct added to one file needs
  its twin in the other; a bit-field struct needs none, its twin being its
  integer type (ADR 0059), checked in the structs that hold it.
- **ELF checks** (`tests/elf.rs`) build a program and read its sections,
  symbols and program headers with `llvm-readelf`: where each kind of
  static goes, the symbols `_start` uses to initialize `.data` and
  `.bss` (ADR 0030), where the memory map puts the reset entry, the
  stack and the statics, and the `link::region-overflow` of a program
  that does not fit in its regions (ADR 0057). Its programs declare a
  `memory` block too, `VIRT_MAP` or a map of their own, but for the one
  showing `link::no-memory-map`. It needs `llvm-readelf`, found like clang:
  `EREBOS_READELF`, or the one in the LLVM archive
  (`C:\llvm-22\bin\llvm-readelf.exe`).
- **Reproducible builds** (`tests/reproducible.rs`) compile a program twice
  at each level and require identical executables (ADR 0027).
- **Fuzzing** (`tests/fuzz.rs`) mutates the test programs and requires the
  compiler to answer every input with output or diagnostics, never a panic.
  The seed is fixed; set `EREBOS_FUZZ_ITERATIONS` to run more inputs.
- **Architecture** (`tests/architecture.rs`, ADR 0063) checks that each
  layer uses only the modules it may, that `inkwell` and `miette` stay in
  `codegen` and `diag`, that unoptimized IR has no `nsw`/`nuw`, and that
  the panics per file of `src/` match its `PANICS` table. Raising a count
  needs a reason in the step's `Decisions`; lower it when panics go.
- **Generated programs** (`tests/generated.rs`, ADR 0068) generate valid
  integer programs from a seed (`tests/common/generator.rs`), call each
  function at compile time (the evaluator) and at run time (LLVM on QEMU),
  unoptimized and with `-O`, and require that the results agree. It finds
  wrong code generation and wrong constant evaluation, which no test with
  a hand-written expected value covers for all inputs. It needs QEMU like
  the end-to-end tests. `EREBOS_GENERATED_PROGRAMS` sets how many programs
  run (default 20) and `EREBOS_GENERATED_SEED` the seed of the first
  (default 0). A failure prints the seed: run it again with
  `EREBOS_GENERATED_SEED=<seed> EREBOS_GENERATED_PROGRAMS=1`.
- **Rejected programs** (`tests/rejected.rs`, ADR 0069) insert a snippet
  that breaks one rule of the language into a valid program of `tests/ui`
  or `tests/e2e`, and require the compiler to report the code of that
  rule. It finds rules that a context hides: an error that is reported in
  one program and missed, or reported as another code, in another. It
  needs no QEMU. `EREBOS_REJECTED_PROGRAMS` sets how many base programs
  receive every snippet (default 40) and `EREBOS_REJECTED_SEED` the seed
  of the choice of programs and places (default 0); a failure prints both.
  Every code of the diagnostics table of `docs/reference.md` needs a
  snippet in the catalog of that file, or an entry of its exempt list with
  the reason: a new diagnostic code fails `every_code_has_a_rule` until
  it has one.
- **SVD generator** (`tests/svd.rs`, ADR 0070 to 0072): each
  `tests/svd/*.svd` is generated by `erebos svd`; its snapshot holds the
  generated module and the rendered diagnostics, and the module is type
  checked, so that invalid Erebos fails the test. A file may expect errors
  of the compiler, named in an XML comment at its top (`<!-- expect-error:
  ty::peripheral-overlap -->`). The ESP32-C3's file
  (`tests/svd/chips/esp32c3.svd`) is generated and built into executables
  at both optimization levels with a small program, and a summary of it is
  snapshotted; it is not run, since QEMU has no such machine. The command
  line is tested through the binary. Add a small hand-written file for a
  construct a change touches, and do not add a vendor's file whose license
  is unclear.
  **A kept generated module** (`tests/e2e/svd_virt/virt.ers`, generated from
  `virt.svd`) must be generated again when the generator's output changes:
  `erebos svd tests/e2e/svd_virt/virt.svd` (with the compiler built, as
  `target/x86_64-pc-windows-msvc/debug/erebos.exe`), then review the diff,
  which is how a change of the generator is reviewed. A test requires
  that the kept file is what the generator writes, and fails otherwise.
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
