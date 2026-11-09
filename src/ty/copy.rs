//! Which types are copy, and the check of `#[copy]` (ADR 0094, 0096).

use crate::diag::Diagnostic;
use crate::hir::{Ty, VariantDef};
use crate::query::Queries;
use crate::span::Span;

/// Whether the values of `ty` are copied rather than moved (ADR 0096): an
/// integer, `bool`, `()`, an enum without payloads, a bit-field struct, a
/// raw pointer, an array of a copy type, `Option(T)` and `Result(T, E)` of
/// copy arguments, and a struct or an enum with payloads declared
/// `#[copy]`. What declares itself is not looked into: the check of
/// `#[copy]` makes the declaration true. `!`, peripheral types, `[T]` and
/// `{error}` have no values to move, and are copy here so that they report
/// nothing more.
pub fn is_copy(q: &Queries, ty: Ty) -> bool {
    match ty {
        Ty::Unit
        | Ty::Never
        | Ty::Bool
        | Ty::Int(_)
        | Ty::Ptr(_)
        | Ty::Slice(_)
        | Ty::BitStruct(_)
        | Ty::Peripheral(_)
        | Ty::Error => true,
        Ty::Array(elem, _) => is_copy(q, *elem),
        Ty::Struct(s) => q.hir.struct_(s.id).copy.is_some(),
        Ty::Applied(applied) => applied.args.iter().all(|&arg| is_copy(q, arg)),
        Ty::Enum(e) if !e.payloads => true,
        Ty::Enum(e) => {
            let def = q.enum_def(e.id);
            match q.builtin_enum(e.id) {
                Some(_) => payload_types(&def.variants).all(|ty| is_copy(q, ty)),
                None => def.copy.is_some(),
            }
        }
    }
}

fn payload_types(variants: &[VariantDef]) -> impl Iterator<Item = Ty> {
    variants
        .iter()
        .flat_map(|variant| variant.payload.iter().map(|payload| payload.ty))
}

/// The type to mark `#[copy]` so that `ty`, which is not copy, becomes copy:
/// the first struct or enum with payloads of the program that makes it
/// non-copy, skipping what is copy already (ADR 0096).
fn culprit(q: &Queries, ty: Ty) -> Option<Ty> {
    if is_copy(q, ty) {
        return None;
    }
    match ty {
        Ty::Struct(_) => Some(ty),
        Ty::Array(elem, _) => culprit(q, *elem),
        Ty::Applied(applied) => applied.args.iter().find_map(|&arg| culprit(q, arg)),
        Ty::Enum(e) if !e.payloads => None,
        Ty::Enum(e) => match q.builtin_enum(e.id) {
            Some(_) => payload_types(&q.enum_def(e.id).variants).find_map(|ty| culprit(q, ty)),
            None => Some(ty),
        },
        _ => None,
    }
}

/// Reports, once per field or payload value, the types declared `#[copy]`
/// that hold one which is not copy (ADR 0096).
pub fn report_copy_fields(q: &mut Queries) {
    let hir = q.hir;
    for def in &hir.structs {
        let Some(attr) = def.copy else {
            continue;
        };
        for field in &def.fields {
            let what = format!("the field `{}`", field.name);
            report(q, attr, &def.name, &what, field.span, field.ty);
        }
    }
    for def in &hir.enums {
        let Some(attr) = def.copy else {
            continue;
        };
        for variant in &def.variants {
            let many = variant.payload.len() > 1;
            for (index, payload) in variant.payload.iter().enumerate() {
                let value = if many {
                    format!("payload value {} of", index + 1)
                } else {
                    "the payload value of".to_owned()
                };
                let what = format!("{value} `{}::{}`", def.name, variant.name);
                let span = payload.span.unwrap_or(attr);
                report(q, attr, &def.name, &what, span, payload.ty);
            }
        }
    }
}

/// `ty::copy-field-not-copy` for the field or payload value `what`, at
/// `span`, of the type `declared` of `name`, if `ty` is not copy.
fn report(q: &mut Queries, attr: Span, name: &str, what: &str, span: Span, declared: Ty) {
    let ty = q.normalize(declared);
    if is_copy(q, ty) {
        return;
    }
    // Whatever is not copy holds a struct or an enum with payloads of the
    // program, which `#[copy]` would make copy.
    let inner = culprit(q, ty).unwrap_or(ty);
    let help = format!(
        "mark `{inner}` with `#[copy]` if its values may be duplicated, or remove `#[copy]` from `{name}` (ADR 0096)"
    );
    q.diags.push(
        Diagnostic::error(
            "ty::copy-field-not-copy",
            format!("`{name}` is `#[copy]`, but {what} is not copy"),
        )
        .label(attr, "declared copy here")
        .label(span, format!("`{ty}` is not copy"))
        .help(help),
    );
}
