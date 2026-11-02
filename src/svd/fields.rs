//! Bit-field structs and enums (ADR 0071, 0072): the fields of a register,
//! their enumerated values, effects and defaults.
//!
//! The writer plans before it writes: [`Writer::plan_type`] names the structs,
//! the enums and their variants of every peripheral type, in the order of
//! ADR 0071, because a set with `derivedFrom` may name the enum written for a
//! set of a later type; [`Writer::struct_blocks`] then writes the text.

use std::collections::HashMap;
use std::rc::Rc;

use crate::diag::Diagnostic;
use crate::span::Span;

use super::names::{self, Case, Index, Scope};
use super::over;
use super::text::{comment, hex};
use super::write::{
    INDENT, Of, Writer, describe, effect_name, elements, join, register_mode, svd_lines,
};
use super::{
    Access, Cluster, EnumNumber, EnumSet, EnumValue, Field, Item, ReadAction, Register, SetId,
    WriteEffect,
};

// ---- which sets are written as enums

/// What the survey of a set holds.
struct SetInfo<'a> {
    /// The integer type's width: 8, 16, 32 or 64.
    int_bits: u32,
    /// Whether the field has only this set.
    single: bool,
    /// Whether the set has a value that is a variant.
    has_variants: bool,
    derived_from: Option<SetId>,
    values: &'a [EnumValue],
}

/// The width of the smallest of `u8`, `u16`, `u32` and `u64` that holds
/// `width` bits (ADR 0059).
pub(super) fn int_bits(width: u32) -> u32 {
    match width {
        0..=8 => 8,
        9..=16 => 16,
        17..=32 => 32,
        _ => 64,
    }
}

/// How an enumerated value is written.
pub(super) enum Class<'a> {
    /// A variant of this name and value.
    Variant { name: &'a str, value: u64 },
    /// No `name`: no variant, nothing warned (ADR 0072).
    Anonymous,
    /// Only `isDefault`: no variant, its values are defined (ADR 0036).
    Default,
    /// Don't-care bits: left out, warned.
    DontCare,
    /// A name with nothing left for an identifier: left out, warned.
    EmptyName,
    /// A value another variant has: left out, warned. The first one's span.
    Duplicate { first: Span },
}

/// Each value of a set, classified. A value with a `value` is a variant
/// whatever its `isDefault`.
pub(super) fn classify(values: &[EnumValue]) -> Vec<Class<'_>> {
    let mut seen: HashMap<u64, Span> = HashMap::new();
    values
        .iter()
        .map(|v| {
            let Some(name) = v.name.as_deref() else {
                return Class::Anonymous;
            };
            match &v.value {
                None => Class::Default,
                Some(EnumNumber::DontCare(_)) => Class::DontCare,
                Some(EnumNumber::Value(value)) => {
                    if names::convert(name, Index::Whole, Case::Pascal).is_none() {
                        Class::EmptyName
                    } else if let Some(&first) = seen.get(value) {
                        Class::Duplicate { first }
                    } else {
                        seen.insert(*value, v.span);
                        Class::Variant {
                            name,
                            value: *value,
                        }
                    }
                }
            }
        })
        .collect()
}

/// Whether two sets state the same names and values, in the same order.
fn same_values(a: &[EnumValue], b: &[EnumValue]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| x.name == y.name && x.value == y.value)
}

/// For each set written as an enum, the set whose enum it is: itself, or the
/// set it derives from when it states the same values on the same integer
/// type (ADR 0072). A set not in the map is not written as an enum.
pub(super) fn written_sets(peripherals: &[super::Peripheral]) -> HashMap<SetId, SetId> {
    let mut infos: HashMap<SetId, SetInfo> = HashMap::new();
    let mut order: Vec<SetId> = Vec::new();
    for p in peripherals.iter().filter(|p| p.shares_type_with.is_none()) {
        survey(&p.items, &mut infos, &mut order);
    }
    // The chain of `derivedFrom` is walked in a loop, not by recursion: it
    // can be as long as the file.
    let mut targets: HashMap<SetId, Option<SetId>> = HashMap::new();
    for &start in &order {
        let mut chain: Vec<SetId> = Vec::new();
        let mut current = Some(start);
        while let Some(set) = current {
            if targets.contains_key(&set) || chain.len() > infos.len() {
                break;
            }
            chain.push(set);
            current = infos
                .get(&set)
                .and_then(|info| info.derived_from)
                .filter(|base| infos.contains_key(base));
        }
        for &set in chain.iter().rev() {
            let Some(info) = infos.get(&set) else {
                continue;
            };
            let own = info.has_variants.then_some(set);
            let target = if info.single {
                let reused = info.derived_from.and_then(|base| {
                    let base_info = infos.get(&base)?;
                    let target = (*targets.get(&base)?)?;
                    (base_info.int_bits == info.int_bits
                        && same_values(base_info.values, info.values))
                    .then_some(target)
                });
                reused.or(own)
            } else {
                None
            };
            targets.insert(set, target);
        }
    }
    targets
        .into_iter()
        .filter_map(|(set, target)| Some((set, target?)))
        .collect()
}

