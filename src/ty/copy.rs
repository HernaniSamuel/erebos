//! Which types are copy, and the check of `#[copy]` (ADR 0094, 0096).

use crate::diag::Diagnostic;
use crate::hir::{Comptime, Expr, ExprKind, Fn, Ty, VariantDef};
use crate::query::Queries;
use crate::span::Span;

use super::Checker;

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

/// The start of a help suggesting `#[copy]` on the type that makes the
/// non-copy `ty` non-copy.
fn mark_copy(q: &Queries, ty: Ty) -> String {
    // Whatever is not copy holds a struct or an enum with payloads of the
    // program, which `#[copy]` would make copy.
    let inner = culprit(q, ty).unwrap_or(ty);
    format!("mark `{inner}` with `#[copy]` if its values may be duplicated")
}

/// The help of a use that would duplicate a value of the non-copy `ty`
/// (ADR 0097): `#[copy]` on `ty` if it is a struct or an enum with payloads of
/// the program whose fields are all copy, so that the attribute would be
/// accepted; `None` for any other type.
pub fn suggest_copy(q: &mut Queries, ty: Ty) -> Option<String> {
    let hir = q.hir;
    let fields: Vec<Ty> = match ty {
        Ty::Struct(s) => hir.struct_(s.id).fields.iter().map(|f| f.ty).collect(),
        Ty::Enum(e) if e.payloads && q.builtin_enum(e.id).is_none() => {
            payload_types(&q.enum_def(e.id).variants).collect()
        }
        _ => return None,
    };
    let all_copy = fields.into_iter().all(|field| {
        let field = q.normalize(field);
        is_copy(q, field)
    });
    all_copy.then(|| format!("mark `{ty}` with `#[copy]` if its values may be duplicated"))
}

impl Checker<'_, '_> {
    /// A `const` of a non-copy type: its value would be evaluated at each
    /// use, which forges what the type stands for (ADR 0096). `f` is the
    /// body of the constant, whose return type is the constant's type; a
    /// static's initializer is not checked.
    pub(super) fn check_constant_copy(&mut self, f: &Fn) {
        if !matches!(f.comptime, Some(Comptime::Const(_))) || is_copy(self.q, self.ret) {
            return;
        }
        let ty = self.ret;
        let help = format!(
            "{}, or declare a `static`, whose initializer may be of any type (ADR 0096)",
            mark_copy(self.q, ty)
        );
        self.q.diags.push(
            Diagnostic::error(
                "ty::non-copy-constant",
                format!("the constant `{}` is of the non-copy type `{ty}`", f.name),
            )
            .label(
                f.ret_span.unwrap_or(f.name_span),
                format!("`{ty}` is not copy"),
            )
            .help(help),
        );
    }

    /// A `comptime` expression of a non-copy type, which would leave
    /// evaluation as a constant to be copied (ADR 0096). What its operand
    /// does while it runs is not checked.
    pub(super) fn check_comptime_copy(&mut self, expr: &Expr, ty: Ty) {
        if is_copy(self.q, ty) {
            return;
        }
        let help = format!(
            "{}, or build the value at run time (ADR 0096)",
            mark_copy(self.q, ty)
        );
        self.q.diags.push(
            Diagnostic::error(
                "ty::non-copy-constant",
                format!("a `comptime` expression cannot be of the non-copy type `{ty}`"),
            )
            .label(expr.span, format!("`{ty}` is not copy"))
            .help(help),
        );
    }

    /// `[v; N]` with `v` of a non-copy type, whatever `N` is, unless `v` is a
    /// variant written without payload values, which holds no value to
    /// duplicate (ADR 0096).
    pub(super) fn check_repeat_copy(&mut self, value: &Expr, ty: Ty) {
        if is_copy(self.q, ty) {
            return;
        }
        if let ExprKind::Variant(_, _, args) | ExprKind::ContextVariant(_, _, args) = &value.kind
            && args.as_ref().is_none_or(Vec::is_empty)
        {
            return;
        }
        let help = format!(
            "{}, or list each element in an array literal (ADR 0096)",
            mark_copy(self.q, ty)
        );
        self.q.diags.push(
            Diagnostic::error(
                "ty::non-copy-repeat",
                format!("`[v; N]` would copy a value of the non-copy type `{ty}`"),
            )
            .label(value.span, format!("`{ty}` is not copy"))
            .help(help),
        );
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
    let help = format!(
        "{}, or remove `#[copy]` from `{name}` (ADR 0096)",
        mark_copy(q, ty)
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
