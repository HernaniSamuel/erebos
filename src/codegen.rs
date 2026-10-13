//! MIR -> LLVM IR -> object file, through inkwell (ADR 0005).

use std::path::Path;

use inkwell::AddressSpace;
use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::basic_block::BasicBlock as LlvmBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target as LlvmTarget, TargetMachine,
    TargetTriple,
};
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum, IntType};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValue, BasicValueEnum, FunctionValue, IntValue, PointerValue,
    ValueKind,
};
use inkwell::{IntPredicate, OptimizationLevel};

use crate::mir::{
    BinOp, Body, Const, IntTy, Local, Mir, Operand, Place, Rvalue, Statement, Terminator, Ty, UnOp,
};
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
        FnCodegen::new(context, body, function, &fns).define();
    }

    module.set_inline_assembly(&start_stub(target, &mir.body(mir.entry).name));

    if let Err(e) = module.verify() {
        panic!("codegen produced invalid LLVM IR: {}", e.to_string());
    }
    // Locals start as stack slots; promote them to registers.
    module
        .run_passes("mem2reg", machine, PassBuilderOptions::create())
        .expect("mem2reg is a valid pass pipeline");
    module
}

fn int_type(context: &Context, int: IntTy) -> IntType<'_> {
    match int.bits() {
        8 => context.i8_type(),
        16 => context.i16_type(),
        32 => context.i32_type(),
        64 => context.i64_type(),
        bits => unreachable!("no {bits}-bit integer type"),
    }
}

/// The LLVM type of a MIR type; `()` and `!` have no representation.
fn basic_type(context: &Context, ty: Ty) -> Option<BasicTypeEnum<'_>> {
    match ty {
        Ty::Bool => Some(context.bool_type().into()),
        Ty::Int(int) => Some(int_type(context, int).into()),
        Ty::Ptr(_) => Some(context.ptr_type(AddressSpace::default()).into()),
        Ty::Unit | Ty::Never => None,
    }
}

fn declare<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    body: &Body,
) -> FunctionValue<'ctx> {
    let params: Vec<BasicMetadataTypeEnum> = body
        .params()
        .iter()
        .map(|param| {
            basic_type(context, param.ty)
                .expect("the type checker rejects `()` and `!` parameters")
                .into()
        })
        .collect();
    let fn_type = match basic_type(context, body.ret) {
        Some(ty) => ty.fn_type(&params, false),
        // Both `()` and `!` return nothing; `!` additionally never returns.
        None => context.void_type().fn_type(&params, false),
    };
    let function = module.add_function(&body.name, fn_type, None);

    // There is no unwinding on bare metal (ADR 0002).
    add_attribute(context, function, AttributeLoc::Function, "nounwind");
    if body.ret == Ty::Never {
        add_attribute(context, function, AttributeLoc::Function, "noreturn");
    }
    // The C ABI (ADR 0006) widens narrow integers to a full register; LLVM
    // needs to be told how, as clang does.
    for (i, param) in body.params().iter().enumerate() {
        if let Some(ext) = extension(param.ty) {
            add_attribute(context, function, AttributeLoc::Param(i as u32), ext);
        }
    }
    if let Some(ext) = extension(body.ret) {
        add_attribute(context, function, AttributeLoc::Return, ext);
    }
    function
}

/// How the RISC-V psABI extends a value narrower than a register.
fn extension(ty: Ty) -> Option<&'static str> {
    match ty {
        Ty::Bool => Some("zeroext"),
        Ty::Int(int) if int.bits() < 32 && int.is_signed() => Some("signext"),
        Ty::Int(int) if int.bits() < 32 => Some("zeroext"),
        _ => None,
    }
}

fn add_attribute(context: &Context, function: FunctionValue, loc: AttributeLoc, name: &str) {
    let kind = Attribute::get_named_enum_kind_id(name);
    function.add_attribute(loc, context.create_enum_attribute(kind, 0));
}

/// Lowers one MIR body. Every local with a representation gets a stack
/// slot; values of type `()` and `!` are skipped.
struct FnCodegen<'a, 'ctx> {
    context: &'ctx Context,
    body: &'a Body,
    function: FunctionValue<'ctx>,
    /// Every function of the module, indexed by body id.
    fns: &'a [FunctionValue<'ctx>],
    builder: Builder<'ctx>,
    blocks: Vec<LlvmBlock<'ctx>>,
    slots: Vec<Option<PointerValue<'ctx>>>,
}

