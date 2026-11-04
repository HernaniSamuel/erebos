//! Inline assembly as LLVM's (ADR 0079, 0082): the template and the constraint
//! string derived from the operands, the call, and what is done when LLVM
//! reports an error of the assembler.

use std::fs;
use std::path::Path;

use inkwell::AddressSpace;
use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::memory_buffer::MemoryBuffer;
use inkwell::module::{Linkage, Module};
use inkwell::targets::{FileType, TargetMachine};
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValueEnum, FunctionValue, PointerValue, ValueKind,
};

use super::diagnostics::Collector;
use super::{FnCodegen, int_type};
use crate::diag::Diagnostic;
use crate::mir::{
    AsmOperandKind, AsmOptions, AsmPiece, AsmReg, AsmSym, Body, InlineAsm, IntTy, Mir, Operand,
    Place, Statement, Terminator, Ty,
};
use crate::span::Span;
use crate::target::Arch;

/// What a register holds as LLVM sees it: an operand is a 32-bit integer or a
/// pointer (ADR 0079), and an integer narrower than a register is widened
/// before the call and narrowed after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RegTy {
    Word,
    Pointer,
}

impl RegTy {
    fn of(ty: Ty) -> RegTy {
        match ty {
            Ty::Ptr(_) => RegTy::Pointer,
            _ => RegTy::Word,
        }
    }

    fn llvm<'ctx>(self, context: &'ctx Context) -> BasicTypeEnum<'ctx> {
        match self {
            RegTy::Word => context.i32_type().into(),
            RegTy::Pointer => context.ptr_type(AddressSpace::default()).into(),
        }
    }
}

/// A value the call takes, in the order of LLVM's inputs.
#[derive(Clone, Copy, Debug)]
struct Input {
    value: Operand,
    ty: RegTy,
}

/// A value the call returns, in the order of LLVM's outputs: stored in
/// `place`, or dropped.
#[derive(Clone, Copy, Debug)]
struct Output {
    place: Option<Place>,
    ty: RegTy,
}

/// An `asm` expression as the call LLVM gets: its text, its constraints and
/// the inputs and outputs they describe.
#[derive(Debug)]
pub(super) struct Plan {
    template: String,
    constraints: String,
    inputs: Vec<Input>,
    outputs: Vec<Output>,
    sideeffect: bool,
    alignstack: bool,
    options: AsmOptions,
}

