//! The chip's interrupts as an enum (ADR 0092): the last item of the
//! generated file, `#[interrupts] pub enum Interrupt: u32 { ... }`.
//!
//! The reader gives every `interrupt` element in the order of the file. Here
//! they are put in the order of their values, merged where they name one line
//! (names are compared after ADR 0071's conversion, so this is the writer's),
//! and named.

use std::collections::{HashMap, HashSet};

use crate::diag::Diagnostic;

use super::Interrupt;
use super::names::{self, Case, Converted, Index, Scope};
use super::text::comment;
use super::write::{INDENT, Of, Writer, describe, join};

/// The name of the enum.
const ENUM_NAME: &str = "Interrupt";

/// A variant of the enum, from the first element that gave it.
struct Variant<'a> {
    /// The element it was written for.
    from: &'a Interrupt,
    /// The name converted by ADR 0071's steps 1 to 5; `None` when nothing was
    /// left of the SVD name, which `claim` reports.
    converted: Option<Converted>,
}

impl Writer<'_, '_> {
    /// Takes the name of the enum in the file's scope, `None` when the file
    /// has no interrupt and so writes none. It is taken after every module
    /// and peripheral, so a peripheral named `Interrupt` keeps the name and
    /// the enum is `Interrupt_`, with `svd::renamed`.
    pub(super) fn name_interrupt_enum(
        &mut self,
        top: &mut Scope,
        interrupts: &[Interrupt],
    ) -> Option<String> {
        let first = interrupts.first()?;
        let of = Of {
            svd: ENUM_NAME,
            span: first.name_span,
            what: "enum of the interrupts",
            report_changes: false,
        };
        let converted = Converted {
            name: ENUM_NAME.to_owned(),
            changed: false,
        };
        self.claim(top, of, Some(converted))
    }

    /// The enum as lines: the `// svd:` comments of the interrupts left out,
    /// the attribute, and the variants in the order of their values.
    pub(super) fn interrupt_block(&mut self, name: &str, interrupts: &[Interrupt]) -> Vec<String> {
        let (variants, left_out) = self.merge_interrupts(interrupts);

        let mut lines: Vec<String> = left_out
            .iter()
            .flat_map(|text| comment(0, "// svd: ", text))
            .collect();
        lines.push("#[interrupts]".to_owned());
        lines.push(format!("pub enum {name}: u32 {{"));

        let mut scope = Scope::default();
        let margin = " ".repeat(INDENT);
        let mut blocks: Vec<Vec<String>> = Vec::new();
        for Variant { from, converted } in variants {
            let of = Of {
                svd: &from.name,
                span: from.name_span,
                what: "interrupt",
                report_changes: true,
            };
            let Some(variant) = self.claim(&mut scope, of, converted) else {
                continue;
            };
            let mut block = describe(INDENT, from.description.as_deref());
            block.push(format!("{margin}{variant} = {},", from.value));
            blocks.push(block);
        }
        lines.extend(join(blocks, false));
        lines.push("}".to_owned());
        lines
    }

    /// The elements in the order of their values (the file's order where two
    /// are equal), as the variants to write, and what was left out as the
    /// texts of the `// svd:` comments.
    ///
    /// An element whose converted name and value are another's is that line
    /// again, as vendors repeat a shared line under each peripheral that uses
    /// it: it is merged, silently. One whose value is another's under another
    /// converted name is left out, since two variants with one value are
    /// `ty::duplicate-discriminant` (ADR 0035): `svd::not-declared`. One whose
    /// name is another's under another value stays, and the scope of the
    /// variants renames the later (ADR 0071).
    fn merge_interrupts<'i>(
        &mut self,
        interrupts: &'i [Interrupt],
    ) -> (Vec<Variant<'i>>, Vec<String>) {
        let mut ordered: Vec<&Interrupt> = interrupts.iter().collect();
        ordered.sort_by_key(|interrupt| interrupt.value);

        let mut variants: Vec<Variant> = Vec::new();
        let mut left_out: Vec<String> = Vec::new();
        // Each (name, value) met, kept or left out.
        let mut met: HashSet<(String, u64)> = HashSet::new();
        // The element that has each value, with its SVD name.
        let mut owner: HashMap<u64, &Interrupt> = HashMap::new();
        for interrupt in ordered {
            let converted = names::convert(&interrupt.name, Index::Whole, Case::Pascal);
            // A name with nothing left is never merged: each one is an error.
            if let Some(converted) = &converted {
                if !met.insert((converted.name.clone(), interrupt.value)) {
                    continue;
                }
                if let Some(first) = owner.get(&interrupt.value) {
                    let subject = format!("interrupt `{}`", interrupt.name);
                    let what = format!(
                        "left out: the interrupt `{}` has the value {}",
                        first.name, interrupt.value
                    );
                    self.rd.warning(
                        Diagnostic::warning("svd::not-declared", format!("{subject}: {what}"))
                            .label(interrupt.name_span, "not declared")
                            .label(first.name_span, "has the value"),
                    );
                    left_out.push(format!("`{}` is {what}", interrupt.name));
                    continue;
                }
            }
            if converted.is_some() {
                owner.insert(interrupt.value, interrupt);
            }
            variants.push(Variant {
                from: interrupt,
                converted,
            });
        }
        (variants, left_out)
    }
}
