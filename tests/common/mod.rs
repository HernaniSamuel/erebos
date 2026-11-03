//! Helpers shared by the integration tests.

use std::env;
use std::path::{Path, PathBuf};

use erebos::target::{Arch, Target};

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

/// The flags that make clang compile for `target`, the machine Erebos
/// compiles for: on riscv32, RV32IMAC with the soft-float ilp32 ABI (ADR
/// 0010); on thumbv7m, a Cortex-M3 with the soft-float AAPCS (ADR 0074);
/// both bare metal. Chosen by the architecture, so that a new one fails to
/// compile here until it has its flags.
#[allow(dead_code, reason = "not every test uses every tool")]
pub fn clang_target(target: &Target) -> &'static [&'static str] {
    match target.arch {
        Arch::Riscv32 => &[
            "--target=riscv32-unknown-elf",
            "-march=rv32imac",
            "-mabi=ilp32",
            "-ffreestanding",
        ],
        Arch::Armv7m => &[
            "--target=thumbv7m-none-eabi",
            "-mcpu=cortex-m3",
            "-mfloat-abi=soft",
            "-ffreestanding",
        ],
    }
}

/// The board modules of the tests (ADR 0077).
#[allow(dead_code, reason = "not every test builds programs with a board")]
pub mod board;

/// Running a program on QEMU (ADR 0010).
#[allow(dead_code, reason = "only some tests run programs")]
pub mod qemu;

/// The generator of valid integer programs (ADR 0068).
#[allow(dead_code, reason = "only tests/generated.rs uses it")]
pub mod generator;
