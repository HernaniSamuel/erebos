//! Abstract syntax tree: what the programmer wrote, with spans, and no
//! meaning attached yet.

pub use crate::ops::{BinOp, LogicalOp, UnOp};
use crate::span::Span;

#[derive(Debug)]
pub struct File {
    pub items: Vec<Item>,
}

#[derive(Debug)]
pub enum Item {
    Fn(FnDecl),
    Struct(StructDecl),
    Const(ConstDecl),
    Comptime(ComptimeDecl),
}

/// `const NAME: Type = value;` (ADR 0029)
#[derive(Debug)]
pub struct ConstDecl {
    pub attrs: Vec<Attr>,
    pub name: Ident,
    pub ty: Ty,
    pub value: Expr,
    pub span: Span,
}

/// `comptime { ... }` at item level, run when the program is compiled
/// (ADR 0029).
#[derive(Debug)]
pub struct ComptimeDecl {
    pub attrs: Vec<Attr>,
    pub block: Block,
    pub span: Span,
}

/// `struct Name { field: Type, ... }` (ADR 0021)
#[derive(Debug)]
pub struct StructDecl {
    pub attrs: Vec<Attr>,
    pub name: Ident,
    pub fields: Vec<FieldDecl>,
    pub span: Span,
}

#[derive(Debug)]
pub struct FieldDecl {
    pub name: Ident,
    pub ty: Ty,
}

/// `#[name]` or `#[name(N)]` before an item (ADR 0022).
#[derive(Debug)]
pub struct Attr {
    pub name: Ident,
    /// The argument, an expression computed at compile time (ADR 0029).
    pub arg: Option<Box<Expr>>,
    pub span: Span,
}

#[derive(Debug)]
pub struct FnDecl {
    pub attrs: Vec<Attr>,
    pub kind: FnKind,
    pub name: Ident,
    pub params: Vec<Param>,
    /// `None` when the `-> Type` part is omitted.
    pub ret: Option<Ty>,
    /// `None` for an `extern fn`, defined outside the program (ADR 0026).
    pub body: Option<Block>,
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

/// `[mut] name: Type`
#[derive(Debug)]
pub struct Param {
    pub mutable: bool,
    pub name: Ident,
    pub ty: Ty,
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
    /// `[T; N]`, where `N` is computed at compile time (ADR 0029).
    Array { elem: Box<Ty>, len: Box<Expr> },
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
    /// `true` or `false`
    Bool(bool),
    /// `()`
    Unit,
    /// `[a, b, c]`
    Array(Vec<Expr>),
    /// `[value; N]`, where `N` is computed at compile time (ADR 0029).
    Repeat(Box<Expr>, Box<Expr>),
    /// A name, such as a local variable.
    Name(Ident),
    /// `name(args)`: a call to a function.
    Call(Ident, Vec<Expr>),
    /// `Name { field: value, ... }`, fields in the order written.
    StructLit(Ident, Vec<FieldInit>),
    /// `base.field`
    Field(Box<Expr>, Ident),
    /// `size_of(T)` (ADR 0022)
    SizeOf(Ty),
    /// `align_of(T)` (ADR 0022)
    AlignOf(Ty),
    /// `offset_of(T, field)` (ADR 0022)
    OffsetOf(Ty, Ident),
    /// `comptime expr`, evaluated by the compiler (ADR 0029)
    Comptime(Box<Expr>),
    Unary(UnOp, Box<Expr>),
    /// `*ptr`
    Deref(Box<Expr>),
    /// `base[index]`
    Index(Box<Expr>, Box<Expr>),
    /// `&raw const place` or `&raw mut place` (ADR 0021)
    AddrOf {
        mutable: bool,
        place: Box<Expr>,
    },
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// `&&` and `||`, which short-circuit.
    Logical(LogicalOp, Box<Expr>, Box<Expr>),
    /// `expr as Type`
    Cast(Box<Expr>, Ty),
    /// `place = value`
    Assign(Box<Expr>, Box<Expr>),
    /// `place op= value`
    CompoundAssign(BinOp, Box<Expr>, Box<Expr>),
    Block(Block),
    /// `if cond { .. } else ..`; the `else` part is a block or another `if`.
    If {
        cond: Box<Expr>,
        then: Block,
        else_: Option<Box<Expr>>,
    },
    While {
        cond: Box<Expr>,
        body: Block,
    },
    Loop(Block),
    /// `break` or `break value`
    Break(Option<Box<Expr>>),
    Continue,
    /// `return` or `return value`
    Return(Option<Box<Expr>>),
}

/// `field: value` in a struct literal.
#[derive(Debug)]
pub struct FieldInit {
    pub name: Ident,
    pub value: Expr,
}

impl ExprKind {
    /// Block-like expressions may stand as statements without `;`.
    pub fn is_block_like(&self) -> bool {
        matches!(
            self,
            ExprKind::Block(_) | ExprKind::If { .. } | ExprKind::While { .. } | ExprKind::Loop(_)
        )
    }
}
