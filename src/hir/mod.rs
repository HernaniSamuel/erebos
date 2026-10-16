//! High-level IR: the AST after name resolution. Names become ids, written
//! types become resolved [`Ty`]s, and every expression gets an [`ExprId`]
//! that later layers use as a key.

mod lower;

pub use lower::lower;

use std::fmt;

pub use crate::int::IntTy;
pub use crate::ops::{BinOp, LogicalOp, UnOp};
use crate::span::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FnId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ExprId(pub u32);

/// A constant, indexing [`Hir::consts`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ConstId(pub u32);

/// A static, indexing [`Hir::statics`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StaticId(pub u32);

/// A struct, indexing [`Hir::structs`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StructId(pub u32);

/// A local variable, indexing [`Fn::locals`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LocalId(pub u32);

#[derive(Debug)]
pub struct Hir {
    pub structs: Vec<StructDef>,
    /// Functions, then the bodies that run at compile time: one for each
    /// constant, each static with an initializer (not an `extern static`)
    /// and each `comptime` block, in source order, then one for each value
    /// computed at compile time.
    pub fns: Vec<Fn>,
    pub consts: Vec<ConstDef>,
    pub statics: Vec<StaticDef>,
    /// The bodies of `comptime { ... }` items.
    pub comptime_blocks: Vec<FnId>,
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

    pub fn struct_(&self, id: StructId) -> &StructDef {
        &self.structs[id.0 as usize]
    }

    pub fn const_(&self, id: ConstId) -> &ConstDef {
        &self.consts[id.0 as usize]
    }

    pub fn static_(&self, id: StaticId) -> &StaticDef {
        &self.statics[id.0 as usize]
    }
}

/// `const NAME: Type = value;` (ADR 0029). The value is computed by running
/// `init`, a body of its own whose return value is the constant.
#[derive(Debug)]
pub struct ConstDef {
    pub name: String,
    pub name_span: Span,
    pub ty: Ty,
    pub init: FnId,
}

/// `static NAME: Type = value;` or `static mut` (ADR 0030): one object in
/// memory, whose initial value is computed by running `init`. An `extern
/// static` is defined outside the program, and has no `init`.
#[derive(Debug)]
pub struct StaticDef {
    pub name: String,
    pub name_span: Span,
    pub mutable: bool,
    pub ty: Ty,
    pub ty_span: Span,
    /// `None` exactly for an `extern static`.
    pub init: Option<FnId>,
    /// `#[align(N)]`, with the span of `N` (ADR 0030).
    pub align: Option<(ArrayLen, Span)>,
}

/// What a body that runs at compile time computes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Comptime {
    /// The value of a constant.
    Const(ConstId),
    /// The initial value of a static.
    Static(StaticId),
    /// A `comptime { ... }` item.
    Block,
    /// A `usize` the program needs at compile time: an array length, a
    /// repeat count, the argument of `#[align]`.
    Value,
}

/// `struct Name { field: Type, ... }` (ADR 0021)
#[derive(Debug)]
pub struct StructDef {
    pub name: String,
    pub name_span: Span,
    pub fields: Vec<FieldDef>,
    /// `#[packed]`: alignment 1, no padding (ADR 0022).
    pub packed: bool,
    /// `#[align(N)]`, with the span of `N` (ADR 0022).
    pub align: Option<(ArrayLen, Span)>,
}

#[derive(Debug)]
pub struct FieldDef {
    pub name: String,
    pub span: Span,
    pub ty: Ty,
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
    /// Parameters are the first `param_count` locals, in order.
    pub param_count: usize,
    pub locals: Vec<Local>,
    /// `None` for an `extern fn`, defined outside the program (ADR 0026).
    pub body: Option<Block>,
    pub span: Span,
    /// Defined by the runtime support functions (ADR 0020).
    pub runtime: bool,
    /// A body run only at compile time, never part of the program.
    pub comptime: Option<Comptime>,
}

