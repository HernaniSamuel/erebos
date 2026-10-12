//! MIR -> LLVM IR -> object file, through inkwell (ADR 0005).

use std::path::Path;

use inkwell::OptimizationLevel;
use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::basic_block::BasicBlock as LlvmBlock;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target as LlvmTarget, TargetMachine,
    TargetTriple,
};
use inkwell::values::FunctionValue;

use crate::mir::{Body, Mir, Terminator, Ty};
use crate::target::Target;

/// Returns the textual LLVM IR, for `--emit=llvm-ir`.
pub fn emit_llvm_ir(mir: &Mir, target: &Target) -> Result<String, String> {
    let machine = target_machine(target)?;
    let context = Context::create();
    let module = lower(&context, mir, target, &machine);
    Ok(module.print_to_string().to_string())
}

pub fn emit_object(mir: &Mir, target: &Target, path: &Path) -> Result<(), String> {
    let machine = target_machine(target)?;
    let context = Context::create();
    let module = lower(&context, mir, target, &machine);
    machine
        .write_to_file(&module, FileType::Object, path)
        .map_err(|e| format!("cannot write `{}`: {e}", path.display()))
}

fn target_machine(target: &Target) -> Result<TargetMachine, String> {
    LlvmTarget::initialize_riscv(&InitializationConfig::default());
    let triple = TargetTriple::create(target.llvm_triple);
    let llvm_target = LlvmTarget::from_triple(&triple).map_err(|e| e.to_string())?;
    llvm_target
        .create_target_machine(
            &triple,
            target.llvm_cpu,
            target.llvm_features,
            OptimizationLevel::Default,
            // Bare metal: code runs at its link address.
            RelocMode::Static,
            CodeModel::Small,
        )
        .ok_or_else(|| format!("LLVM cannot create a machine for `{}`", target.llvm_triple))
}

fn lower<'ctx>(
    context: &'ctx Context,
    mir: &Mir,
    target: &Target,
    machine: &TargetMachine,
) -> Module<'ctx> {
    let module = context.create_module("erebos");
    module.set_triple(&machine.get_triple());
    module.set_data_layout(&machine.get_target_data().get_data_layout());

    // Declare everything first so bodies can refer to any function.
    let fns: Vec<FunctionValue> = mir
        .bodies()
        .map(|(_, body)| declare(context, &module, body))
        .collect();
    for ((_, body), &function) in mir.bodies().zip(&fns) {
        define(context, body, function);
    }

    module.set_inline_assembly(&start_stub(target, &mir.body(mir.entry).name));

    if let Err(e) = module.verify() {
        panic!("codegen produced invalid LLVM IR: {}", e.to_string());
    }
    module
}

fn declare<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    body: &Body,
) -> FunctionValue<'ctx> {
    // Both `()` and `!` return nothing; `!` additionally never returns.
    let fn_type = context.void_type().fn_type(&[], false);
    let function = module.add_function(&body.name, fn_type, None);

    // There is no unwinding on bare metal (ADR 0002).
    add_fn_attribute(context, function, "nounwind");
    if body.ret == Ty::Never {
        add_fn_attribute(context, function, "noreturn");
    }
    function
}

fn add_fn_attribute(context: &Context, function: FunctionValue, name: &str) {
    let kind = Attribute::get_named_enum_kind_id(name);
    function.add_attribute(
        AttributeLoc::Function,
        context.create_enum_attribute(kind, 0),
    );
}

fn define(context: &Context, body: &Body, function: FunctionValue) {
    let builder = context.create_builder();
    let blocks: Vec<LlvmBlock> = body
        .blocks()
        .map(|(bb, _)| context.append_basic_block(function, &bb.to_string()))
        .collect();

    for ((_, data), &block) in body.blocks().zip(&blocks) {
        builder.position_at_end(block);
        let result = match data.terminator {
            Terminator::Goto(target) => {
                builder.build_unconditional_branch(blocks[target.0 as usize])
            }
            Terminator::Return => builder.build_return(None),
            Terminator::Unreachable => builder.build_unreachable(),
        };
        result.expect("builder is positioned at a block");
    }
}

/// Provisional startup code (ADR 0012): sets up the stack and jumps to the
/// entry function. It goes away once the language can express it (M3/M4).
fn start_stub(target: &Target, entry: &str) -> String {
    format!(
        "\t.section .text.start, \"ax\", @progbits\n\
         \t.globl _start\n\
         \t.type _start, @function\n\
         _start:\n\
         \tli sp, {:#x}\n\
         \tj {entry}\n",
        target.stack_top()
    )
}
