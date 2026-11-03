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
};

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
};

/// Every target the compiler knows, in the order errors list them.
pub const TARGETS: &[&Target] = &[&RISCV32IMAC, &THUMBV7M];

/// The target of a build that does not choose one (ADR 0073).
pub const DEFAULT: &Target = &RISCV32IMAC;

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
    fn the_default_is_a_listed_target() {
        assert!(TARGETS.iter().any(|target| std::ptr::eq(*target, DEFAULT)));
    }
}
