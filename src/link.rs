//! Linking: generates the linker script from the target description and
//! runs `ld.lld` (ADR 0005). Memory maps become language-declared in M3.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::target::Target;

/// The sections that are not loaded, placed explicitly so that no section
/// is an orphan; the symbol table is what tools read (ADR 0030).
const UNLOADED: &[&str] = &[
    ".symtab",
    ".strtab",
    ".shstrtab",
    ".comment",
    ".riscv.attributes",
    // Debug information, DWARF 2 to 5 and GNU extensions, each in a
    // section of its own so that it stays valid.
    ".debug_abbrev",
    ".debug_addr",
    ".debug_aranges",
    ".debug_cu_index",
    ".debug_frame",
    ".debug_gnu_pubnames",
    ".debug_gnu_pubtypes",
    ".debug_info",
    ".debug_line",
    ".debug_line_str",
    ".debug_loc",
    ".debug_loclists",
    ".debug_macinfo",
    ".debug_macro",
    ".debug_names",
    ".debug_pubnames",
    ".debug_pubtypes",
    ".debug_ranges",
    ".debug_rnglists",
    ".debug_str",
    ".debug_str_offsets",
    ".debug_tu_index",
    ".debug_types",
];

/// The image laid out as on a board whose program is in flash: code,
/// read-only data and the initial values of `.data` in `IMAGE`; `.data`
/// and `.bss` in `RAM`, where `_start` sets them up from the symbols
/// defined here (ADR 0030). Every symbol is a multiple of 4, and the
/// ranges cover their sections.
pub fn linker_script(target: &Target) -> String {
    let (image, ram) = (target.image(), target.ram());
    let unloaded: String = UNLOADED
        .iter()
        .map(|name| format!("    {name} 0 : {{ *({name}) }}\n"))
        .collect();
    format!(
        "OUTPUT_ARCH(riscv)\n\
         ENTRY(_start)\n\
         \n\
         MEMORY {{\n\
         \x20   IMAGE (rx) : ORIGIN = {:#x}, LENGTH = {:#x}\n\
         \x20   RAM (rw) : ORIGIN = {:#x}, LENGTH = {:#x}\n\
         }}\n\
         \n\
         SECTIONS {{\n\
         \x20   /* The startup stub must be at the reset address (ADR 0012). */\n\
         \x20   .text : {{ KEEP(*(.text.start)) *(.text .text.*) }} > IMAGE\n\
         \x20   .rodata : {{ *(.rodata .rodata.* .srodata .srodata.*) }} > IMAGE\n\
         \x20   /* Aligned on the section, so that its load address is too. */\n\
         \x20   .data : ALIGN(4) {{\n\
         \x20       __data_start = .;\n\
         \x20       *(.data .data.* .sdata .sdata.*)\n\
         \x20       . = ALIGN(4);\n\
         \x20       __data_end = .;\n\
         \x20   }} > RAM AT> IMAGE\n\
         \x20   __data_load = LOADADDR(.data);\n\
         \x20   .bss (NOLOAD) : ALIGN(4) {{\n\
         \x20       __bss_start = .;\n\
         \x20       *(.bss .bss.* .sbss .sbss.* COMMON)\n\
         \x20       . = ALIGN(4);\n\
         \x20       __bss_end = .;\n\
         \x20   }} > RAM\n\
         \x20   /* Not loaded. */\n\
         {unloaded}\
         }}\n",
        image.origin, image.size, ram.origin, ram.size
    )
}

/// Links `object`, with the other objects `extra`, into an ELF executable at
/// `output`.
pub fn link(object: &Path, extra: &[PathBuf], script: &Path, output: &Path) -> Result<(), String> {
    let lld = find_lld();
    // A section the script does not place would be left outside what
    // `_start` initializes (ADR 0030).
    let result = Command::new(&lld)
        .arg("--orphan-handling=error")
        .arg("-T")
        .arg(script)
        .arg(object)
        .args(extra)
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
