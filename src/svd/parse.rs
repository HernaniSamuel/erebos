//! The XML tree into the arena of [`super::raw`]: every element with the
//! values it states itself, numbers and keywords read, and the errors of a
//! single element reported (`svd::missing-element` for a `name`,
//! `svd::invalid-number`, `svd::invalid-value`).

use roxmltree::Node;

use crate::diag::Diagnostic;
use crate::span::{FileId, Span};

use super::number::{self, NumberError};
use super::raw::{
    Cluster, Common, Dim, Field, Id, MAX_DEPTH, Named, Periph, Props, Reg, Set, State, Tree, Value,
};
use super::read::Reader;
use super::{Access, Bits, ReadAction, Usage, WriteEffect};

pub(super) struct Parser<'a, 'r> {
    pub file: FileId,
    pub tree: Tree,
    pub rd: &'a mut Reader<'r>,
}

/// The first child element with this tag name. Namespaces are ignored: an
/// SVD file's elements are known by their local names.
pub(super) fn child<'a, 'i>(node: Node<'a, 'i>, tag: &str) -> Option<Node<'a, 'i>> {
    node.children()
        .find(|c| c.is_element() && c.tag_name().name() == tag)
}

fn children<'a, 'i>(node: Node<'a, 'i>, tag: &'a str) -> impl Iterator<Item = Node<'a, 'i>> {
    node.children()
        .filter(move |c| c.is_element() && c.tag_name().name() == tag)
}

/// The text of an element, entities decoded, as the XML gives it.
pub(super) fn text_of(node: Node) -> String {
    node.children()
        .filter(|c| c.is_text())
        .filter_map(|c| c.text())
        .collect()
}

impl Parser<'_, '_> {
    pub fn span(&self, node: Node) -> Span {
        let range = node.range();
        Span::new(self.file, range.start, range.end)
    }

    /// The text of a child, with the element's span.
    fn child_text(&self, node: Node, tag: &str) -> Option<(String, Span)> {
        let c = child(node, tag)?;
        Some((text_of(c), self.span(c)))
    }

    /// The span of the `name` of an element, or of the element when it has
    /// none: what the diagnostics about a name point at, instead of a whole
    /// peripheral.
    fn name_span(&self, node: Node) -> Span {
        self.span(child(node, "name").unwrap_or(node))
    }

    /// A child whose text is a name or a description, as written.
    pub fn string(&self, node: Node, tag: &str) -> Option<String> {
        child(node, tag).map(text_of)
    }

    /// The `name` of an element (white space around it removed), or the
    /// error that there is none.
    fn name(&mut self, node: Node, what: &str) -> Option<String> {
        match child(node, "name") {
            Some(name) => Some(text_of(name).trim().to_owned()),
            None => {
                self.missing(node, what, "name");
                None
            }
        }
    }

    pub fn missing(&mut self, node: Node, what: &str, element: &str) {
        let span = self.span(node);
        self.rd.error(
            Diagnostic::error(
                "svd::missing-element",
                format!("a {what} needs a `{element}`"),
            )
            .label(span, format!("this {what} has no `{element}`")),
        );
    }

    /// A number, reporting `svd::invalid-number` at the element.
    fn number_in(&mut self, text: &str, span: Span, tag: &str) -> Option<u64> {
        match number::parse_number(text) {
            Ok(n) => Some(n),
            Err(error) => {
                self.invalid_number(text, span, tag, error);
                None
            }
        }
    }

    fn invalid_number(&mut self, text: &str, span: Span, tag: &str, error: NumberError) {
        let (message, label) = match error {
            NumberError::Syntax => (
                format!("`{}` is no number SVD allows in `{tag}`", text.trim()),
                "not a number",
            ),
            NumberError::TooLarge => (
                format!("`{}` does not fit in 64 bits", text.trim()),
                "too large",
            ),
        };
        self.rd.error(
            Diagnostic::error("svd::invalid-number", message)
                .label(span, label)
                .help(
                    "write decimal digits, `0x` then hexadecimal digits, or `#` then binary digits",
                ),
        );
    }

    fn number(&mut self, node: Node, tag: &str) -> Option<u64> {
        let (text, span) = self.child_text(node, tag)?;
        self.number_in(&text, span, tag)
    }

    fn number_u32(&mut self, node: Node, tag: &str) -> Option<u32> {
        let (text, span) = self.child_text(node, tag)?;
        let n = self.number_in(&text, span, tag)?;
        match u32::try_from(n) {
            Ok(n) => Some(n),
            Err(_) => {
                self.rd.error(
                    Diagnostic::error(
                        "svd::invalid-number",
                        format!("`{}` does not fit in 32 bits, as `{tag}` must", text.trim()),
                    )
                    .label(span, "too large"),
                );
                None
            }
        }
    }

