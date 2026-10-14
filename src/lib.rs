//! The Erebos compiler. Layers follow ADR 0003; each module consumes only the
//! output of the layer before it.

pub mod codegen;
pub mod diag;
pub mod driver;
pub mod hir;
pub mod int;
pub mod layout;
pub mod link;
pub mod mir;
pub mod ops;
pub mod span;
pub mod syntax;
pub mod target;
pub mod ty;