impl Fn {
    pub fn local(&self, id: LocalId) -> &Local {
        &self.locals[id.0 as usize]
    }

    pub fn params(&self) -> &[Local] {
        &self.locals[..self.param_count]
    }
}

#[derive(Debug)]
pub struct Local {
    pub name: String,
    pub ty: Ty,
    pub mutable: bool,
    /// Span of the name where it is declared.
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    Unit,
    Never,
    Bool,
    Int(IntTy),
    Ptr(Ptr),
    /// `[T; N]`; the element type is leaked like pointee types.
    Array(&'static Ty, ArrayLen),
    Struct(StructTy),
    /// Produced only by the type checker after reporting an error, so one
    /// mistake does not cascade into more. Compatible with every type.
    Error,
}

/// The length of an array: known, or computed at compile time by a body of
/// its own (ADR 0029). Types are normalized before use, so that only known
/// lengths are compared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrayLen {
    Known(u64),
    Comptime(FnId),
}

impl ArrayLen {
    /// The length of a normalized type.
    pub fn known(self) -> u64 {
        match self {
            ArrayLen::Known(len) => len,
            ArrayLen::Comptime(_) => unreachable!("types are normalized before use"),
        }
    }
}

impl fmt::Display for ArrayLen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArrayLen::Known(len) => write!(f, "{len}"),
            ArrayLen::Comptime(_) => f.write_str("_"),
        }
    }
}

/// A struct type. The name is carried along (leaked, like pointee types)
/// so that types display on their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StructTy {
    pub id: StructId,
    pub name: &'static str,
}

/// A raw pointer type (ADR 0013).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ptr {
    pub mutable: bool,
    pub volatile: bool,
    pub pointee: &'static Ty,
}

impl Ty {
    /// Pointee types are leaked so that `Ty` stays `Copy`; a compiler run is
    /// short and the number of distinct pointer types is tiny.
    pub fn ptr(mutable: bool, volatile: bool, pointee: Ty) -> Ty {
        Ty::Ptr(Ptr {
            mutable,
            volatile,
            pointee: Box::leak(Box::new(pointee)),
        })
    }

    pub fn array(elem: Ty, len: u64) -> Ty {
        Ty::array_of(elem, ArrayLen::Known(len))
    }

    pub fn array_of(elem: Ty, len: ArrayLen) -> Ty {
        Ty::Array(Box::leak(Box::new(elem)), len)
    }

    /// Whether every array length in the type is known.
    pub fn is_normal(self) -> bool {
        match self {
            Ty::Array(elem, len) => matches!(len, ArrayLen::Known(_)) && elem.is_normal(),
            Ty::Ptr(ptr) => ptr.pointee.is_normal(),
            _ => true,
        }
    }

    /// Arrays and structs: values made of other values.
    pub fn is_aggregate(self) -> bool {
        matches!(self, Ty::Array(..) | Ty::Struct(_))
    }
}

