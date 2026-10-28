//! The `--emit=typeck` dump.

use std::fmt::Write;

use crate::hir::{self, EnumId, Expr, ExprKind, Fn, FnKind, ReadEffect, Ty, WriteEffect};
use crate::layout::PAST_ADDRESS_SPACE;
use crate::query::Queries;

/// Lists the memory map with its values (ADR 0057), peripheral types with
/// their layouts and their registers' read effects and alternate groups
/// (ADR 0058, 0060, 0061) and peripherals with their addresses (ADR 0058),
/// struct layouts, bit-field structs with their fields' bits, effects and
/// defaults (ADR 0059), enums with their values and, with payloads, their
/// layouts, `Option(T)` and `Result(T, E)` among them, then every
/// function's locals and expression types, for `--emit=typeck`.
pub fn dump(q: &mut Queries) -> String {
    let hir = q.hir;
    let mut out = String::new();
    // Left out if a value failed, which was reported.
    if let Some(memory) = q.memory_map() {
        write!(out, "{memory}").unwrap();
    }
    // Those reaching a failed value, reported, and `core`'s are left out.
    for (i, def) in hir.peripheral_types.iter().enumerate() {
        if hir.in_core(def.module) {
            continue;
        }
        let Some(layout) = q.peripheral_layout(hir::PeripheralTypeId(i as u32)) else {
            continue;
        };
        let registers: Vec<String> = (def.entries.iter())
            .zip(layout.offsets.iter().zip(&layout.arrays))
            .map(|(register, (offset, array))| {
                let ty = q.normalize(register.ty);
                let vis = match register.vis {
                    hir::Visibility::Pub => "pub ",
                    hir::Visibility::Private => "",
                };
                // A cluster has neither (ADR 0064).
                let access = register
                    .access()
                    .map_or(String::new(), |access| format!("{} ", access.words()));
                let effect = match register.read_effect() {
                    Some((effect, _)) => format!(" {}", read_effect_words(effect)),
                    None => String::new(),
                };
                let group = match &register.group {
                    Some((group, _)) => format!(" in {group}"),
                    None => String::new(),
                };
                // An array's elements are a step apart (ADR 0064).
                let step = match array {
                    Some((_, step)) => format!(" step {step:#x}"),
                    None => String::new(),
                };
                format!(
                    "{vis}{access}{}: {ty} at {offset:#x}{step}{effect}{group}",
                    register.name
                )
            })
            .collect();
        let registers = if registers.is_empty() {
            "{}".to_owned()
        } else {
            format!("{{ {} }}", registers.join(", "))
        };
        // An extent past 2³² is saturated (ADR 0064).
        let extent = if layout.extent >= PAST_ADDRESS_SPACE {
            "past 2³²".to_owned()
        } else {
            layout.extent.to_string()
        };
        writeln!(
            out,
            "peripheral {} {registers} extent {extent}, align {}",
            def.path, layout.align
        )
        .unwrap();
    }
    for (i, def) in hir.peripherals.iter().enumerate() {
        if hir.in_core(def.module) {
            continue;
        }
        let Some((address, layout)) = q.placed_peripheral(hir::PeripheralId(i as u32)) else {
            continue;
        };
        writeln!(
            out,
            "peripheral {}: {} at {address:#x} covers [{address:#x}, {:#x})",
            def.path,
            def.ty,
            address + layout.extent
        )
        .unwrap();
    }
    // `core` is left out (ADR 0052).
    for (i, def) in hir.structs.iter().enumerate() {
        if hir.in_core(def.module) {
            continue;
        }
        let Some(layout) = q.struct_layout(hir::StructId(i as u32)) else {
            continue;
        };
        let fields: Vec<String> = def
            .fields
            .iter()
            .zip(&layout.offsets)
            .map(|(field, offset)| {
                let ty = q.normalize(field.ty);
                format!("{}: {ty} at {offset}", field.name)
            })
            .collect();
        let fields = if fields.is_empty() {
            "{}".to_owned()
        } else {
            format!("{{ {} }}", fields.join(", "))
        };
        let attrs = match (def.packed, def.align) {
            (true, _) => "#[packed] ".to_owned(),
            (false, Some((align, _))) => match q.array_len(align) {
                Some(align) => format!("#[align({align})] "),
                None => "#[align(?)] ".to_owned(),
            },
            (false, None) => String::new(),
        };
        writeln!(
            out,
            "{attrs}struct {} {fields} size {}, align {}",
            def.name, layout.layout.size, layout.layout.align
        )
        .unwrap();
    }
    // Those reaching a failed value, reported, and `core`'s are left out.
    for (i, def) in hir.bit_structs.iter().enumerate() {
        if hir.in_core(def.module) {
            continue;
        }
        let id = hir::BitStructId(i as u32);
        let Some(layout) = q.bit_layout(id).filter(|layout| !layout.failed()) else {
            continue;
        };
        let fields: Vec<String> = (def.fields.iter().zip(&layout.fields))
            .enumerate()
            .map(|(index, (field, &bits))| {
                let (low, high) = bits.expect("a struct that did not fail has every field's bits");
                let ty = q.normalize(field.ty);
                let mut out = if low == high {
                    format!("{}: {ty} at {low}", field.name)
                } else {
                    format!("{}: {ty} at {low}..={high}", field.name)
                };
                if let Some((effect, _)) = field.read_effect {
                    write!(out, " {}", read_effect_words(effect)).unwrap();
                }
                if let Some((effect, _)) = field.write_effect {
                    write!(out, " {}", write_effect_words(effect)).unwrap();
                }
                if field.default.is_some() {
                    let value = q.bit_default(id, index as u32);
                    write!(out, " = {}", default_text(q, ty, value)).unwrap();
                }
                out
            })
            .collect();
        writeln!(
            out,
            "struct {}: {} {{ {} }}",
            def.path,
            def.ty.int,
            fields.join(", ")
        )
        .unwrap();
    }
    // The declared enums, then the `Option(T)` and `Result(T, E)` the
    // program uses, by name (ADR 0045).
    let ids: Vec<EnumId> = (0..hir.enums.len())
        .filter(|&id| !hir.in_core(hir.enums[id].module))
        .map(|id| EnumId(id as u32))
        .chain(q.instances_by_name())
        .collect();
    for id in ids {
        let def = q.enum_def(id);
        // Each payload value with its offset, and the enum's size, if it
        // has a layout (ADR 0039).
        let layout = if def.ty.payloads {
            q.enum_layout(id)
        } else {
            None
        };
        let variants: Vec<String> = def
            .variants
            .iter()
            .enumerate()
            .map(|(index, variant)| {
                let payload: Vec<String> = variant
                    .payload
                    .iter()
                    .enumerate()
                    .map(|(j, value)| {
                        let ty = q.normalize(value.ty);
                        match &layout {
                            Some(layout) => format!("{ty} at {}", layout.offsets[index][j]),
                            None => ty.to_string(),
                        }
                    })
                    .collect();
                let payload = if payload.is_empty() {
                    String::new()
                } else {
                    format!("({})", payload.join(", "))
                };
                match q.discriminant(id, index as u32) {
                    Some(value) => format!(
                        "{}{payload} = {}",
                        variant.name,
                        def.ty.int.display_bits(value)
                    ),
                    None => format!("{}{payload} = ?", variant.name),
                }
            })
            .collect();
        let size = match &layout {
            Some(layout) => format!(
                " size {}, align {}",
                layout.layout.size, layout.layout.align
            ),
            None => String::new(),
        };
        writeln!(
            out,
            "enum {}: {} {{ {} }}{size}",
            def.name,
            def.ty.int,
            variants.join(", ")
        )
        .unwrap();
    }
    for (_, f) in hir.fns().filter(|(_, f)| !hir.in_core(f.module)) {
        let kind = match f.kind {
            FnKind::Plain => "",
            FnKind::Entry => "entry ",
            FnKind::Panic => "panic ",
        };
        let params: Vec<String> = f
            .params()
            .iter()
            .map(|p| format!("{}: {}", p.name, q.normalize(p.ty)))
            .collect();
        let external = if f.body.is_none() { "extern " } else { "" };
        let external = if f.comptime.is_some() {
            "comptime "
        } else {
            external
        };
        writeln!(
            out,
            "{external}{kind}fn {}({}) -> {}",
            f.path,
            params.join(", "),
            q.normalize(f.ret)
        )
        .unwrap();
        for local in &f.locals[f.param_count..] {
            let mutable = if local.mutable { "mut " } else { "" };
            let ty = q.normalize(local.ty);
            writeln!(out, "    let {mutable}{}: {ty}", local.name).unwrap();
        }
        let Some(body) = &f.body else {
            continue;
        };
        body.walk(&mut |e| {
            let ty = q.tables.expr_ty(e.id);
            let converted = match q.tables.coercion(e.id) {
                Some(to) => format!(" as {to}"),
                None => String::new(),
            };
            writeln!(
                out,
                "    expr {} {} {:?}: {ty}{converted}",
                e.id.0,
                expr_label(q, f, e),
                e.span
            )
            .unwrap();
        });
    }
    out
}

