//! High-level IR: the AST after name resolution. Names become ids, written
//! types become resolved [`Ty`]s, and every expression gets an [`ExprId`]
//! that later layers use as a key.

mod lower;

pub use lower::lower;

use std::fmt;

use crate::span::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FnId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ExprId(pub u32);

#[derive(Debug)]
pub struct Hir {
    pub fns: Vec<Fn>,
    pub entry: FnId,
    pub panic: FnId,
    /// Expression ids are `0..expr_count`.
    pub expr_count: u32,
}

impl Hir {
    pub fn fns(&self) -> impl Iterator<Item = (FnId, &Fn)> {
        self.fns
            .iter()
            .enumerate()
            .map(|(i, f)| (FnId(i as u32), f))
    }

    pub fn fn_(&self, id: FnId) -> &Fn {
        &self.fns[id.0 as usize]
    }
}

#[derive(Debug)]
pub struct Fn {
    pub name: String,
    pub name_span: Span,
    pub kind: FnKind,
    pub ret: Ty,
    /// Span of the written return type; `None` when it was omitted.
    pub ret_span: Option<Span>,
    pub body: Block,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FnKind {
    Plain,
    Entry,
    Panic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    Unit,
    Never,
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Ty::Unit => "()",
            Ty::Never => "!",
        })
    }
}

#[derive(Debug)]
pub struct Block {
    pub stmts: Vec<Expr>,
    pub span: Span,
}

#[derive(Debug)]
pub struct Expr {
    pub id: ExprId,
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug)]
pub enum ExprKind {
    Loop(Block),
}
