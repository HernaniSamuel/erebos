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

/// A pattern that type checking resolves, `.Variant` (ADR 0044): one per
/// such pattern in the program, under which the variant it names is
/// recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PatternId(pub u32);

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

/// A module, indexing [`Hir::modules`]; the root module is `ModuleId(0)`
/// (ADR 0049).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModuleId(pub u32);

impl ModuleId {
    pub const ROOT: ModuleId = ModuleId(0);
}

/// Where an item is visible: in its module and the modules inside it, or,
/// with `pub`, wherever its module is (ADR 0050).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    Private,
    Pub,
}

/// A local variable, indexing [`Fn::locals`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LocalId(pub u32);

/// The program, then `core` (ADR 0052): in each list, the items of `core`
/// follow the program's, so that the ids of the program's items, and the
/// dumps that leave `core` out, are those of the program alone.
#[derive(Debug)]
pub struct Hir {
    /// The module tree, the root first, then each module in the order its
    /// `mod` is met, depth first (ADR 0049); then `core`'s tree, likewise.
    pub modules: Vec<ModuleDef>,
    pub structs: Vec<StructDef>,
    pub enums: Vec<EnumDef>,
    /// The program's functions, then its bodies that run at compile time:
    /// one for each constant, each static with an initializer (not an
    /// `extern static`) and each `comptime` block, in source order, then
    /// one for each variant of each enum. Then `core`'s, in the same order.
    /// Then one for each value computed at compile time, the program's
    /// first.
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
    /// Each `Option(T)` and `Result(T, E)` written, with its span: one too
    /// large for the target is reported at each, having no declaration to
    /// be reported at (ADR 0045).
    pub applications: Vec<(Span, Ty)>,
    pub entry: FnId,
    pub panic: FnId,
    /// Expression ids are `0..expr_count`.
    pub expr_count: u32,
    /// Where `core` starts.
    pub core: CoreStart,
}

/// Where `core`'s part starts in the lists of the HIR, whose items are
/// lowered after the program's (ADR 0052).
#[derive(Clone, Copy, Debug)]
pub struct CoreStart {
    /// The root of `core`: every module from it on is `core`'s.
    pub module: ModuleId,
    /// The first expression id of `core`.
    pub expr: u32,
    pub never_in_memory: usize,
    pub unsized_types: usize,
    pub applications: usize,
}

impl Hir {
    /// Whether `module` is a module of `core` (ADR 0052).
    pub fn in_core(&self, module: ModuleId) -> bool {
        module.0 >= self.core.module.0
    }

    /// The HIR of the program, as `--emit=hir` shows it: `core` left out
    /// (ADR 0052). Its fields are those of `Hir`, in its order.
    pub fn dump(&self) -> String {
        #[derive(Debug)]
        #[expect(dead_code, reason = "only its `Debug` output is read")]
        struct Hir<'a> {
            modules: &'a [ModuleDef],
            structs: Vec<&'a StructDef>,
            enums: Vec<&'a EnumDef>,
            fns: Vec<&'a Fn>,
            consts: Vec<&'a ConstDef>,
            statics: Vec<&'a StaticDef>,
            comptime_blocks: Vec<FnId>,
            never_in_memory: &'a [NeverInMemory],
            unsized_types: &'a [UnsizedType],
            applications: &'a [(Span, Ty)],
            entry: FnId,
            panic: FnId,
            expr_count: u32,
        }
        let core = self.core;
        let program = Hir {
            modules: &self.modules[..core.module.0 as usize],
            structs: (self.structs.iter())
                .filter(|s| !self.in_core(s.module))
                .collect(),
            enums: (self.enums.iter())
                .filter(|e| !self.in_core(e.module))
                .collect(),
            fns: (self.fns.iter())
                .filter(|f| !self.in_core(f.module))
                .collect(),
            consts: (self.consts.iter())
                .filter(|c| !self.in_core(c.module))
                .collect(),
            statics: (self.statics.iter())
                .filter(|s| !self.in_core(s.module))
                .collect(),
            comptime_blocks: (self.comptime_blocks.iter().copied())
                .filter(|&id| !self.in_core(self.fn_(id).module))
                .collect(),
            never_in_memory: &self.never_in_memory[..core.never_in_memory],
            unsized_types: &self.unsized_types[..core.unsized_types],
            applications: &self.applications[..core.applications],
            entry: self.entry,
            panic: self.panic,
            expr_count: core.expr,
        };
        format!("{program:#?}\n")
    }

    pub fn fns(&self) -> impl Iterator<Item = (FnId, &Fn)> {
        self.fns
            .iter()
            .enumerate()
            .map(|(i, f)| (FnId(i as u32), f))
    }

    pub fn module(&self, id: ModuleId) -> &ModuleDef {
        &self.modules[id.0 as usize]
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

    /// Whether the field `index` of the struct `id` is visible from
    /// `module`: a private one where a private item of the struct's module
    /// would be, a `pub` one wherever the struct is reachable (ADR 0050).
    pub fn field_visible(&self, id: StructId, index: usize, module: ModuleId) -> bool {
        let def = self.struct_(id);
        match def.fields[index].vis {
            Visibility::Private => is_within(&self.modules, module, def.module),
            Visibility::Pub => def.reachable_from(module),
        }
    }
}

