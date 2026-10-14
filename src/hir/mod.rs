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
    /// Parameters are the first `param_count` locals, in order.
    pub param_count: usize,
    pub locals: Vec<Local>,
    pub body: Block,
    pub span: Span,
    /// Defined by the runtime support functions (ADR 0020).
    pub runtime: bool,
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
    /// Produced only by the type checker after reporting an error, so one
    /// mistake does not cascade into more. Compatible with every type.
    Error,
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
    Local(LocalId),
    Call {
        callee: FnId,
        args: Vec<Expr>,
    },
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
    pub fn walk(&self, f: &mut impl FnMut(&Expr)) {
        f(self);
        match &self.kind {
            ExprKind::Int(_)
            | ExprKind::Byte(_)
            | ExprKind::Bool(_)
            | ExprKind::Unit
            | ExprKind::Local(_)
            | ExprKind::Continue { .. }
            | ExprKind::Break { value: None, .. }
            | ExprKind::Return(None) => {}
            ExprKind::Break { value: Some(e), .. } | ExprKind::Return(Some(e)) => e.walk(f),
            ExprKind::Call { args, .. } => {
                for arg in args {
                    arg.walk(f);
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