fn survey<'a>(items: &'a [Item], infos: &mut HashMap<SetId, SetInfo<'a>>, order: &mut Vec<SetId>) {
    for item in items {
        match item {
            Item::Cluster(cluster) => survey(&cluster.items, infos, order),
            Item::Register(r) => {
                if over::width(r.size).is_none() {
                    continue;
                }
                for f in &r.fields {
                    for set in &f.enums {
                        let has_variants = classify(&set.values)
                            .iter()
                            .any(|class| matches!(class, Class::Variant { .. }));
                        infos.insert(
                            set.id,
                            SetInfo {
                                int_bits: int_bits(f.bits.width),
                                single: f.enums.len() == 1,
                                has_variants,
                                derived_from: set.derived_from,
                                values: &set.values,
                            },
                        );
                        order.push(set.id);
                    }
                }
            }
        }
    }
}

// ---- the plan

/// An enum written, as other structs name it.
pub(super) struct EnumRef {
    /// The modules from the file's root to the enum's.
    module: Vec<String>,
    name: String,
    /// The name of the variant of each value (values are unique).
    variants: HashMap<u64, String>,
}

/// The path from the module `from` to `to`'s enum, through `super`
/// (ADR 0071).
fn path_to(from: &[String], to: &EnumRef) -> String {
    let common = from
        .iter()
        .zip(&to.module)
        .take_while(|(a, b)| a == b)
        .count();
    let mut parts: Vec<&str> = vec!["super"; from.len() - common];
    parts.extend(to.module[common..].iter().map(String::as_str));
    parts.push(&to.name);
    parts.join("::")
}

/// The structs and enums of the items of a peripheral type or a cluster
/// type, parallel to the items.
pub(super) struct TypePlan<'a> {
    pub entries: Vec<Entry<'a>>,
}

pub(super) enum Entry<'a> {
    /// A register's struct, when it has fields and they were named.
    Register(Option<StructPlan<'a>>),
    /// A cluster's names, when its name is an identifier.
    Cluster(Option<ClusterPlan<'a>>),
}

pub(super) struct ClusterPlan<'a> {
    pub module: String,
    pub ty: String,
    pub inner: TypePlan<'a>,
}

pub(super) struct StructPlan<'a> {
    pub name: String,
    /// The module the struct is in.
    path: Vec<String>,
    fields: Vec<FieldPlan<'a>>,
}

struct FieldPlan<'a> {
    field: &'a Field,
    /// One per element: its name and its first bit.
    elements: Vec<(String, u64)>,
    /// The set whose enum is the field's type.
    enum_set: Option<SetId>,
    /// The enum written for the field's set, when the field has its own.
    enum_plan: Option<EnumPlan<'a>>,
    /// The reader's notes of the sets, shown after the field's own.
    set_notes: Vec<String>,
    /// What has no declaration in the field's sets.
    undeclared: Vec<String>,
}

struct EnumPlan<'a> {
    set: &'a EnumSet,
    name: String,
    int: u32,
    members: Vec<Member<'a>>,
}

enum Member<'a> {
    Variant {
        name: String,
        value: u64,
        description: Option<&'a str>,
    },
    /// An enumerated value left out: the comment in its place.
    Left(String),
}

