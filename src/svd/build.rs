//! The resolved tree into the model: properties inherited from the device
//! down to the registers, `dim` read into an array or a list, and what SVD
//! requires and no level supplies reported (`svd::missing-element`,
//! `svd::invalid-dim`).

use crate::diag::Diagnostic;

use super::number::{self, IndexError};
use super::raw::{Common, Dim, Id, Props, Tree};
use super::read::Reader;
use super::{Cluster, EnumSet, EnumValue, Field, Item, Peripheral, Register, Repeat, SetId};

/// The most indices a list is made of. A `dim` above it is an
/// `svd::invalid-dim`: a list of that many elements is written out, and a
/// hostile file would exhaust memory.
pub(super) const MAX_LIST: u32 = 1 << 16;

pub(super) fn build(tree: &Tree, device: Props, rd: &mut Reader) -> Vec<Peripheral> {
    let mut builder = Builder { tree, rd };
    (0..tree.periphs.len())
        .filter_map(|i| builder.peripheral(i, device))
        .collect()
}

struct Builder<'a, 'r> {
    tree: &'a Tree,
    rd: &'a mut Reader<'r>,
}

/// Whether an element repeated may be an array.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// A register or a cluster: an array when its name says `[%s]`.
    Item,
    /// A peripheral or a field: always one element per index.
    Listed,
}

impl Builder<'_, '_> {
    fn peripheral(&mut self, i: usize, device: Props) -> Option<Peripheral> {
        let tree = self.tree;
        let p = &tree.periphs[i];
        let name = p.c.name.clone()?;
        let base_address = self.required(p.base_address, &p.c, "peripheral", "baseAddress");
        let repeat = self.repeat(p.dim.as_ref(), &p.c, &name, "peripheral", Kind::Listed);
        let items = if p.shares.is_some() {
            Vec::new()
        } else {
            self.items(&p.c.children, p.props.over(device))
        };
        Some(Peripheral {
            span: p.c.span,
            name_span: p.c.name_span,
            name,
            description: p.c.description.clone(),
            base_address: base_address?,
            repeat: repeat.ok()?,
            alternate: p.alternate_id,
            shares_type_with: p.shares,
            items,
            notes: p.c.notes.clone(),
        })
    }

    fn items(&mut self, ids: &[Id], inherited: Props) -> Vec<Item> {
        ids.iter()
            .filter_map(|&id| match id {
                Id::Reg(i) => self.register(i, inherited).map(Item::Register),
                Id::Cluster(i) => self.cluster(i, inherited).map(Item::Cluster),
                _ => None,
            })
            .collect()
    }

    fn cluster(&mut self, i: usize, inherited: Props) -> Option<Cluster> {
        let tree = self.tree;
        let c = &tree.clusters[i];
        let name = c.c.name.clone()?;
        let offset = self.required(c.offset, &c.c, "cluster", "addressOffset");
        let repeat = self.repeat(c.dim.as_ref(), &c.c, &name, "cluster", Kind::Item);
        let items = self.items(&c.c.children, c.props.over(inherited));
        Some(Cluster {
            span: c.c.span,
            name_span: c.c.name_span,
            name,
            description: c.c.description.clone(),
            offset: offset?,
            repeat: repeat.ok()?,
            alternate_cluster: c.alternate.clone(),
            items,
            notes: c.c.notes.clone(),
        })
    }

    fn register(&mut self, i: usize, inherited: Props) -> Option<Register> {
        let tree = self.tree;
        let r = &tree.regs[i];
        let name = r.c.name.clone()?;
        let props = r.props.over(inherited);
        let offset = self.required(r.offset, &r.c, "register", "addressOffset");
        let size = self.required(props.size, &r.c, "register", "size");
        let repeat = self.repeat(r.dim.as_ref(), &r.c, &name, "register", Kind::Item);
        let fields =
            r.c.children
                .iter()
                .filter_map(|&id| match id {
                    Id::Field(f) => self.field(f),
                    _ => None,
                })
                .collect();
        let size = size?;
        // The bits above the register's size are not read (ADR 0072).
        let cut = if size >= 64 {
            u64::MAX
        } else {
            (1u64 << size) - 1
        };
        Some(Register {
            span: r.c.span,
            name_span: r.c.name_span,
            name,
            description: r.c.description.clone(),
            offset: offset?,
            repeat: repeat.ok()?,
            size,
            access: props.access,
            reset_value: props.reset_value.map(|v| v & cut),
            reset_mask: props.reset_mask.map_or(cut, |m| m & cut),
            alternate_group: r.alternate_group.clone(),
            alternate_register: r.alternate_register.clone(),
            read_action: r.read_action,
            modified_write_values: r.modified_write_values,
            write_constraint: r.write_constraint,
            protection: r.protection,
            fields,
            notes: r.c.notes.clone(),
        })
    }

