//! Helpers shared by the integration tests.

use std::env;
use std::path::{Path, PathBuf};

/// clang, found like `ld.lld`: `EREBOS_CLANG`, then the LLVM we build
/// against (`LLVM_SYS_221_PREFIX`), then `PATH`.
pub fn clang_path() -> PathBuf {
    if let Some(path) = env::var_os("EREBOS_CLANG") {
        return path.into();
    }
    let exe = format!("clang{}", env::consts::EXE_SUFFIX);
    if let Some(prefix) = env::var_os("LLVM_SYS_221_PREFIX") {
        let candidate = Path::new(&prefix).join("bin").join(&exe);
        if candidate.exists() {
            return candidate;
        }
    }
    exe.into()
}

/// The flags that make clang compile for the target Erebos compiles for
/// (ADR 0010): RV32IMAC, soft-float ilp32 ABI, bare metal.
pub const CLANG_TARGET: &[&str] = &[
    "--target=riscv32-unknown-elf",
    "-march=rv32imac",
    "-mabi=ilp32",
    "-ffreestanding",
];
