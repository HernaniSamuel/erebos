//! The model into Erebos source (ADR 0070 to 0072): a module per peripheral
//! type holding the type and a module per cluster type, then the
//! peripherals. Names are converted and kept apart by [`super::names`],
//! peripherals sharing bytes are found by [`super::over`], and
//! the text of numbers and comments is [`super::text`]'s.
//!
//! The writer runs in three passes over the model: the names of the
//! modules, types and peripherals (which a peripheral's `over` and its type
//! need before anything is written), the peripherals that are `over` others,
//! and the text.

use std::collections::HashMap;
use std::rc::Rc;

use crate::diag::Diagnostic;
use crate::span::Span;

use super::fields::{self, Entry, EnumRef, StructPlan, TypePlan};
use super::names::{self, Case, Converted, Index, Scope};
use super::over::{self, Over, Placed, Shares};
use super::read::Reader;
use super::text::{comment, hex};
use super::{
    Access, Cluster, Device, Item, Peripheral, ReadAction, Register, Repeat, SetId, WriteEffect,
};

/// Spaces per module.
pub(super) const INDENT: usize = 4;

/// The generated file of `device`, read from the file called `file_name`.
/// Errors (`svd::invalid-name`) and warnings go through `rd`; the text is
/// to be used when none is an error.
pub(super) fn write(file_name: &str, device: &Device, rd: &mut Reader) -> String {
    let mut writer = Writer {
        device,
        rd,
        types: device.peripherals.iter().map(|_| None).collect(),
        scopes: device.peripherals.iter().map(|_| None).collect(),
        instances: device.peripherals.iter().map(|_| Vec::new()).collect(),
        first: Vec::new(),
        names: Vec::new(),
        overs: Vec::new(),
        sets: fields::written_sets(&device.peripherals),
        enums: HashMap::new(),
        plans: device.peripherals.iter().map(|_| None).collect(),
    };
    writer.name_peripherals();
    writer.find_overs();
    writer.plan_types();
    writer.file(file_name)
}

/// The names of a peripheral type that has one of its own.
struct TypeNames {
    module: String,
    ty: String,
}

/// A peripheral written: one per element of a list.
struct Instance {
    name: String,
    address: u64,
}

pub(super) struct Writer<'a, 'r> {
    device: &'a Device,
    pub(super) rd: &'a mut Reader<'r>,
    /// By peripheral: the names of its type, when it has one of its own and
    /// a name.
    types: Vec<Option<TypeNames>>,
    /// By peripheral: the names taken in its type's module, until the
    /// module is written.
    scopes: Vec<Option<Scope>>,
    /// By peripheral: the peripherals written for it.
    instances: Vec<Vec<Instance>>,
    /// By peripheral: the position of its first instance among all of them,
    /// in the order of the file.
    first: Vec<usize>,
    /// By position among all the instances: its name.
    names: Vec<String>,
    /// By position among all the instances: its `over`.
    overs: Vec<Over>,
    /// For each set written as an enum, the set whose enum it is.
    pub(super) sets: HashMap<SetId, SetId>,
    /// The enums planned, by the set they are written for.
    pub(super) enums: HashMap<SetId, Rc<EnumRef>>,
    /// By peripheral: the structs and enums of its type, until the module is
    /// written.
    plans: Vec<Option<TypePlan<'a>>>,
}

/// An SVD element whose name is converted.
#[derive(Clone, Copy)]
pub(super) struct Of<'a> {
    pub svd: &'a str,
    pub span: Span,
    /// What it is, for the diagnostics.
    pub what: &'a str,
    /// Whether a change of its characters is reported: the item that stands
    /// for the element asks, and not its module or type.
    pub report_changes: bool,
}

