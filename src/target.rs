//! The machines the compiler generates code for (ADR 0010, 0073): one
//! description per target, and a fixed list of them. What differs between
//! targets and is a number lives in the description; what is code is
//! chosen by a `match` on [`Arch`], each in the layer that owns it.

/// Width of pointers, `usize` and `isize`. Shared by every target
/// (ADR 0073): each description states its own, and a test checks it
/// against this constant.
pub const POINTER_BITS: u32 = 32;

/// Size and alignment of pointers, `usize` and `isize`, in bytes.
pub const POINTER_BYTES: u64 = POINTER_BITS as u64 / 8;

/// The widest memory access a single instruction performs. A volatile
/// access must be one instruction (ADR 0023), so nothing wider can be
/// volatile. Shared by every target, like `POINTER_BITS`.
pub const WIDEST_ACCESS_BITS: u32 = 32;

/// The architecture of a target, which picks the code that differs between
/// them: the calling convention, the reset entry, LLVM's initialization
/// and the linker script's architecture and sections (ADR 0073).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    /// RV32IMAC, the riscv32 C ABI of ADR 0025.
    Riscv32,
    /// ARMv7-M, Thumb-2, the AAPCS of ADR 0075 (ADR 0074).
    Armv7m,
}

/// The machine code is generated for. Where its memory is belongs to the
/// board, which the program's `memory` block describes (ADR 0057).
#[derive(Debug)]
pub struct Target {
    /// What `--target` names.
    pub name: &'static str,
    pub arch: Arch,
    pub llvm_triple: &'static str,
    pub llvm_cpu: &'static str,
    pub llvm_features: &'static str,
    /// Width of pointers, `usize` and `isize`.
    pub pointer_bits: u32,
    /// The widest access one instruction makes (ADR 0023).
    pub widest_access_bits: u32,
    /// The alignment of the stack pointer the C ABI requires: the stack's
    /// start and size are multiples of it (ADR 0057).
    pub stack_align: u64,
    /// The alignment of the reset entry, an instruction: the origin of the
    /// code's region is a multiple of it (ADR 0057).
    pub code_align: u64,
    /// Whether LLVM may emit its 64-bit division, whose calls all have the
    /// C ABI (ADR 0076).
    pub div64: bool,
    /// The registers of the machine that inline assembly names (ADR 0079).
    pub registers: &'static [Register],
    /// The registers `clobber_abi("C")` covers, by their canonical names: the
    /// ones a call of a C function may change (ADR 0079).
    pub c_clobbers: &'static [&'static str],
    /// How the registers are written, for the help of an unknown name.
    pub register_names: &'static str,
    /// The variants of `core::arch::Vector` of this target, in the order of
    /// the enum in `rt/arch/` (ADR 0084).
    pub vectors: &'static [Vector],
}

/// A variant of `core::arch::Vector`, with what the language needs of the
/// vector (ADR 0084); a test checks the list of each target against the
/// enum of its file in `rt/arch/`.
#[derive(Debug)]
pub struct Vector {
    /// The variant's name.
    pub name: &'static str,
    /// The variant's value: the exception number on ARMv7-M, `mcause` on
    /// RISC-V.
    pub value: u32,
    /// The word of the target's vector table that holds the handler
    /// (ADR 0085): on ARMv7-M the value itself; on RISC-V a value `n` is word
    /// `n`, and a value `0x8000_0000 + n` is word `16 + n`.
    pub word: u32,
    pub kind: VectorKind,
    /// A handler must return `!`: returning would run the instruction that
    /// trapped again.
    pub diverges: bool,
}

/// Whether a handler runs in the handler context of ADR 0078, or in a
/// context of its own (ADR 0084).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VectorKind {
    Maskable,
    Unmaskable,
}

impl Vector {
    const fn new(
        name: &'static str,
        value: u32,
        word: u32,
        kind: VectorKind,
        diverges: bool,
    ) -> Vector {
        Vector {
            name,
            value,
            word,
            kind,
            diverges,
        }
    }

