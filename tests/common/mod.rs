//! Helpers shared by the integration tests.

use std::env;
use std::path::{Path, PathBuf};

/// clang, found like `ld.lld`: `EREBOS_CLANG`, then the LLVM we build
/// against (`LLVM_SYS_221_PREFIX`), then `PATH`.
#[allow(dead_code, reason = "not every test uses every tool")]
pub fn clang_path() -> PathBuf {
    llvm_tool("clang", "EREBOS_CLANG")
}

/// `llvm-readelf`, found like clang, with `EREBOS_READELF`.
#[allow(dead_code, reason = "not every test uses every tool")]
pub fn readelf_path() -> PathBuf {
    llvm_tool("llvm-readelf", "EREBOS_READELF")
}

/// A tool of the LLVM we build against: the variable `var` wins, then
/// `LLVM_SYS_221_PREFIX`, then `PATH`.
fn llvm_tool(name: &str, var: &str) -> PathBuf {
    if let Some(path) = env::var_os(var) {
        return path.into();
    }
    let exe = format!("{name}{}", env::consts::EXE_SUFFIX);
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
#[allow(dead_code, reason = "not every test uses every tool")]
pub const CLANG_TARGET: &[&str] = &[
    "--target=riscv32-unknown-elf",
    "-march=rv32imac",
    "-mabi=ilp32",
    "-ffreestanding",
];
