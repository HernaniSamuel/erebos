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
    BitStruct(BitStructDecl),
    Enum(EnumDecl),
    Const(ConstDecl),
    Static(StaticDecl),
    Comptime(ComptimeDecl),
    Mod(ModDecl),
    Use(UseDecl),
    Memory(MemoryDecl),
    PeripheralType(PeripheralTypeDecl),
    Peripheral(PeripheralDecl),
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

/// `memory { ... }`: the regions of the address space, and what each holds
/// (ADR 0057). Attributes and `pub` do not apply to it; they are kept to be
/// reported.
#[derive(Debug)]
pub struct MemoryDecl {
    pub attrs: Vec<Attr>,
    pub vis: Visibility,
    /// In source order.
    pub regions: Vec<RegionDecl>,
    /// In source order; the regions and placements may be interleaved,
    /// which their spans show.
    pub placements: Vec<Placement>,
    pub span: Span,
}

/// `region NAME { origin: expr, size: expr }`, a range of addresses
/// (ADR 0057).
#[derive(Debug)]
pub struct RegionDecl {
    pub name: Ident,
    pub origin: Expr,
    pub size: Expr,
    pub span: Span,
}

/// `code in NAME;`, `data in NAME;` or `stack in NAME { size: expr }`
/// (ADR 0057).
#[derive(Debug)]
pub struct Placement {
    pub kind: PlacementKind,
    /// The region's name.
    pub region: Ident,
    pub span: Span,
}

#[derive(Debug)]
pub enum PlacementKind {
    Code,
    Data,
    Stack { size: Expr },
}

/// `peripheral NAME { entry, ... }`, the registers and clusters of a device
/// (ADR 0058, 0064). Attributes do not apply to it; they are kept to be
/// reported.
#[derive(Debug)]
pub struct PeripheralTypeDecl {
    pub attrs: Vec<Attr>,
    pub vis: Visibility,
    pub name: Ident,
    /// In source order.
    pub entries: Vec<EntryDecl>,
    pub span: Span,
}

/// An entry of a peripheral type (ADR 0064).
#[derive(Debug)]
pub enum EntryDecl {
    Register(RegisterDecl),
    Cluster(ClusterDecl),
}

/// `[pub] ACCESS NAME: Type at OFFSET [step STEP] [READ_EFFECT] [in GROUP]`,
/// a register of a peripheral type, private to its module unless `pub`
/// (ADR 0058, 0060, 0061, 0064).
#[derive(Debug)]
pub struct RegisterDecl {
    pub vis: Visibility,
    pub access: AccessMode,
    pub name: Ident,
    pub ty: Ty,
    /// From the start of the peripheral, computed at compile time.
    pub offset: Expr,
    /// The distance between the elements of a register array, computed at
    /// compile time (ADR 0064).
    pub step: Option<Expr>,
    /// What a read of the register does, with the span of its words
    /// (ADR 0060).
    pub read_effect: Option<(ReadEffect, Span)>,
    /// The alternate group named after `in`, a label of the peripheral
    /// type that names no item (ADR 0061).
    pub group: Option<Ident>,
    pub span: Span,
}

/// `[pub] NAME: Type at OFFSET [step STEP] [in GROUP]`, a cluster of a
/// peripheral type: registers of a peripheral type placed at an offset, or
/// an array of them, private to its module unless `pub` (ADR 0064).
#[derive(Debug)]
pub struct ClusterDecl {
    pub vis: Visibility,
    pub name: Ident,
    /// Written as any type; that it is a peripheral type or an array of one
    /// is checked when names are resolved.
    pub ty: Ty,
    /// From the start of the peripheral type, computed at compile time.
    pub offset: Expr,
    /// The distance between the elements of a cluster array, computed at
    /// compile time.
    pub step: Option<Expr>,
    /// The alternate group named after `in` (ADR 0061).
    pub group: Option<Ident>,
    pub span: Span,
}

/// What a register allows, written before its name (ADR 0058).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessMode {
    /// `read`: read-only.
    Read,
    /// `write`: write-only.
    Write,
    /// `read write`
    ReadWrite,
}

impl AccessMode {
    /// As written: `read`, `write` or `read write`.
    pub fn words(self) -> &'static str {
        match self {
            AccessMode::Read => "read",
            AccessMode::Write => "write",
            AccessMode::ReadWrite => "read write",
        }
    }
}

/// `read clears`, `read sets` or `read modifies`: a read of a register or
/// of a bit field changes the device (ADR 0060).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadEffect {
    Clears,
    Sets,
    Modifies,
}

/// `write BIT clears`, `write BIT sets` or `write BIT toggles`: writing
/// `bit` to a bit field changes the device's bits (ADR 0060).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriteEffect {
    /// 0 or 1: the parser accepts no other.
    pub bit: u8,
    pub action: WriteAction,
}

/// What a write effect does to the bits written with its bit (ADR 0060).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteAction {
    Clears,
    Sets,
    Toggles,
}

