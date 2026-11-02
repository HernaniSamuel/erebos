//! `derivedFrom` (ADR 0072): where a name is looked up, what is copied from
//! the base, how children merge, and when a derived peripheral shares its
//! base's type.

use std::collections::HashSet;

use crate::diag::Diagnostic;

use super::raw::{Id, MAX_CHAIN, MAX_CLONED, MAX_DEPTH, Named, State, Tree, Value};
use super::read::Reader;

/// Resolves every `derivedFrom` of the tree, and the `alternatePeripheral`
/// of the peripherals, reporting `svd::unknown-reference` and
/// `svd::derived-cycle`.
pub(super) fn resolve(tree: &mut Tree, rd: &mut Reader) {
    let mut resolver = Resolver {
        tree,
        rd,
        stack: Vec::new(),
        walked: HashSet::new(),
    };
    for i in 0..resolver.tree.periphs.len() {
        // The errors are reported; the rest of the file is still resolved.
        let _ = resolver.subtree(Id::Periph(i));
    }
    resolver.alternates();
}

struct Resolver<'a, 'r> {
    tree: &'a mut Tree,
    rd: &'a mut Reader<'r>,
    /// The elements being resolved, the last one the innermost.
    stack: Vec<Id>,
    /// Elements whose whole subtree is resolved.
    walked: HashSet<Id>,
}

/// An error was reported; the caller adds none.
type Failed = ();

impl Resolver<'_, '_> {
    /// An element and everything under it, each resolved once its parent is.
    fn subtree(&mut self, id: Id) -> Result<(), Failed> {
        if self.walked.contains(&id) {
            return Ok(());
        }
        self.element(id)?;
        let children = self.children(id);
        let mut result = Ok(());
        for child in children {
            if self.subtree(child).is_err() {
                result = Err(());
            }
        }
        if result.is_ok() {
            self.walked.insert(id);
        }
        result
    }

    /// One element: its own `derivedFrom`, with its base resolved whole.
    fn element(&mut self, id: Id) -> Result<(), Failed> {
        match self.tree.state(id) {
            State::Done => return Ok(()),
            State::Failed => return Err(()),
            State::Active => {
                self.cycle(id);
                return Err(());
            }
            State::Pending => {}
        }
        let derived = self.tree.common(id).and_then(|c| c.derived_from.clone());
        let Some(derived) = derived else {
            self.tree.set_state(id, State::Done);
            return Ok(());
        };
        if self.stack.len() >= MAX_CHAIN {
            self.rd.error(
                Diagnostic::error(
                    "svd::too-large",
                    format!("more than {MAX_CHAIN} elements wait on a `derivedFrom` at once"),
                )
                .label(derived.span, "one more to resolve first"),
            );
            self.tree.set_state(id, State::Failed);
            return Err(());
        }
        self.tree.set_state(id, State::Active);
        self.stack.push(id);
        let result = self.derive(id, &derived);
        self.stack.pop();
        let state = if result.is_ok() {
            State::Done
        } else {
            State::Failed
        };
        self.tree.set_state(id, state);
        result
    }

    fn derive(&mut self, id: Id, derived: &Named) -> Result<(), Failed> {
        let Some(base) = self.lookup(id, derived)? else {
            let kind = kind_name(id);
            self.rd.error(
                Diagnostic::error(
                    "svd::unknown-reference",
                    format!("`{}` names no {kind}", derived.name),
                )
                .label(derived.span, format!("no {kind} of this name"))
                .help(
                    "a name without `.` is looked up among the elements of the same \
                     kind with the same parent; `.` makes it a path from the device",
                ),
            );
            return Err(());
        };
        self.subtree(base)?;
        self.merge(id, base)
    }

    fn cycle(&mut self, id: Id) {
        let from = self.stack.iter().position(|&s| s == id).unwrap_or(0);
        let mut names: Vec<String> = self.stack[from..]
            .iter()
            .map(|&s| self.tree.name(s).unwrap_or("?").to_owned())
            .collect();
        names.push(names.first().cloned().unwrap_or_default());
        let Some(derived) = self.tree.common(id).and_then(|c| c.derived_from.as_ref()) else {
            return;
        };
        let diag = Diagnostic::error(
            "svd::derived-cycle",
            format!("`{}` derives from itself", names[0]),
        )
        .label(derived.span, names.join(" -> "));
        self.rd.error(diag);
    }

    fn children(&self, id: Id) -> Vec<Id> {
        match id {
            Id::Device => (0..self.tree.periphs.len()).map(Id::Periph).collect(),
            _ => self
                .tree
                .common(id)
                .map(|c| c.children.clone())
                .unwrap_or_default(),
        }
    }

