//! High-level IR: the AST after name resolution. Names become ids, written
//! types become resolved [`Ty`]s, and every expression gets an [`ExprId`]
//! that later layers use as a key.

mod lower;

pub use lower::lower;

use std::fmt;

pub use crate::int::IntTy;
pub use crate::ops::{BinOp, UnOp};
use crate::span::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FnId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ExprId(pub u32);

/// A local variable, indexing [`Fn::locals`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LocalId(pub u32);

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FnKind {
    Plain,
    Entry,
    Panic,
}

#[derive(Debug)]
pub struct Fn {
    pub name: String,
    pub name_span: Span,
    pub kind: FnKind,
    pub ret: Ty,
    /// Span of the written return type; `None` when it was omitted.
    pub ret_span: Option<Span>,
    pub locals: Vec<Local>,
    pub body: Block,
    pub span: Span,
}

impl Fn {
    pub fn local(&self, id: LocalId) -> &Local {
        &self.locals[id.0 as usize]
    }
}

#[derive(Debug)]
pub struct Local {
    pub name: String,
    pub ty: Ty,
    pub mutable: bool,
    /// Span of the name in its `let`.
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    Unit,
    Never,
    Int(IntTy),
    /// Produced only by the type checker after reporting an error, so one
    /// mistake does not cascade into more. Compatible with every type.
    Error,
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Unit => f.write_str("()"),
            Ty::Never => f.write_str("!"),
            Ty::Int(int) => write!(f, "{int}"),
            Ty::Error => f.write_str("{error}"),
        }
    }
}

#[derive(Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub tail: Option<Box<Expr>>,
    pub span: Span,
}

#[derive(Debug)]
pub enum Stmt {
    Let {
        local: LocalId,
        init: Expr,
    },
    /// `semi` is the span of the statement's `;`, if written.
    Expr {
        expr: Expr,
        semi: Option<Span>,
    },
}

#[derive(Debug)]
pub struct Expr {
    pub id: ExprId,
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug)]
pub enum ExprKind {
    Int(u64),
    Byte(u8),
    Unit,
    Local(LocalId),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// `expr as ty`; the span is the written type's.
    Cast(Box<Expr>, Ty, Span),
    Assign(Box<Expr>, Box<Expr>),
    CompoundAssign(BinOp, Box<Expr>, Box<Expr>),
    Block(Block),
    Loop(Block),
}

impl Expr {
    /// Calls `f` on this expression and every expression inside it, in
    /// source order.
    pub fn walk(&self, f: &mut impl FnMut(&Expr)) {
        f(self);
        match &self.kind {
            ExprKind::Int(_) | ExprKind::Byte(_) | ExprKind::Unit | ExprKind::Local(_) => {}
            ExprKind::Unary(_, e) | ExprKind::Cast(e, _, _) => e.walk(f),
            ExprKind::Binary(_, a, b)
            | ExprKind::Assign(a, b)
            | ExprKind::CompoundAssign(_, a, b) => {
                a.walk(f);
                b.walk(f);
            }
            ExprKind::Block(block) | ExprKind::Loop(block) => block.walk(f),
        }
    }
}

impl Block {
    pub fn walk(&self, f: &mut impl FnMut(&Expr)) {
        for stmt in &self.stmts {
            match stmt {
                Stmt::Let { init, .. } => init.walk(f),
                Stmt::Expr { expr, .. } => expr.walk(f),
            }
        }
        if let Some(tail) = &self.tail {
            tail.walk(f);
        }
    }
}