impl fmt::Display for Ptr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mutability = if self.mutable { "mut" } else { "const" };
        let volatile = if self.volatile { " volatile" } else { "" };
        write!(f, "*{mutability}{volatile} {}", self.pointee)
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Unit => f.write_str("()"),
            Ty::Never => f.write_str("!"),
            Ty::Bool => f.write_str("bool"),
            Ty::Int(int) => write!(f, "{int}"),
            Ty::Ptr(ptr) => write!(f, "{ptr}"),
            Ty::Array(elem, len) => write!(f, "[{elem}; {len}]"),
            Ty::Struct(s) => f.write_str(s.name),
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
    Bool(bool),
    Unit,
    /// A constant (ADR 0029): a value, not a place.
    Const(ConstId),
    /// A static (ADR 0030): a place.
    Static(StaticId),
    /// `comptime expr`, evaluated at compile time (ADR 0029).
    Comptime(Box<Expr>),
    /// `[a, b, c]`
    Array(Vec<Expr>),
    /// `[value; N]`
    Repeat(Box<Expr>, ArrayLen),
    Local(LocalId),
    Call {
        callee: FnId,
        args: Vec<Expr>,
    },
    /// `Name { field: value, ... }`: field indices with their values, in
    /// the order written, which is the order they are evaluated in.
    StructLit(StructTy, Vec<(u32, Expr)>),
    /// `base.field`; the type checker resolves the field.
    Field(Box<Expr>, String, Span),
    /// `assert(cond)`, built in (ADR 0029).
    Assert(Box<Expr>),
    /// `size_of(T)`, a `usize` constant.
    SizeOf(Ty),
    /// `align_of(T)`, a `usize` constant.
    AlignOf(Ty),
    /// `offset_of(T, field)`, a `usize` constant; the type checker resolves
    /// the field.
    OffsetOf(Ty, String, Span),
    Unary(UnOp, Box<Expr>),
    /// `*ptr`
    Deref(Box<Expr>),
    /// `base[index]`
    Index(Box<Expr>, Box<Expr>),
    /// `&raw const place` or `&raw mut place`
    AddrOf {
        mutable: bool,
        place: Box<Expr>,
    },
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Logical(LogicalOp, Box<Expr>, Box<Expr>),
    /// `expr as ty`; the span is the written type's.
    Cast(Box<Expr>, Ty, Span),
    Assign(Box<Expr>, Box<Expr>),
    CompoundAssign(BinOp, Box<Expr>, Box<Expr>),
    Block(Block),
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
    /// Leaves the loop expression `target` (a `loop` or `while`).
    Break {
        target: ExprId,
        value: Option<Box<Expr>>,
    },
    Continue {
        target: ExprId,
    },
    Return(Option<Box<Expr>>),
}

impl Expr {
    /// Calls `f` on this expression and every expression inside it, in
    /// source order.
    pub fn walk<'a>(&'a self, f: &mut impl FnMut(&'a Expr)) {
        f(self);
        match &self.kind {
            ExprKind::Int(_)
            | ExprKind::Byte(_)
            | ExprKind::Bool(_)
            | ExprKind::Unit
            | ExprKind::SizeOf(_)
            | ExprKind::AlignOf(_)
            | ExprKind::OffsetOf(..)
            | ExprKind::Const(_)
            | ExprKind::Static(_)
            | ExprKind::Local(_)
            | ExprKind::Continue { .. }
            | ExprKind::Break { value: None, .. }
            | ExprKind::Return(None) => {}
            ExprKind::Break { value: Some(e), .. } | ExprKind::Return(Some(e)) => e.walk(f),
            ExprKind::Call { args, .. } | ExprKind::Array(args) => {
                for arg in args {
                    arg.walk(f);
                }
            }
            ExprKind::Repeat(e, _)
            | ExprKind::Field(e, _, _)
            | ExprKind::Assert(e)
            | ExprKind::Comptime(e) => e.walk(f),
            ExprKind::StructLit(_, fields) => {
                for (_, value) in fields {
                    value.walk(f);
                }
            }
            ExprKind::Unary(_, e)
            | ExprKind::Deref(e)
            | ExprKind::Cast(e, _, _)
            | ExprKind::AddrOf { place: e, .. } => e.walk(f),
            ExprKind::Binary(_, a, b)
            | ExprKind::Index(a, b)
            | ExprKind::Logical(_, a, b)
            | ExprKind::Assign(a, b)
            | ExprKind::CompoundAssign(_, a, b) => {
                a.walk(f);
                b.walk(f);
            }
            ExprKind::Block(block) | ExprKind::Loop(block) => block.walk(f),
            ExprKind::If { cond, then, else_ } => {
                cond.walk(f);
                then.walk(f);
                if let Some(else_) = else_ {
                    else_.walk(f);
                }
            }
            ExprKind::While { cond, body } => {
                cond.walk(f);
                body.walk(f);
            }
        }
    }
}

impl Block {
    pub fn walk<'a>(&'a self, f: &mut impl FnMut(&'a Expr)) {
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
