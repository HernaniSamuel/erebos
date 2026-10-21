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
    Enum(EnumDecl),
    Const(ConstDecl),
    Static(StaticDecl),
    Comptime(ComptimeDecl),
    Mod(ModDecl),
    Use(UseDecl),
}

/// Whether an item is written with `pub` (ADR 0050).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    Private,
    Pub,
}

/// `mod name { items }`, or `mod name;`, whose items are in a file of
/// their own (ADR 0049).
#[derive(Debug)]
pub struct ModDecl {
    pub attrs: Vec<Attr>,
    pub vis: Visibility,
    pub name: Ident,
    /// The items between the braces. The parser gives `None` for `mod
    /// name;`, and the driver replaces it with the items of the module's
    /// file, when it can read it.
    pub items: Option<Vec<Item>>,
    pub span: Span,
}

/// `use path;`, `use path as name;` or `use prefix::{path, path as name,
/// ...};`, which brings items and modules into the namespace of its module
/// (ADR 0049).
#[derive(Debug)]
pub struct UseDecl {
    pub attrs: Vec<Attr>,
    pub vis: Visibility,
    /// The path before `::{`, in a `use` with braces.
    pub prefix: Option<Path>,
    /// What it brings in: its path, or with braces each path between them,
    /// which continues the prefix. Never empty.
    pub names: Vec<UseName>,
    pub span: Span,
}

/// A path of a `use`, and the name it brings in when written with `as`
/// (ADR 0049); without, the name is the path's last.
#[derive(Debug)]
pub struct UseName {
    pub path: Path,
    pub alias: Option<Ident>,
}

/// `const NAME: Type = value;` (ADR 0029)
#[derive(Debug)]
pub struct ConstDecl {
    pub attrs: Vec<Attr>,
    pub vis: Visibility,
    pub name: Ident,
    pub ty: Ty,
    pub value: Expr,
    pub span: Span,
}

/// `static NAME: Type = value;` or `static mut NAME: Type = value;`, or
/// `extern static NAME: Type;`, defined outside the program (ADR 0030).
#[derive(Debug)]
pub struct StaticDecl {
    pub attrs: Vec<Attr>,
    pub vis: Visibility,
    pub external: bool,
    pub mutable: bool,
    pub name: Ident,
    pub ty: Ty,
    /// `None` exactly for an `extern static`.
    pub value: Option<Expr>,
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
    pub vis: Visibility,
    pub name: Ident,
    pub fields: Vec<FieldDecl>,
    pub span: Span,
}

/// `enum Name: Type { Variant = value, ... }` (ADR 0035), whose variants
/// may carry payloads (ADR 0039).
#[derive(Debug)]
pub struct EnumDecl {
    pub attrs: Vec<Attr>,
    pub vis: Visibility,
    pub name: Ident,
    /// The integer type written after `:`.
    pub ty: Ty,
    pub variants: Vec<VariantDecl>,
    pub span: Span,
}

/// `Name = value` or `Name(T, ...) = value`, the value computed at compile
/// time (ADR 0035, 0039).
#[derive(Debug)]
pub struct VariantDecl {
    pub name: Ident,
    /// The payload's types, in order; empty for a variant without payload,
    /// which is written without parentheses (ADR 0039).
    pub payload: Vec<Ty>,
    pub value: Expr,
}

/// `name: Type`, or `pub name: Type`, visible wherever its struct is
/// reachable (ADR 0050).
#[derive(Debug)]
pub struct FieldDecl {
    pub vis: Visibility,
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
    pub vis: Visibility,
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

#[derive(Clone, Debug)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

/// Names separated by `::`, wherever an item is named: `x`, `Config`,
/// `uart::Mode::Run`, `super::x` (ADR 0049). What it names is resolved
/// later.
#[derive(Clone, Debug)]
pub struct Path {
    /// At least one. The keywords `crate` and `super` are segments named
    /// so, which no other name can be: `crate` only first, `super` only in
    /// a run at the start.
    pub segments: Vec<Ident>,
    pub span: Span,
}

impl Path {
    /// A path of one name.
    pub fn single(name: Ident) -> Path {
        Path {
            span: name.span,
            segments: vec![name],
        }
    }

