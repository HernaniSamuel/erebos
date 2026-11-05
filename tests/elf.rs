//! Inspects built executables with `llvm-readelf` (ADR 0030): where statics
//! go, and the symbols `_start` uses to fill `.bss` with zeros and copy the
//! initial values of `.data`; where a memory map puts the code, the data
//! and the stack, and what fails to build when they do not fit (ADR 0057).
//! The checks on the layout run for every target (ADR 0073): the stack's
//! alignment is the target's, and where `_start` is depends on its reset
//! entry (ADR 0074).

mod common;

use std::fs;
use std::path::Path;
use std::process::Command;

use erebos::driver::{self, OptLevel};
use erebos::target::{Arch, TARGETS, Target};

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

/// The memory block of the programs that do not test memory maps, as the
/// riscv32 board of the e2e programs holds it (ADR 0077): QEMU `virt`'s RAM
/// in two regions of 64 MiB, the image in the first, and in the second a
/// 1 MiB stack at its start, then `.data` and `.bss`.
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
    /// The size of what it names, in bytes.
    size: u64,
    /// `FUNC`, `OBJECT`, `NOTYPE`...
    kind: String,
    /// The section index, or `ABS`, `UND`.
    section: String,
}

#[test]
fn startup_ranges() {
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("elf");
    fs::create_dir_all(&work).unwrap();
    let source = work.join("startup.ers");
    fs::write(&source, format!("{VIRT_MAP}\n{PROGRAM}")).unwrap();
    for target in TARGETS {
        for opt in [OptLevel::None, OptLevel::Speed] {
            let elf = work.join(format!("startup-{}-{opt:?}.elf", target.name));
            driver::build(&source, &elf, &[], target, opt, false).unwrap();
            check(&elf, opt, false);
        }
    }
}

