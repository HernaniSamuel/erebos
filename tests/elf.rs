//! Inspects built executables with `llvm-readelf` (ADR 0030): where statics
//! go, and the symbols `_start` uses to fill `.bss` with zeros and copy the
//! initial values of `.data`; where a memory map puts the code, the data
//! and the stack, and what fails to build when they do not fit (ADR 0057).

mod common;

use std::fs;
use std::path::Path;
use std::process::Command;

use erebos::driver::{self, OptLevel};

/// Read-only data of odd length right before `.data`, a byte string after
/// the static pointing to it, and `.data` holding only data of alignment 1
/// and odd size: without the padding the script puts inside the sections,
/// the symbols would not be multiples of 4.
const PROGRAM: &str = "\
static ODD: *const [u8; 3] = &raw const b\"odd\";
static mut BYTES: [u8; 3] = [4, 5, 6];
static mut ZEROS: [u8; 5] = [0; 5];

entry fn main() -> ! { loop {} }
panic fn on_panic() -> ! { loop {} }
";

/// The memory block of the programs that do not test memory maps: QEMU
/// `virt`'s RAM laid out as ADR 0030 had it, as the e2e programs declare it.
const VIRT_MAP: &str = "memory {
    region IMAGE { origin: 0x8000_0000, size: 0x0400_0000 }
    region RAM { origin: 0x8400_0000, size: 0x0400_0000 }
    code in IMAGE;
    data in RAM;
    stack in RAM { size: 0x0010_0000 }
}
";

struct Section {
    name: String,
    address: u64,
    size: u64,
}

struct Symbol {
    name: String,
    value: u64,
    /// The section index, or `ABS`, `UND`.
    section: String,
}

#[test]
fn startup_ranges() {
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("elf");
    fs::create_dir_all(&work).unwrap();
    let source = work.join("startup.ers");
    fs::write(&source, format!("{VIRT_MAP}\n{PROGRAM}")).unwrap();
    for opt in [OptLevel::None, OptLevel::Speed] {
        let elf = work.join(format!("startup-{opt:?}.elf"));
        driver::build(&source, &elf, &[], opt, false).unwrap();
        check(&elf, opt, false);
    }
}

/// A region of a memory map, `[origin, origin + size)`.
#[derive(Clone, Copy)]
struct Region {
    origin: u64,
    size: u64,
}

impl Region {
    fn contains(&self, start: u64, end: u64) -> bool {
        self.origin <= start && start <= end && end <= self.origin + self.size
    }
}

/// The regions of a memory map holding the code, the data and the stack,
/// and the stack's size: `memory_block` writes it as a `memory` block.
struct Map {
    code: Region,
    data: Region,
    stack: Region,
    stack_size: u64,
}

impl Map {
    /// The block: a region `CODE`, `DATA` unless data shares the code's
    /// region, `STACK` unless the stack shares the data's, and `spare`,
    /// holding nothing, apart from them.
    fn memory_block(&self, spare: Region) -> String {
        let mut regions = vec![("CODE", self.code)];
        let data = if self.data.origin == self.code.origin {
            "CODE"
        } else {
            regions.push(("DATA", self.data));
            "DATA"
        };
        let stack = if self.stack.origin == self.data.origin {
            data
        } else {
            regions.push(("STACK", self.stack));
            "STACK"
        };
        regions.push(("SPARE", spare));
        let regions: String = regions
            .iter()
            .map(|(name, r)| {
                format!(
                    "    region {name} {{ origin: {:#x}, size: {:#x} }}\n",
                    r.origin, r.size
                )
            })
            .collect();
        format!(
            "memory {{\n{regions}    code in CODE;\n    data in {data};\n    stack in {stack} {{ size: {:#x} }}\n}}\n",
            self.stack_size
        )
    }
}