    fn field(&mut self, i: usize) -> Option<Field> {
        let tree = self.tree;
        let f = &tree.fields[i];
        let name = f.c.name.clone()?;
        let bits = match f.bits {
            Some(bits) => Some(bits),
            None => {
                self.rd.error(
                    Diagnostic::error("svd::missing-element", "a field needs its bits")
                        .label(f.c.span, "this field states no bits")
                        .help(
                            "state `bitOffset` and `bitWidth`, `lsb` and `msb`, or \
                             `bitRange`",
                        ),
                );
                None
            }
        };
        let repeat = self.repeat(f.dim.as_ref(), &f.c, &name, "field", Kind::Listed);
        let enums =
            f.c.children
                .iter()
                .filter_map(|&id| match id {
                    Id::Set(s) => Some(self.set(s)),
                    _ => None,
                })
                .collect();
        Some(Field {
            span: f.c.span,
            name_span: f.c.name_span,
            name,
            description: f.c.description.clone(),
            repeat: repeat.ok()?,
            bits: bits?,
            access: f.access,
            read_action: f.read_action,
            modified_write_values: f.modified_write_values,
            write_constraint: f.write_constraint,
            enums,
            notes: f.c.notes.clone(),
        })
    }

    fn set(&mut self, i: usize) -> EnumSet {
        let s = &self.tree.sets[i];
        EnumSet {
            span: s.c.span,
            id: SetId(i),
            name: s.c.name.clone(),
            description: s.c.description.clone(),
            usage: s.usage,
            derived_from: s.base.map(SetId),
            values: s
                .values
                .iter()
                .map(|v| EnumValue {
                    span: v.span,
                    name_span: v.name_span,
                    name: v.name.clone(),
                    description: v.description.clone(),
                    value: v.value.clone(),
                    is_default: v.is_default,
                })
                .collect(),
            notes: s.c.notes.clone(),
        }
    }

    /// A value SVD requires, which no level supplies.
    fn required<T>(
        &mut self,
        value: Option<T>,
        c: &Common,
        what: &str,
        element: &str,
    ) -> Option<T> {
        if value.is_none() {
            self.rd.error(
                Diagnostic::error(
                    "svd::missing-element",
                    format!("a {what} needs a `{element}`"),
                )
                .label(c.span, format!("this {what} has no `{element}`")),
            );
        }
        value
    }

    /// `dim`, `dimIncrement` and `dimIndex` of an element named `name`.
    /// `Err` when `svd::invalid-dim` was reported.
    fn repeat(
        &mut self,
        dim: Option<&Dim>,
        c: &Common,
        name: &str,
        what: &str,
        kind: Kind,
    ) -> Result<Option<Repeat>, ()> {
        let Some(dim) = dim else {
            return Ok(None);
        };
        let fail = |rd: &mut Reader, message: String, label: &str, help: Option<&str>| {
            let mut diag = Diagnostic::error("svd::invalid-dim", message)
                .label(c.span, label)
                .label(dim.span, "the `dim`");
            if let Some(help) = help {
                diag = diag.help(help);
            }
            rd.error(diag);
            Err(())
        };
        if dim.count == 0 {
            return fail(
                self.rd,
                format!("the `dim` of {what} `{name}` is 0"),
                "repeated 0 times",
                None,
            );
        }
        let Some(step) = dim.increment else {
            return fail(
                self.rd,
                format!("the {what} `{name}` has a `dim` and no `dimIncrement`"),
                "repeated, with no step",
                Some("state the distance between two elements in `dimIncrement`"),
            );
        };
        if !name.contains("%s") {
            return fail(
                self.rd,
                format!("the repeated {what} `{name}` has no `%s` in its name"),
                "repeated, and named the same at every index",
                Some("a repeated name holds `%s`, or `[%s]` for an array"),
            );
        }
        let array_name = kind == Kind::Item && name.contains("[%s]");
        if (dim.index.is_some() || !array_name) && dim.count > MAX_LIST {
            return fail(
                self.rd,
                format!(
                    "the {what} `{name}` is repeated {} times, more than the {MAX_LIST} the \
                     generator writes out",
                    dim.count
                ),
                "repeated too many times",
                None,
            );
        }
        let indices = match &dim.index {
            Some(text) => match number::parse_dim_index(text, dim.count) {
                Ok(indices) => Some(indices),
                Err(error) => {
                    let message = match error {
                        IndexError::Syntax => format!(
                            "the `dimIndex` `{}` of {what} `{name}` is no list or range",
                            text.trim()
                        ),
                        IndexError::Descending => format!(
                            "the `dimIndex` `{}` of {what} `{name}` goes down",
                            text.trim()
                        ),
                        IndexError::Length(n) => format!(
                            "the `dimIndex` of {what} `{name}` has {n} indices, and `dim` is {}",
                            dim.count
                        ),
                    };
                    return fail(
                        self.rd,
                        message,
                        "repeated",
                        Some("write a range (`0-3`, `A-C`) or a list (`A,B,C`) of `dim` indices"),
                    );
                }
            },
            None => None,
        };
        let sequence = |indices: &[String]| {
            indices
                .iter()
                .enumerate()
                .all(|(i, index)| *index == i.to_string())
        };
        if array_name && indices.as_deref().is_none_or(sequence) {
            return Ok(Some(Repeat::Array {
                count: dim.count,
                step,
            }));
        }
        let indices = indices.unwrap_or_else(|| (0..dim.count).map(|i| i.to_string()).collect());
        Ok(Some(Repeat::List { indices, step }))
    }
}