impl<'a> Writer<'a, '_> {
    /// The plan of the items of a type in `path`: the names of the structs
    /// and enums of the registers, in the order of the items, then the
    /// clusters' modules (ADR 0071).
    pub(super) fn plan_type(
        &mut self,
        path: &[String],
        items: &'a [Item],
        scope: &mut Scope,
    ) -> TypePlan<'a> {
        let mut entries: Vec<Entry<'a>> = items
            .iter()
            .map(|item| match item {
                Item::Register(r) => Entry::Register(self.plan_register(path, r, scope)),
                Item::Cluster(_) => Entry::Cluster(None),
            })
            .collect();
        for (item, entry) in items.iter().zip(entries.iter_mut()) {
            if let (Item::Cluster(cluster), Entry::Cluster(slot)) = (item, entry) {
                *slot = self.plan_cluster(path, cluster, scope);
            }
        }
        TypePlan { entries }
    }

    /// The names of a cluster's module (in `scope`, the module it is in) and
    /// of its type; `None` when its name is no identifier.
    fn plan_cluster(
        &mut self,
        path: &[String],
        cluster: &'a Cluster,
        scope: &mut Scope,
    ) -> Option<ClusterPlan<'a>> {
        let of = Of {
            svd: &cluster.name,
            span: cluster.span,
            what: "cluster",
            report_changes: false,
        };
        let module = self.name(scope, of, Index::Whole, Case::Lower);
        let mut inner = Scope::default();
        let ty = self.name(&mut inner, of, Index::Whole, Case::Pascal);
        let (module, ty) = (module?, ty?);
        let mut cluster_path = path.to_vec();
        cluster_path.push(module.clone());
        let plan = self.plan_type(&cluster_path, &cluster.items, &mut inner);
        Some(ClusterPlan {
            module,
            ty,
            inner: plan,
        })
    }

    /// The struct of a register with fields, its fields' names and its enums.
    fn plan_register(
        &mut self,
        path: &[String],
        r: &'a Register,
        scope: &mut Scope,
    ) -> Option<StructPlan<'a>> {
        if r.fields.is_empty() || over::width(r.size).is_none() {
            return None;
        }
        let of = Of {
            svd: &r.name,
            span: r.span,
            what: "register",
            report_changes: false,
        };
        let name = self.name(scope, of, Index::Whole, Case::Pascal)?;
        let mut field_names = Scope::default();
        let mut fields = Vec::new();
        for f in &r.fields {
            let plan = self.plan_field(path, &name, f, &mut field_names, scope);
            fields.push(plan);
        }
        Some(StructPlan {
            name,
            path: path.to_vec(),
            fields,
        })
    }

    fn plan_field(
        &mut self,
        path: &[String],
        struct_name: &str,
        f: &'a Field,
        field_names: &mut Scope,
        module: &mut Scope,
    ) -> FieldPlan<'a> {
        let mut named = Vec::new();
        for (index, delta) in elements(f.repeat.as_ref()) {
            let of = Of {
                svd: &f.name,
                span: f.span,
                what: "field",
                report_changes: true,
            };
            let index = index.as_deref().map_or(Index::Whole, Index::Element);
            if let Some(name) = self.name(field_names, of, index, Case::Lower) {
                named.push((name, u64::from(f.bits.offset).saturating_add(delta)));
            }
        }
        let mut plan = FieldPlan {
            field: f,
            elements: named,
            enum_set: None,
            enum_plan: None,
            set_notes: Vec::new(),
            undeclared: Vec::new(),
        };
        if plan.elements.is_empty() {
            return plan;
        }
        match f.enums.as_slice() {
            [] => {}
            [set] => match self.sets.get(&set.id).copied() {
                Some(target) if target != set.id => {
                    // The enum of the set it derives from; the values and
                    // what they lost are reported there.
                    plan.enum_set = Some(target);
                    plan.set_notes.extend(set.notes.iter().cloned());
                }
                _ => self.plan_enum(path, struct_name, f, set, module, &mut plan),
            },
            sets => {
                let what = format!(
                    "it has {} sets of enumerated values, and a field has one type: it is {}",
                    sets.len(),
                    if f.bits.width == 1 {
                        "a `bool`"
                    } else {
                        "an integer"
                    }
                );
                let subject = format!("field `{}`", f.name);
                let mut diag =
                    Diagnostic::warning("svd::not-declared", format!("{subject}: {what}"))
                        .label(f.span, "not declared");
                for set in sets {
                    diag = diag.label(set.span, "a set of this field");
                    plan.set_notes.extend(set.notes.iter().cloned());
                }
                self.rd.warning(diag);
                plan.undeclared.push(what);
            }
        }
        plan
    }

    /// The enum of a field's only set, judged as any other (ADR 0072): the
    /// values left out are reported; with no variant left, no enum is
    /// written and the comments go to the field.
    fn plan_enum(
        &mut self,
        path: &[String],
        struct_name: &str,
        f: &'a Field,
        set: &'a EnumSet,
        module: &mut Scope,
        plan: &mut FieldPlan<'a>,
    ) {
        let classes = classify(&set.values);
        let mut variant_names = Scope::default();
        let mut members: Vec<Member<'a>> = Vec::new();
        for (value, class) in set.values.iter().zip(classes) {
            let name = value.name.as_deref().unwrap_or_default();
            let subject = format!("enumerated value `{name}`");
            let (what, extra) = match class {
                Class::Variant { name, value: n } => {
                    let of = Of {
                        svd: name,
                        span: value.span,
                        what: "enumerated value",
                        report_changes: true,
                    };
                    if let Some(variant) =
                        self.name(&mut variant_names, of, Index::Whole, Case::Pascal)
                    {
                        members.push(Member::Variant {
                            name: variant,
                            value: n,
                            description: value.description.as_deref(),
                        });
                    }
                    continue;
                }
                Class::Anonymous | Class::Default => continue,
                Class::DontCare => (
                    "left out: it has don't-care bits, which are several values under one name"
                        .to_owned(),
                    None,
                ),
                Class::EmptyName => (
                    "left out: its name leaves nothing for an identifier".to_owned(),
                    None,
                ),
                Class::Duplicate { first } => {
                    let number = match &value.value {
                        Some(EnumNumber::Value(n)) => number(*n),
                        _ => String::new(),
                    };
                    (
                        format!("left out: another variant has the value {number}"),
                        Some(first),
                    )
                }
            };
            let mut diag = Diagnostic::warning("svd::not-declared", format!("{subject}: {what}"))
                .label(value.span, "not declared");
            if let Some(first) = extra {
                diag = diag.label(first, "has the value");
            }
            self.rd.warning(diag);
            members.push(Member::Left(format!("`{name}` is {what}")));
        }
        let variants: HashMap<u64, String> = members
            .iter()
            .filter_map(|m| match m {
                Member::Variant { name, value, .. } => Some((*value, name.clone())),
                Member::Left(_) => None,
            })
            .collect();
        let converted = names::suffix(struct_name, &f.name);
        let of = Of {
            svd: &f.name,
            span: f.span,
            what: "enum of field",
            report_changes: false,
        };
        let name = if variants.is_empty() {
            None
        } else {
            self.claim(module, of, converted)
        };
        let Some(name) = name else {
            plan.set_notes.extend(set.notes.iter().cloned());
            plan.undeclared
                .extend(members.into_iter().filter_map(|m| match m {
                    Member::Left(text) => Some(text),
                    Member::Variant { .. } => None,
                }));
            return;
        };
        self.enums.insert(
            set.id,
            Rc::new(EnumRef {
                module: path.to_vec(),
                name: name.clone(),
                variants,
            }),
        );
        plan.enum_set = Some(set.id);
        plan.enum_plan = Some(EnumPlan {
            set,
            name,
            int: int_bits(f.bits.width),
            members,
        });
    }

    // ---- the text

    /// The struct of a register and the enums of its fields, as blocks of
    /// lines at `depth` modules in.
    pub(super) fn struct_blocks(
        &mut self,
        depth: usize,
        r: &Register,
        plan: &StructPlan<'_>,
    ) -> Vec<Vec<String>> {
        let margin = " ".repeat(depth * INDENT);
        let int = over::width(r.size).map_or(32, |bytes| bytes * 8);
        let (mode, _) = register_mode(r);
        let mut field_blocks = Vec::new();
        for field in &plan.fields {
            field_blocks.extend(self.field_blocks((depth + 1) * INDENT, r, mode, plan, field));
        }
        let mut lines = vec![format!("{margin}pub struct {}: u{int} {{", plan.name)];
        lines.extend(join(field_blocks, false));
        lines.push(format!("{margin}}}"));
        let mut blocks = vec![lines];
        blocks.extend(
            plan.fields
                .iter()
                .filter_map(|field| field.enum_plan.as_ref())
                .map(|e| enum_block(depth * INDENT, e)),
        );
        blocks
    }

    /// The entries a field is: one, or one per element of a list.
    fn field_blocks(
        &mut self,
        indent: usize,
        r: &Register,
        mode: &str,
        plan: &StructPlan<'_>,
        fp: &FieldPlan<'_>,
    ) -> Vec<Vec<String>> {
        let f = fp.field;
        let margin = " ".repeat(indent);
        let subject = format!("field `{}`", f.name);
        let mut undeclared: Vec<String> = fp.undeclared.clone();

        // Its mode, where it differs from the register's.
        let once = |this: &mut Self, undeclared: &mut Vec<String>, state: &str, declared: &str| {
            this.not_declared(
                undeclared,
                f.span,
                &subject,
                format!(
                    "access `{state}` is declared as `{declared}`; that it can be written once \
                     is not declared"
                ),
            );
        };
        let own = match f.access {
            Some(Access::ReadOnly) => Some("read"),
            Some(Access::WriteOnly) => Some("write"),
            Some(Access::WriteOnce) => {
                once(self, &mut undeclared, "writeOnce", "write-only");
                Some("write")
            }
            Some(Access::ReadWriteOnce) => {
                once(self, &mut undeclared, "read-writeOnce", "read-write");
                None
            }
            Some(Access::ReadWrite) | None => None,
        };
        let word = own.filter(|&w| w != mode);

        // Its effects.
        let write = match f.modified_write_values.or(r.modified_write_values) {
            Some(WriteEffect::OneToClear) => Some("write 1 clears"),
            Some(WriteEffect::OneToSet) => Some("write 1 sets"),
            Some(WriteEffect::OneToToggle) => Some("write 1 toggles"),
            Some(WriteEffect::ZeroToClear) => Some("write 0 clears"),
            Some(WriteEffect::ZeroToSet) => Some("write 0 sets"),
            Some(WriteEffect::ZeroToToggle) => Some("write 0 toggles"),
            Some(effect @ (WriteEffect::Clear | WriteEffect::Set)) => {
                self.not_declared(
                    &mut undeclared,
                    f.span,
                    &subject,
                    format!(
                        "`modifiedWriteValues` `{}` is not declared: the field has no write \
                         effect, and a field assignment to another field of its register \
                         triggers it",
                        effect_name(effect)
                    ),
                );
                None
            }
            Some(WriteEffect::Modify) | None => None,
        };
        let read = match f.read_action {
            Some(ReadAction::Clear) => Some("read clears"),
            Some(ReadAction::Set) => Some("read sets"),
            Some(ReadAction::Modify | ReadAction::ModifyExternal) => Some("read modifies"),
            None => None,
        };
        if let Some(span) = f.write_constraint {
            self.not_declared(
                &mut undeclared,
                span,
                &subject,
                "`writeConstraint` is not declared".to_owned(),
            );
        }

        // Its type.
        let enum_ref = fp.enum_set.and_then(|set| self.enums.get(&set).cloned());
        let ty = match &enum_ref {
            Some(e) => path_to(&plan.path, e),
            None if f.bits.width == 1 => "bool".to_owned(),
            None => format!("u{}", int_bits(f.bits.width)),
        };

        let mut docs = describe(indent, f.description.as_deref());
        docs.extend(svd_lines(indent, &f.notes));
        docs.extend(svd_lines(indent, &fp.set_notes));
        let mut blocks = Vec::new();
        for (name, lo) in &fp.elements {
            let mut notes = undeclared.clone();
            let default = if write.is_some() {
                None
            } else {
                self.default(r, f, *lo, enum_ref.as_deref(), &mut notes)
            };
            let hi = lo.saturating_add(u64::from(f.bits.width).saturating_sub(1));
            let bits = if f.bits.width == 1 {
                format!("{lo}")
            } else {
                format!("{lo}..={hi}")
            };
            let mut line = format!("{margin}pub ");
            if let Some(word) = word {
                line.push_str(word);
                line.push(' ');
            }
            line.push_str(&format!("{name}: {ty} at {bits}"));
            for effect in [read, write].into_iter().flatten() {
                line.push(' ');
                line.push_str(effect);
            }
            if let Some(default) = default {
                line.push_str(" = ");
                line.push_str(&default);
            }
            line.push(',');
            let mut lines = docs.clone();
            lines.extend(svd_lines(indent, &notes));
            lines.push(line);
            blocks.push(lines);
        }
        blocks
    }

    /// The default of a field element: the register's reset value at its
    /// bits (ADR 0072). A reset value that is no variant of the field's enum
    /// is not declared.
    fn default(
        &mut self,
        r: &Register,
        f: &Field,
        lo: u64,
        enum_ref: Option<&EnumRef>,
        notes: &mut Vec<String>,
    ) -> Option<String> {
        let value = reset_bits(r, lo, f.bits.width)?;
        match enum_ref {
            Some(e) => {
                if let Some(variant) = e.variants.get(&value) {
                    Some(format!(".{variant}"))
                } else {
                    self.not_declared(
                        notes,
                        f.span,
                        &format!("field `{}`", f.name),
                        format!(
                            "the reset value {} is no variant of `{}`: it has no default",
                            number(value),
                            e.name
                        ),
                    );
                    None
                }
            }
            None if f.bits.width == 1 => Some(if value == 0 { "false" } else { "true" }.to_owned()),
            None => Some(number(value)),
        }
    }
}

