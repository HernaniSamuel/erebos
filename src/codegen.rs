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
    BinOp, Body, Const, IntTy, Local, Mir, Operand, Place, PlaceElem, Ptr, Rvalue, Statement,
    Terminator, Ty, UnOp,
};
use crate::target::Target;

/// Returns the textual LLVM IR, for `--emit=llvm-ir`.
/// How much LLVM optimizes. Without optimization, locals are still promoted
/// to registers (`mem2reg`), which keeps the IR readable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OptLevel {
    #[default]
    None,
    /// LLVM's `-O2` pipeline.
    Speed,
}

impl OptLevel {
    fn passes(self) -> &'static str {
        match self {
            OptLevel::None => "mem2reg",
            OptLevel::Speed => "default<O2>",
        }
    }

    fn backend(self) -> OptimizationLevel {
        match self {
            OptLevel::None => OptimizationLevel::None,
            OptLevel::Speed => OptimizationLevel::Default,
        }
    }
}

pub fn emit_llvm_ir(mir: &Mir, target: &Target, opt: OptLevel) -> Result<String, String> {
    let machine = target_machine(target, opt)?;
    let context = Context::create();
    let module = lower(&context, mir, target, &machine, opt);
    Ok(module.print_to_string().to_string())
}

pub fn emit_object(mir: &Mir, target: &Target, opt: OptLevel, path: &Path) -> Result<(), String> {
    let machine = target_machine(target, opt)?;
    let context = Context::create();
    let module = lower(&context, mir, target, &machine, opt);
    machine
        .write_to_file(&module, FileType::Object, path)
        .map_err(|e| format!("cannot write `{}`: {e}", path.display()))
}

fn target_machine(target: &Target, opt: OptLevel) -> Result<TargetMachine, String> {
    LlvmTarget::initialize_riscv(&InitializationConfig::default());
    let triple = TargetTriple::create(target.llvm_triple);
    let llvm_target = LlvmTarget::from_triple(&triple).map_err(|e| e.to_string())?;
    llvm_target
        .create_target_machine(
            &triple,
            target.llvm_cpu,
            target.llvm_features,
            opt.backend(),
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
    opt: OptLevel,
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
        // An `extern fn` stays a declaration for the linker to resolve.
        if !body.external {
            FnCodegen::new(context, body, function, &fns).define();
        }
    }

    module.set_inline_assembly(&start_stub(target, &mir.body(mir.entry).name));

    if let Err(e) = module.verify() {
        panic!("codegen produced invalid LLVM IR: {}", e.to_string());
    }
    // Locals start as stack slots; every level promotes them to registers.
    module
        .run_passes(opt.passes(), machine, PassBuilderOptions::create())
        .expect("the pass pipelines are valid");
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

/// The LLVM type of a MIR value, as held in registers; `()` and `!` have no
/// representation, and aggregates live only in memory.
fn basic_type(context: &Context, ty: Ty) -> Option<BasicTypeEnum<'_>> {
    match ty {
        Ty::Bool => Some(context.bool_type().into()),
        Ty::Int(int) => Some(int_type(context, int).into()),
        Ty::Ptr(_) => Some(context.ptr_type(AddressSpace::default()).into()),
        Ty::Unit | Ty::Never | Ty::Array(..) | Ty::Struct(_) => None,
    }
}

/// The LLVM type of a MIR value in memory. A `bool` occupies a byte
/// (ADR 0022), unlike in registers.
fn memory_type(context: &Context, ty: Ty) -> Option<BasicTypeEnum<'_>> {
    match ty {
        Ty::Bool => Some(context.i8_type().into()),
        Ty::Array(elem, len) => {
            let len = u32::try_from(len).expect("the type checker limits sizes");
            Some(element_type(context, *elem).array_type(len).into())
        }
        // Bytes laid out by MIR (ADR 0022); fields are reached by offset.
        Ty::Struct(s) => {
            let size = u32::try_from(s.def().layout.size).expect("the type checker limits sizes");
            Some(context.i8_type().array_type(size).into())
        }
        _ => basic_type(context, ty),
    }
}

/// The type whose size is the distance between consecutive elements of
/// type `ty` in memory.
fn element_type(context: &Context, ty: Ty) -> BasicTypeEnum<'_> {
    memory_type(context, ty).unwrap_or_else(|| context.i8_type().array_type(0).into())
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
    // What clang's `-fno-builtin` sets. LLVM replaces copy and fill loops
    // with calls to `memcpy` and `memset`; it spares functions with those
    // two names, but relying on that heuristic for the whole runtime would
    // leave the others free to call into themselves.
    if body.runtime {
        function.add_attribute(
            AttributeLoc::Function,
            context.create_string_attribute("no-builtins", ""),
        );
    }
    // Address 0 is an ordinary address on bare metal (ADR 0019); without
    // this, LLVM treats accesses to it as undefined and may delete them.
    add_attribute(
        context,
        function,
        AttributeLoc::Function,
        "null_pointer_is_valid",
    );
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