impl<'a> Writer<'a, '_> {
    // ---- names

    /// The names of the modules and types, then of the peripherals: the
    /// module of a type is in the file's scope, ahead of the peripherals
    /// (ADR 0071).
    ///
    /// A peripheral that has a type of its own is named for it here and for itself
    /// below, so an invalid name is reported twice: `Reader` reports a diagnostic
    /// once per (code, span, message), and both messages are the same.
    fn name_peripherals(&mut self) {
        let device = self.device;
        let mut top = Scope::default();
        for (i, p) in device.peripherals.iter().enumerate() {
            if p.shares_type_with.is_some() {
                continue;
            }
            let of = Of {
                svd: &p.name,
                span: p.name_span,
                what: "peripheral",
                report_changes: false,
            };
            let module = self.name(&mut top, of, Index::Whole, Case::Lower);
            let mut scope = Scope::default();
            let ty = self.name(&mut scope, of, Index::Whole, Case::Pascal);
            if let (Some(module), Some(ty)) = (module, ty) {
                self.types[i] = Some(TypeNames { module, ty });
                self.scopes[i] = Some(scope);
            }
        }
        for (i, p) in device.peripherals.iter().enumerate() {
            self.first.push(self.names.len());
            let of = Of {
                svd: &p.name,
                span: p.name_span,
                what: "peripheral",
                report_changes: true,
            };
            for (index, delta) in elements(p.repeat.as_ref()) {
                let index = index.as_deref().map_or(Index::Whole, Index::Element);
                if let Some(name) = self.name(&mut top, of, index, Case::Keep) {
                    self.names.push(name.clone());
                    self.instances[i].push(Instance {
                        name,
                        address: p.base_address.saturating_add(delta),
                    });
                }
            }
        }
    }

    /// Converts the SVD name of an element and takes it in `scope`.
    /// `svd::invalid-name` when nothing is left of it; the warning
    /// `svd::renamed` when its characters or a leading digit changed and
    /// when another item of the scope has the name.
    pub(super) fn name(
        &mut self,
        scope: &mut Scope,
        of: Of,
        index: Index,
        case: Case,
    ) -> Option<String> {
        self.claim(scope, of, names::convert(of.svd, index, case))
    }

    /// Takes the converted name of an element in `scope`; `None` converted is
    /// the error `svd::invalid-name`.
    pub(super) fn claim(
        &mut self,
        scope: &mut Scope,
        of: Of,
        converted: Option<Converted>,
    ) -> Option<String> {
        let Of {
            svd,
            span,
            what,
            report_changes,
        } = of;
        let Some(converted) = converted else {
            let message = if svd.is_empty() {
                format!("this {what} has an empty name")
            } else {
                format!("the name `{svd}` of this {what} leaves nothing for an identifier")
            };
            self.rd.error(
                Diagnostic::error("svd::invalid-name", message)
                    .label(span, "this name has no ASCII letter, digit or `_`")
                    .help("an identifier is made of ASCII letters, digits and `_`: rename it"),
            );
            return None;
        };
        let (name, taken_by) = scope.claim(converted.name, span);
        if report_changes && converted.changed {
            self.rd.warning(
                Diagnostic::warning(
                    "svd::renamed",
                    format!("the {what} `{svd}` is named `{name}`"),
                )
                .label(
                    span,
                    "its name has characters no identifier has, or starts with a digit",
                ),
            );
        }
        if let Some(other) = taken_by {
            self.rd.warning(
                Diagnostic::warning(
                    "svd::renamed",
                    format!("the {what} `{svd}` is named `{name}`: its scope has the name taken"),
                )
                .label(span, "renamed, with `_` after it")
                .label(other, "has the name"),
            );
        }
        Some(name)
    }

    /// The structs and enums of every peripheral type that has a name, once
    /// every name of the file is known (a set may derive from a later type's).
    fn plan_types(&mut self) {
        let device = self.device;
        for (i, p) in device.peripherals.iter().enumerate() {
            let Some(module) = self.types[i].as_ref().map(|names| names.module.clone()) else {
                continue;
            };
            let Some(mut scope) = self.scopes[i].take() else {
                continue;
            };
            let plan = self.plan_type(&[module], &p.items, &mut scope);
            self.plans[i] = Some(plan);
        }
    }

    // ---- peripherals over others

    fn find_overs(&mut self) {
        let device = self.device;
        let mut placed = Vec::new();
        for (i, p) in device.peripherals.iter().enumerate() {
            let owner = p.shares_type_with.unwrap_or(i);
            let extent = over::extent(&device.peripherals[owner].items);
            // The first element of a list, when the SVD names one.
            let explicit = p
                .alternate
                .filter(|&target| self.instances.get(target).is_some_and(|v| !v.is_empty()))
                .map(|target| self.first[target]);
            for instance in &self.instances[i] {
                placed.push(Placed {
                    span: p.span,
                    start: u128::from(instance.address),
                    end: u128::from(instance.address) + extent,
                    explicit,
                });
            }
        }
        self.overs = over::find(&placed);
        for (position, over) in self.overs.iter().enumerate() {
            let Over::Unresolved(shares) = over else {
                continue;
            };
            let mut diag = Diagnostic::warning(
                "svd::overlap",
                format!(
                    "the peripheral `{}` shares bytes with {}, which no one `over` names",
                    self.names[position],
                    self.quoted(shares)
                ),
            )
            .label(
                placed[position].span,
                "shares bytes with earlier peripherals",
            );
            for &s in &shares.shown {
                diag = diag.label(
                    placed[s].span,
                    format!("shares bytes with `{}`", self.names[position]),
                );
            }
            self.rd.warning(diag.help(
                "it is written without `over`, and the compiler reports the overlap \
                 (`ty::peripheral-overlap`); state `alternatePeripheral` in the SVD",
            ));
        }
    }

    /// The names of the peripherals of these shares, quoted and listed; when
    /// some are not kept, the first ones and "and N more".
    fn quoted(&self, shares: &Shares) -> String {
        let names: Vec<String> = shares
            .shown
            .iter()
            .map(|&position| format!("`{}`", self.names[position]))
            .collect();
        if shares.hidden() > 0 {
            return format!("{} and {} more", names.join(", "), shares.hidden());
        }
        match names.as_slice() {
            [] => String::new(),
            [only] => only.clone(),
            [init @ .., last] => format!("{} and {last}", init.join(", ")),
        }
    }

    // ---- the text

    fn file(&mut self, file_name: &str) -> String {
        let device = self.device;
        let mut lines = header(file_name, device);
        let mut blocks: Vec<Vec<String>> = Vec::new();
        for (i, p) in device.peripherals.iter().enumerate() {
            let (Some(names), Some(plan)) = (&self.types[i], self.plans[i].take()) else {
                continue;
            };
            let (module, ty) = (names.module.clone(), names.ty.clone());
            let mut docs = describe(INDENT, p.description.as_deref());
            docs.extend(svd_lines(INDENT, &p.notes));
            let mut body = self.type_block(1, &ty, docs, &p.items, &plan);
            let mut block = vec![format!("pub mod {module} {{")];
            block.append(&mut body);
            block.push("}".to_owned());
            blocks.push(block);
        }
        let mut peripherals: Vec<Vec<String>> = Vec::new();
        let mut position = 0;
        for (i, p) in device.peripherals.iter().enumerate() {
            for instance in &self.instances[i] {
                let block = self.instance(p, i, instance, position);
                peripherals.push(block);
                position += 1;
            }
        }
        lines.extend(join(blocks, true));
        if !lines.last().is_some_and(String::is_empty) {
            lines.push(String::new());
        }
        lines.extend(join(peripherals, false));
        let mut out = lines.join("\n");
        out.push('\n');
        out
    }

    /// `peripheral NAME: module::Type at ADDRESS [over OTHER];`
    fn instance(
        &self,
        p: &Peripheral,
        i: usize,
        instance: &Instance,
        position: usize,
    ) -> Vec<String> {
        let owner = p.shares_type_with.unwrap_or(i);
        let Some(names) = &self.types[owner] else {
            // Its type has no name, which was reported.
            return Vec::new();
        };
        let ty = format!("{}::{}", names.module, names.ty);
        let mut lines = describe(0, p.description.as_deref());
        if p.shares_type_with.is_some() {
            lines.extend(svd_lines(0, &p.notes));
        }
        let mut over = String::new();
        match &self.overs[position] {
            Over::No => {}
            Over::Unresolved(shares) => lines.extend(comment(
                0,
                "// svd: ",
                &format!(
                    "shares bytes with {}, which no one `over` names: it is written without \
                     `over`, and the compiler reports the overlap.",
                    self.quoted(shares)
                ),
            )),
            Over::Stated(target) => over = format!(" over {}", self.names[*target]),
            Over::Found { target, shares } => {
                lines.extend(comment(
                    0,
                    "// svd: ",
                    &format!(
                        "the SVD does not say that it is over `{}`: found from the bytes it \
                         shares with {}.",
                        self.names[*target],
                        self.quoted(shares)
                    ),
                ));
                over = format!(" over {}", self.names[*target]);
            }
        }
        lines.push(format!(
            "pub peripheral {}: {ty} at {}{over};",
            instance.name,
            hex(instance.address)
        ));
        lines
    }

    /// A peripheral type, the structs and enums of its registers and the
    /// modules of its clusters, at `depth` modules in, as lines. `docs` are
    /// the comment lines of its item; `plan` holds the names of the structs,
    /// enums and cluster modules.
    fn type_block(
        &mut self,
        depth: usize,
        ty: &str,
        docs: Vec<String>,
        items: &[Item],
        plan: &TypePlan<'_>,
    ) -> Vec<String> {
        let margin = " ".repeat(depth * INDENT);
        let mut entries = Scope::default();
        let mut blocks: Vec<Vec<String>> = Vec::new();
        for (item, entry) in items.iter().zip(&plan.entries) {
            match (item, entry) {
                (Item::Register(register), Entry::Register(structure)) => {
                    blocks.extend(self.register(
                        depth + 1,
                        register,
                        structure.as_ref(),
                        &mut entries,
                    ));
                }
                (Item::Cluster(cluster), Entry::Cluster(Some(cluster_plan))) => {
                    blocks.extend(self.cluster(depth + 1, cluster, cluster_plan, &mut entries));
                }
                _ => {}
            }
        }
        let body = join(blocks, false);
        let mut lines = docs;
        if body.is_empty() {
            lines.push(format!("{margin}pub peripheral {ty} {{}}"));
        } else {
            lines.push(format!("{margin}pub peripheral {ty} {{"));
            lines.extend(body);
            lines.push(format!("{margin}}}"));
        }
        for (item, entry) in items.iter().zip(&plan.entries) {
            if let (Item::Register(register), Entry::Register(Some(structure))) = (item, entry) {
                for block in self.struct_blocks(depth, register, structure) {
                    lines.push(String::new());
                    lines.extend(block);
                }
            }
        }
        for (item, entry) in items.iter().zip(&plan.entries) {
            let (Item::Cluster(cluster), Entry::Cluster(Some(cluster_plan))) = (item, entry) else {
                continue;
            };
            let mut docs = describe((depth + 1) * INDENT, cluster.description.as_deref());
            docs.extend(svd_lines((depth + 1) * INDENT, &cluster.notes));
            let inner = self.type_block(
                depth + 1,
                &cluster_plan.ty,
                docs,
                &cluster.items,
                &cluster_plan.inner,
            );
            lines.push(String::new());
            lines.push(format!("{margin}pub mod {} {{", cluster_plan.module));
            lines.extend(inner);
            lines.push(format!("{margin}}}"));
        }
        lines
    }

    /// The entries a cluster is: one, or one per element of a list.
    fn cluster(
        &mut self,
        depth: usize,
        cluster: &Cluster,
        plan: &fields::ClusterPlan<'_>,
        entries: &mut Scope,
    ) -> Vec<Vec<String>> {
        let margin = " ".repeat(depth * INDENT);
        let path = format!("{}::{}", plan.module, plan.ty);
        let mut docs = describe(depth * INDENT, cluster.description.as_deref());
        docs.extend(svd_lines(depth * INDENT, &cluster.notes));
        let mut blocks = Vec::new();
        for element in self.entry_elements(cluster.repeat.as_ref()) {
            let of = Of {
                svd: &cluster.name,
                span: cluster.name_span,
                what: "cluster",
                report_changes: true,
            };
            let Some(name) = self.name(entries, of, element.index(), Case::Lower) else {
                continue;
            };
            let (ty, step) = match &element {
                Element::Array { count, step } => (
                    format!("[{path}; {count}]"),
                    format!(" step {}", hex(u64::from(*step))),
                ),
                _ => (path.clone(), String::new()),
            };
            let group = if cluster.alternate_cluster.is_some() {
                format!(" in {name}")
            } else {
                String::new()
            };
            let mut lines = docs.clone();
            lines.push(format!(
                "{margin}pub {name}: {ty} at {}{step}{group},",
                hex(element.offset(cluster.offset))
            ));
            blocks.push(lines);
        }
        blocks
    }

    /// The entries a register is: one, one per element of a list, or none
    /// when it is left out.
    fn register(
        &mut self,
        depth: usize,
        r: &Register,
        structure: Option<&StructPlan<'_>>,
        entries: &mut Scope,
    ) -> Vec<Vec<String>> {
        let margin = " ".repeat(depth * INDENT);
        let mut docs = describe(depth * INDENT, r.description.as_deref());
        docs.extend(svd_lines(depth * INDENT, &r.notes));
        let subject = format!("register `{}`", r.name);
        let mut svd: Vec<String> = Vec::new();

        let Some(bytes) = over::width(r.size) else {
            self.not_declared(
                &mut svd,
                r.span,
                &subject,
                format!(
                    "left out: it is {} bits wide, and a register is 8, 16 or 32 bits wide",
                    r.size
                ),
            );
            docs.extend(svd_lines(depth * INDENT, &svd));
            return vec![docs];
        };
        let ty = structure.map_or_else(|| format!("u{}", bytes * 8), |s| s.name.clone());

        let (mode, once) = register_mode(r);
        if let Some(once) = once {
            self.not_declared(&mut svd, r.span, &subject, once);
        }
        if r.fields.is_empty() {
            if let Some(value) = r.reset_value.filter(|&v| v != 0) {
                self.not_declared(
                    &mut svd,
                    r.span,
                    &subject,
                    format!(
                        "the reset value {} is not declared: only the defaults of fields \
                         declare one",
                        hex(value)
                    ),
                );
            }
            if let Some(effect) = r
                .modified_write_values
                .filter(|&e| e != WriteEffect::Modify)
            {
                self.not_declared(
                    &mut svd,
                    r.span,
                    &subject,
                    format!(
                        "`modifiedWriteValues` `{}` is not declared: a register without fields \
                         has no field to carry it",
                        effect_name(effect)
                    ),
                );
            }
        }
        if let Some(span) = r.write_constraint {
            self.not_declared(
                &mut svd,
                span,
                &subject,
                "`writeConstraint` is not declared".to_owned(),
            );
        }
        if let Some(span) = r.protection {
            self.not_declared(
                &mut svd,
                span,
                &subject,
                "`protection` is not declared".to_owned(),
            );
        }
        docs.extend(svd_lines(depth * INDENT, &svd));

        let read = match r.read_action {
            Some(ReadAction::Clear) => " read clears",
            Some(ReadAction::Set) => " read sets",
            Some(ReadAction::Modify | ReadAction::ModifyExternal) => " read modifies",
            None => "",
        };
        let mut blocks = Vec::new();
        for element in self.entry_elements(r.repeat.as_ref()) {
            let of = Of {
                svd: &r.name,
                span: r.name_span,
                what: "register",
                report_changes: true,
            };
            let Some(name) = self.name(entries, of, element.index(), Case::Lower) else {
                continue;
            };
            let (ty, step) = match &element {
                Element::Array { count, step } => (
                    format!("[{ty}; {count}]"),
                    if u64::from(*step) == bytes {
                        String::new()
                    } else {
                        format!(" step {}", hex(u64::from(*step)))
                    },
                ),
                _ => (ty.clone(), String::new()),
            };
            let group = self.group(r, &name);
            let group = group.map(|g| format!(" in {g}")).unwrap_or_default();
            let mut lines = docs.clone();
            lines.push(format!(
                "{margin}pub {mode} {name}: {ty} at {}{step}{read}{group},",
                hex(element.offset(r.offset))
            ));
            blocks.push(lines);
        }
        blocks
    }

    /// The alternate group of a register named `name`: its
    /// `alternateGroup`, or its own name for an `alternateRegister`
    /// (ADR 0072). A group is a label in no namespace, so no name meets it.
    fn group(&mut self, r: &Register, name: &str) -> Option<String> {
        if let Some(group) = &r.alternate_group {
            let of = Of {
                svd: group,
                span: r.span,
                what: "alternate group",
                report_changes: true,
            };
            return self.name(&mut Scope::default(), of, Index::Whole, Case::Lower);
        }
        r.alternate_register.is_some().then(|| name.to_owned())
    }

    /// Reports what has no declaration: the warning `svd::not-declared`, and
    /// `what` as a `// svd:` comment.
    pub(super) fn not_declared(
        &mut self,
        svd: &mut Vec<String>,
        span: Span,
        subject: &str,
        what: String,
    ) {
        self.rd.warning(
            Diagnostic::warning("svd::not-declared", format!("{subject}: {what}"))
                .label(span, "not declared"),
        );
        svd.push(what);
    }

    /// The elements an entry repeated as `repeat` is written as.
    fn entry_elements(&self, repeat: Option<&Repeat>) -> Vec<Element> {
        match repeat {
            None => vec![Element::Single],
            Some(Repeat::Array { count, step }) => vec![Element::Array {
                count: *count,
                step: *step,
            }],
            Some(Repeat::List { indices, step }) => indices
                .iter()
                .enumerate()
                .map(|(n, index)| Element::Listed {
                    index: index.clone(),
                    delta: (n as u64).saturating_mul(u64::from(*step)),
                })
                .collect(),
        }
    }
}