    /// The name, if the path is one name, as `x` or `u32`.
    pub fn as_single(&self) -> Option<&Ident> {
        match self.segments.as_slice() {
            [name] => Some(name),
            _ => None,
        }
    }
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
    /// A type written by its path, such as `u32`.
    Named(Path),
    /// A path followed by type arguments in parentheses, such as
    /// `Option(u32)` or `Result(u32, E)` (ADR 0045); none for `Name()`.
    Applied(Path, Vec<Ty>),
    /// `*const T`, `*mut T`, optionally `volatile` (ADR 0013).
    Ptr {
        mutable: bool,
        volatile: bool,
        pointee: Box<Ty>,
    },
    /// `[T; N]`, where `N` is computed at compile time (ADR 0029).
    Array { elem: Box<Ty>, len: Box<Expr> },
    /// `[T]`, of a length known only at run time: it exists only behind a
    /// pointer, a slice (ADR 0031).
    Slice(Box<Ty>),
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
    /// Byte string `b"..."` (ADR 0031); `None` if the lexer rejected it.
    ByteString(Option<Vec<u8>>),
    /// `true` or `false`
    Bool(bool),
    /// `()`
    Unit,
    /// `[a, b, c]`
    Array(Vec<Expr>),
    /// `[value; N]`, where `N` is computed at compile time (ADR 0029).
    Repeat(Box<Expr>, Box<Expr>),
    /// A path used as a value: a local variable, a constant, a static, or
    /// `Enum::Variant` (ADR 0035).
    Name(Path),
    /// `.Variant` or `.Variant(args)`, a variant of the enum that is the
    /// expected type (ADR 0044); the arguments if written with
    /// parentheses.
    ContextVariant(Ident, Option<Vec<Expr>>),
    /// A path followed by arguments: `name(args)`, a call to a function, or
    /// `Enum::Variant(args)` (ADR 0039). What the path names is resolved
    /// later.
    Call(Path, Vec<Expr>),
    /// `Name { field: value, ... }`, fields in the order written.
    StructLit(Path, Vec<FieldInit>),
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
    /// `base[index]`, with the span of the `[`, where the bounds check is
    /// located (ADR 0053).
    Index(Box<Expr>, Box<Expr>, Span),
    /// `operand?`: the value of an `Option` or a `Result`, or a return of
    /// its failure (ADR 0046), with the span of the `?`, where the check of
    /// its tag is located (ADR 0053).
    Try(Box<Expr>, Span),
    /// `&raw const place` or `&raw mut place` (ADR 0021)
    AddrOf {
        mutable: bool,
        place: Box<Expr>,
    },
    /// `lhs op rhs`, with the span of the operator, where a division's
    /// check is located (ADR 0053).
    Binary(BinOp, Box<Expr>, Box<Expr>, Span),
    /// `&&` and `||`, which short-circuit.
    Logical(LogicalOp, Box<Expr>, Box<Expr>),
    /// `expr as Type`
    Cast(Box<Expr>, Ty),
    /// `place = value`
    Assign(Box<Expr>, Box<Expr>),
    /// `place op= value`, with the span of the operator, as `Binary`.
    CompoundAssign(BinOp, Box<Expr>, Box<Expr>, Span),
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
    /// `match scrutinee { pattern => expr, ... }` (ADR 0037), with the span
    /// of the keyword, where the check of an enum's value is located
    /// (ADR 0053).
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<Arm>,
        keyword: Span,
    },
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

/// `pattern => body`, an arm of a `match` (ADR 0037).
#[derive(Debug)]
pub struct Arm {
    pub pattern: Pattern,
    pub body: Expr,
}

#[derive(Debug)]
pub struct Pattern {
    pub kind: PatternKind,
    pub span: Span,
}

/// What an arm of a `match` matches (ADR 0037).
#[derive(Debug)]
pub enum PatternKind {
    /// `_`: every value.
    Wildcard,
    /// `let [mut] name: Type`: every value, bound to a new variable.
    Binding { mutable: bool, name: Ident, ty: Ty },
    /// An integer literal, or `-` and one; `value` is `None` if the lexer
    /// rejected it.
    Int { value: Option<u64>, negated: bool },
    /// `b'x'`; `None` if the lexer rejected it.
    Byte(Option<u8>),
    /// `true` or `false`
    Bool(bool),
    /// A path: a bare name, which is always a constant, or `Enum::Variant`
    /// (ADR 0035, 0037).
    Name(Path),
    /// A path followed by patterns, `Enum::Variant(p, ...)`, one per
    /// payload value (ADR 0040); none for `Enum::Variant()`.
    PathArgs(Path, Vec<Pattern>),
    /// `.Variant` or `.Variant(p, ...)`, a variant of the enum that is the
    /// type of its position (ADR 0044); the patterns if written with
    /// parentheses.
    ContextVariant(Ident, Option<Vec<Pattern>>),
    /// `p | q | ...`, at least two alternatives.
    Or(Vec<Pattern>),
}

impl ExprKind {
    /// Block-like expressions may stand as statements without `;`.
    pub fn is_block_like(&self) -> bool {
        matches!(
            self,
            ExprKind::Block(_)
                | ExprKind::If { .. }
                | ExprKind::While { .. }
                | ExprKind::Loop(_)
                | ExprKind::Match { .. }
        )
    }
}