impl Plan {
    /// The call for `inline`, found in `body` of `mir`, on `arch`.
    ///
    /// LLVM numbers outputs first, then inputs. An `out` is an output that
    /// no input may share a register with (`=&`), a `lateout` one that may
    /// (`=`), and an `inout` an output (`=`) with an input tied to it. An
    /// output with no place is still an output when it is in the template
    /// (`reg`), and a clobber when it names its register. A register that
    /// `clobber_abi` covers and an explicit input names is lost when the code
    /// ends: the input is tied to an output nothing reads.
    pub(super) fn new(inline: &InlineAsm, body: &Body, mir: &Mir, arch: Arch) -> Plan {
        let operands = &inline.operands;
        let register = |reg: AsmReg| match reg {
            AsmReg::Any => "r".to_owned(),
            AsmReg::Fixed(name) => format!("{{{}}}", llvm_register(arch, name)),
        };
        let clobber = |name: &str| format!("~{{{}}}", llvm_register(arch, name));

        // The registers of explicit outputs, which are not clobbers, and the
        // registers `out(...) _` clobbers.
        let mut written: Vec<&str> = Vec::new();
        let mut lost: Vec<&str> = inline.clobbers.clone();
        for operand in operands {
            let AsmReg::Fixed(name) = operand.reg else {
                continue;
            };
            match operand.kind {
                AsmOperandKind::Out { place: None, .. } => lost.push(name),
                AsmOperandKind::Out { .. } | AsmOperandKind::InOut { .. } => written.push(name),
                _ => {}
            }
        }

        let mut outputs: Vec<Output> = Vec::new();
        let mut output_constraints: Vec<String> = Vec::new();
        // The number the operand has in the template, and the output an input
        // is tied to.
        let mut number = vec![0usize; operands.len()];
        let mut tied: Vec<Option<usize>> = vec![None; operands.len()];
        for (i, operand) in operands.iter().enumerate() {
            match operand.kind {
                AsmOperandKind::Out { late, place } => {
                    if place.is_none() && matches!(operand.reg, AsmReg::Fixed(_)) {
                        continue;
                    }
                    number[i] = outputs.len();
                    let early = if late { "" } else { "&" };
                    output_constraints.push(format!("={early}{}", register(operand.reg)));
                    let ty = place.map_or(RegTy::Word, |place| RegTy::of(body.place_ty(place)));
                    outputs.push(Output { place, ty });
                }
                AsmOperandKind::InOut { input, place } => {
                    number[i] = outputs.len();
                    tied[i] = Some(outputs.len());
                    output_constraints.push(format!("={}", register(operand.reg)));
                    let ty = RegTy::of(body.operand_ty(&input));
                    outputs.push(Output { place, ty });
                }
                _ => {}
            }
        }
        // Explicit inputs whose register the code loses.
        let mut dropped: Vec<&str> = Vec::new();
        for (i, operand) in operands.iter().enumerate() {
            if let (AsmOperandKind::In(value), AsmReg::Fixed(name)) = (&operand.kind, operand.reg)
                && lost.contains(&name)
                && !written.contains(&name)
            {
                tied[i] = Some(outputs.len());
                output_constraints.push(format!("={}", register(operand.reg)));
                let ty = RegTy::of(body.operand_ty(value));
                outputs.push(Output { place: None, ty });
                dropped.push(name);
            }
        }

        let mut inputs: Vec<Input> = Vec::new();
        let mut input_constraints: Vec<String> = Vec::new();
        for (i, operand) in operands.iter().enumerate() {
            let value = match operand.kind {
                AsmOperandKind::In(value) => {
                    number[i] = outputs.len() + inputs.len();
                    value
                }
                AsmOperandKind::InOut { input, .. } => input,
                _ => continue,
            };
            input_constraints.push(match tied[i] {
                Some(output) => output.to_string(),
                None => register(operand.reg),
            });
            inputs.push(Input {
                value,
                ty: RegTy::of(body.operand_ty(&value)),
            });
        }

        // What the code may change without saying so.
        let mut constraints = output_constraints;
        constraints.extend(input_constraints);
        let mut clobbers: Vec<&str> = Vec::new();
        for name in lost {
            if !written.contains(&name) && !dropped.contains(&name) && !clobbers.contains(&name) {
                clobbers.push(name);
            }
        }
        constraints.extend(clobbers.into_iter().map(clobber));
        let options = inline.options;
        if !options.nomem && !options.readonly {
            constraints.push("~{memory}".to_owned());
        }
        // `clobber_abi("C")` covers the flags on thumbv7m (ADR 0079), whatever
        // `preserves_flags` says of the code itself.
        if arch == Arch::Armv7m && (!options.preserves_flags || !inline.clobbers.is_empty()) {
            constraints.push("~{cc}".to_owned());
        }

        Plan {
            template: render(inline, &number, mir),
            constraints: constraints.join(","),
            inputs,
            outputs,
            sideeffect: !options.pure,
            alignstack: !options.nostack,
            options,
        }
    }
}

/// How LLVM names a register that the compiler calls `name` (ADR 0079).
fn llvm_register(arch: Arch, name: &str) -> &str {
    match (arch, name) {
        // LLVM does not know `{r14}`, only `{lr}`; it knows the other
        // registers an operand may name by their numbers.
        (Arch::Armv7m, "r14") => "lr",
        _ => name,
    }
}

/// The template: the text with each `$` doubled, as LLVM reads it, and each
/// placeholder replaced by the operand's number in LLVM's numbering, its
/// symbol, or its value.
fn render(inline: &InlineAsm, number: &[usize], mir: &Mir) -> String {
    let mut template = String::new();
    for piece in &inline.pieces {
        match piece {
            AsmPiece::Text(text) => template.push_str(&text.replace('$', "$$")),
            AsmPiece::Operand(i) => match inline.operands[*i].kind {
                AsmOperandKind::Sym(AsmSym::Body(id)) => template.push_str(&mir.body(id).name),
                AsmOperandKind::Sym(AsmSym::Static(s)) => template.push_str(&s.def().name),
                AsmOperandKind::Const(bits, int) => template.push_str(&int.display_bits(bits)),
                _ => template.push_str(&format!("${{{}}}", number[*i])),
            },
        }
    }
    template
}

