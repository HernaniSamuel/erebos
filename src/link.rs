//! Linking: generates the linker script from the program's memory map
//! (ADR 0057) and runs `ld.lld` (ADR 0005), reading back the regions it
//! reports overflowing.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::diag::Diagnostic;
use crate::mir::MemoryMap;
use crate::target::{self, Target};

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

/// The linker script: from the program's memory map, or with the fixed
/// layout of ADR 0030 for a program without one.
pub fn linker_script(map: Option<&MemoryMap>, target: &Target) -> String {
    match map {
        Some(map) => mapped_script(map),
        None => fixed_script(target),
    }
}

/// The script of a `memory` block (ADR 0057): every region under its own
/// name; in the code's region the reset entry in an output section of its
/// own, then the code, the read-only data and the initial values of
/// `.data`; at the start of the stack's region the stack; `.data` and
/// `.bss` in theirs, as ADR 0030 lays them out.
fn mapped_script(map: &MemoryMap) -> String {
    let regions: String = map
        .regions
        .iter()
        .map(|r| {
            format!(
                "    {} : ORIGIN = {:#x}, LENGTH = {:#x}\n",
                r.name, r.origin, r.size
            )
        })
        .collect();
    let code = &map.regions[map.code].name;
    let data = &map.regions[map.data].name;
    let stack = &map.regions[map.stack].name;
    // When data shares the code's region, the load image of `.data` is its
    // run range (ADR 0057).
    let data_place = if map.data == map.code {
        format!("> {data}")
    } else {
        format!("> {data} AT> {code}")
    };
    format!(
        "{HEADER}\
         MEMORY {{\n\
         {regions}\
         }}\n\
         \n\
         SECTIONS {{\n\
         \x20   /* The reset entry, alone so that only its own alignment counts:\n\
         \x20      it is at the origin of the code's region (ADR 0057). */\n\
         \x20   .text.start : {{ KEEP(*(.text.start)) }} > {code}\n\
         \x20   .text : {{ *(.text .text.*) }} > {code}\n\
         \x20   .rodata : {{ {RODATA} }} > {code}\n\
         \x20   /* At the start of its region, below anything else there (ADR 0057). */\n\
         \x20   .stack (NOLOAD) : ALIGN({align}) {{\n\
         \x20       __stack_start = .;\n\
         \x20       . += {stack_size:#x};\n\
         \x20       __stack_end = .;\n\
         \x20   }} > {stack}\n\
         {statics}\
         {unloaded}\
         }}\n",
        align = target::STACK_ALIGN,
        stack_size = map.stack_size,
        statics = statics(&data_place, data),
        unloaded = unloaded(),
    )
}

/// The image laid out as on a board whose program is in flash: code,
/// read-only data and the initial values of `.data` in `IMAGE`; `.data`
/// and `.bss` in `RAM`, where `_start` sets them up from the symbols
/// defined here (ADR 0030). The stack has no size: it grows down from the
/// end of RAM towards `.bss`. A program without a `memory` block keeps
/// this layout until every program declares one (M3.1).
fn fixed_script(target: &Target) -> String {
    let (image, ram) = (target.image(), target.ram());
    format!(
        "{HEADER}\
         MEMORY {{\n\
         \x20   IMAGE (rx) : ORIGIN = {:#x}, LENGTH = {:#x}\n\
         \x20   RAM (rw) : ORIGIN = {:#x}, LENGTH = {:#x}\n\
         }}\n\
         \n\
         SECTIONS {{\n\
         \x20   /* The startup stub must be at the reset address (ADR 0012). */\n\
         \x20   .text : {{ KEEP(*(.text.start)) *(.text .text.*) }} > IMAGE\n\
         \x20   .rodata : {{ {RODATA} }} > IMAGE\n\
         {statics}\
         \x20   /* The stack, from the end of `.bss` to the end of RAM. */\n\
         \x20   __stack_start = __bss_end;\n\
         \x20   __stack_end = {:#x};\n\
         {unloaded}\
         }}\n",
        image.origin,
        image.size,
        ram.origin,
        ram.size,
        target.stack_top(),
        statics = statics("> RAM AT> IMAGE", "RAM"),
        unloaded = unloaded(),
    )
}

const HEADER: &str = "OUTPUT_ARCH(riscv)\nENTRY(_start)\n\n";

/// The input sections of `.rodata`, with the small-data ones (ADR 0030).
const RODATA: &str = "*(.rodata .rodata.* .srodata .srodata.*)";