/// Builds `PROGRAM` with the memory map `map` at both levels, and checks
/// where the linker script of ADR 0057 puts things: `_start` at the code
/// region's origin; the stack at the start of its region, rounded up to 16,
/// with its size; `.data` above the stack; every loaded section, and the
/// load image of `.data`, in its region; and the guarantees of ADR 0030,
/// without the disjointness of the load image when data shares the code's
/// region, where it is the run range.
fn check_map(name: &str, map: &Map) {
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("elf");
    fs::create_dir_all(&work).unwrap();
    let source = work.join(format!("{name}.ers"));
    let spare = Region {
        origin: 0x8800_0000,
        size: 0x1000,
    };
    fs::write(&source, format!("{}\n{PROGRAM}", map.memory_block(spare))).unwrap();
    let shared = map.code.origin == map.data.origin;
    for opt in [OptLevel::None, OptLevel::Speed] {
        let elf = work.join(format!("{name}-{opt:?}.elf"));
        driver::build(&source, &elf, &[], opt, false).unwrap();
        check(&elf, opt, shared);

        let sections = sections(&elf);
        let symbols = symbols(&elf);
        let value = |name: &str| {
            symbols
                .iter()
                .find(|s| s.name == name)
                .unwrap_or_else(|| panic!("{opt:?}: no symbol {name}"))
                .value
        };
        let section = |name: &str| {
            sections
                .iter()
                .find(|s| s.name == name)
                .unwrap_or_else(|| panic!("{opt:?}: no section {name}"))
        };

        assert_eq!(value("_start"), map.code.origin, "{opt:?}: `_start`");
        let stack_start = map.stack.origin.next_multiple_of(16);
        assert_eq!(
            value("__stack_start"),
            stack_start,
            "{opt:?}: `__stack_start`"
        );
        assert_eq!(
            value("__stack_end"),
            stack_start + map.stack_size,
            "{opt:?}: `__stack_end`"
        );
        if map.data.origin == map.stack.origin {
            assert!(
                section(".data").address >= value("__stack_end"),
                "{opt:?}: `.data` is below the top of the stack"
            );
        }

        let data_load = value("__data_load");
        let data_size = value("__data_end") - value("__data_start");
        assert!(
            map.code.contains(data_load, data_load + data_size),
            "{opt:?}: the load image of `.data` is outside the code's region"
        );
        for (name, region) in [
            (".text.start", map.code),
            (".text", map.code),
            (".rodata", map.code),
            (".stack", map.stack),
            (".data", map.data),
            (".bss", map.data),
        ] {
            let s = section(name);
            assert!(
                region.contains(s.address, s.address + s.size),
                "{opt:?}: {name} is outside its region"
            );
        }
    }
}

/// Code in one region; the stack, then `.data` and `.bss`, in another
/// (ADR 0057).
#[test]
fn memory_map_layout() {
    check_map(
        "map",
        &Map {
            code: Region {
                origin: 0x8000_0000,
                size: 0x1_0000,
            },
            data: Region {
                origin: 0x8002_0000,
                size: 0x4000,
            },
            stack: Region {
                origin: 0x8002_0000,
                size: 0x4000,
            },
            stack_size: 0x800,
        },
    );
}

/// A stack region whose origin is not a multiple of 16: the stack starts
/// at the next one.
#[test]
fn memory_map_stack_rounded_up() {
    let ram = Region {
        origin: 0x8002_0004,
        size: 0x4000,
    };
    check_map(
        "map_rounded",
        &Map {
            code: Region {
                origin: 0x8000_0000,
                size: 0x1_0000,
            },
            data: ram,
            stack: ram,
            stack_size: 0x800,
        },
    );
}

/// Code and data sharing a region, the stack in one of its own: the load
/// image of `.data` is its run range.
#[test]
fn memory_map_shared() {
    let ram = Region {
        origin: 0x8000_0000,
        size: 0x1_0000,
    };
    check_map(
        "map_shared",
        &Map {
            code: ram,
            data: ram,
            stack: Region {
                origin: 0x8100_0000,
                size: 0x1000,
            },
            stack_size: 0x1000,
        },
    );
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("elf");
    for opt in [OptLevel::None, OptLevel::Speed] {
        let symbols = symbols(&work.join(format!("map_shared-{opt:?}.elf")));
        let value = |name: &str| symbols.iter().find(|s| s.name == name).unwrap().value;
        assert_eq!(
            value("__data_load"),
            value("__data_start"),
            "{opt:?}: the load image of `.data` is not its run range"
        );
    }
}