    /// A system exception of ARMv7-M, whose word is its number.
    const fn arm(name: &'static str, value: u32, kind: VectorKind, diverges: bool) -> Vector {
        Vector::new(name, value, value, kind, diverges)
    }

    /// An exception of RISC-V, whose handler never returns (ADR 0084).
    const fn exception(name: &'static str, cause: u32) -> Vector {
        Vector::new(name, cause, cause, VectorKind::Unmaskable, true)
    }

    /// An interrupt of RISC-V, `mcause` with bit 31 set, in the word 16 on.
    const fn interrupt(name: &'static str, cause: u32) -> Vector {
        Vector::new(
            name,
            0x8000_0000 + cause,
            16 + cause,
            VectorKind::Maskable,
            false,
        )
    }
}

/// A register inline assembly may name (ADR 0079).
#[derive(Debug)]
pub struct Register {
    /// The name the compiler uses for it: the architecture's own, `x10` or
    /// `r0`. Two names of one register are the same register.
    pub name: &'static str,
    /// The other names it has: the ABI's, `a0`, or `lr`.
    pub aliases: &'static [&'static str],
    /// The compiler uses it for itself, so no operand names it: the stack
    /// pointer, the program counter, and the registers LLVM keeps for the
    /// frame and for addressing the frame.
    pub reserved: bool,
}

