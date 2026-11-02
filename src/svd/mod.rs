//! `erebos svd`: peripherals generated from SVD files (ADR 0070 to 0072).
//!
//! This module is no layer of the pipeline: no layer uses it, and it uses
//! the shared vocabulary (`span`, `diag`) and, for the writer, `syntax`'s
//! table of keywords. The reader turns an SVD file into the [`Device`]
//! below, with the span of every element in the SVD file, and holds no
//! Erebos names or syntax: those are the writer's.
//!
//! The model is what the file says after the reader applied SVD's own
//! rules (ADR 0072): properties inherited from device to register,
//! `derivedFrom` resolved (children copied and merged), `dim` read into
//! an array or a list. Names are the SVD's, `%s` and `[%s]` included.
//!
//! The writer turns that model into Erebos source: [`generate`].

mod build;
mod derive;
mod names;
mod number;
mod over;
mod parse;
mod raw;
mod read;
mod text;
mod write;

use crate::diag::Diagnostics;
use crate::span::{SourceMap, Span};

pub use read::{read, utf8};

/// The name `erebos svd` gives its output when none is asked for: the
/// input's file name without its last extension, if it has one, made a
/// module's name by ADR 0071's steps, with the extension `.ers` (ADR 0070).
/// `None` when nothing is left of the name for an identifier.
pub fn default_output_name(input_file_name: &str) -> Option<String> {
    let stem = std::path::Path::new(input_file_name)
        .file_stem()?
        .to_string_lossy();
    names::module_name(&stem).map(|name| format!("{name}.ers"))
}

/// The Erebos module generated from the SVD file called `name`, whose text
/// is `text` (ADR 0070): `None` when an error was reported. The file is
/// added to the returned source map, which the diagnostics point into; its
/// name is the one the generated file's header gives. Warnings are among the
/// diagnostics, and leave the text. A file that is not UTF-8 is reported by
/// [`utf8`] before it gets here.
pub fn generate(name: &str, text: &str) -> (Option<String>, Diagnostics, SourceMap) {
    let mut sources = SourceMap::default();
    let mut diags = Diagnostics::default();
    let file = sources.add(name, text);
    let output = read(file, text, &mut diags).map(|device| {
        let mut rd = read::Reader::new(&mut diags);
        let output = write::write(name, &device, &mut rd);
        (output, rd.failed())
    });
    let output = match output {
        Some((output, false)) => Some(output),
        _ => None,
    };
    (output, diags, sources)
}

/// A device: the file's `device` element.
#[derive(Clone, Debug)]
pub struct Device {
    pub span: Span,
    pub name: String,
    pub version: Option<String>,
    /// The text of `licenseText`, as the XML gave it.
    pub license_text: Option<String>,
    /// In the order of the file.
    pub peripherals: Vec<Peripheral>,
}

#[derive(Clone, Debug)]
pub struct Peripheral {
    pub span: Span,
    /// The SVD `name`, `%s` included when the peripheral has a `dim`.
    pub name: String,
    pub description: Option<String>,
    pub base_address: u64,
    /// One peripheral per index (a [`Repeat::List`], never an array: ADR 0072),
    /// at `base_address + position * step`.
    pub repeat: Option<Repeat>,
    /// The index in [`Device::peripherals`] of the peripheral named by
    /// `alternatePeripheral`.
    pub alternate: Option<usize>,
    /// The index of the peripheral whose type this one has, when it
    /// derives from one and states nothing that changes it (ADR 0072): the
    /// last of the chain, which has a type of its own. Then `items` is
    /// empty: the registers are the other peripheral's.
    pub shares_type_with: Option<usize>,
    /// Registers and clusters, in the order of the file.
    pub items: Vec<Item>,
    /// What the reader did with the element's values SVD does not define.
    pub notes: Vec<String>,
}

#[derive(Clone, Debug)]
pub enum Item {
    Register(Register),
    Cluster(Cluster),
}