impl<'a, 'ctx> FnCodegen<'a, 'ctx> {
    fn new(
        context: &'ctx Context,
        body: &'a Body,
        function: FunctionValue<'ctx>,
        fns: &'a [FunctionValue<'ctx>],
    ) -> Self {
        let blocks = body
            .blocks()
            .map(|(bb, _)| context.append_basic_block(function, &bb.to_string()))
            .collect();
        FnCodegen {
            context,
            body,
            function,
            fns,
            builder: context.create_builder(),
            blocks,
            slots: Vec::new(),
        }
    }

    fn define(mut self) {
        // Stack slots go first in the start block, which nothing jumps to.
        self.builder.position_at_end(self.blocks[0]);
        self.slots = self
            .body
            .locals()
            .map(|(local, decl)| {
                basic_type(self.context, decl.ty).map(|ty| {
                    self.builder
                        .build_alloca(ty, &local.to_string())
                        .expect("builder is positioned")
                })
            })
            .collect();
        // Parameters arrive in registers; give them their slots `_1..`.
        for i in 0..self.body.param_count {
            let value = self
                .function
                .get_nth_param(i as u32)
                .expect("declared with this many parameters");
            let slot = self.slots[i + 1].expect("parameters have a representation");
            self.builder
                .build_store(slot, value)
                .expect("builder is positioned");
        }

        for (bb, data) in self.body.blocks() {
            self.builder.position_at_end(self.blocks[bb.0 as usize]);
            for statement in &data.statements {
                self.statement(statement);
            }
            self.terminator(data.terminator);
        }
    }

    fn statement(&self, statement: &Statement) {
        match statement {
            Statement::Assign(place, rvalue) => {
                // Places of type `()` hold nothing.
                if basic_type(self.context, self.body.place_ty(*place)).is_none() {
                    return;
                }
                let value = self.rvalue(rvalue);
                let (address, volatile) = self.address(*place);
                let store = self
                    .builder
                    .build_store(address, value)
                    .expect("builder is positioned");
                if volatile {
                    store.set_volatile(true).expect("stores can be volatile");
                }
            }
            Statement::Call { dest, callee, args } => {
                let args: Vec<BasicMetadataValueEnum> =
                    args.iter().map(|arg| self.operand(arg).into()).collect();
                let call = self
                    .builder
                    .build_call(self.fns[callee.0 as usize], &args, "")
                    .expect("builder is positioned");
                if let Some(dest) = dest {
                    let ValueKind::Basic(value) = call.try_as_basic_value() else {
                        unreachable!("calls with a destination return a value")
                    };
                    let (address, volatile) = self.address(*dest);
                    let store = self
                        .builder
                        .build_store(address, value)
                        .expect("builder is positioned");
                    if volatile {
                        store.set_volatile(true).expect("stores can be volatile");
                    }
                }
            }
        }
    }

    /// The address of a place and whether accesses to it are volatile.
    fn address(&self, place: Place) -> (PointerValue<'ctx>, bool) {
        let slot = self.slots[place.local.0 as usize].expect("place has a representation");
        if !place.deref {
            return (slot, false);
        }
        let Ty::Ptr(ptr) = self.body.local_ty(place.local) else {
            unreachable!("only pointers are dereferenced")
        };
        let pointer = self
            .builder
            .build_load(self.context.ptr_type(AddressSpace::default()), slot, "")
            .expect("builder is positioned")
            .into_pointer_value();
        (pointer, ptr.volatile)
    }

    fn read(&self, place: Place) -> BasicValueEnum<'ctx> {
        let ty = basic_type(self.context, self.body.place_ty(place)).expect("read of `()`");
        let (address, volatile) = self.address(place);
        let value = self
            .builder
            .build_load(ty, address, "")
            .expect("builder is positioned");
        if volatile {
            value
                .as_instruction_value()
                .expect("a load is an instruction")
                .set_volatile(true)
                .expect("loads can be volatile");
        }
        value
    }

    fn rvalue(&self, rvalue: &Rvalue) -> BasicValueEnum<'ctx> {
        let b = &self.builder;
        let int = |operand: &Operand| self.operand(operand).into_int_value();
        let built = match rvalue {
            Rvalue::Use(operand) => return self.operand(operand),
            Rvalue::Unary(UnOp::Neg, operand) => b.build_int_neg(int(operand), ""),
            Rvalue::Unary(UnOp::Not, operand) => b.build_not(int(operand), ""),
            Rvalue::Binary(op, lhs, rhs) if op.is_comparison() => {
                return self.compare(*op, lhs, rhs).into();
            }
            Rvalue::Binary(op, lhs, rhs) => {
                let (l, r) = (int(lhs), int(rhs));
                // Never `nsw`/`nuw`: integer arithmetic wraps (ADR 0007).
                match op {
                    BinOp::Add => b.build_int_add(l, r, ""),
                    BinOp::Sub => b.build_int_sub(l, r, ""),
                    BinOp::Mul => b.build_int_mul(l, r, ""),
                    BinOp::BitAnd => b.build_and(l, r, ""),
                    BinOp::BitOr => b.build_or(l, r, ""),
                    BinOp::BitXor => b.build_xor(l, r, ""),
                    BinOp::Shl | BinOp::Shr => {
                        let ty = self.int_of(lhs);
                        let amount = self.shift_amount(r, ty);
                        match (op, ty.is_signed()) {
                            (BinOp::Shl, _) => b.build_left_shift(l, amount, ""),
                            (_, signed) => b.build_right_shift(l, amount, signed, ""),
                        }
                    }
                    _ => unreachable!("comparisons are handled above"),
                }
            }
            Rvalue::Cast(operand, to) => return self.cast(operand, *to),
        };
        built.expect("builder is positioned").into()
    }

    /// Integers compare by their signedness; `bool` and pointers compare as
    /// unsigned values (pointers by address).
    fn compare(&self, op: BinOp, lhs: &Operand, rhs: &Operand) -> IntValue<'ctx> {
        let signed = matches!(self.body.operand_ty(lhs), Ty::Int(int) if int.is_signed());
        let predicate = match (op, signed) {
            (BinOp::Eq, _) => IntPredicate::EQ,
            (BinOp::Ne, _) => IntPredicate::NE,
            (BinOp::Lt, true) => IntPredicate::SLT,
            (BinOp::Lt, false) => IntPredicate::ULT,
            (BinOp::Le, true) => IntPredicate::SLE,
            (BinOp::Le, false) => IntPredicate::ULE,
            (BinOp::Gt, true) => IntPredicate::SGT,
            (BinOp::Gt, false) => IntPredicate::UGT,
            (BinOp::Ge, true) => IntPredicate::SGE,
            (BinOp::Ge, false) => IntPredicate::UGE,
            _ => unreachable!("`{}` is not a comparison", op.symbol()),
        };
        let as_int = |operand: &Operand| match self.operand(operand) {
            BasicValueEnum::PointerValue(ptr) => self
                .builder
                .build_ptr_to_int(ptr, int_type(self.context, IntTy::Usize), "")
                .expect("builder is positioned"),
            value => value.into_int_value(),
        };
        self.builder
            .build_int_compare(predicate, as_int(lhs), as_int(rhs), "")
            .expect("builder is positioned")
    }

    fn cast(&self, operand: &Operand, to: Ty) -> BasicValueEnum<'ctx> {
        let value = self.operand(operand);
        let b = &self.builder;
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let cast = match (self.body.operand_ty(operand), to) {
            (Ty::Int(from), Ty::Int(to)) => b
                .build_int_cast_sign_flag(
                    value.into_int_value(),
                    int_type(self.context, to),
                    from.is_signed(),
                    "",
                )
                .map(Into::into),
            // `true as u8 == 1`
            (Ty::Bool, Ty::Int(to)) => b
                .build_int_cast_sign_flag(
                    value.into_int_value(),
                    int_type(self.context, to),
                    false,
                    "",
                )
                .map(Into::into),
            // inttoptr/ptrtoint zero-extend or truncate as needed.
            (Ty::Int(_), Ty::Ptr(_)) => b
                .build_int_to_ptr(value.into_int_value(), ptr_type, "")
                .map(Into::into),
            (Ty::Ptr(_), Ty::Int(to)) => b
                .build_ptr_to_int(value.into_pointer_value(), int_type(self.context, to), "")
                .map(Into::into),
            // All pointers share one LLVM type; mutability, volatility and
            // pointee only matter to the type checker and to accesses.
            (Ty::Ptr(_), Ty::Ptr(_)) => return value,
            (from, to) => unreachable!("cast from `{from}` to `{to}`"),
        };
        cast.expect("builder is positioned")
    }

    /// Shift amounts are masked to the shifted type's width (ADR 0007), which
    /// also keeps LLVM's shifts defined.
    fn shift_amount(&self, amount: IntValue<'ctx>, shifted: IntTy) -> IntValue<'ctx> {
        let ty = int_type(self.context, shifted);
        let amount = self
            .builder
            .build_int_cast_sign_flag(amount, ty, false, "")
            .expect("builder is positioned");
        let mask = ty.const_int(u64::from(shifted.bits() - 1), false);
        self.builder
            .build_and(amount, mask, "")
            .expect("builder is positioned")
    }

    fn int_of(&self, operand: &Operand) -> IntTy {
        match self.body.operand_ty(operand) {
            Ty::Int(int) => int,
            ty => unreachable!("integer operation on `{ty}`"),
        }
    }

    fn operand(&self, operand: &Operand) -> BasicValueEnum<'ctx> {
        match operand {
            Operand::Copy(place) => self.read(*place),
            Operand::Const(Const::Int(value, int)) => {
                int_type(self.context, *int).const_int(*value, false).into()
            }
            Operand::Const(Const::Bool(value)) => self
                .context
                .bool_type()
                .const_int(u64::from(*value), false)
                .into(),
            Operand::Const(Const::Unit) => unreachable!("`()` has no value to use"),
        }
    }

    fn terminator(&self, terminator: Terminator) {
        let b = &self.builder;
        let result = match terminator {
            Terminator::Goto(target) => {
                b.build_unconditional_branch(self.blocks[target.0 as usize])
            }
            Terminator::If { cond, then, else_ } => b.build_conditional_branch(
                self.operand(&cond).into_int_value(),
                self.blocks[then.0 as usize],
                self.blocks[else_.0 as usize],
            ),
            Terminator::Return => match basic_type(self.context, self.body.ret) {
                Some(_) => {
                    let value = self.read(Place::local(Local::RETURN));
                    b.build_return(Some(&value))
                }
                None => b.build_return(None),
            },
            Terminator::Unreachable => b.build_unreachable(),
        };
        result.expect("builder is positioned");
    }
}

// Provisional startup code (ADR 0012): sets up the stack and jumps to the
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