/// Whether `inner` is `outer` or a module inside it, in the module tree
/// `modules`: where the private items of `outer` are visible (ADR 0050).
pub(crate) fn is_within(modules: &[ModuleDef], inner: ModuleId, outer: ModuleId) -> bool {
    let mut at = Some(inner);
    while let Some(module) = at {
        if module == outer {
            return true;
        }
        at = modules[module.0 as usize].parent;
    }
    false
}

/// A module: the root, one declared with `mod` (ADR 0049), or the root of
/// `core` (ADR 0052), which has no parent either.
#[derive(Debug)]
pub struct ModuleDef {
    /// Empty for the root module; `core` for the root of `core`.
    pub name: String,
    /// Its path, as diagnostics show it: `uart::regs`; empty for the root
    /// module. The modules of `core` start with `core`, as every module
    /// names them (`core::builtins`).
    pub path: String,
    /// `None` for the root module and the root of `core`.
    pub name_span: Option<Span>,
    /// `None` for the root module and the root of `core`.
    pub parent: Option<ModuleId>,
    /// `Pub` for the root module, which every module is inside, and for
    /// the root of `core`.
    pub vis: Visibility,
}

impl ModuleDef {
    /// How diagnostics name it: "module `uart::regs`", "module `core`", or
    /// "the root module".
    pub fn describe(&self) -> String {
        if self.path.is_empty() {
            "the root module".to_owned()
        } else {
            format!("module `{}`", self.path)
        }
    }
}

/// `const NAME: Type = value;` (ADR 0029). The value is computed by running
/// `init`, a body of its own whose return value is the constant.
#[derive(Debug)]
pub struct ConstDef {
    pub name: String,
    /// Its path from the root module, as diagnostics and dumps show it:
    /// `uart::BASE`, or its name in the root module (ADR 0049).
    pub path: String,
    pub name_span: Span,
    pub module: ModuleId,
    pub vis: Visibility,
    pub ty: Ty,
    pub init: FnId,
}

/// `static NAME: Type = value;` or `static mut` (ADR 0030): one object in
/// memory, whose initial value is computed by running `init`. An `extern
/// static` is defined outside the program, and has no `init`.
#[derive(Debug)]
pub struct StaticDef {
    pub name: String,
    /// Its path from the root module, as diagnostics and dumps show it
    /// (ADR 0049).
    pub path: String,
    /// Its symbol (ADR 0051): its path joined by dots, or its own name in
    /// the root module, with `#[export]` or for an `extern static`.
    pub symbol: String,
    pub name_span: Span,
    pub module: ModuleId,
    pub vis: Visibility,
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
    /// A type of a variant's payload (ADR 0039).
    Payload,
    /// A type argument of `Option` or `Result`, which is a payload type
    /// (ADR 0045).
    TypeArgument,
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
    /// Its path from the root module, `uart::Config`, or its name in the
    /// root module: how diagnostics and dumps show the type (ADR 0049).
    pub name: String,
    pub name_span: Span,
    pub module: ModuleId,
    pub vis: Visibility,
    /// The modules it is reachable from, in id order: those where a path
    /// written would name it and be usable, through a `pub use` too
    /// (ADR 0050). Its `pub` fields are visible there.
    pub reachable: Vec<ModuleId>,
    pub fields: Vec<FieldDef>,
    /// `#[packed]`: alignment 1, no padding (ADR 0022).
    pub packed: bool,
    /// `#[align(N)]`, with the span of `N` (ADR 0022).
    pub align: Option<(ArrayLen, Span)>,
}