/// The address of `_start` in a program whose code region starts at
/// `origin`: the reset entry (ADR 0057) starts the region on riscv32; on
/// ARMv7-M the 16 words of the vector table, the section `.vector_table`,
/// come first (ADR 0074, 0083, 0085), and the symbol of a Thumb function has
/// bit 0 set.
fn start_address(target: &Target, origin: u64) -> u64 {
    match target.arch {
        Arch::Riscv32 => origin,
        Arch::Armv7m => origin + 64 + 1,
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

/// Builds `PROGRAM` with the memory map `map` at both levels for every
/// target, and checks where the linker script of ADR 0057 puts things:
/// `_start` at the code region's origin, or after the vector table words
/// the target's reset entry starts with; the stack at the start of its
/// region, rounded up to the target's stack alignment, with its size;
/// `.data` above the stack; every loaded section, and the load image of
/// `.data`, in its region; and the guarantees of ADR 0030, without the
/// disjointness of the load image when data shares the code's region, where
/// it is the run range.
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
    for target in TARGETS {
        for opt in [OptLevel::None, OptLevel::Speed] {
            let elf = work.join(format!("{name}-{}-{opt:?}.elf", target.name));
            driver::build(&source, &elf, &[], target, opt, false).unwrap();
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

            assert_eq!(
                value("_start"),
                start_address(target, map.code.origin),
                "{} {opt:?}: `_start`",
                target.name
            );
            let stack_start = map.stack.origin.next_multiple_of(target.stack_align);
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
    for target in TARGETS {
        for opt in [OptLevel::None, OptLevel::Speed] {
            let symbols = symbols(&work.join(format!("map_shared-{}-{opt:?}.elf", target.name)));
            let value = |name: &str| symbols.iter().find(|s| s.name == name).unwrap().value;
            assert_eq!(
                value("__data_load"),
                value("__data_start"),
                "{} {opt:?}: the load image of `.data` is not its run range",
                target.name
            );
        }
    }
}

/// An object linked in whose code section is aligned to 64 does not move
/// `_start` off the reset entry's place in the code's region: the reset
/// entry has an output section of its own (ADR 0057). The origin is the
/// target's code alignment past 0x8000_0000, so that it is as unaligned as
/// the target lets it be.
#[test]
fn reset_entry_alone() {
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("elf");
    fs::create_dir_all(&work).unwrap();
    for target in TARGETS {
        reset_entry_alone_on(target, &work);
    }
}

fn reset_entry_alone_on(target: &'static Target, work: &Path) {
    let (asm_text, asm_name) = match target.arch {
        Arch::Riscv32 => (
            "\t.text\n\t.p2align 6\n\t.globl aligned\naligned:\n\tret\n",
            "aligned-riscv32.s",
        ),
        Arch::Armv7m => (
            "\t.syntax unified\n\t.thumb\n\t.text\n\t.p2align 6\n\t.globl aligned\n\t.type aligned, %function\n\t.thumb_func\naligned:\n\tbx lr\n",
            "aligned-armv7m.s",
        ),
    };
    let asm = work.join(asm_name);
    fs::write(&asm, asm_text).unwrap();
    let object = work.join(format!("aligned-{}.o", target.name));
    let clang = common::clang_path();
    let output = Command::new(&clang)
        .args(common::clang_target(target))
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

    let origin = 0x8000_0000 + target.code_align;
    let source = work.join(format!("reset_entry-{}.ers", target.name));
    let block = format!(
        "memory {{
    region FLASH {{ origin: {origin:#x}, size: 0x1_0000 }}
    region RAM {{ origin: 0x8002_0000, size: 0x4000 }}
    code in FLASH;
    data in RAM;
    stack in RAM {{ size: 0x800 }}
}}
"
    );
    fs::write(&source, format!("{block}\n{PROGRAM}")).unwrap();
    for opt in [OptLevel::None, OptLevel::Speed] {
        let elf = work.join(format!("reset_entry-{}-{opt:?}.elf", target.name));
        driver::build(
            &source,
            &elf,
            std::slice::from_ref(&object),
            target,
            opt,
            false,
        )
        .unwrap();
        let symbols = symbols(&elf);
        let value = |name: &str| symbols.iter().find(|s| s.name == name).unwrap().value;
        // A Thumb function's symbol has bit 0 set.
        assert_eq!(
            value("aligned") & !1 & 63,
            0,
            "{} {opt:?}: the object's code is not aligned to 64, so the test proves nothing",
            target.name
        );
        assert_eq!(
            value("_start"),
            start_address(target, origin),
            "{} {opt:?}: `_start` moved",
            target.name
        );
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
    let error = driver::build(
        &source,
        &elf,
        &[],
        erebos::target::DEFAULT,
        OptLevel::None,
        false,
    )
    .unwrap_err();
    assert!(!elf.exists(), "{name}: an executable was written");
    error
}

/// Where the load image of `.data` starts and ends when `PROGRAM` is
/// built with its code in a region large enough at 0x8000_0000, which tells
/// how far a smaller region overflows; `name` names the files.
fn data_image(name: &str) -> (u64, u64) {
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("elf");
    fs::create_dir_all(&work).unwrap();
    let source = work.join(format!("{name}.ers"));
    let block = "memory {
    region FLASH { origin: 0x8000_0000, size: 0x1_0000 }
    region RAM { origin: 0x8002_0000, size: 0x4000 }
    code in FLASH;
    data in RAM;
    stack in RAM { size: 0x800 }
}
";
    fs::write(&source, format!("{block}\n{PROGRAM}")).unwrap();
    let elf = work.join(format!("{name}.elf"));
    driver::build(
        &source,
        &elf,
        &[],
        erebos::target::DEFAULT,
        OptLevel::None,
        false,
    )
    .unwrap();
    let symbols = symbols(&elf);
    let value = |name: &str| symbols.iter().find(|s| s.name == name).unwrap().value;
    let start = value("__data_load");
    (start, start + value("__data_end") - value("__data_start"))
}

/// Code and `.rodata` past the end of their region: `ld.lld` reports each
/// section that ends past it, the compiler one diagnostic naming the first,
/// with the largest overflow, that of the load image of `.data`, which
/// comes last. That count changes with the size of the code, so the
/// snapshot has it redacted.
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

    let (_, end) = data_image("overflow_code_fits");
    let lacks = format!("{} bytes too small", end - (0x8000_0000 + 0x100));
    assert!(error.contains(&lacks), "{error}");
    insta::assert_snapshot!(error.replace(&lacks, "[N] bytes too small"));
}

/// Two regions overflowing at once, one diagnostic each, in the order of
/// the regions: `FLASH` ends where the read-only data does, so only the
/// load image of `.data` is past it, by the size of `.data`; `RAM` lacks
/// room for `.bss`. `FLASH`'s size depends on the size of the code, so it
/// is a constant declared away from the lines the snapshot shows.
#[test]
fn region_overflow_data_image() {
    let (start, end) = data_image("overflow_data_image_fits");
    let block = "memory {
    region FLASH { origin: 0x8000_0000, size: FLASH_SIZE }
    region RAM { origin: 0x8002_0000, size: 0x808 }
    code in FLASH;
    data in RAM;
    stack in RAM { size: 0x800 }
}
";
    let error = overflow(
        "overflow_data_image",
        &format!(
            "{block}{}\nconst FLASH_SIZE: usize = {:#x};\n",
            "\n".repeat(4),
            start - 0x8000_0000
        ),
    );
    assert_eq!(error.matches("link::region-overflow").count(), 2, "{error}");
    let lacks = end - start;
    assert!(
        error.contains(&format!("{lacks} bytes too small")),
        "{error}"
    );
    insta::assert_snapshot!(error);
}

/// A region overflowing into the next one, which holds the stack: the
/// sections overlap there, and `ld.lld`'s reports of it follow from the
/// overflow, so only the overflow is reported.
#[test]
fn region_overflow_into_next() {
    let error = overflow(
        "overflow_into_next",
        "memory {
    region FLASH { origin: 0x8000_0000, size: 0x100 }
    region RAM { origin: 0x8000_0100, size: 0x4000 }
    code in FLASH;
    data in RAM;
    stack in RAM { size: 0x800 }
}
",
    );
    assert_eq!(error.matches("link::region-overflow").count(), 1, "{error}");
    assert!(!error.contains("linking failed"), "{error}");
    assert!(!error.contains("overlaps"), "{error}");
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

/// The linker's errors other than a region overflow are reported as they
/// were before ADR 0057: an `extern fn` that is called and that nothing
/// defines fails to link with `error: linking failed:` and `ld.lld`'s line,
/// and no executable is written.
#[test]
fn link_failure_other() {
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("elf");
    fs::create_dir_all(&work).unwrap();
    let source = work.join("undefined_extern.ers");
    let program = "\
extern fn missing(x: u32) -> u32;

entry fn main() -> ! {
    let x: u32 = missing(1);
    loop {}
}
panic fn on_panic() -> ! { loop {} }
";
    fs::write(&source, format!("{VIRT_MAP}\n{program}")).unwrap();
    let elf = work.join("undefined_extern.elf");
    let _ = fs::remove_file(&elf);
    let error = driver::build(
        &source,
        &elf,
        &[],
        erebos::target::DEFAULT,
        OptLevel::None,
        false,
    )
    .unwrap_err();
    assert!(error.starts_with("error: linking failed:\n"), "{error}");
    assert!(error.contains("undefined symbol: missing"), "{error}");
    assert!(!error.contains("link::region-overflow"), "{error}");
    assert!(!elf.exists(), "an executable was written");
}

/// A program without a `memory` block is not built: `link::no-memory-map`
/// (ADR 0057), and neither the executable nor the object and the linker
/// script beside it are written.
#[test]
fn no_memory_map() {
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("elf");
    fs::create_dir_all(&work).unwrap();
    let source = work.join("no_memory_map.ers");
    fs::write(&source, PROGRAM).unwrap();
    let elf = work.join("no_memory_map.elf");
    let written = [
        elf.clone(),
        elf.with_extension("o"),
        elf.with_extension("ld"),
    ];
    for path in &written {
        let _ = fs::remove_file(path);
    }
    let error = driver::build(
        &source,
        &elf,
        &[],
        erebos::target::DEFAULT,
        OptLevel::None,
        false,
    )
    .unwrap_err();
    assert!(error.starts_with("link::no-memory-map\n"), "{error}");
    assert_eq!(error.matches("link::").count(), 1, "{error}");
    for path in &written {
        assert!(!path.exists(), "`{}` was written", path.display());
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
    fs::write(&source, format!("{VIRT_MAP}\n{MODULES}")).unwrap();
    for target in TARGETS {
        for opt in [OptLevel::None, OptLevel::Speed] {
            let elf = work.join(format!("modules-{}-{opt:?}.elf", target.name));
            driver::build(&source, &elf, &[], target, opt, false).unwrap();
            let symbols = symbols(&elf);
            let defined = |name: &str| {
                symbols
                    .iter()
                    .any(|s| s.name == name && s.section != "UND" && s.section != "ABS")
            };
            for name in ["m.f", "m.n.S", "exported", "main", "on_panic"] {
                assert!(defined(name), "{} {opt:?}: no symbol {name}", target.name);
            }
            assert!(
                !symbols.iter().any(|s| s.name == "m.exported"),
                "{} {opt:?}: the `#[export]` function has its path as symbol",
                target.name
            );
        }
    }
}

/// An interrupt handler is part of the program whether or not anything names
/// it (ADR 0084): each is a function in the executable, named as any
/// function is (ADR 0051), also in a module and with `#[export]`, on each
/// target at both levels. The `Vector` differs by target, so does the
/// program.
#[test]
fn handlers_are_kept() {
    const ENDING: &str = "
entry fn main() -> ! { loop {} }
panic fn on_panic() -> ! { loop {} }
";
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("elf");
    fs::create_dir_all(&work).unwrap();
    for target in TARGETS {
        let (handlers, names) = match target.arch {
            Arch::Riscv32 => (
                "
use core::arch::Vector;
interrupt fn timer() at Vector::MachineTimer {}
interrupt fn crash() -> ! at Vector::IllegalInstruction { loop {} }
mod drivers {
    pub interrupt fn software() at core::arch::Vector::MachineSoftware {}
    #[export]
    pub interrupt fn external() at core::arch::Vector::MachineExternal {}
}
",
                ["timer", "crash", "drivers.software", "external"],
            ),
            Arch::Armv7m => (
                "
use core::arch::Vector;
interrupt fn tick() at Vector::SysTick {}
interrupt fn fault() -> ! at Vector::HardFault { loop {} }
mod drivers {
    pub interrupt fn pend() at core::arch::Vector::PendSV {}
    #[export]
    pub interrupt fn svc() at core::arch::Vector::SVCall {}
}
",
                ["tick", "fault", "drivers.pend", "svc"],
            ),
        };
        let source = work.join(format!("handlers-{}.ers", target.name));
        fs::write(&source, format!("{VIRT_MAP}{handlers}{ENDING}")).unwrap();
        for opt in [OptLevel::None, OptLevel::Speed] {
            let elf = work.join(format!("handlers-{}-{opt:?}.elf", target.name));
            driver::build(&source, &elf, &[], target, opt, false).unwrap();
            let symbols = symbols(&elf);
            for name in names {
                assert!(
                    symbols
                        .iter()
                        .any(|s| s.name == name && s.kind == "FUNC" && s.size > 0),
                    "{} {opt:?}: no function {name}",
                    target.name
                );
            }
            // `#[export]` names the handler by itself, not by its path.
            for path in ["drivers.external", "drivers.svc"] {
                assert!(
                    !symbols.iter().any(|s| s.name == path),
                    "{} {opt:?}: the `#[export]` handler has its path as symbol",
                    target.name
                );
            }
        }
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
    for target in TARGETS {
        for opt in [OptLevel::None, OptLevel::Speed] {
            let elf = work.join(format!("core-{}-{opt:?}.elf", target.name));
            driver::build(&source, &elf, &[], target, opt, false).unwrap();
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
                "__aeabi_memcpy",
                "__aeabi_memclr8",
                "__udivdi3",
                "__moddi3",
                "core.builtins.__erebos_udivmod64",
            ] {
                assert!(defined(name), "{} {opt:?}: no symbol {name}", target.name);
            }
            for name in ["core.builtins.memcpy", "core.builtins.__udivdi3"] {
                assert!(
                    !symbols.iter().any(|s| s.name == name),
                    "{} {opt:?}: the `#[export]` function {name} has its path as symbol",
                    target.name
                );
            }
        }
    }
}

/// `#[align(N)]` on a function moves its symbol (ADR 0087): an `#[export]`
/// function and a naked function with `#[align(64)]` are at multiples of 64
/// (a Thumb function's symbol has bit 0 set, which is not part of the
/// address), on both targets at both levels.
#[test]
fn function_alignment() {
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("elf");
    fs::create_dir_all(&work).unwrap();
    for target in TARGETS {
        let ret = match target.arch {
            Arch::Riscv32 => "ret",
            Arch::Armv7m => "bx lr",
        };
        let source = work.join(format!("align-{}.ers", target.name));
        let program = format!(
            "{VIRT_MAP}
#[export]
#[align(64)]
fn aligned(x: u32) -> u32 {{ x + 1 }}

fn unaligned(x: u32) -> u32 {{ x + 2 }}

#[align(64)]
#[naked]
fn naked_aligned() {{
    asm(\"{ret}\", options(noreturn))
}}

entry fn main() -> ! {{
    aligned(1);
    unaligned(1);
    naked_aligned();
    loop {{}}
}}

panic fn on_panic() -> ! {{ loop {{}} }}
"
        );
        fs::write(&source, program).unwrap();
        for opt in [OptLevel::None, OptLevel::Speed] {
            let elf = work.join(format!("align-{}-{opt:?}.elf", target.name));
            driver::build(&source, &elf, &[], target, opt, false).unwrap();
            let symbols = symbols(&elf);
            for name in ["aligned", "naked_aligned"] {
                let symbol = symbols
                    .iter()
                    .find(|s| s.name == name && s.section != "UND")
                    .unwrap_or_else(|| panic!("{} {opt:?}: no symbol {name}", target.name));
                let address = symbol.value & !1;
                assert_eq!(
                    address % 64,
                    0,
                    "{} {opt:?}: `{name}` is at {address:#x}",
                    target.name
                );
            }
        }
    }
}

/// The attribute moves the symbol where the template of a naked function
/// cannot, whose `.p2align` would put padding after it (ADR 0087). On
/// riscv32, `pad` holds one return, which the assembler compresses to two bytes, and `next` follows
/// it. In the control build `next` has no attribute and is at 2 mod 4, right
/// after `pad`, which also pins the order of the two; with `#[align(4)]` on
/// `next` it is at 0 mod 4.
#[test]
fn function_alignment_moves_the_symbol() {
    let target = erebos::target::by_name("riscv32imac-unknown-none-elf").unwrap();
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("elf");
    fs::create_dir_all(&work).unwrap();
    for (attribute, build) in [
        ("", "control"),
        (
            "#[align(4)]
",
            "aligned",
        ),
    ] {
        let source = work.join(format!("align-pad-{build}.ers"));
        let program = format!(
            "{VIRT_MAP}
#[align(4)]
#[naked]
fn pad() {{
    asm(\"ret\", options(noreturn))
}}

{attribute}#[naked]
fn next() {{
    asm(\"ret\", options(noreturn))
}}

entry fn main() -> ! {{
    pad();
    next();
    loop {{}}
}}

panic fn on_panic() -> ! {{ loop {{}} }}
"
        );
        fs::write(&source, program).unwrap();
        for opt in [OptLevel::None, OptLevel::Speed] {
            let elf = work.join(format!("align-pad-{build}-{opt:?}.elf"));
            driver::build(&source, &elf, &[], target, opt, false).unwrap();
            let symbols = symbols(&elf);
            let at = |name: &str| {
                symbols
                    .iter()
                    .find(|s| s.name == name && s.section != "UND")
                    .unwrap_or_else(|| panic!("{build} {opt:?}: no symbol {name}"))
                    .value
            };
            let (pad, next) = (at("pad"), at("next"));
            assert_eq!(pad % 4, 0, "{build} {opt:?}: `pad` is at {pad:#x}");
            if attribute.is_empty() {
                assert_eq!(next, pad + 2, "{build} {opt:?}: `next` is at {next:#x}");
                assert_eq!(next % 4, 2, "{build} {opt:?}: `next` is at {next:#x}");
            } else {
                assert_eq!(next % 4, 0, "{build} {opt:?}: `next` is at {next:#x}");
            }
        }
    }
}

/// The reset entry of thumbv7m (ADR 0074, 0083, 0085), in a program with
/// QEMU's `mps2-an385` map, whose code starts at 0: the section
/// `.vector_table` is at the origin, 64 bytes, and its first two words are the
/// initial stack pointer, `__stack_end`, and `_start` with bit 0 set; `_start`
/// follows the table, alone in `.text.start`, at 64; the unwind tables of the
/// functions are discarded, and the attributes section is there, not loaded.
#[test]
fn thumbv7m_reset_entry() {
    let target = erebos::target::by_name("thumbv7m-none-eabi").unwrap();
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("elf");
    fs::create_dir_all(&work).unwrap();
    let source = work.join("thumbv7m_reset_entry.ers");
    let map = "memory {
    region CODE { origin: 0x0000_0000, size: 0x40_0000 }
    region DATA { origin: 0x2000_0000, size: 0x40_0000 }
    code in CODE;
    data in DATA;
    stack in DATA { size: 0x1000 }
}
";
    fs::write(&source, format!("{map}\n{PROGRAM}")).unwrap();
    for opt in [OptLevel::None, OptLevel::Speed] {
        let elf = work.join(format!("thumbv7m_reset_entry-{opt:?}.elf"));
        driver::build(&source, &elf, &[], target, opt, false).unwrap();
        let sections = sections(&elf);
        let symbols = symbols(&elf);
        let value = |name: &str| symbols.iter().find(|s| s.name == name).unwrap().value;

        // `[Nr] Name Type Address Off Size ...`, after the `]`: the address
        // and the offset in the file of the section `name`.
        let headers = readelf(&elf, &["--section-headers"]);
        let place = |name: &str| {
            let line = headers
                .lines()
                .find(|line| line.contains(&format!(" {name} ")))
                .unwrap_or_else(|| panic!("{opt:?}: no `{name}`"));
            let fields: Vec<&str> = line.split_once(']').unwrap().1.split_whitespace().collect();
            (hex(fields[2]), hex(fields[3]) as usize, hex(fields[4]))
        };
        let (address, offset, size) = place(".vector_table");
        assert_eq!(address, 0, "{opt:?}: `.vector_table` is not at the origin");
        assert_eq!(size, 64, "{opt:?}: `.vector_table` is not 16 words");
        let (text_start, _, _) = place(".text.start");
        assert_eq!(text_start, 64, "{opt:?}: `.text.start` is not at 64");
        let image = fs::read(&elf).unwrap();
        let word = |at: usize| u64::from(u32::from_le_bytes(image[at..at + 4].try_into().unwrap()));
        assert_eq!(word(offset), value("__stack_end"), "{opt:?}: first word");
        assert_eq!(word(offset + 4), value("_start"), "{opt:?}: second word");
        assert_eq!(
            value("_start"),
            64 + 1,
            "{opt:?}: `_start` is not at 64, Thumb"
        );

        assert!(
            !sections
                .iter()
                .any(|s| s.name.starts_with(".ARM.exidx") || s.name.starts_with(".ARM.extab")),
            "{opt:?}: an unwind table was kept"
        );
        let attributes = sections
            .iter()
            .find(|s| s.name == ".ARM.attributes")
            .unwrap_or_else(|| panic!("{opt:?}: no `.ARM.attributes`"));
        assert_eq!(
            attributes.address, 0,
            "{opt:?}: `.ARM.attributes` is loaded"
        );
    }
}

/// The words of the vector table of thumbv7m (ADR 0085) in three programs, at
/// both levels: with handlers (one in a module, one `#[export]`), with none and
/// a panic function that takes the information, with none and a panic function
/// that does not. A handler's word is its symbol, bit 0 set as a Thumb
/// function's; a vector without one holds a default handler, a function of its
/// own with the information and one shared without it; the words that the
/// architecture reserves are 0; the first two are `__stack_end` and `_start`.
#[test]
fn thumbv7m_vector_table() {
    // The words that hold a vector, by the name of its variant (ADR 0084).
    const VECTORS: [(usize, &str); 9] = [
        (2, "NMI"),
        (3, "HardFault"),
        (4, "MemManage"),
        (5, "BusFault"),
        (6, "UsageFault"),
        (11, "SVCall"),
        (12, "DebugMonitor"),
        (14, "PendSV"),
        (15, "SysTick"),
    ];
    const RESERVED: [usize; 5] = [7, 8, 9, 10, 13];
    const WITH_INFO: &str = "panic fn on_panic(info: *const core::PanicInfo) -> ! { loop {} }\n";
    const WITHOUT_INFO: &str = "panic fn on_panic() -> ! { loop {} }\n";
    const HANDLERS: &str = "
use core::arch::Vector;
interrupt fn tick() at Vector::SysTick {}
interrupt fn fault() -> ! at Vector::HardFault { loop {} }
mod drivers {
    pub interrupt fn pend() at core::arch::Vector::PendSV {}
    #[export]
    pub interrupt fn svc() at core::arch::Vector::SVCall {}
}
entry fn main() -> ! { loop {} }
";
    const NONE: &str = "entry fn main() -> ! { loop {} }\n";
    // The map of QEMU's `mps2-an385`: the code at 0.
    const MAP: &str = "memory {
    region CODE { origin: 0x0000_0000, size: 0x40_0000 }
    region DATA { origin: 0x2000_0000, size: 0x40_0000 }
    code in CODE;
    data in DATA;
    stack in DATA { size: 0x1000 }
}
";

    let target = erebos::target::by_name("thumbv7m-none-eabi").unwrap();
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("elf");
    fs::create_dir_all(&work).unwrap();
    /// One program: its name and source, the symbol of each handler by the
    /// variant it serves, and the symbol of the default handler of a variant
    /// that has no handler.
    struct Program {
        name: &'static str,
        source: String,
        handlers: &'static [(&'static str, &'static str)],
        default: fn(&str) -> String,
    }
    let per_vector: fn(&str) -> String = |variant| format!("__erebos_unhandled_{variant}");
    let shared: fn(&str) -> String = |_| "__erebos_unhandled".to_owned();
    let programs = [
        Program {
            name: "handlers",
            source: format!("{HANDLERS}{WITH_INFO}"),
            handlers: &[
                ("SysTick", "tick"),
                ("HardFault", "fault"),
                ("PendSV", "drivers.pend"),
                ("SVCall", "svc"),
            ],
            default: per_vector,
        },
        Program {
            name: "none",
            source: format!("{NONE}{WITH_INFO}"),
            handlers: &[],
            default: per_vector,
        },
        Program {
            name: "none_without_info",
            source: format!("{NONE}{WITHOUT_INFO}"),
            handlers: &[],
            default: shared,
        },
    ];
    for Program {
        name,
        source: program,
        handlers,
        default,
    } in programs
    {
        let source = work.join(format!("thumbv7m_vector_table_{name}.ers"));
        fs::write(&source, format!("{MAP}{program}")).unwrap();
        for opt in [OptLevel::None, OptLevel::Speed] {
            let elf = work.join(format!("thumbv7m_vector_table_{name}-{opt:?}.elf"));
            driver::build(&source, &elf, &[], target, opt, false).unwrap();
            let symbols = symbols(&elf);
            let value = |symbol: &str| {
                let found = symbols.iter().find(|s| s.name == symbol);
                let found = found.unwrap_or_else(|| panic!("{name} {opt:?}: no `{symbol}`"));
                assert_eq!(
                    found.kind, "FUNC",
                    "{name} {opt:?}: `{symbol}` is no function"
                );
                assert!(found.size > 0, "{name} {opt:?}: `{symbol}` has no size");
                found.value
            };
            let table = section_bytes(&elf, ".vector_table");
            assert_eq!(table.len(), 64, "{name} {opt:?}: the table is not 16 words");
            let word = |n: usize| {
                let bytes: [u8; 4] = table[4 * n..4 * n + 4].try_into().unwrap();
                u64::from(u32::from_le_bytes(bytes))
            };
            let stack_end = symbols.iter().find(|s| s.name == "__stack_end").unwrap();
            assert_eq!(word(0), stack_end.value, "{name} {opt:?}: word 0");
            assert_eq!(word(1), value("_start"), "{name} {opt:?}: word 1");
            for (n, variant) in VECTORS {
                let symbol = match handlers.iter().find(|(v, _)| *v == variant) {
                    Some((_, symbol)) => (*symbol).to_owned(),
                    None => default(variant),
                };
                let expected = value(&symbol);
                assert_eq!(expected & 1, 1, "{name} {opt:?}: `{symbol}` is not Thumb");
                assert_eq!(word(n), expected, "{name} {opt:?}: word {n} ({variant})");
            }
            for n in RESERVED {
                assert_eq!(word(n), 0, "{name} {opt:?}: word {n} is reserved");
            }
        }
    }
}

/// The trap table and the trap entry of riscv32 (ADR 0085) in three programs, at
/// both levels: with handlers (one in a module, one `#[export]`, one named like a
/// register), with none and a panic function that takes the information, with
/// none and a panic function that does not. `__erebos_trap_table` is 33 words,
/// the exceptions, then the interrupts, then the word of any other cause, each
/// the address of the expected function: the handler, the default handler of a
/// vector that has no handler, or `__erebos_unhandled_other` for a cause without
/// a variant (the shared `__erebos_unhandled` without the information). The trap
/// entry, `core.arch.__erebos_trap`, is a function at a multiple of 4, which
/// `mtvec` needs, though LLVM would align it to 2.
#[test]
fn riscv32_trap_table() {
    // The words that hold a vector, by the name of its variant (ADR 0084): the
    // exception `n` is word `n`, and the interrupt `n` is word `16 + n`.
    const VECTORS: [(usize, &str); 12] = [
        (0, "InstructionMisaligned"),
        (1, "InstructionFault"),
        (2, "IllegalInstruction"),
        (3, "Breakpoint"),
        (4, "LoadMisaligned"),
        (5, "LoadFault"),
        (6, "StoreMisaligned"),
        (7, "StoreFault"),
        (11, "EnvironmentCall"),
        (19, "MachineSoftware"),
        (23, "MachineTimer"),
        (27, "MachineExternal"),
    ];
    const WITH_INFO: &str = "panic fn on_panic(info: *const core::PanicInfo) -> ! { loop {} }\n";
    const WITHOUT_INFO: &str = "panic fn on_panic() -> ! { loop {} }\n";
    const HANDLERS: &str = "
use core::arch::Vector;
interrupt fn tick() at Vector::MachineTimer {}
interrupt fn sp() -> ! at Vector::IllegalInstruction { loop {} }
mod drivers {
    pub interrupt fn soft() at core::arch::Vector::MachineSoftware {}
    #[export]
    pub interrupt fn call() -> ! at core::arch::Vector::EnvironmentCall { loop {} }
}
entry fn main() -> ! { loop {} }
";
    const NONE: &str = "entry fn main() -> ! { loop {} }\n";

    let target = erebos::target::by_name("riscv32imac-unknown-none-elf").unwrap();
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("elf");
    fs::create_dir_all(&work).unwrap();
    /// One program: its name and source, the symbol of each handler by the
    /// variant it serves, and the symbol of the default handler of a variant
    /// that has no handler, and of a cause without a variant.
    struct Program {
        name: &'static str,
        source: String,
        handlers: &'static [(&'static str, &'static str)],
        default: fn(&str) -> String,
        other: &'static str,
    }
    let per_vector: fn(&str) -> String = |variant| format!("__erebos_unhandled_{variant}");
    let shared: fn(&str) -> String = |_| "__erebos_unhandled".to_owned();
    let programs = [
        Program {
            name: "handlers",
            source: format!("{HANDLERS}{WITH_INFO}"),
            handlers: &[
                ("MachineTimer", "tick"),
                ("IllegalInstruction", "sp"),
                ("MachineSoftware", "drivers.soft"),
                ("EnvironmentCall", "call"),
            ],
            default: per_vector,
            other: "__erebos_unhandled_other",
        },
        Program {
            name: "none",
            source: format!("{NONE}{WITH_INFO}"),
            handlers: &[],
            default: per_vector,
            other: "__erebos_unhandled_other",
        },
        Program {
            name: "none_without_info",
            source: format!("{NONE}{WITHOUT_INFO}"),
            handlers: &[],
            default: shared,
            other: "__erebos_unhandled",
        },
    ];
    for Program {
        name,
        source: program,
        handlers,
        default,
        other,
    } in programs
    {
        let source = work.join(format!("riscv32_trap_table_{name}.ers"));
        fs::write(&source, format!("{VIRT_MAP}{program}")).unwrap();
        for opt in [OptLevel::None, OptLevel::Speed] {
            let elf = work.join(format!("riscv32_trap_table_{name}-{opt:?}.elf"));
            driver::build(&source, &elf, &[], target, opt, false).unwrap();
            let symbols = symbols(&elf);
            let function = |symbol: &str| {
                let found = symbols.iter().find(|s| s.name == symbol);
                let found = found.unwrap_or_else(|| panic!("{name} {opt:?}: no `{symbol}`"));
                assert_eq!(
                    found.kind, "FUNC",
                    "{name} {opt:?}: `{symbol}` is no function"
                );
                assert!(found.size > 0, "{name} {opt:?}: `{symbol}` has no size");
                found.value
            };

            // The table is an object of 33 words, inside `.rodata`.
            let table = symbols
                .iter()
                .find(|s| s.name == "__erebos_trap_table")
                .unwrap_or_else(|| panic!("{name} {opt:?}: no `__erebos_trap_table`"));
            assert_eq!(table.kind, "OBJECT", "{name} {opt:?}: the table's kind");
            assert_eq!(table.size, 132, "{name} {opt:?}: the table is not 33 words");
            assert_eq!(table.value % 4, 0, "{name} {opt:?}: the table is unaligned");
            let sections = sections(&elf);
            let rodata = sections
                .iter()
                .find(|s| s.name == ".rodata")
                .unwrap_or_else(|| panic!("{name} {opt:?}: no `.rodata`"));
            assert!(
                rodata.address <= table.value && table.value + 132 <= rodata.address + rodata.size,
                "{name} {opt:?}: the table is outside `.rodata`"
            );
            let bytes = section_bytes(&elf, ".rodata");
            let at = (table.value - rodata.address) as usize;
            let word = |n: usize| {
                let word: [u8; 4] = bytes[at + 4 * n..at + 4 * n + 4].try_into().unwrap();
                u64::from(u32::from_le_bytes(word))
            };
            for n in 0..33 {
                let (symbol, what) = match VECTORS.iter().find(|(word, _)| *word == n) {
                    Some((_, variant)) => match handlers.iter().find(|(v, _)| v == variant) {
                        Some((_, symbol)) => ((*symbol).to_owned(), (*variant).to_owned()),
                        None => (default(variant), (*variant).to_owned()),
                    },
                    None => (other.to_owned(), "a cause without a variant".to_owned()),
                };
                assert_eq!(
                    word(n),
                    function(&symbol),
                    "{name} {opt:?}: word {n} ({what}) is not `{symbol}`"
                );
            }

            let trap = function("core.arch.__erebos_trap");
            assert_eq!(
                trap % 4,
                0,
                "{name} {opt:?}: the trap entry is at {trap:#x}"
            );
        }
    }
}

/// The reach of `_start`'s jump on riscv32 (ADR 0083): in
/// `tests/e2e/startup_far_entry.ers`, the entry function is more than 1 MiB
/// past `_start`, where `j` would not reach. The e2e test boots the program;
/// this one checks the distance, so that it does not pass without
/// proving anything if the functions are emitted in another order.
#[test]
fn the_entry_function_is_far_from_start() {
    let target = erebos::target::by_name("riscv32imac-unknown-none-elf").unwrap();
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("elf");
    fs::create_dir_all(&work).unwrap();
    let program = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e/startup_far_entry.ers");
    let source = common::board::stage(&program, target, &work).unwrap();
    for opt in [OptLevel::None, OptLevel::Speed] {
        let elf = work.join(format!("startup_far_entry-{opt:?}.elf"));
        driver::build(&source, &elf, &[], target, opt, false).unwrap();
        let symbols = symbols(&elf);
        let value = |name: &str| symbols.iter().find(|s| s.name == name).unwrap().value;
        let distance = value("main").wrapping_sub(value("_start"));
        assert!(
            distance > 0x10_0000,
            "{opt:?}: the entry function is {distance:#x} bytes past `_start`, within the reach of `j`"
        );
    }
}

/// An `asm` expression is assembled into the program's code (ADR 0079): the
/// template reaches the assembler as rendered, so `.text` holds the
/// instruction it names, in the register the operand names. `csrr a0,
/// mhartid` is `csrrs a0, mhartid, zero`, and `mrs r0, ipsr` is two Thumb-2
/// halfwords. The programs that run on QEMU are in `tests/e2e`.
#[test]
fn an_asm_expression_is_assembled_into_the_code() {
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("elf");
    fs::create_dir_all(&work).unwrap();
    for (target, template, register, instruction) in [
        (
            "riscv32imac-unknown-none-elf",
            "csrr a0, mhartid",
            "a0",
            [0x73, 0x25, 0x40, 0xf1],
        ),
        (
            "thumbv7m-none-eabi",
            "mrs r0, ipsr",
            "r0",
            [0xef, 0xf3, 0x05, 0x80],
        ),
    ] {
        let target = erebos::target::by_name(target).unwrap();
        let source = work.join(format!("asm-{}.ers", target.name));
        let program = format!(
            "{VIRT_MAP}
static mut OUT: u32 = 0;

entry fn main() -> ! {{
    let mut v: u32 = 0;
    asm(\"{template}\", out(\"{register}\") v, options(nomem, nostack));
    OUT = v;
    loop {{}}
}}

panic fn on_panic() -> ! {{ loop {{}} }}
"
        );
        fs::write(&source, program).unwrap();
        for opt in [OptLevel::None, OptLevel::Speed] {
            let elf = work.join(format!("asm-{}-{opt:?}.elf", target.name));
            driver::build(&source, &elf, &[], target, opt, false).unwrap();
            let text = section_bytes(&elf, ".text");
            assert!(
                text.windows(instruction.len())
                    .any(|window| window == instruction),
                "{} {opt:?}: `.text` does not hold `{template}` ({instruction:02x?})",
                target.name
            );
        }
    }
}

/// `core::arch::fence()` is the target's hardware fence, and
/// `core::arch::compiler_fence()` emits no instruction (ADR 0081): the code
/// of the function `core.arch.fence` holds `fence iorw, iorw` on riscv32 and
/// `dmb sy` on thumbv7m, and the code of `core.arch.compiler_fence` holds
/// nothing but the return. The `ui` snapshots cannot show it: they leave
/// `core`'s code out.
#[test]
fn a_fence_is_an_instruction_and_a_compiler_fence_is_not() {
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("elf");
    fs::create_dir_all(&work).unwrap();
    for (target, instruction) in [
        ("riscv32imac-unknown-none-elf", [0x0f, 0x00, 0xf0, 0x0f]),
        ("thumbv7m-none-eabi", [0xbf, 0xf3, 0x5f, 0x8f]),
    ] {
        let target = erebos::target::by_name(target).unwrap();
        let source = work.join(format!("fence-{}.ers", target.name));
        let program = format!(
            "{VIRT_MAP}
entry fn main() -> ! {{
    core::arch::compiler_fence();
    core::arch::fence();
    loop {{}}
}}

panic fn on_panic() -> ! {{ loop {{}} }}
"
        );
        fs::write(&source, program).unwrap();
        for opt in [OptLevel::None, OptLevel::Speed] {
            let elf = work.join(format!("fence-{}-{opt:?}.elf", target.name));
            driver::build(&source, &elf, &[], target, opt, false).unwrap();
            let text = sections(&elf)
                .into_iter()
                .find(|section| section.name == ".text")
                .unwrap();
            let bytes = section_bytes(&elf, ".text");
            let symbols = symbols(&elf);
            // The code of a function; a Thumb function's address has bit 0 set.
            let code = |name: &str| {
                let symbol = symbols
                    .iter()
                    .find(|symbol| symbol.name == name)
                    .unwrap_or_else(|| panic!("{} {opt:?}: no symbol {name}", target.name));
                let from = (symbol.value & !1) - text.address;
                &bytes[from as usize..(from + symbol.size) as usize]
            };
            let holds = |code: &[u8]| {
                code.windows(instruction.len())
                    .any(|window| window == instruction)
            };
            assert!(
                holds(code("core.arch.fence")),
                "{} {opt:?}: `fence` lacks {instruction:02x?}",
                target.name
            );
            let compiler_fence = code("core.arch.compiler_fence");
            assert!(
                !holds(compiler_fence) && compiler_fence.len() <= 4,
                "{} {opt:?}: `compiler_fence` is {compiler_fence:02x?}",
                target.name
            );
        }
    }
}

/// `core` defines the EABI's 64-bit division on thumbv7m and only there (ADR
/// 0080): a program linked with the C file of `tests/e2e/c_division64.ers`,
/// whose divisions clang compiles to calls of those names, defines
/// `__aeabi_uldivmod` and `__aeabi_ldivmod` in `.text` as Thumb functions
/// (bit 0 of the address set, a size), and a riscv32 build defines neither,
/// where the same C file calls libgcc's names. The programs that run the
/// functions are in `tests/e2e`.
#[test]
fn the_eabi_division_is_defined_on_thumbv7m_only() {
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("elf");
    fs::create_dir_all(&work).unwrap();
    let c_file = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e/c_division64.c");
    let source = work.join("eabi_division.ers");
    fs::write(
        &source,
        format!(
            "{VIRT_MAP}
extern fn c_udivmod(a: u64, b: u64, rem: *mut u64) -> u64;
extern fn c_sdivmod(a: i64, b: i64, rem: *mut i64) -> i64;

static mut OUT: u64 = 0;

entry fn main() -> ! {{
    let mut u: u64 = 0;
    let mut s: i64 = 0;
    OUT = c_udivmod(7, 2, &raw mut u) + c_sdivmod(7, 2, &raw mut s) as u64;
    loop {{}}
}}

panic fn on_panic() -> ! {{ loop {{}} }}
"
        ),
    )
    .unwrap();
    for target in TARGETS {
        for opt in [OptLevel::None, OptLevel::Speed] {
            let object = common::qemu::compile_c(&c_file, &[], &work, target, opt).unwrap();
            let elf = work.join(format!("eabi_division-{}-{opt:?}.elf", target.name));
            driver::build(&source, &elf, &[object], target, opt, false).unwrap();
            let sections = sections(&elf);
            let symbols = symbols(&elf);
            let text = |name: &str| {
                let symbol = symbols.iter().find(|symbol| symbol.name == name)?;
                let index: usize = symbol.section.parse().ok()?;
                (sections[index].name == ".text").then_some(symbol)
            };
            for name in ["__aeabi_uldivmod", "__aeabi_ldivmod"] {
                let symbol = text(name);
                match target.arch {
                    Arch::Armv7m => {
                        let symbol = symbol.unwrap_or_else(|| {
                            panic!(
                                "{} {opt:?}: `{name}` is not defined in `.text`",
                                target.name
                            )
                        });
                        assert_eq!(
                            symbol.value & 1,
                            1,
                            "{} {opt:?}: `{name}` is not a Thumb function",
                            target.name
                        );
                        assert!(
                            symbol.size > 0,
                            "{} {opt:?}: `{name}` is empty",
                            target.name
                        );
                    }
                    Arch::Riscv32 => assert!(
                        !symbols.iter().any(|symbol| symbol.name == name),
                        "{} {opt:?}: `{name}` exists",
                        target.name
                    ),
                }
            }
        }
    }
}

/// The contents of the section `name` of `elf`, from its file.
fn section_bytes(elf: &Path, name: &str) -> Vec<u8> {
    let headers = readelf(elf, &["--section-headers"]);
    let line = headers
        .lines()
        .find(|line| line.contains(&format!(" {name} ")))
        .unwrap_or_else(|| panic!("no section `{name}`"));
    // `[Nr] Name Type Address Off Size ...`, after the `]`.
    let fields: Vec<&str> = line.split_once(']').unwrap().1.split_whitespace().collect();
    let (offset, size) = (hex(fields[3]) as usize, hex(fields[4]) as usize);
    fs::read(elf).unwrap()[offset..offset + size].to_vec()
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

    // `_start` is `core`'s naked function, and not a label of module assembly
    // (ADR 0083): a function symbol with a size.
    let start = symbol("_start");
    assert_eq!(start.kind, "FUNC", "{opt:?}: `_start` is not a function");
    assert!(start.size > 0, "{opt:?}: `_start` has no size");

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
                [num, value, size, kind, _bind, _vis, section, name]
                    if num.ends_with(':') && *num != "Num:" =>
                {
                    Some(Symbol {
                        name: (*name).to_owned(),
                        value: hex(value),
                        size: size.parse().unwrap_or(0),
                        kind: (*kind).to_owned(),
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