impl Register {
    const fn new(name: &'static str, aliases: &'static [&'static str]) -> Register {
        Register {
            name,
            aliases,
            reserved: false,
        }
    }

    const fn reserved(name: &'static str, aliases: &'static [&'static str]) -> Register {
        Register {
            name,
            aliases,
            reserved: true,
        }
    }

    /// Whether `name` is this register's name or one of its aliases.
    pub fn is_named(&self, name: &str) -> bool {
        self.name == name || self.aliases.contains(&name)
    }
}

/// RV32IMAC, as on QEMU's `virt` machine.
pub static RISCV32IMAC: Target = Target {
    name: "riscv32imac-unknown-none-elf",
    arch: Arch::Riscv32,
    llvm_triple: "riscv32-unknown-none-elf",
    llvm_cpu: "generic-rv32",
    llvm_features: "+m,+a,+c",
    pointer_bits: 32,
    widest_access_bits: 32,
    stack_align: 16,
    code_align: 4,
    div64: true,
    registers: &RISCV32_REGISTERS,
    c_clobbers: &RISCV32_C_CLOBBERS,
    register_names: "`x0` to `x31`, or their ABI names: `zero`, `ra`, `sp`, `gp`, `tp`, `t0` to `t6`, `s0` (`fp`) to `s11`, `a0` to `a7`",
    vectors: &RISCV32_VECTORS,
};

/// The variants of `Vector` in `rt/arch/riscv32.ers` (ADR 0084).
static RISCV32_VECTORS: [Vector; 12] = [
    Vector::exception("InstructionMisaligned", 0),
    Vector::exception("InstructionFault", 1),
    Vector::exception("IllegalInstruction", 2),
    Vector::exception("Breakpoint", 3),
    Vector::exception("LoadMisaligned", 4),
    Vector::exception("LoadFault", 5),
    Vector::exception("StoreMisaligned", 6),
    Vector::exception("StoreFault", 7),
    Vector::exception("EnvironmentCall", 11),
    Vector::interrupt("MachineSoftware", 3),
    Vector::interrupt("MachineTimer", 7),
    Vector::interrupt("MachineExternal", 11),
];

/// RV32I's registers, by the architecture's names and the ABI's.
static RISCV32_REGISTERS: [Register; 32] = [
    Register::reserved("x0", &["zero"]),
    Register::new("x1", &["ra"]),
    Register::reserved("x2", &["sp"]),
    Register::reserved("x3", &["gp"]),
    Register::reserved("x4", &["tp"]),
    Register::new("x5", &["t0"]),
    Register::new("x6", &["t1"]),
    Register::new("x7", &["t2"]),
    Register::reserved("x8", &["s0", "fp"]),
    // LLVM's base pointer.
    Register::reserved("x9", &["s1"]),
    Register::new("x10", &["a0"]),
    Register::new("x11", &["a1"]),
    Register::new("x12", &["a2"]),
    Register::new("x13", &["a3"]),
    Register::new("x14", &["a4"]),
    Register::new("x15", &["a5"]),
    Register::new("x16", &["a6"]),
    Register::new("x17", &["a7"]),
    Register::new("x18", &["s2"]),
    Register::new("x19", &["s3"]),
    Register::new("x20", &["s4"]),
    Register::new("x21", &["s5"]),
    Register::new("x22", &["s6"]),
    Register::new("x23", &["s7"]),
    Register::new("x24", &["s8"]),
    Register::new("x25", &["s9"]),
    Register::new("x26", &["s10"]),
    Register::new("x27", &["s11"]),
    Register::new("x28", &["t3"]),
    Register::new("x29", &["t4"]),
    Register::new("x30", &["t5"]),
    Register::new("x31", &["t6"]),
];

/// `ra`, `t0` to `t6` and `a0` to `a7`.
static RISCV32_C_CLOBBERS: [&str; 16] = [
    "x1", "x5", "x6", "x7", "x10", "x11", "x12", "x13", "x14", "x15", "x16", "x17", "x28", "x29",
    "x30", "x31",
];

/// ARMv7-M, as on the Cortex-M3 (ADR 0074). The board is the program's: QEMU's
/// `mps2-an385` in the tests.
pub static THUMBV7M: Target = Target {
    name: "thumbv7m-none-eabi",
    arch: Arch::Armv7m,
    llvm_triple: "thumbv7m-none-eabi",
    llvm_cpu: "cortex-m3",
    llvm_features: "",
    pointer_bits: 32,
    widest_access_bits: 32,
    stack_align: 8,
    code_align: 128,
    div64: false,
    registers: &ARMV7M_REGISTERS,
    c_clobbers: &ARMV7M_C_CLOBBERS,
    register_names: "`r0` to `r15`, or `sb` (`r9`), `sl` (`r10`), `fp` (`r11`), `ip` (`r12`), `sp` (`r13`), `lr` (`r14`) and `pc` (`r15`)",
    vectors: &ARMV7M_VECTORS,
};

/// The variants of `Vector` in `rt/arch/armv7m.ers` (ADR 0084).
static ARMV7M_VECTORS: [Vector; 9] = [
    Vector::arm("NMI", 2, VectorKind::Unmaskable, false),
    Vector::arm("HardFault", 3, VectorKind::Unmaskable, true),
    Vector::arm("MemManage", 4, VectorKind::Unmaskable, true),
    Vector::arm("BusFault", 5, VectorKind::Unmaskable, true),
    Vector::arm("UsageFault", 6, VectorKind::Unmaskable, true),
    Vector::arm("SVCall", 11, VectorKind::Unmaskable, false),
    Vector::arm("DebugMonitor", 12, VectorKind::Unmaskable, true),
    Vector::arm("PendSV", 14, VectorKind::Maskable, false),
    Vector::arm("SysTick", 15, VectorKind::Maskable, false),
];

/// ARMv7-M's general-purpose registers. `r6` is LLVM's base pointer and `r7`
/// the frame pointer in Thumb code.
static ARMV7M_REGISTERS: [Register; 16] = [
    Register::new("r0", &[]),
    Register::new("r1", &[]),
    Register::new("r2", &[]),
    Register::new("r3", &[]),
    Register::new("r4", &[]),
    Register::new("r5", &[]),
    Register::reserved("r6", &[]),
    Register::reserved("r7", &[]),
    Register::new("r8", &[]),
    Register::new("r9", &["sb"]),
    Register::new("r10", &["sl"]),
    Register::new("r11", &["fp"]),
    Register::new("r12", &["ip"]),
    Register::reserved("r13", &["sp"]),
    Register::new("r14", &["lr"]),
    Register::reserved("r15", &["pc"]),
];

/// `r0` to `r3`, `r12` and `lr`; the flags are no register an operand names.
static ARMV7M_C_CLOBBERS: [&str; 6] = ["r0", "r1", "r2", "r3", "r12", "r14"];

/// Every target the compiler knows, in the order errors list them.
pub const TARGETS: &[&Target] = &[&RISCV32IMAC, &THUMBV7M];

/// The target of a build that does not choose one (ADR 0073).
pub const DEFAULT: &Target = &RISCV32IMAC;

impl Target {
    /// The register that `name` names on this target, by its name or an
    /// alias.
    pub fn register(&self, name: &[u8]) -> Option<&'static Register> {
        let name = std::str::from_utf8(name).ok()?;
        self.registers
            .iter()
            .find(|register| register.is_named(name))
    }

    /// The vector that the variant `name` of `core::arch::Vector` names, by
    /// the variant's name (ADR 0084).
    pub fn vector(&self, name: &str) -> Option<&'static Vector> {
        self.vectors.iter().find(|vector| vector.name == name)
    }
}