impl WriteAction {
    /// As written: `clears`, `sets` or `toggles`.
    pub fn word(self) -> &'static str {
        match self {
            WriteAction::Clears => "clears",
            WriteAction::Sets => "sets",
            WriteAction::Toggles => "toggles",
        }
    }
}

/// `peripheral NAME: Type at ADDRESS [over PATH];`, an instance of a
/// peripheral type at an address (ADR 0058, 0066). Attributes do not apply to it; they are kept to
/// be reported.
#[derive(Debug)]
pub struct PeripheralDecl {
    pub attrs: Vec<Attr>,
    pub vis: Visibility,
    pub name: Ident,
    /// Written as any type; that it names a peripheral type is checked
    /// when names are resolved.
    pub ty: Ty,
    /// Computed at compile time.
    pub address: Expr,
    /// The peripheral named after `over`, whose device this one is another
    /// view of (ADR 0066); that it is a peripheral is checked when names
    /// are resolved.
    pub over: Option<Path>,
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

/// `struct Name: Type { field: Type at BITS, ... }`, a struct stored in the
/// integer type written after `:`, each field at its bits (ADR 0059).
#[derive(Debug)]
pub struct BitStructDecl {
    pub attrs: Vec<Attr>,
    pub vis: Visibility,
    pub name: Ident,
    /// The type written after `:`; that it is an integer type is checked
    /// when names are resolved.
    pub ty: Ty,
    pub fields: Vec<BitFieldDecl>,
    pub span: Span,
}

/// `[pub] [read | write] name: Type at BITS [READ_EFFECT] [WRITE_EFFECT]
/// [= default]`, a field of a bit-field struct, private to its module
/// unless `pub` (ADR 0059, 0060, 0065).
#[derive(Debug)]
pub struct BitFieldDecl {
    pub vis: Visibility,
    /// `read` or `write` written before the name, with its span; never
    /// `ReadWrite` (ADR 0065).
    pub access: Option<(AccessMode, Span)>,
    pub name: Ident,
    pub ty: Ty,
    /// The lowest bit, or the only one; computed at compile time.
    pub low: Expr,
    /// The highest bit, written after `..=`; computed at compile time.
    pub high: Option<Expr>,
    /// With the span of its words.
    pub read_effect: Option<(ReadEffect, Span)>,
    /// With the span of its words.
    pub write_effect: Option<(WriteEffect, Span)>,
    /// The value written after `=`, computed at compile time.
    pub default: Option<Expr>,
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
/// (where execution starts) and one panic function (ADR 0014), and an
/// interrupt handler for each vector it serves, which the hardware runs
/// (ADR 0084).
#[derive(Clone, Debug)]
pub enum FnKind {
    Plain,
    Entry,
    Panic,
    /// `interrupt fn NAME() at PATH`, with the path after `at`: the vector
    /// it serves.
    Interrupt(Path),
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
    /// `&T` or `&mut T` (ADR 0105). `&&T` is two of them.
    Ref { mutable: bool, referent: Box<Ty> },
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
    /// `Name { field: value, ... }`, fields in the order written, and the
    /// span of a final `..`, which leaves the other fields out (ADR 0059).
    StructLit(Path, Vec<FieldInit>, Option<Span>),
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
    /// `base[from..to]`, `base[from..]` or `base[..to]` (ADR 0108), with the
    /// span of the `[`, where the checks are located. A range exists only
    /// inside `[ ]`: it is a node of the index, not an expression.
    Subslice {
        base: Box<Expr>,
        from: Option<Box<Expr>>,
        to: Option<Box<Expr>>,
        open: Span,
    },
    /// `operand?`: the value of an `Option` or a `Result`, or a return of
    /// its failure (ADR 0046), with the span of the `?`, where the check of
    /// its tag is located (ADR 0053).
    Try(Box<Expr>, Span),
    /// `&raw const place` or `&raw mut place` (ADR 0021)
    AddrOf {
        mutable: bool,
        place: Box<Expr>,
    },
    /// `&expr` or `&mut expr` (ADR 0105). `&&expr` is two of them.
    Ref {
        mutable: bool,
        expr: Box<Expr>,
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
    /// `critical { .. }`: the block, run with interrupts masked (ADR 0078).
    Critical(Block),
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
    /// `asm("template", operands, options(...))` (ADR 0079).
    Asm(AsmExpr),
}

/// A string literal (ADR 0079): the bytes it holds, with its escapes
/// resolved, and the span of the literal. Empty when the lexer rejected it.
#[derive(Clone)]
pub struct StrLit {
    pub bytes: Vec<u8>,
    pub span: Span,
    /// Where each byte starts in the source, from the start of `span`, and the
    /// closing quote after them, so that a diagnostic about some bytes can
    /// label their text, escapes included. Empty when the lexer rejected the
    /// literal.
    pub starts: Vec<u32>,
}

impl StrLit {
    /// The source text of the bytes `from..to`, or the whole literal if the
    /// offsets are not known.
    pub fn span_of(&self, from: usize, to: usize) -> Span {
        match (self.starts.get(from), self.starts.get(to)) {
            (Some(&lo), Some(&hi)) => Span {
                file: self.span.file,
                lo: self.span.lo + lo,
                hi: self.span.lo + hi,
            },
            _ => self.span,
        }
    }
}

impl std::fmt::Debug for StrLit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The bytes as text, so that a dump is readable.
        f.debug_struct("StrLit")
            .field("bytes", &format_args!("\"{}\"", self.bytes.escape_ascii()))
            .field("span", &self.span)
            .finish()
    }
}

/// An `asm` expression (ADR 0079), as written: the templates, which the
/// compiler joins with line breaks, the operands in the order written, and
/// the options in the order written. The span of the expression is the
/// one of its `Expr`.
#[derive(Debug)]
pub struct AsmExpr {
    /// At least one, but for an `asm` the parser reported an error in.
    pub templates: Vec<StrLit>,
    pub operands: Vec<AsmOperand>,
    pub options: Vec<AsmOption>,
}

/// `[name =] direction(where) ...`, `sym path`, `const expr` or
/// `clobber_abi("C")`, with its span, which starts at the name.
#[derive(Debug)]
pub struct AsmOperand {
    /// The name that a template places the operand by. The parser lets
    /// only the operands that a template may name have one.
    pub name: Option<Ident>,
    pub kind: AsmOperandKind,
    pub span: Span,
}

#[derive(Debug)]
pub enum AsmOperandKind {
    /// `in(where) expr`
    In { reg: AsmReg, expr: Expr },
    /// `out(where) place` or `lateout(where) place`; `None` for `_`, which
    /// drops the value.
    Out {
        reg: AsmReg,
        late: bool,
        place: Option<Expr>,
    },
    /// `inout(where) expr`, and `=> place` or `=> _` after it: `out` is
    /// `None` when absent, `Some(None)` for `=> _`, `Some(Some(place))`
    /// for `=> place`.
    InOut {
        reg: AsmReg,
        expr: Expr,
        out: Option<Option<Expr>>,
    },
    /// `sym path` or `sym entry`
    Sym(SymTarget),
    /// `const expr`
    Const(Expr),
    /// `clobber_abi("C")`: the name of the ABI.
    ClobberAbi(StrLit),
}

/// What a `sym` operand names (ADR 0079, 0083).
#[derive(Debug)]
pub enum SymTarget {
    /// An item, by its path.
    Path(Path),
    /// `entry`, the entry function, with the span of the keyword.
    Entry(Span),
}

/// Where an operand lives: `reg`, a register the compiler chooses, or the
/// name of one, a string literal (ADR 0079).
#[derive(Debug)]
pub enum AsmReg {
    /// `reg`: the name of a register class.
    Class(Ident),
    /// `"a0"`
    Explicit(StrLit),
}

/// An option of `options(...)`, with its span.
#[derive(Debug)]
pub struct AsmOption {
    pub kind: AsmOptionKind,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AsmOptionKind {
    Pure,
    Nomem,
    Readonly,
    Nostack,
    PreservesFlags,
    Noreturn,
}

impl AsmOptionKind {
    /// The options, as written.
    pub const ALL: [(&'static str, AsmOptionKind); 6] = [
        ("pure", AsmOptionKind::Pure),
        ("nomem", AsmOptionKind::Nomem),
        ("readonly", AsmOptionKind::Readonly),
        ("nostack", AsmOptionKind::Nostack),
        ("preserves_flags", AsmOptionKind::PreservesFlags),
        ("noreturn", AsmOptionKind::Noreturn),
    ];
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
                | ExprKind::Critical(_)
                | ExprKind::If { .. }
                | ExprKind::While { .. }
                | ExprKind::Loop(_)
                | ExprKind::Match { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::SourceMap;

    /// The text of some bytes, escapes included; the whole literal when the
    /// offsets are not known.
    #[test]
    fn a_literal_gives_the_text_of_some_of_its_bytes() {
        let file = SourceMap::default().add("main.ers", "");
        // `"a\x41b"` at 10, as the parser makes it.
        let lit = StrLit {
            bytes: b"aAb".to_vec(),
            span: Span::new(file, 10, 18),
            starts: vec![1, 2, 6, 7],
        };
        assert_eq!(lit.span_of(0, 1), Span::new(file, 11, 12));
        assert_eq!(lit.span_of(1, 2), Span::new(file, 12, 16));
        assert_eq!(lit.span_of(0, 3), Span::new(file, 11, 17));
        // Past the end, or unknown: the whole literal.
        assert_eq!(lit.span_of(2, 9), lit.span);
        let rejected = StrLit {
            bytes: Vec::new(),
            span: lit.span,
            starts: Vec::new(),
        };
        assert_eq!(rejected.span_of(0, 1), lit.span);
    }
}