/// An object linked in whose code section is aligned to 64 does not move
/// `_start` off the origin of the code's region: the reset entry has an
/// output section of its own (ADR 0057).
#[test]
fn reset_entry_alone() {
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("elf");
    fs::create_dir_all(&work).unwrap();
    let asm = work.join("aligned.s");
    fs::write(
        &asm,
        "\t.text\n\t.p2align 6\n\t.globl aligned\naligned:\n\tret\n",
    )
    .unwrap();
    let object = work.join("aligned.o");
    let clang = common::clang_path();
    let output = Command::new(&clang)
        .args(common::CLANG_TARGET)
        .arg("-c")
        .arg(&asm)
        .arg("-o")
        .arg(&object)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "cannot run `{}` ({e}); install clang or set EREBOS_CLANG",
                clang.display()
            )
        });
    assert!(
        output.status.success(),
        "clang failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let source = work.join("reset_entry.ers");
    let block = "memory {
    region FLASH { origin: 0x8000_0004, size: 0x1_0000 }
    region RAM { origin: 0x8002_0000, size: 0x4000 }
    code in FLASH;
    data in RAM;
    stack in RAM { size: 0x800 }
}
";
    fs::write(&source, format!("{block}\n{PROGRAM}")).unwrap();
    for opt in [OptLevel::None, OptLevel::Speed] {
        let elf = work.join(format!("reset_entry-{opt:?}.elf"));
        driver::build(&source, &elf, std::slice::from_ref(&object), opt, false).unwrap();
        let symbols = symbols(&elf);
        let value = |name: &str| symbols.iter().find(|s| s.name == name).unwrap().value;
        assert_eq!(
            value("aligned") % 64,
            0,
            "{opt:?}: the object's code is not aligned to 64, so the test proves nothing"
        );
        assert_eq!(value("_start"), 0x8000_0004, "{opt:?}: `_start` moved");
    }
}

/// `link::region-overflow` (ADR 0057): building `name`, `PROGRAM` with the
/// memory block `block`, fails, and the rendered diagnostics are returned.
/// No executable is written.
fn overflow(name: &str, block: &str) -> String {
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("elf");
    fs::create_dir_all(&work).unwrap();
    let source = work.join(format!("{name}.ers"));
    fs::write(&source, format!("{block}\n{PROGRAM}")).unwrap();
    let elf = work.join(format!("{name}.elf"));
    let _ = fs::remove_file(&elf);
    let error = driver::build(&source, &elf, &[], OptLevel::None, false).unwrap_err();
    assert!(!elf.exists(), "{name}: an executable was written");
    error
}

/// Code and `.rodata` past the end of their region: `ld.lld` reports each
/// section that ends past it, the compiler one diagnostic naming the first,
/// with the largest overflow, that of the load image of `.data`, which
/// comes last.
#[test]
fn region_overflow_code() {
    let error = overflow(
        "overflow_code",
        "memory {
    region FLASH { origin: 0x8000_0000, size: 0x100 }
    region RAM { origin: 0x8002_0000, size: 0x4000 }
    code in FLASH;
    data in RAM;
    stack in RAM { size: 0x800 }
}
",
    );
    assert_eq!(error.matches("link::region-overflow").count(), 1, "{error}");

    // The overflow is how far the load image of `.data` ends past the
    // region, as in the same program with a region large enough.
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("elf");
    let source = work.join("overflow_code_fits.ers");
    let block = "memory {
    region FLASH { origin: 0x8000_0000, size: 0x1_0000 }
    region RAM { origin: 0x8002_0000, size: 0x4000 }
    code in FLASH;
    data in RAM;
    stack in RAM { size: 0x800 }
}
";
    fs::write(&source, format!("{block}\n{PROGRAM}")).unwrap();
    let elf = work.join("overflow_code_fits.elf");
    driver::build(&source, &elf, &[], OptLevel::None, false).unwrap();
    let symbols = symbols(&elf);
    let value = |name: &str| symbols.iter().find(|s| s.name == name).unwrap().value;
    let end = value("__data_load") + value("__data_end") - value("__data_start");
    let lacks = end - (0x8000_0000 + 0x100);
    assert!(
        error.contains(&format!("{lacks} bytes too small")),
        "{error}"
    );

    insta::assert_snapshot!(error);
}

