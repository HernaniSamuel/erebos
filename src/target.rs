//! The machine being compiled for. Only one target exists for now
//! (ADR 0010); this module is where target-specific values live until a
//! real target abstraction is needed.

/// Width of pointers, `usize` and `isize`. Fixed while there is one target.
pub const POINTER_BITS: u32 = 32;

/// Size and alignment of pointers, `usize` and `isize`, in bytes.
pub const POINTER_BYTES: u64 = POINTER_BITS as u64 / 8;

/// The widest memory access a single instruction performs. A volatile
/// access must be one instruction (ADR 0023), so nothing wider can be
/// volatile.
pub const WIDEST_ACCESS_BITS: u32 = 32;

/// The alignment of the stack pointer the riscv32 C ABI requires: the
/// stack's start and size are multiples of it (ADR 0057).
pub const STACK_ALIGN: u64 = 16;

/// The alignment of the reset entry, an instruction: the origin of the
/// code's region is a multiple of it (ADR 0057).
pub const CODE_ALIGN: u64 = 4;

/// The machine code is generated for. Where its memory is belongs to the
/// board, which the program's `memory` block describes (ADR 0057).
pub struct Target {
    pub llvm_triple: &'static str,
    pub llvm_cpu: &'static str,
    pub llvm_features: &'static str,
}

/// RV32IMAC, as on QEMU's `virt` machine.
pub const RISCV32_VIRT: Target = Target {
    llvm_triple: "riscv32-unknown-none-elf",
    llvm_cpu: "generic-rv32",
    llvm_features: "+m,+a,+c",
};
