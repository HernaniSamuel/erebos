//! Inspects built executables with `llvm-readelf` (ADR 0030): where statics
//! go, and the symbols `_start` uses to fill `.bss` with zeros and copy the
//! initial values of `.data`.

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
    fs::write(&source, PROGRAM).unwrap();
    for opt in [OptLevel::None, OptLevel::Speed] {
        let elf = work.join(format!("startup-{opt:?}.elf"));
        driver::build(&source, &elf, &[], opt, false).unwrap();
        check(&elf, opt);
    }
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
    fs::write(&source, MODULES).unwrap();
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
    fs::write(&source, PROGRAM).unwrap();
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

fn check(elf: &Path, opt: OptLevel) {
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