/// `.bss` past the end of the region it shares with the stack.
#[test]
fn region_overflow_bss() {
    let error = overflow(
        "overflow_bss",
        "memory {
    region FLASH { origin: 0x8000_0000, size: 0x1_0000 }
    region RAM { origin: 0x8002_0000, size: 0x808 }
    code in FLASH;
    data in RAM;
    stack in RAM { size: 0x800 }
}
",
    );
    insta::assert_snapshot!(error);
}

/// Functions and statics of modules, named by their paths joined by dots,
/// and an `#[export]` function, named by its own name (ADR 0051).
const MODULES: &str = "\
mod m {
    pub fn f() -> u32 { n::S }

    pub mod n {
        pub static S: u32 = 7;
    }

    #[export]
    pub fn exported() -> u32 { 1 }
}

entry fn main() -> ! {
    let x: u32 = m::f() + m::exported();
    loop {}
}
panic fn on_panic() -> ! { loop {} }
";

#[test]
fn module_symbols() {
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("elf");
    fs::create_dir_all(&work).unwrap();
    let source = work.join("modules.ers");
    fs::write(&source, format!("{VIRT_MAP}\n{MODULES}")).unwrap();
    for opt in [OptLevel::None, OptLevel::Speed] {
        let elf = work.join(format!("modules-{opt:?}.elf"));
        driver::build(&source, &elf, &[], opt, false).unwrap();
        let symbols = symbols(&elf);
        let defined = |name: &str| {
            symbols
                .iter()
                .any(|s| s.name == name && s.section != "UND" && s.section != "ABS")
        };
        for name in ["m.f", "m.n.S", "exported", "main", "on_panic"] {
            assert!(defined(name), "{opt:?}: no symbol {name}");
        }
        assert!(
            !symbols.iter().any(|s| s.name == "m.exported"),
            "{opt:?}: the `#[export]` function has its path as symbol"
        );
    }
}

/// The functions of `core` (ADR 0052): the runtime support functions keep
/// the names LLVM and C call them by, being `#[export]`, and a private
/// helper has its path as symbol (ADR 0051).
#[test]
fn core_symbols() {
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("elf");
    fs::create_dir_all(&work).unwrap();
    let source = work.join("core.ers");
    fs::write(&source, format!("{VIRT_MAP}\n{PROGRAM}")).unwrap();
    for opt in [OptLevel::None, OptLevel::Speed] {
        let elf = work.join(format!("core-{opt:?}.elf"));
        driver::build(&source, &elf, &[], opt, false).unwrap();
        let symbols = symbols(&elf);
        let defined = |name: &str| {
            symbols
                .iter()
                .any(|s| s.name == name && s.section != "UND" && s.section != "ABS")
        };
        for name in [
            "memcpy",
            "memmove",
            "memset",
            "__udivdi3",
            "__moddi3",
            "core.builtins.__erebos_udivmod64",
        ] {
            assert!(defined(name), "{opt:?}: no symbol {name}");
        }
        for name in ["core.builtins.memcpy", "core.builtins.__udivdi3"] {
            assert!(
                !symbols.iter().any(|s| s.name == name),
                "{opt:?}: the `#[export]` function {name} has its path as symbol"
            );
        }
    }
}

