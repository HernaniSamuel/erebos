# 0011. Development environment: native Windows with prebuilt LLVM 22

- Status: Accepted
- Date: 2026-10-12

## Context

`inkwell` links against LLVM through `llvm-sys`, which needs an LLVM
development installation (`llvm-config` plus static libraries). The LLVM
Windows *installer* provides neither. The newest LLVM supported by `inkwell`
0.10 is 22.1 (feature `llvm22-1`).

A WSL setup was tried and worked, but splitting the work between Windows and
Linux (two filesystems, two toolchains, two editor contexts) was confusing.

## Decision

- Development happens natively on **Windows** with the Rust `msvc` toolchain
  and the Visual Studio Build Tools.
- LLVM is pinned to **22.x**, using the official prebuilt archive
  `clang+llvm-22.1.x-x86_64-pc-windows-msvc.tar.xz` (it ships `llvm-config.exe`
  and static libraries), extracted to `C:\llvm-22`.
- `LLVM_SYS_221_PREFIX=C:\llvm-22` points `llvm-sys` at it.
- `inkwell` uses the `llvm22-1` feature with only the targets we need.
- The repository keeps LF line endings (`.gitattributes`).
- Setup steps live in `CONTRIBUTING.md`.

## Consequences

- One OS, one toolchain, no compiling LLVM from source.
- The prebuilt archive declares `xml2s.lib` (libxml2) as a system library
  without shipping it. Only `LLVMWindowsManifest` needs it and `inkwell` never
  uses that component, so an empty stub `xml2s.lib` is placed in
  `C:\llvm-22\lib` (see `CONTRIBUTING.md`). If a future feature needs manifest
  merging, a real static libxml2 is required instead.
- An LLVM 23 installed separately (e.g. for `clang`) must not be used for
  building; only `C:\llvm-22` is.
- Upgrading LLVM is a deliberate change: bump the `inkwell` feature, the
  prefix variable and `CONTRIBUTING.md` together, in a new ADR.

## Alternatives considered

- **WSL 2 with apt.llvm.org packages** — verified working, rejected for the
  two-OS workflow described above. Still the recommended path on native Linux.
- **Building LLVM from source on Windows** — hours of build time and many
  configuration pitfalls for no benefit over the prebuilt archive.