impl StructDef {
    /// Whether a path written in `module` can name it (ADR 0050).
    pub fn reachable_from(&self, module: ModuleId) -> bool {
        self.reachable.contains(&module)
    }
}

/// `enum Name: Type { Variant = value, ... }` (ADR 0035): named values of
/// an integer type, which the enum is stored as; or, if a variant carries a
/// payload, an aggregate of a tag of that type and the values of the
/// variant it holds (ADR 0039). Also an `Option(T)` or `Result(T, E)`,
/// which is declared nowhere (ADR 0045): its spans are `None`.
#[derive(Debug)]
pub struct EnumDef {
    /// Its path from the root module, or its name in the root module, as
    /// a struct's (ADR 0049).
    pub name: String,
    pub name_span: Option<Span>,
    /// The root module and `pub` for an enum declared nowhere, whose name
    /// is in every module (ADR 0049).
    pub module: ModuleId,
    pub vis: Visibility,
    /// The modules it is reachable from, in id order, as a struct's
    /// (ADR 0050): its variants are visible there. Empty for an enum
    /// declared nowhere, which is reachable from every module.
    pub reachable: Vec<ModuleId>,
    pub ty: EnumTy,
    pub variants: Vec<VariantDef>,
}

impl EnumDef {
    /// Whether it is declared in the program, unlike an `Option(T)` or a
    /// `Result(T, E)` (ADR 0045).
    pub fn is_declared(&self) -> bool {
        self.name_span.is_some()
    }

    /// Whether a path written in `module` can name it, so that `.V` may
    /// name its variants there (ADR 0050).
    pub fn reachable_from(&self, module: ModuleId) -> bool {
        !self.is_declared() || self.reachable.contains(&module)
    }

    /// How diagnostics write its variant `index`: `E::V`, or `.V` for a
    /// variant of `Option` or `Result`, which has no path (ADR 0044, 0045).
    pub fn variant_path(&self, index: usize) -> String {
        let variant = &self.variants[index].name;
        if self.is_declared() {
            format!("{}::{variant}", self.name)
        } else {
            format!(".{variant}")
        }
    }
}

#[derive(Debug)]
pub struct VariantDef {
    pub name: String,
    /// The span of its name; `None` in an enum declared nowhere.
    pub span: Option<Span>,
    /// The types of its payload values, in order; empty without payload
    /// (ADR 0039).
    pub payload: Vec<PayloadDef>,
    pub value: VariantValue,
}

/// The value of a variant (ADR 0035).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VariantValue {
    /// Computed by a body of its own.
    Comptime(FnId),
    /// Known without evaluation, as those of `Option` and `Result` are
    /// (ADR 0045).
    Known(u64),
}

/// One value of a variant's payload (ADR 0039).
#[derive(Debug)]
pub struct PayloadDef {
    /// The span of its written type; `None` in an enum declared nowhere.
    pub span: Option<Span>,
    pub ty: Ty,
}

/// An enum with type arguments, known to the compiler (ADR 0045).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuiltinEnum {
    /// `Option(T)`: `None = 0`, `Some(T) = 1`.
    Option,
    /// `Result(T, E)`: `Ok(T) = 0`, `Err(E) = 1`.
    Result,
}

impl BuiltinEnum {
    pub fn from_name(name: &str) -> Option<BuiltinEnum> {
        match name {
            "Option" => Some(BuiltinEnum::Option),
            "Result" => Some(BuiltinEnum::Result),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            BuiltinEnum::Option => "Option",
            BuiltinEnum::Result => "Result",
        }
    }