/// The sections that are not loaded, each placed explicitly (ADR 0030).
fn unloaded() -> String {
    let sections: String = UNLOADED
        .iter()
        .map(|name| format!("    {name} 0 : {{ *({name}) }}\n"))
        .collect();
    format!("    /* Not loaded. */\n{sections}")
}

/// `.data`, placed by `data_place`, and `.bss`, in the region `data`, with
/// the symbols `_start` sets them up from (ADR 0030).
fn statics(data_place: &str, data: &str) -> String {
    format!(
        "\x20   /* Aligned on the section, so that its load address is too. */\n\
         \x20   .data : ALIGN(4) {{\n\
         \x20       __data_start = .;\n\
         \x20       *(.data .data.* .sdata .sdata.*)\n\
         \x20       . = ALIGN(4);\n\
         \x20       __data_end = .;\n\
         \x20   }} {data_place}\n\
         \x20   __data_load = LOADADDR(.data);\n\
         \x20   .bss (NOLOAD) : ALIGN(4) {{\n\
         \x20       __bss_start = .;\n\
         \x20       *(.bss .bss.* .sbss .sbss.* COMMON)\n\
         \x20       . = ALIGN(4);\n\
         \x20       __bss_end = .;\n\
         \x20   }} > {data}\n"
    )
}

/// Why linking failed.
#[derive(Debug)]
pub enum LinkError {
    /// `ld.lld` could not be run.
    Run(String),
    /// `ld.lld` ran and failed, with this error output.
    Failed(String),
}

/// Links `object`, with the other objects `extra`, into an ELF executable at
/// `output`.
pub fn link(
    object: &Path,
    extra: &[PathBuf],
    script: &Path,
    output: &Path,
) -> Result<(), LinkError> {
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
        .map_err(|e| LinkError::Run(format!("cannot run `{}`: {e}", lld.display())))?;

    if !result.status.success() {
        return Err(LinkError::Failed(
            String::from_utf8_lossy(&result.stderr).into_owned(),
        ));
    }
    Ok(())
}

/// A section `ld.lld` reports ending past its region, in a line of its
/// error output such as `ld.lld: error: section '.text' will not fit in
/// region 'FLASH': overflowed by 12 bytes`; the overflow is counted up to
/// the section's end.
#[derive(Debug, PartialEq, Eq)]
struct Overflow<'a> {
    section: &'a str,
    region: &'a str,
    bytes: u64,
}

impl<'a> Overflow<'a> {
    fn parse(line: &'a str) -> Option<Self> {
        let (_, rest) = line.split_once("error: section '")?;
        let (section, rest) = rest.split_once("' will not fit in region '")?;
        let (region, rest) = rest.split_once("': overflowed by ")?;
        let bytes = rest.trim_end().strip_suffix(" bytes")?.parse().ok()?;
        Some(Overflow {
            section,
            region,
            bytes,
        })
    }
}

/// Reads the region overflows in `stderr`, `ld.lld`'s error output when
/// linking with the script of `map`: one `link::region-overflow` per
/// region, at its name in the source, naming the first section reported
/// there and giving the largest overflow, which is how many bytes the
/// region lacks (ADR 0057); in the order of the regions. Also returns the
/// lines of `stderr` that report something else.
pub fn region_overflows<'a>(stderr: &'a str, map: &MemoryMap) -> (Vec<Diagnostic>, Vec<&'a str>) {
    // Per region, the first section and the largest overflow.
    let mut found: Vec<Option<(&str, u64)>> = vec![None; map.regions.len()];
    let mut rest = Vec::new();
    for line in stderr.lines() {
        let overflow = Overflow::parse(line).and_then(|overflow| {
            let index = map.regions.iter().position(|r| r.name == overflow.region)?;
            Some((index, overflow))
        });
        match overflow {
            Some((index, overflow)) => {
                let entry = found[index].get_or_insert((overflow.section, 0));
                entry.1 = entry.1.max(overflow.bytes);
            }
            None => rest.push(line),
        }
    }
    let diags = found
        .into_iter()
        .enumerate()
        .filter_map(|(index, found)| {
            let (section, bytes) = found?;
            Some(overflow_diagnostic(map, index, section, bytes))
        })
        .collect();
    (diags, rest)
}

