//! The diagnostic handler of LLVM's contexts (ADR 0079, 0082).
//!
//! LLVM reports an error it finds while it assembles inline assembly, or
//! while it allocates the registers of its operands, to the diagnostic
//! handler of the context; without one it prints the message and ends the
//! process. `inkwell` cannot install a handler, so this is the one file of
//! the compiler that names `llvm_sys`, and only for the handler (ADR 0082):
//! it builds nothing, and `tests/architecture.rs` keeps the names it uses to
//! a list.

use std::cell::RefCell;
use std::ffi::{CStr, c_void};

use inkwell::context::Context;
use inkwell::llvm_sys::LLVMDiagnosticSeverity::{
    LLVMDSError, LLVMDSNote, LLVMDSRemark, LLVMDSWarning,
};
use inkwell::llvm_sys::core::{
    LLVMContextSetDiagnosticHandler, LLVMDisposeMessage, LLVMGetDiagInfoDescription,
    LLVMGetDiagInfoSeverity,
};
use inkwell::llvm_sys::prelude::{LLVMContextRef, LLVMDiagnosticInfoRef};

/// How serious LLVM says a diagnostic is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Remark,
    Note,
}

/// The errors LLVM reported to the contexts it is installed on, as their
/// text. Warnings, remarks and notes are dropped: the compiler reports none
/// of them (ADR 0082).
///
/// It is shared with LLVM by address, so it must be declared before the
/// `Context` it is installed on: locals are dropped in reverse order, and
/// the collector then outlives every call LLVM makes to the handler. The
/// state is per collector, never a `static` (builds are a function of their
/// sources, and tests compile in parallel).
#[derive(Default)]
pub struct Collector {
    errors: RefCell<Vec<String>>,
}

impl Collector {
    /// Makes `context` report its diagnostics here.
    ///
    /// The collector must outlive the context (see the type).
    pub fn install(&self, context: &Context) {
        let raw: LLVMContextRef = context.raw();
        let data = std::ptr::from_ref(self).cast_mut().cast::<c_void>();
        // SAFETY: `raw` is the live context of `context`, and `data` points
        // to this collector, which the caller keeps alive for longer than
        // the context. The callback only takes a shared reference to it.
        unsafe { LLVMContextSetDiagnosticHandler(raw, Some(handle), data) }
    }

    /// The errors reported so far, in order; the collector is left empty.
    pub fn take(&self) -> Vec<String> {
        self.errors.take()
    }
}

/// The function LLVM calls with each diagnostic. It must not panic, since
/// a panic out of an `extern "C"` function aborts the process: it has no
/// `unwrap`, `expect`, indexing or assertion, a null description is empty
/// text, text that is not UTF-8 is read lossily, and a collector found
/// borrowed (which cannot happen, nothing holds a borrow while LLVM runs)
/// drops the diagnostic.
extern "C" fn handle(info: LLVMDiagnosticInfoRef, data: *mut c_void) {
    // SAFETY: `info` is the diagnostic LLVM is reporting.
    let severity = severity(unsafe { LLVMGetDiagInfoSeverity(info) });
    if severity != Severity::Error || data.is_null() {
        return;
    }
    // SAFETY: `info` is the diagnostic LLVM is reporting.
    let text = unsafe { LLVMGetDiagInfoDescription(info) };
    let message = if text.is_null() {
        String::new()
    } else {
        // SAFETY: a non-null description is a NUL-terminated string.
        let message = unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned();
        // SAFETY: `text` came from `LLVMGetDiagInfoDescription`, which the
        // caller frees with this function, and it is not used afterwards.
        unsafe { LLVMDisposeMessage(text) };
        message
    };
    // SAFETY: `data` is the collector `install` gave, alive until after the
    // context; only a shared reference is made.
    let collector = unsafe { &*data.cast::<Collector>() };
    if let Ok(mut errors) = collector.errors.try_borrow_mut() {
        errors.push(message);
    }
}

fn severity(severity: inkwell::llvm_sys::LLVMDiagnosticSeverity) -> Severity {
    match severity {
        LLVMDSError => Severity::Error,
        LLVMDSWarning => Severity::Warning,
        LLVMDSRemark => Severity::Remark,
        LLVMDSNote => Severity::Note,
    }
}

#[cfg(test)]
mod tests {
    use inkwell::targets::FileType;

    use super::*;
    use crate::codegen::{OptLevel, target_machine};
    use crate::target::RISCV32IMAC;

    /// What the handler of `collector` collects while `context` writes an
    /// object whose module assembly is `text`.
    fn assemble(collector: &Collector, context: &Context, text: &str) -> Vec<String> {
        let machine = target_machine(&RISCV32IMAC, OptLevel::None).unwrap();
        let module = context.create_module("m");
        module.set_triple(&machine.get_triple());
        module.set_inline_assembly(text);
        machine
            .write_to_memory_buffer(&module, FileType::Object)
            .unwrap();
        collector.take()
    }

    /// An error of the assembler reaches the collector instead of ending the
    /// process, and each context has its own: an error in one is not seen by
    /// the other, and a correct module reports nothing.
    #[test]
    fn errors_are_collected_per_context() {
        let (bad, good) = (Collector::default(), Collector::default());
        let (bad_context, good_context) = (Context::create(), Context::create());
        bad.install(&bad_context);
        good.install(&good_context);

        let errors = assemble(&bad, &bad_context, "frobnicate x1, x2");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].starts_with("<inline asm>:"), "{errors:?}");
        assert!(errors[0].contains("frobnicate"), "{errors:?}");
        assert_eq!(assemble(&good, &good_context, "nop"), Vec::<String>::new());
        // Taking the errors empties the collector.
        assert!(bad.take().is_empty());
    }

    #[test]
    fn severities_are_told_apart() {
        assert_eq!(severity(LLVMDSError), Severity::Error);
        assert_eq!(severity(LLVMDSWarning), Severity::Warning);
        assert_eq!(severity(LLVMDSRemark), Severity::Remark);
        assert_eq!(severity(LLVMDSNote), Severity::Note);
    }
}