/// The guarantees of ADR 0030 on the startup ranges; with `shared`, data
/// shares the code's region and the load image of `.data` is its run range
/// (ADR 0057), so it is not disjoint from it.
fn check(elf: &Path, opt: OptLevel, shared: bool) {
    let sections = sections(elf);
    let symbols = symbols(elf);
    let section = |name: &str| {
        sections
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("{opt:?}: no section {name}"))
    };
    let symbol = |name: &str| {
        symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("{opt:?}: no symbol {name}"))
    };
    let value = |name: &str| symbol(name).value;

    // The case the padding guards against really occurs.
    let rodata = section(".rodata");
    assert_ne!(
        (rodata.address + rodata.size) % 4,
        0,
        "{opt:?}: `.rodata` ends on a multiple of 4, so the test proves nothing"
    );

    for name in [
        "__data_start",
        "__data_end",
        "__data_load",
        "__bss_start",
        "__bss_end",
    ] {
        assert_eq!(value(name) % 4, 0, "{opt:?}: {name} is not a multiple of 4");
    }

    let data = section(".data");
    let bss = section(".bss");
    let (data_start, data_end) = (value("__data_start"), value("__data_end"));
    let (bss_start, bss_end) = (value("__bss_start"), value("__bss_end"));
    let data_load = value("__data_load");
    assert!(
        data_start <= data.address && data.address + data.size <= data_end,
        "{opt:?}: the `.data` range does not cover its section"
    );
    assert!(
        bss_start <= bss.address && bss.address + bss.size <= bss_end,
        "{opt:?}: the `.bss` range does not cover its section"
    );

    // QEMU loads `.data` at the physical address of its segment, where
    // `_start` copies it from.
    let segment = load_segments(elf)
        .into_iter()
        .find(|(virtual_address, _)| *virtual_address == data.address)
        .unwrap_or_else(|| panic!("{opt:?}: no segment holds `.data`"));
    assert_eq!(
        data_load, segment.1,
        "{opt:?}: `__data_load` is not where `.data` is loaded"
    );

    let ranges = [
        ("data", data_start, data_end),
        ("bss", bss_start, bss_end),
        ("load", data_load, data_load + (data_end - data_start)),
    ];
    for (i, a) in ranges.iter().enumerate() {
        for b in &ranges[i + 1..] {
            if shared && (a.0, b.0) == ("data", "load") {
                continue;
            }
            assert!(
                a.2 <= b.1 || b.2 <= a.1,
                "{opt:?}: the {} and {} ranges overlap",
                a.0,
                b.0
            );
        }
    }

    // Where each kind of static goes.
    let in_section = |symbol_name: &str, names: &[&str]| {
        let index: usize = symbol(symbol_name).section.parse().unwrap();
        let section = &sections[index].name;
        assert!(
            names.contains(&section.as_str()),
            "{opt:?}: `{symbol_name}` is in {section}, not in {names:?}"
        );
    };
    in_section("ODD", &[".rodata", ".srodata"]);
    in_section("BYTES", &[".data", ".sdata"]);
    in_section("ZEROS", &[".bss", ".sbss"]);
}

fn readelf(elf: &Path, args: &[&str]) -> String {
    let readelf = common::readelf_path();
    let output = Command::new(&readelf)
        .args(args)
        .arg("--wide")
        .arg(elf)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "cannot run `{}` ({e}); install LLVM or set EREBOS_READELF",
                readelf.display()
            )
        });
    assert!(output.status.success(), "llvm-readelf failed");
    String::from_utf8(output.stdout).unwrap()
}

fn hex(text: &str) -> u64 {
    u64::from_str_radix(text.trim_start_matches("0x"), 16).unwrap()
}

/// `[Nr] Name Type Address Off Size ...`, indexed by number; the null
/// section has no name.
fn sections(elf: &Path) -> Vec<Section> {
    readelf(elf, &["--section-headers"])
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix('['))
        .filter_map(|line| line.split_once(']'))
        .filter(|(index, _)| index.trim().parse::<usize>().is_ok())
        .map(
            |(_, rest)| match rest.split_whitespace().collect::<Vec<_>>()[..] {
                [name, _type, address, _offset, size, ..] if name != "NULL" => Section {
                    name: name.to_owned(),
                    address: hex(address),
                    size: hex(size),
                },
                _ => Section {
                    name: String::new(),
                    address: 0,
                    size: 0,
                },
            },
        )
        .collect()
}

/// `Num: Value Size Type Bind Vis Ndx Name`.
fn symbols(elf: &Path) -> Vec<Symbol> {
    readelf(elf, &["--symbols"])
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            match fields.as_slice() {
                [num, value, _size, _type, _bind, _vis, section, name]
                    if num.ends_with(':') && *num != "Num:" =>
                {
                    Some(Symbol {
                        name: (*name).to_owned(),
                        value: hex(value),
                        section: (*section).to_owned(),
                    })
                }
                _ => None,
            }
        })
        .collect()
}

/// The virtual and physical address of each `LOAD` segment.
fn load_segments(elf: &Path) -> Vec<(u64, u64)> {
    readelf(elf, &["--program-headers"])
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            match fields.as_slice() {
                ["LOAD", _offset, virtual_address, physical_address, ..] => {
                    Some((hex(virtual_address), hex(physical_address)))
                }
                _ => None,
            }
        })
        .collect()
}