    /// The element `derived` names for `id`: `Ok(None)` when it names none.
    fn lookup(&mut self, id: Id, derived: &Named) -> Result<Option<Id>, Failed> {
        if !derived.name.contains('.') {
            let candidates = match id {
                Id::Set(_) => {
                    // The sets of every field of the same register.
                    let register = self.tree.parent(self.tree.parent(id));
                    self.children(register)
                        .into_iter()
                        .flat_map(|field| self.children(field))
                        .collect()
                }
                _ => self.children(self.tree.parent(id)),
            };
            return Ok(self.named(&candidates, &derived.name, id));
        }
        let segments: Vec<&str> = derived.name.split('.').collect();
        let mut current = Id::Device;
        for (i, segment) in segments.iter().enumerate() {
            if current != Id::Device {
                // The children of an element are final once it is resolved.
                self.element(current)?;
            }
            let candidates = self.children(current);
            let found = if i + 1 == segments.len() {
                self.named(&candidates, segment, id)
            } else {
                candidates
                    .into_iter()
                    .find(|&c| self.tree.name(c) == Some(segment))
            };
            let Some(found) = found else {
                return Ok(None);
            };
            current = found;
        }
        Ok(Some(current))
    }

    /// The first of `candidates` of the kind of `like` with this `name`.
    fn named(&self, candidates: &[Id], name: &str, like: Id) -> Option<Id> {
        candidates
            .iter()
            .copied()
            .find(|&c| same_kind(c, like) && self.tree.name(c) == Some(name))
    }

    /// Makes `id` what `base` is, `id`'s own over it: the properties `id`
    /// states stay, the others are the base's; the children are the base's
    /// copies, those of `id` replacing the one with their name where the
    /// base had it and added after the others.
    fn merge(&mut self, id: Id, base: Id) -> Result<(), Failed> {
        let tree = &mut *self.tree;
        match (id, base) {
            (Id::Periph(a), Id::Periph(b)) => {
                let base = tree.periphs[b].clone();
                let p = &mut tree.periphs[a];
                p.shares =
                    (!p.registers_stated && p.props.is_empty()).then(|| base.shares.unwrap_or(b));
                p.base_address = p.base_address.or(base.base_address);
                p.alternate = p.alternate.take().or(base.alternate);
                p.props = p.props.over(base.props);
                p.dim = p.dim.take().or(base.dim);
                p.c.description = p.c.description.take().or(base.c.description);
            }
            (Id::Cluster(a), Id::Cluster(b)) => {
                let base = tree.clusters[b].clone();
                let c = &mut tree.clusters[a];
                c.offset = c.offset.or(base.offset);
                c.alternate = c.alternate.take().or(base.alternate);
                c.props = c.props.over(base.props);
                c.dim = c.dim.take().or(base.dim);
                c.c.description = c.c.description.take().or(base.c.description);
            }
            (Id::Reg(a), Id::Reg(b)) => {
                let base = tree.regs[b].clone();
                let r = &mut tree.regs[a];
                r.offset = r.offset.or(base.offset);
                r.props = r.props.over(base.props);
                r.dim = r.dim.take().or(base.dim);
                r.alternate_group = r.alternate_group.take().or(base.alternate_group);
                r.alternate_register = r.alternate_register.take().or(base.alternate_register);
                r.read_action = r.read_action.or(base.read_action);
                r.modified_write_values = r.modified_write_values.or(base.modified_write_values);
                r.write_constraint = r.write_constraint.or(base.write_constraint);
                r.protection = r.protection.or(base.protection);
                r.c.description = r.c.description.take().or(base.c.description);
            }
            (Id::Field(a), Id::Field(b)) => {
                let base = tree.fields[b].clone();
                let f = &mut tree.fields[a];
                f.bits = f.bits.or(base.bits);
                f.access = f.access.or(base.access);
                f.read_action = f.read_action.or(base.read_action);
                f.modified_write_values = f.modified_write_values.or(base.modified_write_values);
                f.write_constraint = f.write_constraint.or(base.write_constraint);
                f.dim = f.dim.take().or(base.dim);
                f.c.description = f.c.description.take().or(base.c.description);
            }
            (Id::Set(a), Id::Set(b)) => {
                let base_set = tree.sets[b].clone();
                let s = &mut tree.sets[a];
                s.usage = s.usage.or(base_set.usage);
                s.base = Some(b);
                s.c.description = s.c.description.take().or(base_set.c.description);
                let own = std::mem::take(&mut s.values);
                s.values = merge_values(base_set.values, own);
            }
            _ => return Ok(()),
        }
        self.merge_children(id, base)
    }

