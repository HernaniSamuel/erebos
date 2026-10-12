//! Abstract syntax tree: what the programmer wrote, with spans, and no
//! meaning attached yet.

pub use crate::ops::{BinOp, UnOp};
use crate::span::Span;

#[derive(Debug)]
pub struct File {
    pub items: Vec<Item>,
}

#[derive(Debug)]
pub enum Item {
    Fn(FnDecl),
}

#[derive(Debug)]
pub struct FnDecl {
    pub kind: FnKind,
    pub name: Ident,
    /// `None` when the `-> Type` part is omitted.
    pub ret: Option<Ty>,
    pub body: Block,
    pub span: Span,
}

/// Plain functions are called; the program has exactly one entry function
/// (where execution starts) and one panic function (ADR 0014).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FnKind {
    Plain,
    Entry,
    Panic,
}

#[derive(Debug)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

#[derive(Debug)]
pub struct Ty {
    pub kind: TyKind,
    pub span: Span,
}

#[derive(Debug)]
pub enum TyKind {
    /// `!`
    Never,
    /// `()`
    Unit,
    /// A type written by name, such as `u32`.
    Named(Ident),
    /// `*const T`, `*mut T`, optionally `volatile` (ADR 0013).
    Ptr {
        mutable: bool,
        volatile: bool,
        pointee: Box<Ty>,
    },
}

/// `{ stmts; tail }`. The tail is the block's value (ADR 0015).
#[derive(Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub tail: Option<Box<Expr>>,
    pub span: Span,
}

#[derive(Debug)]
pub enum Stmt {
    Let(Let),
    /// An expression statement. `semi` is the span of its `;`, if written
    /// (block-like expressions such as `loop` may omit it).
    Expr {
        expr: Expr,
        semi: Option<Span>,
    },
}

/// `let [mut] name: Type = init;`
#[derive(Debug)]
pub struct Let {
    pub mutable: bool,
    pub name: Ident,
    pub ty: Ty,
    pub init: Expr,
    pub span: Span,
}

#[derive(Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug)]
pub enum ExprKind {
    /// Integer literal; `value` is `None` if the lexer rejected it.
    Int(Option<u64>),
    /// Byte literal `b'x'`; `None` if the lexer rejected it.
    Byte(Option<u8>),
    /// `()`
    Unit,
    /// A name, such as a local variable.
    Name(Ident),
    Unary(UnOp, Box<Expr>),
    /// `*ptr`
    Deref(Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// `expr as Type`
    Cast(Box<Expr>, Ty),
    /// `place = value`
    Assign(Box<Expr>, Box<Expr>),
    /// `place op= value`
    CompoundAssign(BinOp, Box<Expr>, Box<Expr>),
    Block(Block),
    Loop(Block),
}

impl ExprKind {
    /// Block-like expressions may stand as statements without `;`.
    pub fn is_block_like(&self) -> bool {
        matches!(self, ExprKind::Block(_) | ExprKind::Loop(_))
    }
}
