//! The vector table and the default handlers (ADR 0085): the part of startup
//! that depends on the program's handlers. Everything else the machine runs
//! when it takes a vector is `core`'s, written in Erebos.

use std::fmt::Write as _;

use inkwell::attributes::AttributeLoc;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::values::{BasicMetadataValueEnum, FunctionValue, PointerValue};

use super::add_attribute;
use crate::mir::{DefaultHandler, Mir, VectorTable, VectorWord};
use crate::target::Arch;

/// Emits the table as module assembly, in a section of its own that the
/// linker script places, and defines the default handlers it names; returns
/// them, for `keep_symbols`. Nothing without a table (ADR 0085 leaves it out
/// of a dump of a program with no handler).
pub(super) fn lower<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    mir: &Mir,
    arch: Arch,
    fns: &[Option<FunctionValue<'ctx>>],
    data: &[PointerValue<'ctx>],
) -> Vec<FunctionValue<'ctx>> {
    let Some(table) = &mir.vector_table else {
        return Vec::new();
    };
    let Some(header) = header(arch) else {
        return Vec::new();
    };
    let defaults = (table.defaults.iter())
        .map(|default| default_handler(context, module, mir, fns, data, default))
        .collect();
    module.set_inline_assembly(&text(header, mir, table));
    defaults
}

/// What starts the table's assembly on `arch`: its section and alignment.
/// ARMv7-M's table is the section `.vector_table`, which the linker script
/// places first in the code's region, where the core reads it (ADR 0074,
/// 0083). riscv32's is not generated yet.
fn header(arch: Arch) -> Option<&'static str> {
    match arch {
        Arch::Riscv32 => None,
        Arch::Armv7m => Some("\t.section .vector_table, \"a\", %progbits\n\t.p2align 2\n"),
    }
}

/// The module assembly: one `.word` per word of the table, in order. The
/// linker gives bit 0 to the address of a Thumb function, as `bx` needs.
fn text(header: &str, mir: &Mir, table: &VectorTable) -> String {
    let mut text = header.to_owned();
    for word in &table.words {
        let value = match *word {
            VectorWord::StackEnd => "__stack_end",
            VectorWord::Reset => "_start",
            VectorWord::Reserved => "0",
            VectorWord::Handler(body) => &mir.body(body).name,
            VectorWord::Default(index) => &table.defaults[index].symbol,
        };
        // Writing to a `String` cannot fail.
        let _ = writeln!(text, "\t.word {value}");
    }
    text
}

/// `void symbol()`: calls the panic function with the default's information,
/// if the panic function takes it, as a failed check does (ADR 0053), and
/// never returns.
fn default_handler<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    mir: &Mir,
    fns: &[Option<FunctionValue<'ctx>>],
    data: &[PointerValue<'ctx>],
    default: &DefaultHandler,
) -> FunctionValue<'ctx> {
    let function = module.add_function(
        &default.symbol,
        context.void_type().fn_type(&[], false),
        None,
    );
    // As the functions of the program: no unwinding on bare metal (ADR 0002),
    // address 0 is an ordinary address (ADR 0019), and it never returns.
    add_attribute(context, function, AttributeLoc::Function, "nounwind");
    add_attribute(
        context,
        function,
        AttributeLoc::Function,
        "null_pointer_is_valid",
    );
    add_attribute(context, function, AttributeLoc::Function, "noreturn");

    let builder = context.create_builder();
    builder.position_at_end(context.append_basic_block(function, "bb0"));
    let panic = fns[mir.panic.0 as usize].expect("the panic function is part of the program");
    let args: Vec<BasicMetadataValueEnum> = (default.info.iter())
        .map(|info| data[info.0 as usize].into())
        .collect();
    builder
        .build_call(panic, &args, "")
        .expect("builder is positioned");
    builder.build_unreachable().expect("builder is positioned");
    function
}