    /// Fails with `svd::too-large` when the copies would pass
    /// [`MAX_CLONED`].
    fn merge_children(&mut self, id: Id, base: Id) -> Result<(), Failed> {
        // The copies stand below `id`: the deepest is as far below it as
        // the base's children go.
        if self.tree.depth(id) + self.tree.height(base) > MAX_DEPTH {
            self.report_too_deep(id, base);
            return Err(());
        }
        let copies: usize = self
            .children(base)
            .into_iter()
            .map(|child| self.tree.size(child))
            .sum();
        if self.tree.cloned.saturating_add(copies) > MAX_CLONED {
            self.too_large(id, base);
            return Err(());
        }
        let own = self.children(id);
        let mut merged: Vec<Id> = self
            .children(base)
            .into_iter()
            .map(|child| self.tree.clone_deep(child, id))
            .collect();
        let from_base = merged.len();
        for child in own {
            let at = self.tree.name(child).and_then(|name| {
                merged[..from_base]
                    .iter()
                    .position(|&m| self.tree.name(m) == Some(name))
            });
            match at {
                Some(i) => merged[i] = child,
                None => merged.push(child),
            }
        }
        if let Some(c) = self.tree.common_mut(id) {
            c.children = merged;
        }
        Ok(())
    }

    fn report_too_deep(&mut self, id: Id, base: Id) {
        let Some(derived) = self.tree.common(id).and_then(|c| c.derived_from.clone()) else {
            return;
        };
        let name = self.tree.name(id).unwrap_or("?").to_owned();
        self.rd.error(
            Diagnostic::error(
                "svd::too-large",
                format!(
                    "`{name}` would hold the {} levels of `{}`, and elements stand at most \
                     {MAX_DEPTH} levels deep",
                    self.tree.height(base),
                    derived.name
                ),
            )
            .label(derived.span, "copied here")
            .help("each `derivedFrom` copies its base's children, one level below"),
        );
    }

    fn too_large(&mut self, id: Id, base: Id) {
        let Some(derived) = self.tree.common(id).and_then(|c| c.derived_from.clone()) else {
            return;
        };
        let name = self.tree.name(id).unwrap_or("?").to_owned();
        self.rd.error(
            Diagnostic::error(
                "svd::too-large",
                format!(
                    "`{name}` copies the {} elements of `{}`, and the copies of the file \
                     would pass {MAX_CLONED}",
                    self.tree.size(base) - 1,
                    derived.name
                ),
            )
            .label(derived.span, "copied here")
            .help(
                "each `derivedFrom` copies its base's children; a chain of them doubles the tree",
            ),
        );
    }

    /// Finds the peripheral each `alternatePeripheral` names, among the
    /// peripherals as a `derivedFrom` without `.` is.
    fn alternates(&mut self) {
        for i in 0..self.tree.periphs.len() {
            let Some(alternate) = self.tree.periphs[i].alternate.clone() else {
                continue;
            };
            let found = self
                .tree
                .periphs
                .iter()
                .position(|p| p.c.name.as_deref() == Some(&alternate.name));
            match found {
                Some(j) => self.tree.periphs[i].alternate_id = Some(j),
                None => self.rd.error(
                    Diagnostic::error(
                        "svd::unknown-reference",
                        format!("`{}` names no peripheral", alternate.name),
                    )
                    .label(alternate.span, "no peripheral of this name"),
                ),
            }
        }
    }
}

/// The values of a derived set: the base's, those of the derived set with
/// the name of one replacing it, the others added after. A value without a
/// name replaces none.
fn merge_values(base: Vec<Value>, own: Vec<Value>) -> Vec<Value> {
    let from_base = base.len();
    let mut merged = base;
    for value in own {
        let at = value.name.as_ref().and_then(|name| {
            merged[..from_base]
                .iter()
                .position(|v| v.name.as_ref() == Some(name))
        });
        match at {
            Some(i) => merged[i] = value,
            None => merged.push(value),
        }
    }
    merged
}

fn same_kind(a: Id, b: Id) -> bool {
    std::mem::discriminant(&a) == std::mem::discriminant(&b)
}

fn kind_name(id: Id) -> &'static str {
    match id {
        Id::Device => "device",
        Id::Periph(_) => "peripheral",
        Id::Cluster(_) => "cluster",
        Id::Reg(_) => "register",
        Id::Field(_) => "field",
        Id::Set(_) => "set of enumerated values",
    }
}