/// The attribute `memory(effects)`, whose encoding belongs to LLVM: it is read
/// from a declaration that has it.
fn memory_attribute(context: &Context, effects: &str) -> Attribute {
    let ir = format!("declare void @f() memory({effects})");
    let buffer = MemoryBuffer::create_from_memory_range_copy(ir.as_bytes(), "memory");
    let module = (context.create_module_from_ir(buffer)).expect("the declaration is valid IR");
    let kind = Attribute::get_named_enum_kind_id("memory");
    (module.get_function("f"))
        .and_then(|function| function.get_enum_attribute(AttributeLoc::Function, kind))
        .expect("the declaration has the attribute")
}

/// The call of the inline assembly of `plan`, with the values of its inputs;
/// returns the values of its outputs. The code of a scratch module (see
/// [`assembles`]) is never deleted, so that the assembler reads it.
fn call<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    plan: &Plan,
    inputs: &[BasicValueEnum<'ctx>],
    scratch: bool,
) -> Vec<BasicValueEnum<'ctx>> {
    let params: Vec<BasicMetadataTypeEnum> = (plan.inputs.iter())
        .map(|input| input.ty.llvm(context).into())
        .collect();
    let fn_type = match plan.outputs.as_slice() {
        [] => context.void_type().fn_type(&params, false),
        [output] => output.ty.llvm(context).fn_type(&params, false),
        outputs => {
            let fields: Vec<BasicTypeEnum> = (outputs.iter())
                .map(|output| output.ty.llvm(context))
                .collect();
            context.struct_type(&fields, false).fn_type(&params, false)
        }
    };
    let asm = context.create_inline_asm(
        fn_type,
        plan.template.clone(),
        plan.constraints.clone(),
        plan.sideeffect || scratch,
        plan.alignstack,
        None,
        false,
    );
    let args: Vec<BasicMetadataValueEnum> = inputs.iter().map(|&value| value.into()).collect();
    let site = builder
        .build_indirect_call(fn_type, asm, &args, "")
        .expect("builder is positioned");

    // No unwinding on bare metal (ADR 0002). What the options promise is
    // told to LLVM as the call's memory effects: a `pure` call may be deleted
    // when its outputs are unused (it returns), and one that is not may not
    // be, nor moved across what it has an effect on.
    let flag =
        |name: &str| context.create_enum_attribute(Attribute::get_named_enum_kind_id(name), 0);
    site.add_attribute(AttributeLoc::Function, flag("nounwind"));
    let options = plan.options;
    if options.pure && !scratch {
        site.add_attribute(AttributeLoc::Function, flag("willreturn"));
    }
    if !scratch {
        let effects = match (options.nomem, options.readonly, options.pure) {
            (true, _, true) => Some("none"),
            (true, _, false) => Some("inaccessiblemem: readwrite"),
            (false, true, _) => Some("read"),
            (false, false, _) => None,
        };
        if let Some(effects) = effects {
            site.add_attribute(AttributeLoc::Function, memory_attribute(context, effects));
        }
    }

    let ValueKind::Basic(result) = site.try_as_basic_value() else {
        return Vec::new();
    };
    match plan.outputs.len() {
        0 | 1 => vec![result],
        n => (0..n)
            .map(|i| {
                builder
                    .build_extract_value(result.into_struct_value(), i as u32, "")
                    .expect("the call returns a struct of its outputs")
            })
            .collect(),
    }
}

impl<'ctx> FnCodegen<'_, 'ctx> {
    /// An `asm` expression: the inputs are widened to a register, the call is
    /// made, and the outputs are narrowed and stored in order (ADR 0079).
    pub(super) fn inline_asm(&self, inline: &InlineAsm) {
        let plan = Plan::new(inline, self.body, self.mir, self.arch);
        let inputs: Vec<BasicValueEnum> = (plan.inputs.iter())
            .map(|input| self.widened(&input.value))
            .collect();
        let outputs = call(self.context, &self.builder, &plan, &inputs, false);
        for (output, value) in plan.outputs.iter().zip(outputs) {
            if let Some(place) = output.place {
                self.write(place, self.narrowed(value, self.body.place_ty(place)));
            }
        }
    }

    /// The value of an input in a register: an integer narrower than one is
    /// extended by its signedness.
    fn widened(&self, value: &Operand) -> BasicValueEnum<'ctx> {
        let register = self.operand(value);
        match self.body.operand_ty(value) {
            Ty::Int(int) => self
                .builder
                .build_int_cast_sign_flag(
                    register.into_int_value(),
                    int_type(self.context, IntTy::U32),
                    int.is_signed(),
                    "",
                )
                .expect("builder is positioned")
                .into(),
            _ => register,
        }
    }

    /// What a register holds, as a value of type `ty`: the low bits of it.
    fn narrowed(&self, register: BasicValueEnum<'ctx>, ty: Ty) -> BasicValueEnum<'ctx> {
        match ty {
            Ty::Int(int) => self
                .builder
                .build_int_cast_sign_flag(
                    register.into_int_value(),
                    int_type(self.context, int),
                    false,
                    "",
                )
                .expect("builder is positioned")
                .into(),
            _ => register,
        }
    }
}