/// An entry of a peripheral type, as written.
enum Element {
    /// An element that stands for the whole element: not repeated.
    Single,
    /// A register array or a cluster array.
    Array { count: u32, step: u32 },
    /// One of the elements of a list.
    Listed { index: String, delta: u64 },
}

impl Element {
    fn index(&self) -> Index<'_> {
        match self {
            Element::Single | Element::Array { .. } => Index::Whole,
            Element::Listed { index, .. } => Index::Element(index),
        }
    }

    fn offset(&self, offset: u64) -> u64 {
        match self {
            Element::Listed { delta, .. } => offset.saturating_add(*delta),
            _ => offset,
        }
    }
}

/// The elements a peripheral is written as: each with its index and its
/// distance from the first. A peripheral is never an array (ADR 0072).
pub(super) fn elements(repeat: Option<&Repeat>) -> Vec<(Option<String>, u64)> {
    match repeat {
        Some(Repeat::List { indices, step }) => indices
            .iter()
            .enumerate()
            .map(|(n, index)| {
                (
                    Some(index.clone()),
                    (n as u64).saturating_mul(u64::from(*step)),
                )
            })
            .collect(),
        _ => vec![(None, 0)],
    }
}

/// A register's mode (ADR 0072): its `access`, or taken from its fields
/// when no element states one. `writeOnce` and `read-writeOnce` are `write`
/// and `read write`, and the second value says that "once" is not declared.
pub(super) fn register_mode(r: &Register) -> (&'static str, Option<String>) {
    let once = |declared: &str, state: &str| {
        Some(format!(
            "access `{state}` is declared as `{declared}`; that it can be written once is not \
             declared"
        ))
    };
    match r.access {
        Some(Access::ReadOnly) => ("read", None),
        Some(Access::WriteOnly) => ("write", None),
        Some(Access::ReadWrite) => ("read write", None),
        Some(Access::WriteOnce) => ("write", once("write", "writeOnce")),
        Some(Access::ReadWriteOnce) => ("read write", once("read write", "read-writeOnce")),
        None => {
            let all = |wanted: &[Access]| {
                !r.fields.is_empty()
                    && r.fields
                        .iter()
                        .all(|f| f.access.is_some_and(|a| wanted.contains(&a)))
            };
            if all(&[Access::ReadOnly]) {
                ("read", None)
            } else if all(&[Access::WriteOnly, Access::WriteOnce]) {
                ("write", None)
            } else {
                ("read write", None)
            }
        }
    }
}

