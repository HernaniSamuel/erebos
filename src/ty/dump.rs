//! The `--emit=typeck` dump.

use std::fmt::Write;

use crate::hir::{self, EnumId, Expr, ExprKind, Fn, FnKind, Ty};
use crate::query::Queries;

/// Lists the memory map with its values (ADR 0057), peripheral types with
/// their layouts and peripherals with their addresses (ADR 0058), struct
/// layouts, enums with their values and, with payloads, their layouts,
/// `Option(T)` and `Result(T, E)` among them, then every function's locals
/// and expression types, for `--emit=typeck`.
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
        let registers: Vec<String> = (def.registers.iter())
            .zip(&layout.offsets)
            .map(|(register, offset)| {
                let ty = q.normalize(register.ty);
                let access = register.access.words();
                format!("{access} {}: {ty} at {offset:#x}", register.name)
            })
            .collect();
        let registers = if registers.is_empty() {
            "{}".to_owned()
        } else {
            format!("{{ {} }}", registers.join(", "))
        };
        writeln!(
            out,
            "peripheral {} {registers} extent {}, align {}",
            def.path, layout.extent, layout.align
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
        // A register when the base is of a peripheral type (ADR 0058).
        ExprKind::Field(base, name, _) => match q.tables.expr_ty(base.id) {
            Ty::Peripheral(_) => format!("register {name}"),
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