#[derive(Clone, Debug)]
pub struct Cluster {
    pub span: Span,
    pub name: String,
    pub description: Option<String>,
    /// From the start of the peripheral (or of the cluster it is in).
    pub offset: u64,
    pub repeat: Option<Repeat>,
    /// The `alternateCluster`, as written (it is not looked up).
    pub alternate_cluster: Option<String>,
    pub items: Vec<Item>,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Register {
    pub span: Span,
    pub name: String,
    pub description: Option<String>,
    pub offset: u64,
    pub repeat: Option<Repeat>,
    /// In bits, inherited from the levels above; any number (the writer
    /// judges it).
    pub size: u32,
    /// Inherited from the levels above; `None` when none states one.
    pub access: Option<Access>,
    /// Inherited, and cut to `size` bits; `None` when no level states one.
    pub reset_value: Option<u64>,
    /// Inherited, cut to `size` bits; every bit when no level states one.
    pub reset_mask: u64,
    pub alternate_group: Option<String>,
    pub alternate_register: Option<String>,
    pub read_action: Option<ReadAction>,
    pub modified_write_values: Option<WriteEffect>,
    /// Where the register states a `writeConstraint`, which has no
    /// declaration.
    pub write_constraint: Option<Span>,
    /// Where it states a `protection`, which has none.
    pub protection: Option<Span>,
    pub fields: Vec<Field>,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Field {
    pub span: Span,
    pub name: String,
    pub description: Option<String>,
    /// One field per index (a [`Repeat::List`]), at `bits.offset + position
    /// * step`.
    pub repeat: Option<Repeat>,
    pub bits: Bits,
    /// The field's own; its register's applies when it states none.
    pub access: Option<Access>,
    pub read_action: Option<ReadAction>,
    /// The field's own; the writer takes its register's when it has none.
    pub modified_write_values: Option<WriteEffect>,
    pub write_constraint: Option<Span>,
    /// The sets of enumerated values, in the order of the file.
    pub enums: Vec<EnumSet>,
    pub notes: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bits {
    pub offset: u32,
    pub width: u32,
}

/// One `enumeratedValues` element.
#[derive(Clone, Debug)]
pub struct EnumSet {
    pub span: Span,
    /// Names this set among the sets of the model: the one a set with
    /// `derivedFrom` points to is [`EnumSet::derived_from`].
    pub id: SetId,
    pub name: Option<String>,
    pub usage: Option<Usage>,
    /// The set this one derives from, whose values were copied into
    /// `values` and merged with the ones the set states itself; a copy of a
    /// derived element made for another peripheral keeps pointing at the
    /// original.
    pub derived_from: Option<SetId>,
    pub values: Vec<EnumValue>,
    pub notes: Vec<String>,
}

/// The identity of an [`EnumSet`]: two sets with one id are one set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SetId(pub usize);

#[derive(Clone, Debug)]
pub struct EnumValue {
    pub span: Span,
    /// `None` for an `enumeratedValue` without a `name`.
    pub name: Option<String>,
    pub description: Option<String>,
    /// `None` when the value only has `isDefault`.
    pub value: Option<EnumNumber>,
    pub is_default: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnumNumber {
    Value(u64),
    /// A value with don't-care bits (`#1x0`), as the file wrote it.
    DontCare(String),
}

/// How an element repeats (`dim`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Repeat {
    /// One element standing for `count` of them (a register or a cluster
    /// whose name has `[%s]` and whose `dimIndex` is `0` to `count - 1`, or
    /// absent), `step` bytes apart.
    Array { count: u32, step: u32 },
    /// One element per index, the `n`th at `n * step` from the first
    /// (bytes, or bits for a field): the indices from `dimIndex`, or `0` to
    /// `dim - 1`.
    List { indices: Vec<String>, step: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    ReadOnly,
    WriteOnly,
    ReadWrite,
    WriteOnce,
    ReadWriteOnce,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadAction {
    Clear,
    Set,
    Modify,
    ModifyExternal,
}

/// `modifiedWriteValues`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteEffect {
    OneToClear,
    OneToSet,
    OneToToggle,
    ZeroToClear,
    ZeroToSet,
    ZeroToToggle,
    Clear,
    Set,
    Modify,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Usage {
    Read,
    Write,
    ReadWrite,
}