/// The reset value of the `width` bits from `lo`, when the register has a
/// reset value and its mask covers them all.
fn reset_bits(r: &Register, lo: u64, width: u32) -> Option<u64> {
    reset_at(r.reset_value, r.reset_mask, lo, width)
}

fn reset_at(reset: Option<u64>, reset_mask: u64, lo: u64, width: u32) -> Option<u64> {
    let reset = reset?;
    let end = lo.checked_add(u64::from(width))?;
    if end > 64 || width == 0 {
        return None;
    }
    let ones = if width >= 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    };
    let mask = ones << lo;
    (reset_mask & mask == mask).then(|| (reset >> lo) & ones)
}

/// A number as defaults and discriminants are written: decimal below 10,
/// hexadecimal from 10.
fn number(value: u64) -> String {
    if value < 10 {
        value.to_string()
    } else {
        hex(value)
    }
}

/// An enum, as lines `margin` spaces in.
fn enum_block(indent: usize, plan: &EnumPlan<'_>) -> Vec<String> {
    let margin = " ".repeat(indent);
    let inner = indent + INDENT;
    let mut lines = describe(indent, plan.set.description.as_deref());
    lines.extend(svd_lines(indent, &plan.set.notes));
    lines.push(format!("{margin}pub enum {}: u{} {{", plan.name, plan.int));
    let blocks: Vec<Vec<String>> = plan
        .members
        .iter()
        .map(|member| match member {
            Member::Variant {
                name,
                value,
                description,
            } => {
                let mut block = describe(inner, *description);
                block.push(format!("{}{name} = {},", " ".repeat(inner), number(*value)));
                block
            }
            Member::Left(text) => comment(inner, "// svd: ", text),
        })
        .collect();
    lines.extend(join(blocks, false));
    lines.push(format!("{margin}}}"));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::Diagnostics;
    use crate::span::SourceMap;

    #[test]
    fn a_field_takes_the_smallest_integer_that_holds_its_bits() {
        for (width, bits) in [
            (1, 8),
            (8, 8),
            (9, 16),
            (16, 16),
            (17, 32),
            (32, 32),
            (33, 64),
            (64, 64),
            (1000, 64),
        ] {
            assert_eq!(int_bits(width), bits, "{width}");
        }
    }

    #[test]
    fn numbers_are_decimal_below_ten_and_hexadecimal_from_ten() {
        assert_eq!(number(0), "0");
        assert_eq!(number(9), "9");
        assert_eq!(number(10), "0xA");
        assert_eq!(number(0x1234_5678), "0x1234_5678");
    }

    #[test]
    fn the_reset_value_is_read_at_the_fields_bits_when_the_mask_covers_them() {
        assert_eq!(reset_at(Some(0xA5), u64::MAX, 4, 4), Some(0xA));
        assert_eq!(reset_at(Some(0xA5), 0xFF, 0, 8), Some(0xA5));
        assert_eq!(reset_at(None, u64::MAX, 0, 1), None);
        // A bit out of the mask.
        assert_eq!(reset_at(Some(0xFF), 0x0F, 2, 4), None);
        assert_eq!(reset_at(Some(0xFF), 0x0F, 4, 4), None);
        // The ends of 64 bits, and past them: no shift overflows.
        assert_eq!(reset_at(Some(u64::MAX), u64::MAX, 0, 64), Some(u64::MAX));
        assert_eq!(reset_at(Some(1 << 63), u64::MAX, 63, 1), Some(1));
        assert_eq!(reset_at(Some(1), u64::MAX, 63, 2), None);
        assert_eq!(reset_at(Some(1), u64::MAX, 64, 1), None);
        assert_eq!(reset_at(Some(1), u64::MAX, u64::MAX, 1), None);
        assert_eq!(reset_at(Some(1), u64::MAX, 0, 0), None);
        assert_eq!(reset_at(Some(1), u64::MAX, 0, u32::MAX), None);
    }

    fn module(path: &[&str]) -> Vec<String> {
        path.iter().map(|s| (*s).to_owned()).collect()
    }

    fn enum_in(path: &[&str], name: &str) -> EnumRef {
        EnumRef {
            module: module(path),
            name: name.to_owned(),
            variants: HashMap::new(),
        }
    }

    #[test]
    fn a_path_to_an_enum_goes_up_through_super_and_down() {
        let target = enum_in(&["uart0"], "LcrWls");
        assert_eq!(path_to(&module(&["uart0"]), &target), "LcrWls");
        assert_eq!(
            path_to(&module(&["uart1"]), &target),
            "super::uart0::LcrWls"
        );
        assert_eq!(path_to(&module(&["uart0", "ch"]), &target), "super::LcrWls");
        assert_eq!(
            path_to(&module(&["uart1", "ch", "sub"]), &target),
            "super::super::super::uart0::LcrWls"
        );
        let deep = enum_in(&["uart0", "ch"], "X");
        assert_eq!(path_to(&module(&["uart0"]), &deep), "ch::X");
        assert_eq!(path_to(&module(&["uart0", "ch"]), &deep), "X");
        assert_eq!(path_to(&module(&["uart0", "other"]), &deep), "super::ch::X");
    }

    fn value(name: Option<&str>, value: Option<EnumNumber>) -> EnumValue {
        let mut sources = SourceMap::default();
        let file = sources.add("x.svd", "x");
        EnumValue {
            span: Span::new(file, 0, 1),
            name: name.map(str::to_owned),
            description: None,
            value,
            is_default: false,
        }
    }

    fn kinds(values: &[EnumValue]) -> Vec<&'static str> {
        classify(values)
            .iter()
            .map(|class| match class {
                Class::Variant { .. } => "variant",
                Class::Anonymous => "anonymous",
                Class::Default => "default",
                Class::DontCare => "dont-care",
                Class::EmptyName => "empty",
                Class::Duplicate { .. } => "duplicate",
            })
            .collect()
    }

    #[test]
    fn each_enumerated_value_is_a_variant_or_left_out_for_one_reason() {
        use EnumNumber::{DontCare, Value};
        let values = [
            value(Some("A"), Some(Value(0))),
            value(Some("B"), Some(Value(1))),
            value(Some("C"), Some(Value(1))),
            value(None, Some(Value(2))),
            value(Some("OTHERS"), None),
            value(Some("X"), Some(DontCare("#1x".to_owned()))),
            value(Some("-"), Some(Value(3))),
            // The name that left nothing takes no value: the next is a variant.
            value(Some("D"), Some(Value(3))),
            // A duplicate by its value, whatever its name.
            value(Some("E"), Some(Value(0))),
        ];
        assert_eq!(
            kinds(&values),
            [
                "variant",
                "variant",
                "duplicate",
                "anonymous",
                "default",
                "dont-care",
                "empty",
                "variant",
                "duplicate"
            ]
        );
        // A duplicate names the first holder of the value.
        let classes = classify(&values);
        assert!(matches!(
            &classes[2],
            Class::Duplicate { first } if *first == values[1].span
        ));
    }

    #[test]
    fn a_name_for_an_enum_is_the_structs_then_the_fields() {
        let name = |struct_name, field| names::suffix(struct_name, field).map(|c| c.name);
        assert_eq!(name("Lcr", "WLS").as_deref(), Some("LcrWls"));
        assert_eq!(name("Lcr", "TX%s").as_deref(), Some("LcrTx"));
        // The struct's own `_` stays, and a leading digit is no leading digit.
        assert_eq!(name("RtcCntl_", "MODE").as_deref(), Some("RtcCntl_Mode"));
        assert_eq!(name("Lcr", "1MHZ").as_deref(), Some("Lcr1mhz"));
        assert_eq!(name("Lcr", "-"), None);
        assert_eq!(name("Lcr", ""), None);
        let changed = |field| names::suffix("Lcr", field).is_some_and(|c| c.changed);
        assert!(!changed("WLS"));
        assert!(changed("A.B"));
    }

    /// The device of one register at 32 bits holding `fields`.
    fn device(fields: &str) -> crate::svd::Device {
        let text = format!(
            "<device><name>D</name><size>32</size><peripherals><peripheral><name>P</name>\
             <baseAddress>0</baseAddress><registers><register><name>R</name>\
             <addressOffset>0</addressOffset><fields>{fields}</fields></register></registers>\
             </peripheral></peripherals></device>"
        );
        let mut sources = SourceMap::default();
        let file = sources.add("t.svd", &text);
        let mut diags = Diagnostics::default();
        let device = crate::svd::read(file, &text, &mut diags);
        assert!(!diags.has_errors(), "{:?}", diags.into_vec());
        device.expect("the device reads")
    }

    /// For each field of `fields`, the set whose enum its first set is
    /// written as, if any.
    fn written(fields: &str) -> Vec<(String, Option<usize>)> {
        let device = device(fields);
        let sets = written_sets(&device.peripherals);
        let Item::Register(r) = &device.peripherals[0].items[0] else {
            panic!("not a register");
        };
        r.fields
            .iter()
            .map(|f| {
                let target = f.enums.first().and_then(|s| sets.get(&s.id)).map(|t| t.0);
                (f.name.clone(), target)
            })
            .collect()
    }

    fn field(name: &str, offset: u32, width: u32, sets: &str) -> String {
        format!(
            "<field><name>{name}</name><bitOffset>{offset}</bitOffset>\
             <bitWidth>{width}</bitWidth>{sets}</field>"
        )
    }

    const BASE: &str = "<enumeratedValues><name>E</name>\
        <enumeratedValue><name>A</name><value>0</value></enumeratedValue>\
        <enumeratedValue><name>B</name><value>1</value></enumeratedValue></enumeratedValues>";

    fn derived(value: &str) -> String {
        format!("<enumeratedValues derivedFrom=\"E\">{value}</enumeratedValues>")
    }

    #[test]
    fn a_derived_set_reuses_the_base_enum_only_when_its_values_equal_the_bases() {
        // ADR 0072 (amended): the same names and values, in the same order.
        let value = |name, number| {
            format!("<enumeratedValue><name>{name}</name><value>{number}</value></enumeratedValue>")
        };
        let fields = [
            field("BASE", 0, 2, BASE),
            // Derived with nothing of its own.
            field("SAME", 2, 2, &derived("")),
            // Restating a value as it is.
            field("RESTATED", 4, 2, &derived(&value("B", 1))),
            // Adding a value.
            field("ADDS", 6, 3, &derived(&value("C", 2))),
            // Replacing a value's number.
            field("CHANGES", 9, 2, &derived(&value("B", 2))),
            // The same values on another integer type.
            field("WIDER", 11, 9, &derived("")),
        ]
        .concat();
        let written = written(&fields);
        let target = |name: &str| written.iter().find(|(n, _)| n == name).unwrap().1;
        let base = target("BASE");
        assert!(base.is_some());
        assert_eq!(target("SAME"), base);
        assert_eq!(target("RESTATED"), base);
        for own in ["ADDS", "CHANGES", "WIDER"] {
            assert!(target(own).is_some_and(|t| Some(t) != base), "{own}");
        }
    }

    #[test]
    fn a_set_with_nothing_to_write_or_a_second_set_is_not_an_enum() {
        let only_default = "<enumeratedValues><name>D</name><enumeratedValue><name>X</name>\
                            <isDefault>true</isDefault></enumeratedValue></enumeratedValues>";
        let two = format!("{BASE}{}", BASE.replace("<name>E</name>", "<name>F</name>"));
        let fields = [
            field("DEFAULT", 0, 2, only_default),
            field("DERIVED", 2, 2, "<enumeratedValues derivedFrom=\"D\"/>"),
            field("TWO", 4, 2, &two),
            field("NONE", 6, 2, ""),
        ]
        .concat();
        let written = written(&fields);
        assert!(
            written.iter().all(|(_, target)| target.is_none()),
            "{written:?}"
        );
    }

    #[test]
    fn a_long_chain_of_derived_sets_is_walked_without_recursion() {
        // A set derived from the one before it, 3 000 times over: the chain
        // the walk follows is as long as the file allows.
        let mut fields = field("F0", 0, 2, BASE);
        for i in 1..3_000 {
            let base = if i == 1 {
                "E".to_owned()
            } else {
                format!("S{}", i - 1)
            };
            fields.push_str(&format!(
                "<field><name>F{i}</name><bitOffset>0</bitOffset><bitWidth>2</bitWidth>\
                 <enumeratedValues derivedFrom=\"{base}\"><name>S{i}</name></enumeratedValues>\
                 </field>"
            ));
        }
        let written = written(&fields);
        let base = written[0].1;
        assert!(base.is_some());
        assert_eq!(written.len(), 3_000);
        assert!(written.iter().all(|(_, target)| *target == base));
    }
}