/// Keeps what `sym` operands name: the items join `llvm.compiler.used`, so
/// that LLVM neither removes nor renames what the text refers to
/// (ADR 0079).
pub(super) fn keep_symbols<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    mir: &Mir,
    fns: &[Option<FunctionValue<'ctx>>],
    statics: &[PointerValue<'ctx>],
) {
    let mut used: Vec<PointerValue> = Vec::new();
    for (_, body) in mir.bodies().filter(|(_, body)| !body.comptime) {
        for inline in asms(body) {
            for sym in inline.syms() {
                let item = match sym {
                    AsmSym::Body(id) => fns[id.0 as usize]
                        .map(|function| function.as_global_value().as_pointer_value()),
                    AsmSym::Static(s) => Some(statics[s.def().id.0 as usize]),
                };
                if let Some(item) = item
                    && !used.contains(&item)
                {
                    used.push(item);
                }
            }
        }
    }
    if used.is_empty() {
        return;
    }
    let pointer = context.ptr_type(AddressSpace::default());
    let array = pointer.const_array(&used);
    let global = module.add_global(array.get_type(), None, "llvm.compiler.used");
    global.set_linkage(Linkage::Appending);
    global.set_section(Some("llvm.metadata"));
    global.set_initializer(&array);
}

/// The `asm` expressions of a body, in order.
fn asms(body: &Body) -> impl Iterator<Item = &InlineAsm> {
    body.blocks.iter().flat_map(|block| {
        let statements = block
            .statements
            .iter()
            .filter_map(|statement| match statement {
                Statement::InlineAsm(inline) => Some(&**inline),
                _ => None,
            });
        let ends = match block.terminator {
            Terminator::InlineAsm(inline) => Some(inline),
            _ => None,
        };
        statements.chain(ends)
    })
}

/// The name LLVM gives the text of an `asm` expression when the assembler
/// reports an error in it, and in a module's.
const ASSEMBLER_PREFIX: &str = "<inline asm>";

/// What LLVM says when no register can be found for the constraint of an
/// operand.
fn is_register_error(message: &str) -> bool {
    message.contains("inline assembly requires more registers than available")
        || (message.contains("allocate") && message.contains("for constraint"))
}

/// Whether an error LLVM reported is about inline assembly: the assembler
/// rejected it, or no register could be found for the constraint of an
/// operand.
fn is_asm_error(message: &str) -> bool {
    message.starts_with(ASSEMBLER_PREFIX) || is_register_error(message)
}

/// What LLVM said, as a diagnostic: `codegen::asm` for the `asm` expressions
/// the assembler rejects, each once and at its span, in the order of the
/// source, `codegen::llvm` for the rest (ADR 0079).
///
/// LLVM does not say which `asm` expression an error is in, and it may have
/// copied one by inlining, so each expression of the program is assembled
/// alone, with the constraints it has in the program. When none fails alone,
/// the error is one of the whole object (a label defined twice by inlining,
/// or no register left in context): it is reported without a label, once for
/// each message.
pub(super) fn diagnose(
    errors: Vec<String>,
    mir: &Mir,
    machine: &TargetMachine,
    arch: Arch,
) -> Vec<Diagnostic> {
    let (asm_errors, other): (Vec<String>, Vec<String>) =
        errors.into_iter().partition(|error| is_asm_error(error));
    let mut found: Vec<Diagnostic> = Vec::new();
    if !asm_errors.is_empty() {
        let mut seen: Vec<Span> = Vec::new();
        let mut rejected: Vec<(Span, String)> = Vec::new();
        for (_, body) in mir.bodies().filter(|(_, body)| !body.comptime) {
            for inline in asms(body) {
                if seen.contains(&inline.span) {
                    continue;
                }
                seen.push(inline.span);
                let plan = Plan::new(inline, body, mir, arch);
                if let Err(message) = assembles(machine, &plan) {
                    rejected.push((inline.span, message));
                }
            }
        }
        rejected.sort_by_key(|(span, _)| (span.file, span.lo, span.hi));
        if rejected.is_empty() {
            let mut shown: Vec<&String> = Vec::new();
            for message in &asm_errors {
                if !shown.contains(&message) {
                    shown.push(message);
                    found.push(Diagnostic::error("codegen::asm", whole(message)).help(
                        "LLVM found this in the whole object, so it cannot say which `asm` \
                         expression it comes from",
                    ));
                }
            }
        }
        for (span, message) in rejected {
            let label = if message.starts_with(ASSEMBLER_PREFIX) {
                "the assembler rejects this code"
            } else {
                "LLVM finds no register for this code"
            };
            found.push(Diagnostic::error("codegen::asm", whole(&message)).label(span, label));
        }
    }
    found.extend(
        other
            .into_iter()
            .map(|message| Diagnostic::error("codegen::llvm", message)),
    );
    found
}

/// LLVM's message, without the name it gives the text and the position in it.
fn whole(message: &str) -> String {
    message.trim().to_owned()
}

/// Whether the assembler accepts `plan` alone, and, if not, what it says.
///
/// The `asm` expression is the only code of a module of the machine's
/// target, called from a function with its inputs as parameters; its outputs
/// are not used. It is assembled with a context and a handler of its own.
fn assembles(machine: &TargetMachine, plan: &Plan) -> Result<(), String> {
    let collector = Collector::default();
    let context = Context::create();
    collector.install(&context);
    let module = context.create_module("asm");
    module.set_triple(&machine.get_triple());
    module.set_data_layout(&machine.get_target_data().get_data_layout());
    let params: Vec<BasicMetadataTypeEnum> = (plan.inputs.iter())
        .map(|input| input.ty.llvm(&context).into())
        .collect();
    let function = module.add_function("asm", context.void_type().fn_type(&params, false), None);
    let builder = context.create_builder();
    builder.position_at_end(context.append_basic_block(function, "entry"));
    let inputs: Vec<BasicValueEnum> = function.get_param_iter().collect();
    call(&context, &builder, plan, &inputs, true);
    builder.build_return(None).expect("builder is positioned");
    let written = machine.write_to_memory_buffer(&module, FileType::Object);
    match collector.take().into_iter().next() {
        Some(message) => Err(message),
        None => written.map(|_| ()).map_err(|e| e.to_string()),
    }
}

/// Removes the object a failed build left, so that it is never linked.
pub(super) fn discard(path: &Path) {
    let _ = fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The errors LLVM reports for inline assembly are told from the rest:
    /// the assembler's, which start with the name of the text, and the ones
    /// about registers (ADR 0079).
    #[test]
    fn errors_about_inline_assembly_are_told_apart() {
        for message in [
            "<inline asm>:1:2: unrecognized instruction mnemonic\n\tfrob\n\t^",
            "<inline asm>:1:2: symbol 'label' is already defined",
            "inline assembly requires more registers than available",
            "couldn't allocate output register for constraint '{r14}'",
            "could not allocate input register for constraint 'r'",
        ] {
            assert!(is_asm_error(message), "{message}");
        }
        for message in [
            "cannot write `main.o`: permission denied",
            "LLVM cannot create a machine for `x`",
            "the module is broken",
        ] {
            assert!(!is_asm_error(message), "{message}");
        }
    }

    /// LLVM knows `r14` as `lr` and every other register by the compiler's
    /// name.
    #[test]
    fn registers_are_named_as_llvm_does() {
        assert_eq!(llvm_register(Arch::Armv7m, "r14"), "lr");
        assert_eq!(llvm_register(Arch::Armv7m, "r12"), "r12");
        assert_eq!(llvm_register(Arch::Riscv32, "x10"), "x10");
        assert_eq!(llvm_register(Arch::Riscv32, "x1"), "x1");
    }
}