/// A read effect as written (ADR 0060): "read clears".
fn read_effect_words(effect: ReadEffect) -> &'static str {
    match effect {
        ReadEffect::Clears => "read clears",
        ReadEffect::Sets => "read sets",
        ReadEffect::Modifies => "read modifies",
    }
}

/// A write effect as written (ADR 0060): "write 1 clears".
fn write_effect_words(effect: WriteEffect) -> String {
    format!("write {} {}", effect.bit, effect.action.word())
}

/// The default `value` of a bit field of type `ty` (ADR 0059): `true`, an
/// integer, or the variant it is, as `Mode::Run`; `?` if it failed, which
/// was reported.
fn default_text(q: &mut Queries, ty: Ty, value: Option<u64>) -> String {
    let Some(value) = value else {
        return "?".to_owned();
    };
    match ty {
        Ty::Bool => (value != 0).to_string(),
        Ty::Enum(e) => {
            let def = q.enum_def(e.id);
            (0..def.variants.len())
                .find(|&index| q.discriminant(e.id, index as u32) == Some(value))
                .map_or_else(
                    || e.int.display_bits(value),
                    |index| def.variant_path(index),
                )
        }
        Ty::Int(int) => int.display_bits(value),
        _ => value.to_string(),
    }
}

fn expr_label(q: &Queries, f: &Fn, expr: &Expr) -> String {
    let hir = q.hir;
    match &expr.kind {
        ExprKind::Bool(value) => format!("bool {value}"),
        ExprKind::Logical(op, _, _) => format!("logical {}", op.symbol()),
        ExprKind::If { .. } => "if".to_owned(),
        ExprKind::While { .. } => "while".to_owned(),
        ExprKind::Break { .. } => "break".to_owned(),
        ExprKind::Continue { .. } => "continue".to_owned(),
        ExprKind::Return(_) => "return".to_owned(),
        ExprKind::Int(value) => format!("int {value}"),
        ExprKind::Byte(value) => format!("byte {value}"),
        ExprKind::ByteString(bytes) => format!("byte string b\"{}\"", bytes.escape_ascii()),
        ExprKind::Unit => "unit".to_owned(),
        ExprKind::Const(id) => format!("const {}", hir.const_(*id).path),
        ExprKind::Static(id) => format!("static {}", hir.static_(*id).path),
        ExprKind::Peripheral(id) => format!("peripheral {}", hir.peripheral(*id).path),
        ExprKind::Variant(id, index, _) => {
            format!("variant {}", q.enum_def(*id).variant_path(*index as usize))
        }
        // With the variant it names, once resolved (ADR 0044).
        ExprKind::ContextVariant(name, _, _) => match q.tables.variants.get(&expr.id) {
            Some(&(id, index)) => {
                let def = q.enum_def(id);
                let variant = &def.variants[index as usize].name;
                format!("variant .{name} ({}::{variant})", def.name)
            }
            None => format!("variant .{name}"),
        },
        ExprKind::Comptime(_) => "comptime".to_owned(),
        ExprKind::Array(_) => "array".to_owned(),
        ExprKind::StructLit(s, _) => format!("struct {}", s.name),
        ExprKind::BitStructLit(s, _, None) => format!("struct {}", s.name),
        ExprKind::BitStructLit(s, _, Some(_)) => format!("struct {} ..", s.name),
        // A register when the base is of a peripheral type (ADR 0058), a
        // bit field when it is of a bit-field struct type (ADR 0059).
        ExprKind::Field(base, name, _) => match q.tables.expr_ty(base.id) {
            Ty::Peripheral(_) => format!("register {name}"),
            _ if q.tables.bit_field(expr.id).is_some() => format!("bit field {name}"),
            _ => format!("field {name}"),
        },
        ExprKind::SizeOf(ty) => format!("size_of({ty})"),
        ExprKind::AlignOf(ty) => format!("align_of({ty})"),
        ExprKind::OffsetOf(ty, name, _) => format!("offset_of({ty}, {name})"),
        ExprKind::Repeat(..) => "repeat".to_owned(),
        ExprKind::Local(id) => format!("local {}", f.local(*id).name),
        ExprKind::Call { .. } => "call".to_owned(),
        ExprKind::Unary(op, _) => format!("{op:?}").to_lowercase(),
        ExprKind::Deref(_) => "deref".to_owned(),
        ExprKind::Index(..) => "index".to_owned(),
        ExprKind::Try(..) => "try".to_owned(),
        ExprKind::AddrOf { mutable: true, .. } => "&raw mut".to_owned(),
        ExprKind::AddrOf { mutable: false, .. } => "&raw const".to_owned(),
        ExprKind::Binary(op, _, _, _) => format!("binary {}", op.symbol()),
        ExprKind::Cast(_, ty, _) => format!("as {ty}"),
        ExprKind::Assign(..) => "assign".to_owned(),
        ExprKind::CompoundAssign(op, _, _, _) => format!("assign {}=", op.symbol()),
        ExprKind::Block(_) => "block".to_owned(),
        ExprKind::Assert(..) => "assert".to_owned(),
        ExprKind::Len(_) => "len".to_owned(),
        ExprKind::Slice(..) => "slice".to_owned(),
        ExprKind::Loop(_) => "loop".to_owned(),
        ExprKind::Match { .. } => "match".to_owned(),
    }
}