    /// How it is written with its type arguments, named as ADR 0045 does.
    pub fn written(self) -> &'static str {
        match self {
            BuiltinEnum::Option => "Option(T)",
            BuiltinEnum::Result => "Result(T, E)",
        }
    }

    /// How many type arguments it takes.
    pub fn arity(self) -> usize {
        match self {
            BuiltinEnum::Option => 1,
            BuiltinEnum::Result => 2,
        }
    }

    /// The indices of the variant whose payload `?` gives, `Some` or `Ok`,
    /// and of the one it returns, `None` or `Err` (ADR 0046).
    pub fn try_variants(self) -> (u32, u32) {
        match self {
            BuiltinEnum::Option => (1, 0),
            BuiltinEnum::Result => (0, 1),
        }
    }

    /// Its variants, in order: their names, values and the index of the
    /// type argument each carries, if any.
    fn variants(self) -> [(&'static str, u64, Option<usize>); 2] {
        match self {
            BuiltinEnum::Option => [("None", 0, None), ("Some", 1, Some(0))],
            BuiltinEnum::Result => [("Ok", 0, Some(0)), ("Err", 1, Some(1))],
        }
    }

    /// The enum it is with the type arguments `args`, normalized, as the
    /// enum `id`: one with payloads on `u8`, whose variants have values
    /// known without evaluation, and whose name shows its arguments, as in
    /// `Result([u8; 4], ParseError)` (ADR 0045).
    pub fn def(self, id: EnumId, args: &[Ty]) -> EnumDef {
        let shown: Vec<String> = args.iter().map(Ty::to_string).collect();
        let name = format!("{}({})", self.name(), shown.join(", "));
        let ty = EnumTy {
            id,
            name: Box::leak(name.clone().into_boxed_str()),
            int: IntTy::U8,
            payloads: true,
        };
        let variants = self
            .variants()
            .into_iter()
            .map(|(name, value, arg)| VariantDef {
                name: name.to_owned(),
                span: None,
                payload: arg
                    .map(|arg| PayloadDef {
                        span: None,
                        ty: args[arg],
                    })
                    .into_iter()
                    .collect(),
                value: VariantValue::Known(value),
            })
            .collect();
        EnumDef {
            name,
            name_span: None,
            module: ModuleId::ROOT,
            vis: Visibility::Pub,
            reachable: Vec::new(),
            ty,
            variants,
        }
    }
}

#[derive(Debug)]
pub struct FieldDef {
    pub name: String,
    pub span: Span,
    /// Private unless written `pub` (ADR 0050).
    pub vis: Visibility,
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
    /// The function's name as declared; for a body run at compile time,
    /// what it computes.
    pub name: String,
    /// Its path from the root module, as diagnostics and dumps show it
    /// (ADR 0049).
    pub path: String,
    /// Its symbol (ADR 0051): its path joined by dots, or its own name in
    /// the root module, with `#[export]` or for an `extern fn`. `None` for
    /// a body run at compile time, which is never emitted.
    pub symbol: Option<String>,
    pub name_span: Span,
    /// The module its code is written in, where its names are looked up.
    pub module: ModuleId,
    /// Private for a body run at compile time, which nothing names.
    pub vis: Visibility,
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
    /// A function of `core` (ADR 0052), which holds the runtime support
    /// functions (ADR 0020): LLVM must not turn its code into calls to
    /// them.
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
    /// `Option(T)` or `Result(T, E)` as written (ADR 0045); normalizing it
    /// makes it the enum it is for its arguments.
    Applied(Applied),
    /// Produced only by the type checker after reporting an error, so one
    /// mistake does not cascade into more. Compatible with every type.
    Error,
}

/// `Option(T)` or `Result(T, E)` before normalization; the arguments are
/// leaked like pointee types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Applied {
    pub builtin: BuiltinEnum,
    pub args: &'static [Ty],
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