    /// A child naming one of `table`, `None` when it states none. A value
    /// SVD does not define is read as absent (ADR 0072): the warning
    /// `svd::invalid-value`, and a note the writer puts in a comment.
    fn keyword<T: Copy>(
        &mut self,
        node: Node,
        tag: &str,
        table: &[(&str, T)],
        notes: &mut Vec<String>,
    ) -> Option<T> {
        let (text, span) = self.child_text(node, tag)?;
        let text = text.trim();
        if let Some(&(_, value)) = table.iter().find(|(word, _)| *word == text) {
            return Some(value);
        }
        let message = format!("`{text}` is no SVD value of `{tag}`; read as absent");
        self.rd.warning(
            Diagnostic::warning("svd::invalid-value", message.clone())
                .label(span, "not an SVD value")
                .help(format!(
                    "SVD defines: {}",
                    table
                        .iter()
                        .map(|(word, _)| format!("`{word}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
        );
        notes.push(message);
        None
    }

    fn access(&mut self, node: Node, notes: &mut Vec<String>) -> Option<Access> {
        const TABLE: &[(&str, Access)] = &[
            ("read-only", Access::ReadOnly),
            ("write-only", Access::WriteOnly),
            ("read-write", Access::ReadWrite),
            ("writeOnce", Access::WriteOnce),
            ("read-writeOnce", Access::ReadWriteOnce),
        ];
        self.keyword(node, "access", TABLE, notes)
    }

    fn read_action(&mut self, node: Node, notes: &mut Vec<String>) -> Option<ReadAction> {
        const TABLE: &[(&str, ReadAction)] = &[
            ("clear", ReadAction::Clear),
            ("set", ReadAction::Set),
            ("modify", ReadAction::Modify),
            ("modifyExternal", ReadAction::ModifyExternal),
        ];
        self.keyword(node, "readAction", TABLE, notes)
    }

    fn modified_write_values(
        &mut self,
        node: Node,
        notes: &mut Vec<String>,
    ) -> Option<WriteEffect> {
        const TABLE: &[(&str, WriteEffect)] = &[
            ("oneToClear", WriteEffect::OneToClear),
            ("oneToSet", WriteEffect::OneToSet),
            ("oneToToggle", WriteEffect::OneToToggle),
            ("zeroToClear", WriteEffect::ZeroToClear),
            ("zeroToSet", WriteEffect::ZeroToSet),
            ("zeroToToggle", WriteEffect::ZeroToToggle),
            ("clear", WriteEffect::Clear),
            ("set", WriteEffect::Set),
            ("modify", WriteEffect::Modify),
        ];
        self.keyword(node, "modifiedWriteValues", TABLE, notes)
    }

    fn usage(&mut self, node: Node, notes: &mut Vec<String>) -> Option<Usage> {
        const TABLE: &[(&str, Usage)] = &[
            ("read", Usage::Read),
            ("write", Usage::Write),
            ("read-write", Usage::ReadWrite),
        ];
        self.keyword(node, "usage", TABLE, notes)
    }

    pub fn props(&mut self, node: Node, notes: &mut Vec<String>) -> Props {
        Props {
            size: self.number_u32(node, "size"),
            access: self.access(node, notes),
            reset_value: self.number(node, "resetValue"),
            reset_mask: self.number(node, "resetMask"),
        }
    }

    fn dim(&mut self, node: Node) -> Option<Dim> {
        let count = self.number_u32(node, "dim")?;
        Some(Dim {
            span: self.span(child(node, "dim")?),
            count,
            increment: self.number_u32(node, "dimIncrement"),
            index: self.string(node, "dimIndex"),
        })
    }

    fn marker(&self, node: Node, tag: &str) -> Option<Span> {
        child(node, tag).map(|c| self.span(c))
    }

    fn common(
        &self,
        node: Node,
        name: Option<String>,
        parent: Id,
        children: Vec<Id>,
        notes: Vec<String>,
    ) -> Common {
        let derived_from = node.attribute_node("derivedFrom").map(|attr| {
            let range = attr.range_value();
            Named {
                name: attr.value().trim().to_owned(),
                span: Span::new(self.file, range.start, range.end),
            }
        });
        Common {
            span: self.span(node),
            name_span: self.name_span(node),
            name,
            description: self.string(node, "description"),
            derived_from,
            state: State::Pending,
            parent,
            children,
            notes,
        }
    }

    /// The `peripheral` elements of `peripherals`.
    pub fn peripherals(&mut self, node: Node) {
        for p in children(node, "peripheral") {
            self.peripheral(p);
        }
    }

    fn peripheral(&mut self, node: Node) {
        let Some(name) = self.name(node, "peripheral") else {
            return;
        };
        let mut notes = Vec::new();
        let base_address = self.number(node, "baseAddress");
        let alternate = self
            .child_text(node, "alternatePeripheral")
            .map(|(name, span)| Named {
                name: name.trim().to_owned(),
                span,
            });
        let props = self.props(node, &mut notes);
        let dim = self.dim(node);
        let registers = child(node, "registers");
        let common = self.common(node, Some(name), Id::Device, Vec::new(), notes);
        self.tree.periphs.push(Periph {
            c: common,
            base_address,
            alternate,
            alternate_id: None,
            props,
            dim,
            registers_stated: registers.is_some(),
            shares: None,
        });
        let id = Id::Periph(self.tree.periphs.len() - 1);
        if let Some(registers) = registers {
            let items = self.items(registers, id);
            if let Some(c) = self.tree.common_mut(id) {
                c.children = items;
            }
        }
    }

    /// The registers and clusters of a `registers` or `cluster` element,
    /// in the order of the file.
    fn items(&mut self, node: Node, parent: Id) -> Vec<Id> {
        let mut items = Vec::new();
        for c in node.children().filter(|c| c.is_element()) {
            match c.tag_name().name() {
                "register" => items.extend(self.register(c, parent)),
                "cluster" => items.extend(self.cluster(c, parent)),
                _ => {}
            }
        }
        items
    }

    fn cluster(&mut self, node: Node, parent: Id) -> Option<Id> {
        // A register in it is one level deeper, its field one more, and
        // the set of the field one more: none may pass `MAX_DEPTH`. The
        // check comes before the cluster is read, so the recursion of
        // `items` and `cluster` stops at the cap.
        if self.tree.depth(parent) + 1 + 3 > MAX_DEPTH {
            let span = self.span(node);
            self.rd.error(
                Diagnostic::error(
                    "svd::too-large",
                    format!("clusters are nested too deep: elements stand at most {MAX_DEPTH} levels deep"),
                )
                .label(span, "this cluster is too deep"),
            );
            return None;
        }
        let name = self.name(node, "cluster")?;
        let mut notes = Vec::new();
        let offset = self.number(node, "addressOffset");
        let alternate = self.string(node, "alternateCluster");
        let props = self.props(node, &mut notes);
        let dim = self.dim(node);
        let common = self.common(node, Some(name), parent, Vec::new(), notes);
        self.tree.clusters.push(Cluster {
            c: common,
            offset,
            alternate: alternate.map(|a| a.trim().to_owned()),
            props,
            dim,
        });
        let id = Id::Cluster(self.tree.clusters.len() - 1);
        let items = self.items(node, id);
        if let Some(c) = self.tree.common_mut(id) {
            c.children = items;
        }
        Some(id)
    }

    fn register(&mut self, node: Node, parent: Id) -> Option<Id> {
        let name = self.name(node, "register")?;
        let mut notes = Vec::new();
        let offset = self.number(node, "addressOffset");
        let props = self.props(node, &mut notes);
        let dim = self.dim(node);
        let read_action = self.read_action(node, &mut notes);
        let modified_write_values = self.modified_write_values(node, &mut notes);
        let common = self.common(node, Some(name), parent, Vec::new(), notes);
        self.tree.regs.push(Reg {
            c: common,
            offset,
            props,
            dim,
            alternate_group: self
                .string(node, "alternateGroup")
                .map(|s| s.trim().to_owned()),
            alternate_register: self
                .string(node, "alternateRegister")
                .map(|s| s.trim().to_owned()),
            read_action,
            modified_write_values,
            write_constraint: self.marker(node, "writeConstraint"),
            protection: self.marker(node, "protection"),
        });
        let id = Id::Reg(self.tree.regs.len() - 1);
        if let Some(fields) = child(node, "fields") {
            let mut ids = Vec::new();
            for f in children(fields, "field") {
                ids.extend(self.field(f, id));
            }
            if let Some(c) = self.tree.common_mut(id) {
                c.children = ids;
            }
        }
        Some(id)
    }

    fn field(&mut self, node: Node, parent: Id) -> Option<Id> {
        let name = self.name(node, "field")?;
        let mut notes = Vec::new();
        let bits = self.bits(node);
        let access = self.access(node, &mut notes);
        let read_action = self.read_action(node, &mut notes);
        let modified_write_values = self.modified_write_values(node, &mut notes);
        let dim = self.dim(node);
        let common = self.common(node, Some(name), parent, Vec::new(), notes);
        self.tree.fields.push(Field {
            c: common,
            bits,
            access,
            read_action,
            modified_write_values,
            write_constraint: self.marker(node, "writeConstraint"),
            dim,
        });
        let id = Id::Field(self.tree.fields.len() - 1);
        let mut sets = Vec::new();
        for s in children(node, "enumeratedValues") {
            sets.push(self.set(s, id));
        }
        if let Some(c) = self.tree.common_mut(id) {
            c.children = sets;
        }
        Some(id)
    }

    /// A field's bits, from the first form it states: `bitOffset` and
    /// `bitWidth`, `lsb` and `msb`, or `bitRange` (ADR 0072). `None` when
    /// it states none, or the one it takes is invalid (reported).
    fn bits(&mut self, node: Node) -> Option<Bits> {
        if child(node, "bitOffset").is_some() && child(node, "bitWidth").is_some() {
            let offset = self.number_u32(node, "bitOffset");
            let width = self.number_u32(node, "bitWidth");
            let span = self.span(child(node, "bitWidth")?);
            return self.bits_of(offset?, u64::from(width?), span);
        }
        if child(node, "lsb").is_some() && child(node, "msb").is_some() {
            let lsb = self.number_u32(node, "lsb");
            let msb = self.number_u32(node, "msb");
            let span = self.span(child(node, "msb")?);
            return self.bits_from_range(msb?, lsb?, span);
        }
        let (text, span) = self.child_text(node, "bitRange")?;
        match number::parse_bit_range(&text) {
            Some((msb, lsb)) => self.bits_from_range(msb, lsb, span),
            None => {
                self.rd.error(
                    Diagnostic::error(
                        "svd::invalid-number",
                        format!("`{}` is no `bitRange`", text.trim()),
                    )
                    .label(span, "not `[msb:lsb]`"),
                );
                None
            }
        }
    }

    fn bits_from_range(&mut self, msb: u32, lsb: u32, span: Span) -> Option<Bits> {
        if msb < lsb {
            self.rd.error(
                Diagnostic::error(
                    "svd::invalid-number",
                    format!("the most significant bit {msb} is below the least, {lsb}"),
                )
                .label(span, "a field spans at least one bit"),
            );
            return None;
        }
        self.bits_of(lsb, u64::from(msb - lsb) + 1, span)
    }

    fn bits_of(&mut self, offset: u32, width: u64, span: Span) -> Option<Bits> {
        match u32::try_from(width) {
            Ok(width) if width > 0 => Some(Bits { offset, width }),
            _ => {
                self.rd.error(
                    Diagnostic::error(
                        "svd::invalid-number",
                        format!("a field of {width} bits cannot be read"),
                    )
                    .label(span, "a field spans from 1 to 2^32 - 1 bits"),
                );
                None
            }
        }
    }

    fn set(&mut self, node: Node, parent: Id) -> Id {
        let mut notes = Vec::new();
        let usage = self.usage(node, &mut notes);
        let name = self.string(node, "name").map(|name| name.trim().to_owned());
        let values = children(node, "enumeratedValue")
            .map(|v| self.value(v))
            .collect();
        let common = self.common(node, name, parent, Vec::new(), notes);
        self.tree.sets.push(Set {
            c: common,
            usage,
            base: None,
            values,
        });
        Id::Set(self.tree.sets.len() - 1)
    }

    fn value(&mut self, node: Node) -> Value {
        let value = child(node, "value").and_then(|v| {
            let text = text_of(v);
            match number::parse_enum_number(&text) {
                Ok(n) => Some(n),
                Err(error) => {
                    let span = self.span(v);
                    self.invalid_number(&text, span, "value", error);
                    None
                }
            }
        });
        let is_default = self.is_default(node);
        if child(node, "value").is_none() && !is_default {
            self.missing(node, "enumeratedValue", "value");
        }
        Value {
            span: self.span(node),
            name_span: self.name_span(node),
            name: self
                .string(node, "name")
                .map(|name| name.trim().to_owned())
                .filter(|name| !name.is_empty()),
            description: self.string(node, "description"),
            value,
            is_default,
        }
    }

    /// `isDefault`, an XML Schema boolean; any other text is no boolean
    /// and reads as absent, with the warning `svd::invalid-value`.
    fn is_default(&mut self, node: Node) -> bool {
        let Some((text, span)) = self.child_text(node, "isDefault") else {
            return false;
        };
        match text.trim() {
            "true" | "1" => true,
            "false" | "0" => false,
            other => {
                self.rd.warning(
                    Diagnostic::warning(
                        "svd::invalid-value",
                        format!("`{other}` is no SVD value of `isDefault`; read as absent"),
                    )
                    .label(span, "not `true` or `false`"),
                );
                false
            }
        }
    }
}
