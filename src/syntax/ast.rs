//! Abstract syntax tree: what the programmer wrote, with spans, and no
//! meaning attached yet.

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
    pub is_entry: bool,
    pub name: Ident,
    /// `None` when the `-> Type` part is omitted.
    pub ret: Option<Ty>,
    pub body: Block,
    pub span: Span,
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
}

#[derive(Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug)]
pub enum Stmt {
    Expr(Expr),
}

#[derive(Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug)]
pub enum ExprKind {
    Loop(Block),
}