/// `link::region-overflow` for the region `index` of `map`, where `section`
/// is the first section that does not fit and `bytes` how many the region
/// lacks.
fn overflow_diagnostic(map: &MemoryMap, index: usize, section: &str, bytes: u64) -> Diagnostic {
    let region = &map.regions[index];
    // In the code's region, `.data` is its load image, unless the run
    // addresses are there too (ADR 0057).
    let what = if section == ".data" && index == map.code && map.data != map.code {
        "the initial values of `.data` do".to_owned()
    } else {
        format!("`{section}` does")
    };
    let unit = if bytes == 1 { "byte" } else { "bytes" };
    Diagnostic::error(
        "link::region-overflow",
        format!("{what} not fit in the region `{}`", region.name),
    )
    .label(region.span, format!("{bytes} {unit} too small"))
    .help(format!(
        "`{}` holds {}",
        region.name,
        listed(&contents(map, index))
    ))
}

/// `items` joined by commas, with `and` before the last.
fn listed(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// What the region `index` of `map` holds, in the order the script places
/// it there.
fn contents(map: &MemoryMap, index: usize) -> Vec<String> {
    let mut contents = Vec::new();
    if index == map.code {
        contents.extend(["the reset entry", "the code", "the read-only data"].map(String::from));
        if map.data != map.code {
            contents.push("the initial values of `.data`".to_owned());
        }
    }
    if index == map.stack {
        contents.push(format!("the stack ({} bytes)", map.stack_size));
    }
    if index == map.data {
        contents.extend(["`.data`", "`.bss`"].map(String::from));
    }
    contents
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::Region;
    use crate::span::{SourceMap, Span};

    /// `FLASH` holding the code, `RAM` the data and the stack, and `SPARE`
    /// nothing.
    fn map() -> MemoryMap {
        let file = SourceMap::default().add("main.ers", "");
        let region = |name: &str, origin| Region {
            name: name.to_owned(),
            origin,
            size: 0x1000,
            span: Span::new(file, 0, 0),
        };
        MemoryMap {
            regions: vec![
                region("FLASH", 0x8000_0000),
                region("RAM", 0x8001_0000),
                region("SPARE", 0x8002_0000),
            ],
            code: 0,
            data: 1,
            stack: 1,
            stack_size: 0x800,
        }
    }

    /// The program's name before `error:` does not matter, so that the
    /// linker's messages are read whatever its path.
    #[test]
    fn parses_overflows() {
        let line = r"C:\llvm\bin\ld.lld.exe: error: section '.text' will not fit in region 'FLASH': overflowed by 12 bytes";
        assert_eq!(
            Overflow::parse(line),
            Some(Overflow {
                section: ".text",
                region: "FLASH",
                bytes: 12
            })
        );
        assert_eq!(Overflow::parse("ld.lld: error: undefined symbol: f"), None);
    }

    /// One diagnostic per region, in the order of the regions, with the
    /// first section reported there and the largest overflow; `.data` in
    /// the code's region is its load image. The other lines are kept, and
    /// so are overflows of regions the map does not have.
    #[test]
    fn one_diagnostic_per_region() {
        let stderr = "\
ld.lld: error: section '.bss' will not fit in region 'RAM': overflowed by 4 bytes
ld.lld: error: section '.data' will not fit in region 'FLASH': overflowed by 40 bytes
ld.lld: error: section '.rodata' will not fit in region 'FLASH': overflowed by 35 bytes
ld.lld: error: section '.x' will not fit in region 'OTHER': overflowed by 1 bytes
ld.lld: error: something else
";
        let (diags, rest) = region_overflows(stderr, &map());
        let messages: Vec<_> = diags
            .iter()
            .map(|d| (d.message.as_str(), d.labels[0].message.as_str()))
            .collect();
        assert_eq!(
            messages,
            [
                (
                    "the initial values of `.data` do not fit in the region `FLASH`",
                    "40 bytes too small"
                ),
                (
                    "`.bss` does not fit in the region `RAM`",
                    "4 bytes too small"
                ),
            ]
        );
        assert_eq!(
            rest,
            [
                "ld.lld: error: section '.x' will not fit in region 'OTHER': overflowed by 1 bytes",
                "ld.lld: error: something else"
            ]
        );
    }

    /// When data shares the code's region, `.data` there is its run range,
    /// and is named as such.
    #[test]
    fn data_in_the_code_region() {
        let mut map = map();
        map.data = 0;
        let stderr =
            "ld.lld: error: section '.data' will not fit in region 'FLASH': overflowed by 1 bytes";
        let (diags, _) = region_overflows(stderr, &map);
        assert_eq!(
            diags[0].message,
            "`.data` does not fit in the region `FLASH`"
        );
        assert_eq!(diags[0].labels[0].message, "1 byte too small");
        assert_eq!(
            diags[0].help.as_deref(),
            Some("`FLASH` holds the reset entry, the code, the read-only data, `.data` and `.bss`")
        );
    }
}
