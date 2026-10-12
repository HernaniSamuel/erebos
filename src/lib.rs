//! The Erebos compiler. Layers follow ADR 0003; each module consumes only the
//! output of the layer before it.

pub mod diag;
pub mod driver;
pub mod span;
pub mod syntax;