/// `modifiedWriteValues` as SVD spells it.
pub(super) fn effect_name(effect: WriteEffect) -> &'static str {
    match effect {
        WriteEffect::OneToClear => "oneToClear",
        WriteEffect::OneToSet => "oneToSet",
        WriteEffect::OneToToggle => "oneToToggle",
        WriteEffect::ZeroToClear => "zeroToClear",
        WriteEffect::ZeroToSet => "zeroToSet",
        WriteEffect::ZeroToToggle => "zeroToToggle",
        WriteEffect::Clear => "clear",
        WriteEffect::Set => "set",
        WriteEffect::Modify => "modify",
    }
}

/// The comment of an element's `description`, `indent` spaces in.
pub(super) fn describe(indent: usize, description: Option<&str>) -> Vec<String> {
    description.map_or_else(Vec::new, |text| comment(indent, "// ", text))
}

/// `// svd:` comments, one per text: the values the reader read as absent,
/// and what has no declaration.
pub(super) fn svd_lines(indent: usize, lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .flat_map(|text| comment(indent, "// svd: ", text))
        .collect()
}

/// Blocks of lines as one. A block with a comment is set apart by a blank
/// line from the blocks around it; for modules (`always`), every block is.
pub(super) fn join(blocks: Vec<Vec<String>>, always: bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut previous_commented = false;
    for block in blocks {
        if block.is_empty() {
            continue;
        }
        let commented = block.iter().any(|line| line.trim_start().starts_with("//"));
        if !out.is_empty() && (always || commented || previous_commented) {
            out.push(String::new());
        }
        out.extend(block);
        previous_commented = commented;
    }
    out
}

