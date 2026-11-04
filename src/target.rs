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
    /// `reg` may choose it: a general-purpose register the compiler may give
    /// to any operand.
    pub in_reg_class: bool,
}

impl Register {
    const fn new(name: &'static str, aliases: &'static [&'static str]) -> Register {
        Register {
            name,
            aliases,
            reserved: false,
            in_reg_class: true,
        }
    }

    const fn reserved(name: &'static str, aliases: &'static [&'static str]) -> Register {
        Register {
            name,
            aliases,
            reserved: true,
            in_reg_class: false,
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
};

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
};

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
                // `reg` never chooses a register the compiler keeps.
                assert!(
                    !(register.reserved && register.in_reg_class),
                    "{}",
                    register.name
                );
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
}
