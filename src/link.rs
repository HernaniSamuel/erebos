//! Linking: generates the linker script from the target description and
//! runs `ld.lld` (ADR 0005). Memory maps become language-declared in M3.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::target::Target;

pub fn linker_script(target: &Target) -> String {
    format!(
        "OUTPUT_ARCH(riscv)\n\
         ENTRY(_start)\n\
         \n\
         MEMORY {{\n\
         \x20   RAM (rwx) : ORIGIN = {:#x}, LENGTH = {:#x}\n\
         }}\n\
         \n\
         SECTIONS {{\n\
         \x20   /* The startup stub must be at the reset address (ADR 0012). */\n\
         \x20   .text : {{ KEEP(*(.text.start)) *(.text .text.*) }} > RAM\n\
         \x20   .rodata : {{ *(.rodata .rodata.*) }} > RAM\n\
         \x20   .data : {{ *(.data .data.*) }} > RAM\n\
         \x20   .bss (NOLOAD) : {{ *(.bss .bss.* COMMON) }} > RAM\n\
         }}\n",
        target.ram_origin, target.ram_size
    )
}

/// Links `object` into an ELF executable at `output`.
pub fn link(object: &Path, script: &Path, output: &Path) -> Result<(), String> {
    let lld = find_lld();
    let result = Command::new(&lld)
        .arg("-T")
        .arg(script)
        .arg(object)
        .arg("-o")
        .arg(output)
        .output()
        .map_err(|e| format!("cannot run `{}`: {e}", lld.display()))?;

    if !result.status.success() {
        return Err(format!(
            "linking failed:\n{}",
            String::from_utf8_lossy(&result.stderr).trim_end()
        ));
    }
    Ok(())
}

/// `EREBOS_LLD` wins; otherwise the `ld.lld` of the LLVM we build against
/// (ADR 0011); otherwise whatever is on `PATH`.
fn find_lld() -> PathBuf {
    if let Some(path) = env::var_os("EREBOS_LLD") {
        return path.into();
    }
    let exe = format!("ld.lld{}", env::consts::EXE_SUFFIX);
    if let Some(prefix) = env::var_os("LLVM_SYS_221_PREFIX") {
        let candidate = Path::new(&prefix).join("bin").join(&exe);
        if candidate.exists() {
            return candidate;
        }
    }
    exe.into()
}