/// The comment the file starts with (ADR 0070), and a blank line.
fn header(file_name: &str, device: &Device) -> Vec<String> {
    // Text of the SVD file reaches a comment line: it must not hold a line
    // break, which would end the comment and start Erebos source.
    let file_name = one_line(file_name);
    let name = one_line(&device.name);
    let mut lines = vec![format!("// Generated by `erebos svd` from {file_name}.")];
    lines.push(match &device.version {
        Some(version) => format!("// Device: {name}, version {}.", one_line(version)),
        None => format!("// Device: {name}."),
    });
    lines.push("// Edits are lost when it is generated again.".to_owned());
    if let Some(text) = &device.license_text {
        let text = break_free(text);
        let all: Vec<&str> = text.lines().map(str::trim_end).collect();
        // Blank lines at either end of the text are not part of the notice.
        let start = all.iter().position(|line| !line.is_empty());
        let end = all.iter().rposition(|line| !line.is_empty());
        let license = match (start, end) {
            (Some(start), Some(end)) => &all[start..=end],
            _ => &[][..],
        };
        if !license.is_empty() {
            lines.push("//".to_owned());
            for line in license {
                lines.push(if line.is_empty() {
                    "//".to_owned()
                } else {
                    format!("// {line}")
                });
            }
        }
    }
    lines.push(String::new());
    lines
}

/// `text` on one line: its white space, line breaks included, collapsed.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `text` with `\n` as its only line break: `\r\n` becomes `\n`, and any
/// other control character or Unicode line separator, which a lexer might
/// take for a line break, becomes a space. A tab stays.
fn break_free(text: &str) -> String {
    text.replace("\r\n", "\n")
        .chars()
        .map(|c| match c {
            '\n' | '\t' => c,
            c if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') => ' ',
            c => c,
        })
        .collect()
}