/// Where a place is in memory, and how it may be accessed.
#[derive(Clone, Copy)]
struct Address<'ctx> {
    ptr: PointerValue<'ctx>,
    /// The last pointer on the path to the place is volatile (ADR 0023).
    volatile: bool,
    /// The path from that pointer goes through a field of a `#[packed]`
    /// struct, so the place may be misaligned (ADR 0022).
    misaligned: bool,
}

impl Address<'_> {
    /// The alignment to promise LLVM for an access of type `ty`.
    fn align(self, ty: Ty) -> u32 {
        let align = if self.misaligned {
            1
        } else {
            ty.layout().align
        };
        u32::try_from(align).expect("alignments are small")
    }
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
    /// Locals whose address is taken, so that their slot is memory others
    /// can access, in the memory representation (ADR 0022).
    in_memory: Vec<bool>,
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
        let mut in_memory = vec![false; body.locals.len()];
        for (_, data) in body.blocks() {
            for statement in &data.statements {
                if let Statement::Assign(_, Rvalue::AddressOf { place, .. }) = statement {
                    in_memory[place.local.0 as usize] = true;
                }
            }
        }
        FnCodegen {
            context,
            body,
            function,
            fns,
            builder: context.create_builder(),
            blocks,
            slots: Vec::new(),
            in_memory,
        }
    }

    fn define(mut self) {
        // Stack slots go first in the start block, which nothing jumps to.
        self.builder.position_at_end(self.blocks[0]);
        self.slots = self
            .body
            .locals()
            .map(|(local, _)| {
                self.representation(Place::local(local)).map(|ty| {
                    let slot = self
                        .builder
                        .build_alloca(ty, &local.to_string())
                        .expect("builder is positioned");
                    // A struct is an array of bytes to LLVM, which would
                    // otherwise align it to 1.
                    let align = self.body.local_ty(local).layout().align;
                    slot.as_instruction_value()
                        .expect("an alloca is an instruction")
                        .set_alignment(u32::try_from(align).expect("alignments are small"))
                        .expect("the alignment is a power of two");
                    slot
                })
            })
            .collect();
        // Parameters arrive in registers; give them their slots `_1..`.
        for i in 0..self.body.param_count {
            let value = self
                .function
                .get_nth_param(i as u32)
                .expect("declared with this many parameters");
            self.write(Place::local(Local(i as u32 + 1)), value);
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
                if self.representation(*place).is_none() {
                    return;
                }
                if self.body.place_ty(*place).is_aggregate() {
                    let Rvalue::Use(Operand::Copy(source)) = rvalue else {
                        unreachable!("aggregates are only copied")
                    };
                    self.copy(*place, *source);
                    return;
                }
                let value = self.rvalue(rvalue);
                self.write(*place, value);
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
                    self.write(*dest, value);
                }
            }
        }
    }

    /// The address of a place. A place is volatile when the last pointer on
    /// its path is (ADR 0023).
    fn address(&self, place: Place) -> Address<'ctx> {
        let Some(slot) = self.slots[place.local.0 as usize] else {
            // A local of type `()` occupies no memory; its address is any
            // aligned one (ADR 0022).
            let one = int_type(self.context, IntTy::Usize).const_int(1, false);
            return Address {
                ptr: one.const_to_pointer(self.context.ptr_type(AddressSpace::default())),
                volatile: false,
                misaligned: false,
            };
        };
        let mut address = Address {
            ptr: slot,
            volatile: false,
            misaligned: false,
        };
        let mut ty = self.body.local_ty(place.local);
        let usize_type = int_type(self.context, IntTy::Usize);
        let byte_type = self.context.i8_type().into();
        for elem in place.projection {
            let (step, index) = match (elem, ty) {
                (PlaceElem::Deref, Ty::Ptr(ptr)) => {
                    address = self.through(address, ptr);
                    ty = *ptr.pointee;
                    continue;
                }
                (PlaceElem::Offset(index), Ty::Ptr(ptr)) => {
                    address = self.through(address, ptr);
                    ty = *ptr.pointee;
                    let index = self.read(Place::local(*index)).into_int_value();
                    (element_type(self.context, ty), index)
                }
                (PlaceElem::Index(index), Ty::Array(elem, _)) => {
                    ty = *elem;
                    let index = self.read(Place::local(*index)).into_int_value();
                    (element_type(self.context, ty), index)
                }
                (PlaceElem::ConstIndex(index), Ty::Array(elem, _)) => {
                    ty = *elem;
                    (
                        element_type(self.context, ty),
                        usize_type.const_int(*index, false),
                    )
                }
                (PlaceElem::Field(index), Ty::Struct(s)) => {
                    let field = &s.def().fields()[*index as usize];
                    ty = field.ty;
                    address.misaligned |= s.def().packed;
                    (byte_type, usize_type.const_int(field.offset, false))
                }
                (elem, ty) => unreachable!("projection {elem:?} of `{ty}`"),
            };
            // Not `inbounds`: the arithmetic wraps and is never undefined
            // (ADR 0021).
            // SAFETY: the element type is sized and the index is an integer,
            // as LLVM requires of a `getelementptr`.
            let element = unsafe { self.builder.build_gep(step, address.ptr, &[index], "") }
                .expect("builder is positioned");
            address.ptr = element;
        }
        address
    }

    /// The address a pointer stored at `address` points to. Pointers are
    /// assumed aligned (ADR 0019).
    fn through(&self, address: Address<'ctx>, ptr: Ptr) -> Address<'ctx> {
        let load = self
            .builder
            .build_load(
                self.context.ptr_type(AddressSpace::default()),
                address.ptr,
                "",
            )
            .expect("builder is positioned");
        self.set_access(load, address, Ty::Ptr(ptr));
        Address {
            ptr: load.into_pointer_value(),
            volatile: ptr.volatile,
            misaligned: false,
        }
    }

    /// Makes a load or store as volatile and as aligned as its place.
    fn set_access(&self, access: BasicValueEnum<'ctx>, address: Address<'ctx>, ty: Ty) {
        let instruction = access
            .as_instruction_value()
            .expect("loads and stores are instructions");
        if address.volatile {
            instruction
                .set_volatile(true)
                .expect("loads and stores can be volatile");
        }
        instruction
            .set_alignment(address.align(ty))
            .expect("loads and stores have an alignment");
    }

    /// Copies an aggregate. Places that may overlap (reached through
    /// pointers, or a local whose address is taken) use `memmove`, which
    /// LLVM turns into `memcpy` where it can tell they do not.
    fn copy(&self, dest: Place, source: Place) {
        let layout = self.body.place_ty(dest).layout();
        if layout.size == 0 {
            return;
        }
        let may_overlap = [dest, source]
            .iter()
            .any(|place| place.is_indirect() || self.in_memory[place.local.0 as usize]);
        let ty = self.body.place_ty(dest);
        let to = self.address(dest);
        let from = self.address(source);
        let size = int_type(self.context, IntTy::Usize).const_int(layout.size, false);
        let (to_align, from_align) = (to.align(ty), from.align(ty));
        let copy = if may_overlap {
            self.builder
                .build_memmove(to.ptr, to_align, from.ptr, from_align, size)
        } else {
            self.builder
                .build_memcpy(to.ptr, to_align, from.ptr, from_align, size)
        };
        copy.expect("the alignment is a power of two");
    }

    /// Locals are private to the function, so they hold values as in
    /// registers; memory reached through a pointer, or that a pointer may
    /// reach, uses the memory representation (ADR 0022).
    fn representation(&self, place: Place) -> Option<BasicTypeEnum<'ctx>> {
        let ty = self.body.place_ty(place);
        // A projection reaches into memory: through a pointer, or into an
        // aggregate, which lives in memory.
        if ty.is_aggregate()
            || !place.projection.is_empty()
            || self.in_memory[place.local.0 as usize]
        {
            memory_type(self.context, ty)
        } else {
            basic_type(self.context, ty)
        }
    }

    fn read(&self, place: Place) -> BasicValueEnum<'ctx> {
        let memory = self.representation(place).expect("read of `()`");
        let address = self.address(place);
        let value = self
            .builder
            .build_load(memory, address.ptr, "")
            .expect("builder is positioned");
        self.set_access(value, address, self.body.place_ty(place));
        if memory != self.context.i8_type().into() || self.body.place_ty(place) != Ty::Bool {
            return value;
        }
        // Any nonzero byte is `true`, so every byte is a valid `bool` in
        // memory, wherever it came from (ADR 0022).
        let zero = self.context.i8_type().const_zero();
        self.builder
            .build_int_compare(IntPredicate::NE, value.into_int_value(), zero, "")
            .expect("builder is positioned")
            .into()
    }

    /// Writes a value in the place's representation: a `bool` in memory
    /// becomes a byte holding 0 or 1.
    fn write(&self, place: Place, value: BasicValueEnum<'ctx>) {
        let address = self.address(place);
        let value = match (self.representation(place), value) {
            (Some(BasicTypeEnum::IntType(memory)), BasicValueEnum::IntValue(int))
                if memory != int.get_type() =>
            {
                self.builder
                    .build_int_z_extend(int, memory, "")
                    .expect("builder is positioned")
                    .into()
            }
            (_, value) => value,
        };
        let store = self
            .builder
            .build_store(address.ptr, value)
            .expect("builder is positioned");
        if address.volatile {
            store.set_volatile(true).expect("stores can be volatile");
        }
        store
            .set_alignment(address.align(self.body.place_ty(place)))
            .expect("stores have an alignment");
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
                    // MIR guarantees a nonzero divisor and no `MIN / -1`
                    // (ADR 0007), where LLVM's division is undefined.
                    BinOp::Div | BinOp::Rem => {
                        let signed = self.int_of(lhs).is_signed();
                        match (op, signed) {
                            (BinOp::Div, true) => b.build_int_signed_div(l, r, ""),
                            (BinOp::Div, false) => b.build_int_unsigned_div(l, r, ""),
                            (_, true) => b.build_int_signed_rem(l, r, ""),
                            (_, false) => b.build_int_unsigned_rem(l, r, ""),
                        }
                    }
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
            Rvalue::AddressOf { place, .. } => return self.address(*place).ptr.into(),
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
