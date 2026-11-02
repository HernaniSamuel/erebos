//! The SVD file as the XML states it, before `derivedFrom` is resolved and
//! properties are inherited: an arena of elements that refer to each other
//! by [`Id`], so that a derived element can copy the subtree of its base.

use crate::span::Span;

use super::{Access, EnumNumber, ReadAction, Usage, WriteEffect};

/// An element of the tree, or the device itself, whose children are
/// [`Tree::peripherals`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Id {
    Device,
    Periph(usize),
    Cluster(usize),
    Reg(usize),
    Field(usize),
    Set(usize),
}

/// Where an element stands in resolving its `derivedFrom`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum State {
    Pending,
    /// Being resolved: reaching it again is a cycle.
    Active,
    Done,
    /// Reported as an error; what depends on it is not resolved again.
    Failed,
}

/// A name written in the file, and where.
#[derive(Clone, Debug)]
pub(super) struct Named {
    pub name: String,
    pub span: Span,
}

/// What every element has.
#[derive(Clone, Debug)]
pub(super) struct Common {
    pub span: Span,
    /// The `name` element, or the element itself when it has none.
    pub name_span: Span,
    /// Only a set of enumerated values may have none.
    pub name: Option<String>,
    pub description: Option<String>,
    pub derived_from: Option<Named>,
    pub state: State,
    pub parent: Id,
    pub children: Vec<Id>,
    pub notes: Vec<String>,
}

/// The properties a register inherits from the elements it is in.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Props {
    pub size: Option<u32>,
    pub access: Option<Access>,
    pub reset_value: Option<u64>,
    pub reset_mask: Option<u64>,
}

impl Props {
    pub fn is_empty(&self) -> bool {
        self.size.is_none()
            && self.access.is_none()
            && self.reset_value.is_none()
            && self.reset_mask.is_none()
    }

    /// `self` over `base`: each property `self` states, else `base`'s.
    pub fn over(self, base: Props) -> Props {
        Props {
            size: self.size.or(base.size),
            access: self.access.or(base.access),
            reset_value: self.reset_value.or(base.reset_value),
            reset_mask: self.reset_mask.or(base.reset_mask),
        }
    }
}

/// `dim`, `dimIncrement` and `dimIndex` of an element, which derive as one.
#[derive(Clone, Debug)]
pub(super) struct Dim {
    /// The element that states `dim`.
    pub span: Span,
    pub count: u32,
    pub increment: Option<u32>,
    pub index: Option<String>,
}

#[derive(Clone, Debug)]
pub(super) struct Periph {
    pub c: Common,
    pub base_address: Option<u64>,
    pub alternate: Option<Named>,
    /// The peripheral `alternate` names, once found.
    pub alternate_id: Option<usize>,
    pub props: Props,
    pub dim: Option<Dim>,
    /// Whether the element has a `registers` element of its own.
    pub registers_stated: bool,
    pub shares: Option<usize>,
}

#[derive(Clone, Debug)]
pub(super) struct Cluster {
    pub c: Common,
    pub offset: Option<u64>,
    pub alternate: Option<String>,
    pub props: Props,
    pub dim: Option<Dim>,
}

#[derive(Clone, Debug)]
pub(super) struct Reg {
    pub c: Common,
    pub offset: Option<u64>,
    pub props: Props,
    pub dim: Option<Dim>,
    pub alternate_group: Option<String>,
    pub alternate_register: Option<String>,
    pub read_action: Option<ReadAction>,
    pub modified_write_values: Option<WriteEffect>,
    pub write_constraint: Option<Span>,
    pub protection: Option<Span>,
}

#[derive(Clone, Debug)]
pub(super) struct Field {
    pub c: Common,
    pub bits: Option<super::Bits>,
    pub access: Option<Access>,
    pub read_action: Option<ReadAction>,
    pub modified_write_values: Option<WriteEffect>,
    pub write_constraint: Option<Span>,
    pub dim: Option<Dim>,
}

#[derive(Clone, Debug)]
pub(super) struct Set {
    pub c: Common,
    pub usage: Option<Usage>,
    /// The set `derivedFrom` names, once found.
    pub base: Option<usize>,
    pub values: Vec<Value>,
}

#[derive(Clone, Debug)]
pub(super) struct Value {
    pub span: Span,
    /// The `name` element, or the element itself when it has none.
    pub name_span: Span,
    pub name: Option<String>,
    pub description: Option<String>,
    pub value: Option<EnumNumber>,
    pub is_default: bool,
}

/// Every element of the file. A copy made for a derived element is added
/// to it, the original staying where it is.
#[derive(Default)]
pub(super) struct Tree {
    pub periphs: Vec<Periph>,
    pub clusters: Vec<Cluster>,
    pub regs: Vec<Reg>,
    pub fields: Vec<Field>,
    pub sets: Vec<Set>,
    /// How many elements [`Tree::clone_deep`] made, against
    /// [`MAX_CLONED`].
    pub cloned: usize,
}