/// An enum type (ADR 0035), with its integer type, which it is stored as,
/// or which is its tag's when it has payloads (ADR 0039). The name is
/// carried along like a struct's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnumTy {
    pub id: EnumId,
    pub name: &'static str,
    pub int: IntTy,
    /// A variant carries a payload: the enum is an aggregate, not its
    /// integer type (ADR 0039).
    pub payloads: bool,
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

    /// `Option(T)` or `Result(T, E)`, as written.
    pub fn applied(builtin: BuiltinEnum, args: Vec<Ty>) -> Ty {
        Ty::Applied(Applied {
            builtin,
            args: Box::leak(args.into_boxed_slice()),
        })
    }

    /// Whether every array length in the type is known, and every
    /// `Option(T)` and `Result(T, E)` is the enum it is (ADR 0045).
    pub fn is_normal(self) -> bool {
        match self {
            Ty::Array(elem, len) => matches!(len, ArrayLen::Known(_)) && elem.is_normal(),
            Ty::Slice(elem) => elem.is_normal(),
            Ty::Ptr(ptr) => ptr.pointee.is_normal(),
            Ty::Applied(_) => false,
            _ => true,
        }
    }

    /// Arrays, structs, slices and enums with payloads: values made of
    /// other values. A slice is an address and a length (ADR 0031); an
    /// enum with payloads, a tag and the values of a variant (ADR 0039),
    /// as `Option(T)` and `Result(T, E)` are (ADR 0045).
    pub fn is_aggregate(self) -> bool {
        matches!(
            self,
            Ty::Array(..)
                | Ty::Struct(_)
                | Ty::Enum(EnumTy { payloads: true, .. })
                | Ty::Applied(_)
        ) || self.slice_elem().is_some()
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
            Ty::Applied(applied) => {
                let args: Vec<String> = applied.args.iter().map(Ty::to_string).collect();
                write!(f, "{}({})", applied.builtin.name(), args.join(", "))
            }
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
    /// constant. With its arguments, `Enum::Variant(args)`, if written with
    /// parentheses: the values of its payload, in order (ADR 0039).
    Variant(EnumId, u32, Option<Vec<Expr>>),
    /// `.Variant`, with the span of its name, or `.Variant(args)`: a
    /// variant of the enum that is the expected type, which the type
    /// checker resolves (ADR 0044). Otherwise as `Variant`.
    ContextVariant(String, Span, Option<Vec<Expr>>),
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
    /// `operand?` (ADR 0046): the payload of an `Option`'s `Some` or a
    /// `Result`'s `Ok`, or else a return of its `.None` or `.Err(x)`.
    Try(Box<Expr>),
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
    /// `let [mut] name: T`: every value, copied into `local`. `ty` is the
    /// type written at `ty_span`, the local's own unless an earlier
    /// alternative of an or-pattern declared it (ADR 0041).
    Binding {
        local: LocalId,
        ty: Ty,
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
    /// `Enum::Variant`, by index, matching its value (ADR 0035, 0037), and
    /// the patterns of its payload values if written with parentheses,
    /// whatever their number (ADR 0040).
    Variant(EnumId, u32, Option<Vec<Pattern>>),
    /// `.Variant`, with the span of its name, or `.Variant(p, ...)`: a
    /// variant of the enum that is the type of its position, which the
    /// type checker resolves (ADR 0044). Otherwise as `Variant`.
    ContextVariant(PatternId, String, Span, Option<Vec<Pattern>>),
    /// `p | q | ...`, at any position; no alternative is an `Or`. Every
    /// alternative binds the same variables (ADR 0041).
    Or(Vec<Pattern>),
}

/// From a position holding a value of an enum with payloads, to one of the
/// payload values of its variant `variant` (ADR 0040).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PayloadStep {
    pub variant: u32,
    pub index: u32,
}

impl Pattern {
    /// The alternatives of an or-pattern, or the pattern itself.
    pub fn alternatives(&self) -> &[Pattern] {
        match &self.kind {
            PatternKind::Or(alternatives) => alternatives,
            _ => std::slice::from_ref(self),
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
            | ExprKind::Variant(_, _, None)
            | ExprKind::ContextVariant(_, _, None)
            | ExprKind::Local(_)
            | ExprKind::Continue { .. }
            | ExprKind::Break { value: None, .. }
            | ExprKind::Return(None) => {}
            ExprKind::Break { value: Some(e), .. } | ExprKind::Return(Some(e)) => e.walk(f),
            ExprKind::Call { args, .. }
            | ExprKind::Array(args)
            | ExprKind::Variant(_, _, Some(args))
            | ExprKind::ContextVariant(_, _, Some(args)) => {
                for arg in args {
                    arg.walk(f);
                }
            }
            ExprKind::Repeat(e, _)
            | ExprKind::Field(e, _, _)
            | ExprKind::Assert(e, None)
            | ExprKind::Len(e)
            | ExprKind::Comptime(e)
            | ExprKind::Try(e) => e.walk(f),
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
