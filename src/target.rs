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

pub struct Target {
    pub llvm_triple: &'static str,
    pub llvm_cpu: &'static str,
    pub llvm_features: &'static str,
    /// Where RAM starts; with QEMU `-bios none`, execution starts here too.
    pub ram_origin: u64,
    pub ram_size: u64,
}

impl Target {
    /// The stack grows down from the end of RAM.
    pub fn stack_top(&self) -> u64 {
        self.ram_origin + self.ram_size
    }
}

/// RV32IMAC on QEMU's `virt` machine with its default 128 MiB of RAM.
pub const RISCV32_VIRT: Target = Target {
    llvm_triple: "riscv32-unknown-none-elf",
    llvm_cpu: "generic-rv32",
    llvm_features: "+m,+a,+c",
    ram_origin: 0x8000_0000,
    ram_size: 128 * 1024 * 1024,
};
