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

/// An enum, indexing [`Hir::enums`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EnumId(pub u32);

/// A local variable, indexing [`Fn::locals`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LocalId(pub u32);

#[derive(Debug)]
pub struct Hir {
    pub structs: Vec<StructDef>,
    pub enums: Vec<EnumDef>,
    /// Functions, then the bodies that run at compile time: one for each
    /// constant, each static with an initializer (not an `extern static`)
    /// and each `comptime` block, in source order, then one for each
    /// variant of each enum, then one for each value computed at compile
    /// time.
    pub fns: Vec<Fn>,
    pub consts: Vec<ConstDef>,
    pub statics: Vec<StaticDef>,
    /// The bodies of `comptime { ... }` items.
    pub comptime_blocks: Vec<FnId>,
    /// Each `!` written where it would be the type of memory, whose type
    /// was lowered as `{error}`; the type checker reports them (ADR 0034).
    pub never_in_memory: Vec<NeverInMemory>,
    /// Each `[T]` written where a size is needed, whose type was lowered
    /// as `{error}`; the type checker reports them (ADR 0031).
    pub unsized_types: Vec<UnsizedType>,
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

    pub fn enum_(&self, id: EnumId) -> &EnumDef {
        &self.enums[id.0 as usize]
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

/// A `!` written where memory would have its type (ADR 0034).
#[derive(Clone, Copy, Debug)]
pub struct NeverInMemory {
    /// The span of the `!`.
    pub span: Span,
    pub position: NeverPosition,
}

/// Where a `!` would be the type of memory (ADR 0034).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NeverPosition {
    Pointee,
    /// The element type of a slice, `[T]` (ADR 0031).
    SliceElement,
    Element,
    Field,
    /// The type of a `static` or `extern static`.
    Static,
    Const,
    /// The operand of `size_of`, `align_of` or `offset_of`, named.
    LayoutQuery(&'static str),
}

/// A `[T]` written where a size is needed (ADR 0031).
#[derive(Clone, Copy, Debug)]
pub struct UnsizedType {
    /// The span of the `[T]`.
    pub span: Span,
    /// The type written, `[T]`.
    pub ty: Ty,
    pub position: UnsizedPosition,
}

/// Where a `[T]` would need a size (ADR 0031): any position of memory but
/// the pointee of a pointer, or the type of a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnsizedPosition {
    Memory(NeverPosition),
    Variable,
    Parameter,
    Return,
    Cast,
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
    /// The value of a variant, by index, of the enum's integer type
    /// (ADR 0035).
    Discriminant(EnumId, u32),
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

/// `enum Name: Type { Variant = value, ... }` (ADR 0035): named values of
/// an integer type, which the enum is stored as.
#[derive(Debug)]
pub struct EnumDef {
    pub name: String,
    pub name_span: Span,
    pub ty: EnumTy,
    pub variants: Vec<VariantDef>,
}

#[derive(Debug)]
pub struct VariantDef {
    pub name: String,
    pub span: Span,
    /// The body computing its value (ADR 0035).
    pub value: FnId,
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
    /// `[T]`, of a length known only at run time; only the pointee of a
    /// pointer, which is then a slice (ADR 0031). Leaked likewise.
    Slice(&'static Ty),
    Struct(StructTy),
    Enum(EnumTy),
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

/// An enum type (ADR 0035), with its integer type, which it is stored as.
/// The name is carried along like a struct's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnumTy {
    pub id: EnumId,
    pub name: &'static str,
    pub int: IntTy,
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

    /// `[elem]`, the pointee of a slice.
    pub fn slice(elem: Ty) -> Ty {
        Ty::Slice(Box::leak(Box::new(elem)))
    }

    /// The element type of a slice, `*const [T]` or `*mut [T]` (ADR 0031).
    pub fn slice_elem(self) -> Option<Ty> {
        match self {
            Ty::Ptr(Ptr {
                pointee: Ty::Slice(elem),
                ..
            }) => Some(**elem),
            _ => None,
        }
    }

    /// Whether every array length in the type is known.
    pub fn is_normal(self) -> bool {
        match self {
            Ty::Array(elem, len) => matches!(len, ArrayLen::Known(_)) && elem.is_normal(),
            Ty::Slice(elem) => elem.is_normal(),
            Ty::Ptr(ptr) => ptr.pointee.is_normal(),
            _ => true,
        }
    }

    /// Arrays, structs and slices: values made of other values. A slice
    /// is an address and a length (ADR 0031).
    pub fn is_aggregate(self) -> bool {
        matches!(self, Ty::Array(..) | Ty::Struct(_)) || self.slice_elem().is_some()
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
            Ty::Slice(elem) => write!(f, "[{elem}]"),
            Ty::Struct(s) => f.write_str(s.name),
            Ty::Enum(e) => f.write_str(e.name),
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
    /// `b"..."` (ADR 0031): a place in read-only memory, holding its bytes.
    ByteString(Vec<u8>),
    Bool(bool),
    Unit,
    /// A constant (ADR 0029): a value, not a place.
    Const(ConstId),
    /// A static (ADR 0030): a place.
    Static(StaticId),
    /// `Enum::Variant`, by index (ADR 0035): a value, not a place, like a
    /// constant.
    Variant(EnumId, u32),
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
    /// `assert(cond)` or `assert(cond, msg)`, built in (ADR 0029, 0031).
    Assert(Box<Expr>, Option<Box<Expr>>),
    /// `len(s)`, built in (ADR 0031): the length of a slice.
    Len(Box<Expr>),
    /// `slice(p, n)`, built in (ADR 0031): a slice of `n` elements from `p`.
    Slice(Box<Expr>, Box<Expr>),
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
    /// `match scrutinee { arms }` (ADR 0037).
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<Arm>,
    },
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

/// `pattern => body`, an arm of a `match` (ADR 0037). A variable the
/// pattern binds exists in `body` only.
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

/// What a pattern matches (ADR 0037). Patterns run no code: a constant is
/// a value computed at compile time.
#[derive(Debug)]
pub enum PatternKind {
    /// `_`: every value.
    Wildcard,
    /// `let [mut] name: T`: every value, copied into `local`, whose type
    /// is written at `ty_span`.
    Binding {
        local: LocalId,
        ty_span: Span,
    },
    /// An integer literal, or `-` and one: its magnitude.
    Int {
        value: u64,
        negated: bool,
    },
    /// `b'x'`, a `u8`.
    Byte(u8),
    Bool(bool),
    /// A constant, matching its value.
    Const(ConstId),
    /// `Enum::Variant`, by index, matching its value (ADR 0035, 0037).
    Variant(EnumId, u32),
    /// `p | q | ...`; no alternative is a `Binding` or an `Or`.
    Or(Vec<Pattern>),
}

impl Pattern {
    /// The alternatives of an or-pattern, or the pattern itself.
    pub fn alternatives(&self) -> &[Pattern] {
        match &self.kind {
            PatternKind::Or(alternatives) => alternatives,
            _ => std::slice::from_ref(self),
        }
    }

    /// The variable the pattern binds, if it binds one.
    pub fn binding(&self) -> Option<LocalId> {
        match self.kind {
            PatternKind::Binding { local, .. } => Some(local),
            _ => None,
        }
    }
}

impl Expr {
    /// Calls `f` on this expression and every expression inside it, in
    /// source order.
    pub fn walk<'a>(&'a self, f: &mut impl FnMut(&'a Expr)) {
        f(self);
        match &self.kind {
            ExprKind::Int(_)
            | ExprKind::Byte(_)
            | ExprKind::ByteString(_)
            | ExprKind::Bool(_)
            | ExprKind::Unit
            | ExprKind::SizeOf(_)
            | ExprKind::AlignOf(_)
            | ExprKind::OffsetOf(..)
            | ExprKind::Const(_)
            | ExprKind::Static(_)
            | ExprKind::Variant(..)
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
            | ExprKind::Assert(e, None)
            | ExprKind::Len(e)
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
            | ExprKind::Slice(a, b)
            | ExprKind::Assert(a, Some(b))
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
            ExprKind::Match { scrutinee, arms } => {
                scrutinee.walk(f);
                for arm in arms {
                    arm.body.walk(f);
                }
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