/// The target named `name`, if there is one.
pub fn by_name(name: &str) -> Option<&'static Target> {
    TARGETS.iter().copied().find(|target| target.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shared constants hold for every target (ADR 0073).
    #[test]
    fn descriptions_agree_with_the_shared_constants() {
        for target in TARGETS {
            assert_eq!(target.pointer_bits, POINTER_BITS, "{}", target.name);
            assert_eq!(
                target.widest_access_bits, WIDEST_ACCESS_BITS,
                "{}",
                target.name
            );
        }
    }

    #[test]
    fn names_are_unique() {
        for (i, target) in TARGETS.iter().enumerate() {
            for other in &TARGETS[..i] {
                assert_ne!(target.name, other.name);
            }
        }
    }

    #[test]
    fn by_name_finds_each_target_and_only_those() {
        for target in TARGETS {
            let found = by_name(target.name).expect(target.name);
            assert!(std::ptr::eq(found, *target), "{}", target.name);
        }
        assert!(by_name("cortex-m3").is_none());
        assert!(by_name("").is_none());
        assert!(by_name("RISCV32IMAC-UNKNOWN-NONE-ELF").is_none());
    }

    #[test]
    fn registers_have_unique_names_and_the_clobbers_exist() {
        for target in TARGETS {
            let mut seen: Vec<&str> = Vec::new();
            for register in target.registers {
                for name in std::iter::once(&register.name).chain(register.aliases) {
                    assert!(!seen.contains(name), "{}: `{name}`", target.name);
                    seen.push(name);
                }
            }
            for name in target.c_clobbers {
                let register = target.register(name.as_bytes()).expect(name);
                assert_eq!(register.name, *name, "canonical names");
                assert!(!register.reserved, "{name}");
            }
        }
    }

    #[test]
    fn registers_are_found_by_every_name() {
        let a0 = RISCV32IMAC.register(b"a0").expect("an ABI name");
        assert_eq!(a0.name, "x10");
        let x10 = RISCV32IMAC.register(b"x10").expect("its own name");
        assert!(std::ptr::eq(a0, x10));
        assert_eq!(RISCV32IMAC.register(b"fp").map(|r| r.name), Some("x8"));
        assert_eq!(THUMBV7M.register(b"lr").map(|r| r.name), Some("r14"));
        // A name of the other architecture, and ones that are no name.
        assert!(THUMBV7M.register(b"a0").is_none());
        assert!(RISCV32IMAC.register(b"r0").is_none());
        assert!(RISCV32IMAC.register(b"x32").is_none());
        assert!(RISCV32IMAC.register(b"\xff").is_none());
    }

    #[test]
    fn the_registers_the_compiler_reserves_are_the_ones_of_adr_0079() {
        let reserved = |target: &Target| -> Vec<&str> {
            (target.registers.iter())
                .filter(|register| register.reserved)
                .map(|register| register.name)
                .collect()
        };
        assert_eq!(reserved(&RISCV32IMAC), ["x0", "x2", "x3", "x4", "x8", "x9"]);
        assert_eq!(reserved(&THUMBV7M), ["r6", "r7", "r13", "r15"]);
    }

    #[test]
    fn the_default_is_a_listed_target() {
        assert!(TARGETS.iter().any(|target| std::ptr::eq(*target, DEFAULT)));
    }

    /// The variants `NAME = VALUE,` of the `enum Vector: u32` that `source`
    /// declares, one per line, with their values (ADR 0084).
    fn declared_vectors(source: &str) -> Vec<(String, u32)> {
        const OPEN: &str = "pub enum Vector: u32 {";
        let after = &source[source.find(OPEN).expect("the file declares `Vector`") + OPEN.len()..];
        let body = &after[..after.find('}').expect("the enum is closed")];
        body.lines()
            .map(|line| line.split("//").next().unwrap_or("").trim())
            .filter(|line| !line.is_empty())
            .map(|line| {
                let (name, value) = line
                    .trim_end_matches(',')
                    .split_once(" = ")
                    .unwrap_or_else(|| panic!("`{line}` is not `NAME = VALUE,`"));
                let digits = value.replace('_', "");
                let value = match digits.strip_prefix("0x") {
                    Some(hex) => u32::from_str_radix(hex, 16),
                    None => digits.parse(),
                };
                (name.to_owned(), value.expect("a number"))
            })
            .collect()
    }

    /// The `Vector` of each file of `core::arch` and the list of its target
    /// are the same: names, values and the words of the table (ADR 0084).
    #[test]
    fn the_vectors_agree_with_the_enums_of_core_arch() {
        let files = [
            (&RISCV32IMAC, include_str!("../rt/arch/riscv32.ers")),
            (&THUMBV7M, include_str!("../rt/arch/armv7m.ers")),
        ];
        for (target, source) in files {
            let declared = declared_vectors(source);
            let listed: Vec<(String, u32)> = (target.vectors.iter())
                .map(|vector| (vector.name.to_owned(), vector.value))
                .collect();
            assert_eq!(declared, listed, "{}", target.name);
            let mut words: Vec<u32> = Vec::new();
            for vector in target.vectors {
                let (word, table) = match target.arch {
                    // The word is the exception number, in a table of 16.
                    Arch::Armv7m => (vector.value, 16),
                    // A cause `n` is word `n`, an interrupt `16 + n`, in a
                    // table of 32 and the word of any other cause.
                    Arch::Riscv32 => match vector.value.checked_sub(0x8000_0000) {
                        Some(cause) => (16 + cause, 32),
                        None => (vector.value, 32),
                    },
                };
                assert_eq!(vector.word, word, "{}: {}", target.name, vector.name);
                assert!(word < table, "{}: {}", target.name, vector.name);
                assert!(!words.contains(&word), "{}: {}", target.name, vector.name);
                words.push(word);
                if target.arch == Arch::Riscv32 {
                    // An interrupt can be masked and may return; an
                    // exception cannot be masked and never returns.
                    let interrupt = vector.value >= 0x8000_0000;
                    assert_eq!(vector.kind == VectorKind::Maskable, interrupt);
                    assert_eq!(vector.diverges, !interrupt, "{}", vector.name);
                }
            }
        }
    }

    #[test]
    fn a_vector_is_found_by_the_name_of_its_variant() {
        let tick = THUMBV7M.vector("SysTick").expect("a vector of thumbv7m");
        assert_eq!((tick.value, tick.word), (15, 15));
        assert!(!tick.diverges);
        assert!(THUMBV7M.vector("HardFault").expect("listed").diverges);
        // The other target's, and what is no variant.
        assert!(THUMBV7M.vector("MachineTimer").is_none());
        assert!(RISCV32IMAC.vector("SysTick").is_none());
        let timer = RISCV32IMAC.vector("MachineTimer").expect("listed");
        assert_eq!((timer.value, timer.word), (0x8000_0007, 23));
        assert!(RISCV32IMAC.vector("systick").is_none());
    }
}