/// The most elements the copies made for derived elements may add up to
/// (ADR 0072 copies the base's children): `derivedFrom` can make a file of
/// a few kilobytes double its tree at each level, and a hostile file would
/// exhaust memory. The real files copy a few thousand.
pub(super) const MAX_CLONED: usize = 1 << 18;

/// The deepest an element may stand, the peripheral being at depth 1 (so
/// a register of a peripheral is at 2, and a register in a cluster of a
/// peripheral at 3). The walks over the tree recurse, and a file (or the
/// copies of `derivedFrom`) must not make them overflow the stack. The real
/// files go down to 6: peripheral, cluster, cluster, register, field, set.
pub(super) const MAX_DEPTH: usize = 32;

/// The most `derivedFrom` an element may wait on at once: resolving one
/// resolves its base first, which recurses.
pub(super) const MAX_CHAIN: usize = 32;

impl Tree {
    pub fn common(&self, id: Id) -> Option<&Common> {
        Some(match id {
            Id::Device => return None,
            Id::Periph(i) => &self.periphs[i].c,
            Id::Cluster(i) => &self.clusters[i].c,
            Id::Reg(i) => &self.regs[i].c,
            Id::Field(i) => &self.fields[i].c,
            Id::Set(i) => &self.sets[i].c,
        })
    }

    pub fn common_mut(&mut self, id: Id) -> Option<&mut Common> {
        Some(match id {
            Id::Device => return None,
            Id::Periph(i) => &mut self.periphs[i].c,
            Id::Cluster(i) => &mut self.clusters[i].c,
            Id::Reg(i) => &mut self.regs[i].c,
            Id::Field(i) => &mut self.fields[i].c,
            Id::Set(i) => &mut self.sets[i].c,
        })
    }

    /// The SVD `name` of an element.
    pub fn name(&self, id: Id) -> Option<&str> {
        self.common(id)?.name.as_deref()
    }

    pub fn state(&self, id: Id) -> State {
        self.common(id).map_or(State::Done, |c| c.state)
    }

    pub fn set_state(&mut self, id: Id, state: State) {
        if let Some(c) = self.common_mut(id) {
            c.state = state;
        }
    }

    pub fn parent(&self, id: Id) -> Id {
        self.common(id).map_or(Id::Device, |c| c.parent)
    }

    /// How deep an element stands: the peripheral is at depth 1.
    pub fn depth(&self, id: Id) -> usize {
        let mut depth = 0;
        let mut at = id;
        while at != Id::Device {
            depth += 1;
            at = self.parent(at);
        }
        depth
    }

    /// How many levels there are below an element: 0 for one with no
    /// children. Only for trees that respect [`MAX_DEPTH`].
    pub fn height(&self, id: Id) -> usize {
        self.common(id).map_or(0, |c| {
            c.children
                .iter()
                .map(|&child| 1 + self.height(child))
                .max()
                .unwrap_or(0)
        })
    }

    /// The element and everything under it, counted.
    pub fn size(&self, id: Id) -> usize {
        let below = self.common(id).map_or(0, |c| {
            c.children.iter().map(|&child| self.size(child)).sum()
        });
        1 + below
    }

    /// A copy of an element and all it holds, as a child of `parent`, taken
    /// of an element already resolved.
    pub fn clone_deep(&mut self, id: Id, parent: Id) -> Id {
        let children = self.common(id).map(|c| c.children.clone());
        self.cloned += 1;
        let new = match id {
            Id::Device => return Id::Device,
            Id::Periph(i) => {
                let element = self.periphs[i].clone();
                self.periphs.push(element);
                Id::Periph(self.periphs.len() - 1)
            }
            Id::Cluster(i) => {
                let element = self.clusters[i].clone();
                self.clusters.push(element);
                Id::Cluster(self.clusters.len() - 1)
            }
            Id::Reg(i) => {
                let element = self.regs[i].clone();
                self.regs.push(element);
                Id::Reg(self.regs.len() - 1)
            }
            Id::Field(i) => {
                let element = self.fields[i].clone();
                self.fields.push(element);
                Id::Field(self.fields.len() - 1)
            }
            Id::Set(i) => {
                let element = self.sets[i].clone();
                self.sets.push(element);
                Id::Set(self.sets.len() - 1)
            }
        };
        let copies: Vec<Id> = children
            .unwrap_or_default()
            .into_iter()
            .map(|child| self.clone_deep(child, new))
            .collect();
        if let Some(c) = self.common_mut(new) {
            c.parent = parent;
            c.children = copies;
            c.state = State::Done;
            // A note belongs to the element that states the value.
            c.notes.clear();
        }
        new
    }
}
