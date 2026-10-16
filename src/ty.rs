//! Type checking (ADR 0008): every type is written or fixed by the
//! construct, so this pass only checks; it never infers.
//!
//! Checking is bidirectional: an expression may receive the type expected
//! where it appears. That is how literals get their types (`let x: u8 = 1`)
//! and why `x + 1` works: the `1` is checked against the type of `x`.
//!
//! The result is a side table from [`ExprId`] to [`Ty`]; HIR plus this table
//! is the "typed HIR" that MIR building consumes. Functions are checked one
//! at a time, on demand (ADR 0028).

use std::collections::HashMap;
use std::fmt::Write;

use crate::diag::Diagnostic;
use crate::hir::{
    self, BinOp, Block, Expr, ExprId, ExprKind, Fn, FnKind, Hir, IntTy, LocalId, NeverPosition,
    Ptr, StaticId, Stmt, Ty, UnOp, UnsizedPosition,
};
use crate::layout;
use crate::query::Queries;
use crate::span::Span;
use crate::target;

/// What type checking found out about expressions, for every function
/// checked so far.
pub struct Tables {
    expr_types: Vec<Ty>,
    /// The field index of each field access.
    fields: HashMap<ExprId, u32>,
    /// The slice type each pointer to an array is converted to, where it
    /// is (ADR 0031): unlike the other conversion (ADR 0024), it changes
    /// the representation.
    coercions: HashMap<ExprId, Ty>,
}

impl Tables {
    pub fn new(expr_count: u32) -> Tables {
        Tables {
            // Overwritten for every expression checked; the placeholder is
            // never read.
            expr_types: vec![Ty::Error; expr_count as usize],
            fields: HashMap::new(),
            coercions: HashMap::new(),
        }
    }

    pub fn expr_ty(&self, id: ExprId) -> Ty {
        self.expr_types[id.0 as usize]
    }

    /// The index of the field that the field access `id` reads.
    pub fn field(&self, id: ExprId) -> u32 {
        self.fields[&id]
    }

    /// The slice type that the value of `id`, a pointer to an array, is
    /// converted to, if it is (ADR 0031).
    pub fn coercion(&self, id: ExprId) -> Option<Ty> {
        self.coercions.get(&id).copied()
    }
}

/// Reports each `!` written where memory would have its type, which HIR
/// lowering made `{error}` (ADR 0034).
pub fn report_never_in_memory(q: &mut Queries) {
    let hir = q.hir;
    for never in &hir.never_in_memory {
        let help = match never.position {
            NeverPosition::Static => NeverHelp::Static,
            _ => NeverHelp::Unit,
        };
        q.diags.push(never_in_memory(
            never.span,
            &memory_position(never.position),
            help,
        ));
    }
}

/// Reports each `[T]` written where a size is needed, which HIR lowering
/// made `{error}` (ADR 0031).
pub fn report_unsized_types(q: &mut Queries) {
    let hir = q.hir;
    for found in &hir.unsized_types {
        let what = match found.position {
            UnsizedPosition::Memory(position) => memory_position(position),
            UnsizedPosition::Variable => "the type of a variable".to_owned(),
            UnsizedPosition::Parameter => "the type of a parameter".to_owned(),
            UnsizedPosition::Return => "a return type".to_owned(),
            UnsizedPosition::Cast => "the target of a cast".to_owned(),
        };
        // A length that failed was reported; the type shows it as `_`.
        let ty = match q.normalize(found.ty) {
            Ty::Error => found.ty,
            ty => ty,
        };
        q.diags.push(
            unsized_ty(ty)
                .label(found.span, format!("`{ty}` as {what}"))
                .help("its length is known only at run time, so it exists only behind a pointer, as a slice: `*const [T]` or `*mut [T]` (ADR 0031)"),
        );
    }
}

/// How diagnostics name a position of memory.
fn memory_position(position: NeverPosition) -> String {
    match position {
        NeverPosition::Pointee => "the pointee of a pointer".to_owned(),
        NeverPosition::SliceElement => "the element type of a slice".to_owned(),
        NeverPosition::Element => "an array element".to_owned(),
        NeverPosition::Field => "the type of a field".to_owned(),
        NeverPosition::Static => "the type of a static".to_owned(),
        NeverPosition::Const => "the type of a constant".to_owned(),
        NeverPosition::LayoutQuery(name) => format!("the operand of `{name}`"),
    }
}

/// `ty::unsized` for `ty`, a `[T]` (ADR 0031), without labels.
fn unsized_ty(ty: Ty) -> Diagnostic {
    Diagnostic::error(
        "ty::unsized",
        format!("`{ty}` has no size known at compile time"),
    )
}

/// What `ty::never-in-memory` suggests instead.
#[derive(Clone, Copy)]
enum NeverHelp {
    Unit,
    /// A static's address must be a real one.
    Static,
}

fn never_in_memory(span: Span, what: &str, help: NeverHelp) -> Diagnostic {
    let instead = match help {
        NeverHelp::Unit => "for a type without data, use `()` (ADR 0034)",
        NeverHelp::Static => {
            "use a type of non-zero size, such as `u8`, whose address is a real one (ADR 0022, 0034)"
        }
    };
    Diagnostic::error("ty::never-in-memory", "`!` is not a type of memory")
        .label(span, format!("`!` as {what}"))
        .help(format!(
            "`!` is the type of expressions that do not finish, so no memory holds one; {instead}"
        ))
}

/// Checks one function; `query` decides when.
pub fn check_fn(q: &mut Queries, f: &Fn) {
    let hir = q.hir;
    let mut checker = Checker {
        q,
        hir,
        ret: Ty::Error,
        loops: Vec::new(),
    };
    checker.fn_(f);
}

/// Whether a value of type `from` may be used where `to` is expected.
/// `!` has no values, so it fits anywhere; `{error}` was already reported.
/// The conversions are from `*mut T` to `*const T` (ADR 0024), and from a
/// pointer to an array to a slice of its elements (ADR 0031), both of the
/// outermost pointer only and keeping volatility.
fn coerces(from: Ty, to: Ty) -> bool {
    from == to
        || matches!(from, Ty::Never | Ty::Error)
        || to == Ty::Error
        || matches!((from, to), (Ty::Ptr(from_ptr), Ty::Ptr(to_ptr))
            if (from_ptr.mutable || !to_ptr.mutable)
                && from_ptr.volatile == to_ptr.volatile
                && from_ptr.pointee == to_ptr.pointee)
        || unsizes(from, to)
}

/// Whether `from` is a pointer to an array that converts to the slice `to`
/// (ADR 0031): of the same elements, mutability and volatility, or from
/// `*mut` to `*const`.
fn unsizes(from: Ty, to: Ty) -> bool {
    let (Ty::Ptr(from), Ty::Ptr(to)) = (from, to) else {
        return false;
    };
    matches!((*from.pointee, *to.pointee), (Ty::Array(elem, _), Ty::Slice(want)) if elem == want)
        && (from.mutable || !to.mutable)
        && from.volatile == to.volatile
}

/// The type that accepts `ty` and, if it is a pointer, the pointer with the
/// other mutability: the type a comparison expects of its second operand.
fn comparable(ty: Ty) -> Ty {
    match ty {
        Ty::Ptr(ptr) => Ty::Ptr(Ptr {
            mutable: false,
            ..ptr
        }),
        ty => ty,
    }
}

/// Expressions that cannot be typed on their own: integer literals, and
/// operators applied only to them.
fn needs_expected_type(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Int(_) => true,
        ExprKind::Unary(_, operand) | ExprKind::Comptime(operand) => needs_expected_type(operand),
        // A comparison is a `bool` whatever its operands are.
        ExprKind::Binary(op, _, _) if op.is_comparison() => false,
        ExprKind::Binary(op, lhs, _) if op.is_shift() => needs_expected_type(lhs),
        ExprKind::Binary(_, lhs, rhs) => needs_expected_type(lhs) && needs_expected_type(rhs),
        _ => false,
    }
}

/// The element type an expected array type asks for.
fn element_of(expected: Option<Ty>) -> Option<Ty> {
    match expected {
        Some(Ty::Array(elem, _)) => Some(*elem),
        Some(Ty::Error) => Some(Ty::Error),
        _ => None,
    }
}

struct Checker<'a, 'h> {
    q: &'a mut Queries<'h>,
    hir: &'h Hir,
    /// The return type of the function being checked, normalized.
    ret: Ty,
    /// Enclosing `loop`/`while` expressions, innermost last.
    loops: Vec<LoopCtx>,
}

/// What the type checker knows about a place expression.
#[derive(Clone, Copy)]
struct PlaceInfo {
    ty: Ty,
    /// Why the place cannot be written, if it cannot.
    immutable: Option<Immutable>,
    /// The last pointer on the path to the place, which decides whether
    /// accesses are volatile (ADR 0023).
    via: Option<Ptr>,
    /// The path from that pointer goes through a field of a `#[packed]`
    /// struct, so the place may be misaligned (ADR 0022).
    misaligned: bool,
    /// Part of a constant's value, which has no address (ADR 0029): it
    /// can be read, but `&raw` of it is not a place.
    constant: bool,
}

impl PlaceInfo {
    /// The place of an expression that failed to type; already reported.
    const ERROR: PlaceInfo = PlaceInfo {
        ty: Ty::Error,
        immutable: None,
        via: None,
        misaligned: false,
        constant: false,
    };

    /// The value of `base`, which is not a place expression, indexed or
    /// projected where it is.
    fn temporary(ty: Ty, base: &Expr) -> PlaceInfo {
        PlaceInfo {
            ty,
            immutable: Some(Immutable::Temporary(base.span)),
            via: None,
            misaligned: false,
            constant: matches!(base.kind, ExprKind::Const(_)),
        }
    }

    /// The place a pointer points to; `pointer` is the pointer's expression.
    fn through(ptr: Ptr, pointer: Span) -> PlaceInfo {
        PlaceInfo {
            ty: *ptr.pointee,
            immutable: (!ptr.mutable).then_some(Immutable::ConstPointer(ptr, pointer)),
            via: Some(ptr),
            // Pointers are assumed aligned (ADR 0019).
            misaligned: false,
            constant: false,
        }
    }
}

#[derive(Clone, Copy)]
enum Immutable {
    /// A local declared without `mut`.
    Local(LocalId),
    /// A place reached through a `*const` pointer, written at the span.
    ConstPointer(Ptr, Span),
    /// Part of a value that lives in no variable, written at the span.
    Temporary(Span),
    /// A `static`, which never changes (ADR 0030).
    Static(StaticId),
    /// A byte string, in read-only memory (ADR 0031), written at the span.
    ByteString(Span),
}

struct LoopCtx {
    id: ExprId,
    /// The type expected of the loop's value, if any.
    expected: Option<Ty>,
    /// The type of the first `break`, which later ones must match.
    break_ty: Option<Ty>,
}

impl Checker<'_, '_> {
    fn fn_(&mut self, f: &Fn) {
        let special = match f.kind {
            FnKind::Entry => Some((
                "ty::entry-returns",
                "entry",
                "there is nothing to return to on bare metal",
            )),
            FnKind::Panic => Some((
                "ty::panic-returns",
                "panic",
                "execution cannot continue after a panic (ADR 0014)",
            )),
            FnKind::Plain => None,
        };
        self.ret = self.norm(f.ret);
        if let Some((code, what, why)) = special
            && f.ret != Ty::Never
        {
            self.q.diags.push(
                Diagnostic::error(code, format!("the {what} function must never return"))
                    .label(
                        f.ret_span.unwrap_or(f.name_span),
                        format!("return type is `{}`", f.ret),
                    )
                    .help(format!("{why}; declare it `-> !` and end it with a `loop`")),
            );
        }

        self.params(f);
        // A constant's type is not a return type.
        let because = if f.comptime.is_some() {
            None
        } else {
            f.ret_span
        };
        for local in &f.locals {
            if self.q.layout_of(local.ty).is_none() {
                self.too_large(local.ty, local.span, "does not fit in the address space");
            }
        }

        if let Some(body) = &f.body {
            self.checked_block(f, body, self.ret, because);
        }
    }

    /// A type as written, with its array lengths computed (ADR 0029).
    fn norm(&mut self, ty: Ty) -> Ty {
        self.q.normalize(ty)
    }

    fn local_ty(&mut self, f: &Fn, id: LocalId) -> Ty {
        self.norm(f.local(id).ty)
    }

    /// The type queried by `size_of`, `align_of` or `offset_of` must have a
    /// layout (ADR 0022).
    fn queried_layout(&mut self, ty: Ty, span: Span) {
        if self.q.layout_of(ty).is_none() {
            self.too_large(ty, span, "larger than any object can be");
        }
    }

    fn too_large(&mut self, ty: Ty, span: Span, label: &str) {
        self.q.diags.push(
            Diagnostic::error("ty::too-large", format!("`{ty}` is too large"))
                .label(span, label)
                .help(format!(
                    "objects are limited to {} bytes on this target",
                    layout::MAX_SIZE
                )),
        );
    }

    /// Checks a block against an expected type. A tail expression reports its
    /// own mismatch; without a tail, the block itself is reported here.
    /// `because` points at what set the expectation, such as a return type.
    fn checked_block(&mut self, f: &Fn, block: &Block, expected: Ty, because: Option<Span>) -> Ty {
        let ty = self.block(f, block, Some(expected));
        if block.tail.is_none() && !coerces(ty, expected) {
            self.report_block_mismatch(block, expected, ty, because);
        }
        ty
    }

    /// Parameters carry values, so `()` and `!` make no sense there; the entry
    /// and panic functions are called by no one and take none.
    fn params(&mut self, f: &Fn) {
        if f.kind != FnKind::Plain
            && let Some(first) = f.params().first()
        {
            let what = if f.kind == FnKind::Entry {
                "entry"
            } else {
                "panic"
            };
            self.q.diags.push(
                Diagnostic::error(
                    "ty::special-fn-params",
                    format!("the {what} function takes no parameters"),
                )
                .label(first.span, "parameter here"),
            );
        }
        for param in f.params() {
            if matches!(param.ty, Ty::Unit | Ty::Never) {
                self.q.diags.push(
                    Diagnostic::error(
                        "ty::param-type",
                        format!("a parameter cannot have type `{}`", param.ty),
                    )
                    .label(param.span, "has no value to pass"),
                );
            }
        }
    }

    /// A block without a tail whose type is not the expected one.
    fn report_block_mismatch(
        &mut self,
        block: &Block,
        expected: Ty,
        found: Ty,
        because: Option<Span>,
    ) {
        let found_at = match block.stmts.last() {
            Some(Stmt::Expr { expr, .. }) => expr.span,
            _ => block.span,
        };
        let mut diag = Diagnostic::error("ty::mismatched-types", "mismatched types")
            .label(found_at, format!("expected `{expected}`, found `{found}`"));
        if let Some(because) = because {
            diag = diag.label(because, "expected because of this return type");
        }
        // ADR 0015: a value followed by `;` by habit.
        if let Some(Stmt::Expr {
            expr,
            semi: Some(semi),
        }) = block.stmts.last()
            && coerces(self.q.tables.expr_types[expr.id.0 as usize], expected)
        {
            diag = diag
                .label(*semi, "remove this semicolon to return the value")
                .help("a block's value is its last expression without `;`");
        }
        self.q.diags.push(diag);
    }

    /// A block's type is its tail's, or `()` without one; a block that cannot
    /// reach its end has type `!`.
    fn block(&mut self, f: &Fn, block: &Block, expected: Option<Ty>) -> Ty {
        let mut diverges = false;
        for stmt in &block.stmts {
            let ty = match stmt {
                Stmt::Let { local, init } => {
                    let ty = self.local_ty(f, *local);
                    self.check(f, init, Some(ty))
                }
                // A statement without `;` (`if`, `loop`, ...) must not
                // silently drop a value (ADR 0015).
                Stmt::Expr { expr, semi: None } => self.check(f, expr, Some(Ty::Unit)),
                Stmt::Expr { expr, .. } => self.check(f, expr, None),
            };
            diverges |= ty == Ty::Never;
        }
        match &block.tail {
            Some(tail) => self.check(f, tail, expected),
            None if diverges => Ty::Never,
            None => Ty::Unit,
        }
    }

    /// Types `expr`, records the type, and reports a mismatch with
    /// `expected` if there is one. Returns the expression's own type.
    fn check(&mut self, f: &Fn, expr: &Expr, expected: Option<Ty>) -> Ty {
        let mut ty = self.expr_kind(f, expr, expected);
        // A block passes the expected type to its tail, and `comptime` to
        // its operand, which report any mismatch themselves.
        let reported_inside = matches!(&expr.kind, ExprKind::Block(b) if b.tail.is_some())
            || matches!(expr.kind, ExprKind::Comptime(_));
        if let Some(expected) = expected
            && unsizes(ty, expected)
        {
            if reported_inside {
                // Converted inside, so this value is the slice.
                ty = expected;
            } else {
                self.q.tables.coercions.insert(expr.id, expected);
            }
        }
        self.q.tables.expr_types[expr.id.0 as usize] = ty;
        // Variables were checked where they are declared; any other
        // aggregate value needs a temporary of its size.
        if ty.is_aggregate()
            && !matches!(expr.kind, ExprKind::Local(_))
            && self.q.layout_of(ty).is_none()
        {
            self.too_large(
                ty,
                expr.span,
                "this value does not fit in the address space",
            );
        }
        if let Some(expected) = expected
            && !coerces(ty, expected)
            && !reported_inside
        {
            match &expr.kind {
                ExprKind::Block(block) => self.report_block_mismatch(block, expected, ty, None),
                _ => self.mismatch(expr, expected, ty),
            }
        }
        ty
    }

    fn mismatch(&mut self, expr: &Expr, expected: Ty, found: Ty) {
        let mut diag = Diagnostic::error("ty::mismatched-types", "mismatched types")
            .label(expr.span, format!("expected `{expected}`, found `{found}`"));
        if matches!((expected, found), (Ty::Int(_), Ty::Int(_))) {
            diag = diag.help(format!(
                "integer types never convert implicitly; use `as {expected}` (ADR 0008)"
            ));
        }
        if expected == Ty::Unit && !matches!(found, Ty::Unit | Ty::Never | Ty::Error) {
            diag = diag.help("this value would be discarded; add `;` after it if that is intended");
        }
        if let (Ty::Ptr(want), Ty::Ptr(have)) = (expected, found)
            && want.mutable
            && !have.mutable
        {
            diag = diag.help(
                "a `*const` pointer never becomes `*mut` implicitly; cast with `as` if writing through it is allowed",
            );
        }
        if expected == Ty::Never && matches!(expr.kind, ExprKind::While { .. }) {
            diag =
                diag.help("a `while` loop can finish even with `true`; use `loop` to run forever");
        }
        self.q.diags.push(diag);
    }

    fn expr_kind(&mut self, f: &Fn, expr: &Expr, expected: Option<Ty>) -> Ty {
        match &expr.kind {
            ExprKind::Int(value) => self.int_literal(expr, *value, false, expected),
            ExprKind::Byte(_) => Ty::Int(IntTy::U8),
            ExprKind::Bool(_) => Ty::Bool,
            ExprKind::Unit => Ty::Unit,
            ExprKind::Array(elems) => self.array(f, expr, elems, expected),
            ExprKind::Repeat(value, len) => {
                let ty = self.check(f, value, element_of(expected));
                match (ty, self.q.array_len(*len)) {
                    (Ty::Error, _) | (_, None) => Ty::Error,
                    // Like a block that does not finish (ADR 0034).
                    (Ty::Never, _) => Ty::Never,
                    (ty, Some(len)) => Ty::array(ty, len),
                }
            }
            ExprKind::Local(local) => self.local_ty(f, *local),
            ExprKind::Const(id) => self.norm(self.hir.const_(*id).ty),
            // The value is computed later; its type is checked here, with
            // the expected type passed through (ADR 0029).
            ExprKind::Comptime(inner) => match self.check(f, inner, expected) {
                // A value computed at compile time is data (ADR 0034).
                Ty::Never => {
                    self.q.diags.push(never_in_memory(
                        expr.span,
                        "the value of a `comptime` expression",
                        NeverHelp::Unit,
                    ));
                    Ty::Error
                }
                ty => ty,
            },
            ExprKind::SizeOf(ty) | ExprKind::AlignOf(ty) => {
                self.queried_layout(*ty, expr.span);
                Ty::Int(IntTy::Usize)
            }
            ExprKind::OffsetOf(ty, name, name_span) => {
                self.queried_layout(*ty, expr.span);
                match ty {
                    // The struct is not accessed: any place will do.
                    Ty::Struct(_) => {
                        self.field(expr, *ty, PlaceInfo::ERROR, name, *name_span);
                    }
                    Ty::Error => {}
                    other => self.q.diags.push(
                        Diagnostic::error("ty::no-fields", format!("`{other}` has no fields"))
                            .label(*name_span, "no such field"),
                    ),
                }
                Ty::Int(IntTy::Usize)
            }
            ExprKind::StructLit(s, fields) => {
                let hir = self.hir;
                let def = hir.struct_(s.id);
                for (index, value) in fields {
                    let ty = self.norm(def.fields[*index as usize].ty);
                    self.check(f, value, Some(ty));
                }
                Ty::Struct(*s)
            }
            ExprKind::Call { callee, args } => self.call(f, expr, *callee, args),
            ExprKind::Unary(op, operand) => self.unary(f, expr, *op, operand, expected),
            ExprKind::Binary(op, lhs, rhs) if op.is_shift() => {
                let lhs_ty = self.check(f, lhs, expected);
                self.require(lhs, lhs_ty, op.symbol(), Operands::Int);
                self.shift_amount(f, rhs, lhs_ty, op.symbol());
                lhs_ty
            }
            ExprKind::Binary(op, lhs, rhs) if op.is_comparison() => {
                // Both sides have one type, never converted (ADR 0008),
                // except that pointers may differ in mutability (ADR 0024).
                let ty = self.same_type_operands(f, lhs, rhs, None, comparable);
                let allowed = if matches!(op, BinOp::Eq | BinOp::Ne) {
                    Operands::Equatable
                } else {
                    Operands::Ordered
                };
                self.require(expr, ty, op.symbol(), allowed);
                Ty::Bool
            }
            ExprKind::Binary(op, lhs, rhs) => {
                let ty = self.same_type_operands(f, lhs, rhs, expected, |ty| ty);
                let allowed = if op.is_bitwise() {
                    Operands::IntOrBool
                } else {
                    Operands::Int
                };
                self.require(expr, ty, op.symbol(), allowed);
                ty
            }
            ExprKind::Logical(_, lhs, rhs) => {
                self.check(f, lhs, Some(Ty::Bool));
                self.check(f, rhs, Some(Ty::Bool));
                Ty::Bool
            }
            ExprKind::Cast(operand, target, target_span) => {
                let target = self.norm(*target);
                self.cast(f, operand, target, *target_span)
            }
            ExprKind::Deref(_)
            | ExprKind::Index(..)
            | ExprKind::Field(..)
            | ExprKind::Static(_)
            | ExprKind::ByteString(_) => {
                let info = self
                    .place_info(f, expr)
                    .expect("dereferences, indexing, fields, statics and byte strings are places");
                self.access(&info, expr.span);
                info.ty
            }
            ExprKind::AddrOf { mutable, place } => self.addr_of(f, *mutable, place),
            ExprKind::Assign(place, value) => {
                let place_ty = self.place(f, place);
                self.check(f, value, Some(place_ty));
                Ty::Unit
            }
            ExprKind::CompoundAssign(op, place, value) => {
                let place_ty = self.place(f, place);
                let allowed = if op.is_bitwise() {
                    Operands::IntOrBool
                } else {
                    Operands::Int
                };
                self.require(place, place_ty, op.symbol(), allowed);
                if op.is_shift() {
                    self.shift_amount(f, value, place_ty, op.symbol());
                } else {
                    self.check(f, value, Some(place_ty));
                }
                Ty::Unit
            }
            ExprKind::Assert(cond) => {
                self.check(f, cond, Some(Ty::Bool));
                Ty::Unit
            }
            ExprKind::Len(slice) => {
                self.len(f, slice);
                Ty::Int(IntTy::Usize)
            }
            ExprKind::Slice(pointer, len) => self.make_slice(f, pointer, len),
            ExprKind::Block(block) => self.block(f, block, expected),
            ExprKind::If { cond, then, else_ } => {
                self.check(f, cond, Some(Ty::Bool));
                let Some(else_) = else_ else {
                    // Without `else`, the missing branch has type `()`.
                    if let Some(want) = expected
                        && !coerces(Ty::Unit, want)
                    {
                        self.q.diags.push(
                            Diagnostic::error(
                                "ty::if-without-else",
                                format!("`if` without `else` has no `{want}` value"),
                            )
                            .label(expr.span, format!("expected `{want}`"))
                            .help("add an `else` branch for when the condition is false"),
                        );
                        self.block(f, then, Some(want));
                        return Ty::Error;
                    }
                    self.block(f, then, Some(Ty::Unit));
                    return Ty::Unit;
                };
                match expected {
                    Some(expected) => {
                        let then_ty = self.checked_block(f, then, expected, None);
                        let else_ty = self.check(f, else_, Some(expected));
                        // If neither branch finishes, neither does the `if`,
                        // so the code after it is unreachable.
                        if then_ty == Ty::Never && else_ty == Ty::Never {
                            Ty::Never
                        } else {
                            expected
                        }
                    }
                    None => {
                        // The first branch that can finish gives the type.
                        let then_ty = self.block(f, then, None);
                        let else_expected = (then_ty != Ty::Never).then_some(then_ty);
                        let else_ty = self.check(f, else_, else_expected);
                        if then_ty == Ty::Never {
                            else_ty
                        } else {
                            then_ty
                        }
                    }
                }
            }
            ExprKind::While { cond, body } => {
                self.check(f, cond, Some(Ty::Bool));
                self.in_loop(expr.id, None, |this| this.block(f, body, Some(Ty::Unit)));
                // The condition is not evaluated at compile time, so even
                // `while true` may finish.
                Ty::Unit
            }
            ExprKind::Loop(body) => {
                let ctx = self.in_loop(expr.id, expected, |this| {
                    this.block(f, body, Some(Ty::Unit))
                });
                // Without a `break`, a loop never finishes.
                ctx.break_ty.unwrap_or(Ty::Never)
            }
            ExprKind::Break { target, value } => {
                self.break_(f, expr, *target, value.as_deref());
                Ty::Never
            }
            ExprKind::Continue { .. } => Ty::Never,
            ExprKind::Return(value) => {
                match value {
                    Some(value) => {
                        self.check(f, value, Some(self.ret));
                    }
                    None if !coerces(Ty::Unit, self.ret) => self.mismatch(expr, self.ret, Ty::Unit),
                    None => {}
                }
                Ty::Never
            }
        }
    }

    /// `[a, b, c]`: the elements share one type, which the expected array
    /// type gives or, without one, the first element of a type other than
    /// `!` typed on its own. An element of type `!` makes the whole array
    /// one, like a block that does not finish (ADR 0034).
    fn array(&mut self, f: &Fn, expr: &Expr, elems: &[Expr], expected: Option<Ty>) -> Ty {
        let mut never = false;
        let elem_ty = match element_of(expected) {
            Some(want) => {
                for elem in elems {
                    never |= self.check(f, elem, Some(want)) == Ty::Never;
                }
                Some(want)
            }
            None => {
                let mut elem_ty = None;
                // Waiting for a type to check against.
                let mut pending = Vec::new();
                for elem in elems {
                    match elem_ty {
                        Some(ty) => never |= self.check(f, elem, Some(ty)) == Ty::Never,
                        None if needs_expected_type(elem) => pending.push(elem),
                        None => match self.check(f, elem, None) {
                            Ty::Never => never = true,
                            ty => {
                                elem_ty = Some(ty);
                                for elem in pending.drain(..) {
                                    never |= self.check(f, elem, Some(ty)) == Ty::Never;
                                }
                            }
                        },
                    }
                }
                // `[]` has no element to take a type from either.
                if !pending.is_empty() || (elem_ty.is_none() && !never) {
                    self.q.diags.push(
                        Diagnostic::error(
                            "ty::untyped-literal",
                            "cannot tell the type of this array's elements",
                        )
                        .label(expr.span, "no type is expected here")
                        .help("bind it first, e.g. `let a: [u8; 3] = ...` (ADR 0008)"),
                    );
                    for elem in pending {
                        self.check(f, elem, Some(Ty::Error));
                    }
                    return Ty::Error;
                }
                elem_ty
            }
        };
        match elem_ty {
            _ if never => Ty::Never,
            Some(Ty::Error) => Ty::Error,
            Some(ty) => Ty::array(ty, elems.len() as u64),
            None => unreachable!("an array without an element type was reported"),
        }
    }

    /// Each argument is checked against its parameter's type.
    fn call(&mut self, f: &Fn, expr: &Expr, callee: hir::FnId, args: &[Expr]) -> Ty {
        let target = self.hir.fn_(callee);
        let params = target.params();
        if args.len() != params.len() {
            let plural = if params.len() == 1 { "" } else { "s" };
            self.q.diags.push(
                Diagnostic::error(
                    "ty::wrong-arg-count",
                    format!(
                        "`{}` takes {} argument{plural}, but {} {} given",
                        target.name,
                        params.len(),
                        args.len(),
                        if args.len() == 1 { "was" } else { "were" }
                    ),
                )
                .label(expr.span, "in this call")
                .label(target.name_span, "declared here"),
            );
        }
        for (i, arg) in args.iter().enumerate() {
            // Extra arguments were reported above; `{error}` keeps them quiet.
            let expected = params.get(i).map_or(Ty::Error, |param| param.ty);
            let expected = self.norm(expected);
            self.check(f, arg, Some(expected));
        }
        self.norm(target.ret)
    }

    /// `len(s)` takes any slice, or a pointer to an array, which converts
    /// to one with its mutability and volatility (ADR 0031).
    fn len(&mut self, f: &Fn, slice: &Expr) {
        let ty = self.check(f, slice, None);
        let Ty::Ptr(ptr) = ty else {
            if !matches!(ty, Ty::Never | Ty::Error) {
                self.q.diags.push(
                    Diagnostic::error("ty::mismatched-types", "mismatched types")
                        .label(slice.span, format!("expected a slice, found `{ty}`"))
                        .help("`len(s)` takes a slice, such as a `*const [u8]`, or a pointer to an array (ADR 0031)"),
                );
            }
            return;
        };
        match *ptr.pointee {
            Ty::Slice(_) => {}
            Ty::Array(elem, _) => {
                let slice_ty = Ty::ptr(ptr.mutable, ptr.volatile, Ty::slice(*elem));
                self.q.tables.coercions.insert(slice.id, slice_ty);
            }
            _ => self.q.diags.push(
                Diagnostic::error("ty::mismatched-types", "mismatched types")
                    .label(slice.span, format!("expected a slice, found `{ty}`"))
                    .help("`len(s)` takes a slice, or a pointer to an array; a slice of what a plain pointer points to is made with `slice(p, n)` (ADR 0031)"),
            ),
        }
    }

    /// `slice(p, n)`: `n` elements from where `p` points, of its
    /// mutability and volatility, unchecked (ADR 0031).
    fn make_slice(&mut self, f: &Fn, pointer: &Expr, len: &Expr) -> Ty {
        let ty = self.check(f, pointer, None);
        self.check(f, len, Some(Ty::Int(IntTy::Usize)));
        match ty {
            Ty::Ptr(ptr) => match *ptr.pointee {
                Ty::Slice(_) => {
                    self.q.diags.push(
                        unsized_ty(*ptr.pointee)
                            .label(
                                pointer.span,
                                format!("a `{ty}`, so its elements would be `{}`", ptr.pointee),
                            )
                            .help("`slice` takes a pointer to the first element, such as `s as *const T` for a slice `s` (ADR 0031)"),
                    );
                    Ty::Error
                }
                pointee => Ty::ptr(ptr.mutable, ptr.volatile, Ty::slice(pointee)),
            },
            Ty::Never => Ty::Never,
            Ty::Error => Ty::Error,
            other => {
                self.q.diags.push(
                    Diagnostic::error("ty::mismatched-types", "mismatched types")
                        .label(pointer.span, format!("expected a pointer, found `{other}`"))
                        .help("`slice(p, n)` makes a slice from a pointer to its first element and a length (ADR 0031)"),
                );
                Ty::Error
            }
        }
    }

    fn in_loop(
        &mut self,
        id: ExprId,
        expected: Option<Ty>,
        body: impl FnOnce(&mut Self) -> Ty,
    ) -> LoopCtx {
        self.loops.push(LoopCtx {
            id,
            expected,
            break_ty: None,
        });
        body(self);
        self.loops.pop().expect("pushed above")
    }

    /// The value of a `break` must have the loop's expected type or, without
    /// one, the type of the loop's first `break`.
    fn break_(&mut self, f: &Fn, expr: &Expr, target: ExprId, value: Option<&Expr>) {
        let index = self
            .loops
            .iter()
            .rposition(|ctx| ctx.id == target)
            .expect("lowering resolved the loop");
        let ctx = &self.loops[index];
        let want = ctx.expected.or(ctx.break_ty);
        let ty = match value {
            Some(value) => self.check(f, value, want),
            None => {
                if let Some(want) = want
                    && !coerces(Ty::Unit, want)
                {
                    self.mismatch(expr, want, Ty::Unit);
                }
                Ty::Unit
            }
        };
        let ctx = &mut self.loops[index];
        if ctx.break_ty.is_none() {
            ctx.break_ty = Some(want.unwrap_or(ty));
        }
    }

    /// `negated` covers `-128` for `i8`, whose magnitude is one past the max.
    fn int_literal(&mut self, expr: &Expr, value: u64, negated: bool, expected: Option<Ty>) -> Ty {
        match expected {
            Some(Ty::Int(int)) => {
                let limit = if negated && int.is_signed() {
                    int.max() + 1
                } else {
                    int.max()
                };
                if value > limit {
                    let sign = if negated { "-" } else { "" };
                    self.q.diags.push(
                        Diagnostic::error(
                            "ty::literal-out-of-range",
                            format!("literal out of range for `{int}`"),
                        )
                        .label(
                            expr.span,
                            format!("`{sign}{value}` does not fit in `{int}`"),
                        ),
                    );
                }
                Ty::Int(int)
            }
            Some(Ty::Error) => Ty::Error,
            Some(other) => {
                self.q.diags.push(
                    Diagnostic::error("ty::mismatched-types", "mismatched types")
                        .label(expr.span, format!("expected `{other}`, found an integer")),
                );
                Ty::Error
            }
            None => {
                self.q.diags.push(
                    Diagnostic::error(
                        "ty::untyped-literal",
                        "cannot tell the type of this literal",
                    )
                    .label(expr.span, "no type is expected here")
                    .help("literals take the type expected where they appear; bind it first, e.g. `let x: u32 = ...` (ADR 0008)"),
                );
                Ty::Error
            }
        }
    }

    fn unary(&mut self, f: &Fn, expr: &Expr, op: UnOp, operand: &Expr, expected: Option<Ty>) -> Ty {
        let ty = match (op, &operand.kind) {
            (UnOp::Neg, ExprKind::Int(value)) => {
                let ty = self.int_literal(expr, *value, true, expected);
                self.q.tables.expr_types[operand.id.0 as usize] = ty;
                ty
            }
            _ => self.check(f, operand, expected),
        };
        match (op, ty) {
            (UnOp::Not, ty) => {
                self.require(expr, ty, "!", Operands::IntOrBool);
                ty
            }
            (UnOp::Neg, Ty::Int(int)) if !int.is_signed() => {
                self.q.diags.push(
                    Diagnostic::error(
                        "ty::negate-unsigned",
                        format!("cannot negate a value of unsigned type `{int}`"),
                    )
                    .label(expr.span, "unsigned")
                    .help("for a wrapping negation, write `0 - x`"),
                );
                Ty::Error
            }
            (UnOp::Neg, ty) => {
                self.require(expr, ty, "-", Operands::Int);
                ty
            }
        }
    }

    /// Both operands must have the same integer type. Whichever side can be
    /// typed alone gives its type to the other (`1 + x` and `x + 1`).
    /// `second` gives the type expected of the operand typed second from
    /// the type of the first.
    fn same_type_operands(
        &mut self,
        f: &Fn,
        lhs: &Expr,
        rhs: &Expr,
        expected: Option<Ty>,
        second: fn(Ty) -> Ty,
    ) -> Ty {
        if expected.is_none() && needs_expected_type(lhs) && !needs_expected_type(rhs) {
            let rhs_ty = self.check(f, rhs, None);
            self.check(f, lhs, Some(second(rhs_ty)));
            rhs_ty
        } else {
            let lhs_ty = self.check(f, lhs, expected);
            self.check(f, rhs, Some(second(lhs_ty)));
            lhs_ty
        }
    }

    /// Shift amounts may be any integer type and are masked to the operand
    /// width (ADR 0007); a literal amount takes the shifted type. Whether a
    /// known amount is in range is checked with MIR (ADR 0032).
    fn shift_amount(&mut self, f: &Fn, amount: &Expr, shifted: Ty, op: &str) {
        if needs_expected_type(amount) {
            self.check(f, amount, Some(shifted));
        } else {
            let ty = self.check(f, amount, None);
            self.require(amount, ty, op, Operands::Int);
        }
    }

    /// Integers and pointers convert to each other with `as` (ADR 0013). A
    /// literal operand takes the target type, or `usize` for a pointer
    /// target (ADR 0017).
    fn cast(&mut self, f: &Fn, operand: &Expr, target: Ty, target_span: Span) -> Ty {
        let literal_ty = match target {
            Ty::Int(_) => Some(target),
            Ty::Ptr(_) => Some(Ty::Int(IntTy::Usize)),
            // Reported, such as `as *const !` (ADR 0034).
            Ty::Error => Some(Ty::Error),
            _ => None,
        };
        let expected = if needs_expected_type(operand) {
            literal_ty
        } else {
            None
        };
        let from = self.check(f, operand, expected);
        let castable = |ty: Ty| matches!(ty, Ty::Int(_) | Ty::Ptr(_) | Ty::Error | Ty::Never);
        if from == Ty::Bool {
            // `true as u8 == 1`; a bool converts to integers only.
            if !matches!(target, Ty::Int(_) | Ty::Error) {
                self.q.diags.push(
                    Diagnostic::error(
                        "ty::invalid-cast",
                        format!("cannot cast `bool` to `{target}`"),
                    )
                    .label(target_span, "a `bool` converts only to integer types"),
                );
                return Ty::Error;
            }
            return target;
        }
        if !castable(from) {
            self.q.diags.push(
                Diagnostic::error("ty::invalid-cast", format!("cannot cast `{from}`")).label(
                    operand.span,
                    "only integers, pointers and `bool` can be cast",
                ),
            );
        }
        if target == Ty::Bool {
            self.q.diags.push(
                Diagnostic::error("ty::invalid-cast", "cannot cast to `bool`")
                    .label(target_span, "no conversion to `bool`")
                    .help("say what you mean with a comparison, such as `x != 0`"),
            );
            return Ty::Error;
        }
        if !castable(target) {
            self.q.diags.push(
                Diagnostic::error("ty::invalid-cast", format!("cannot cast to `{target}`"))
                    .label(target_span, "only integers and pointers can be cast to"),
            );
            return Ty::Error;
        }
        if from.slice_elem().is_some() || target.slice_elem().is_some() {
            self.slice_cast(from, target, target_span);
            return target;
        }
        // Pointers convert only to and from `usize`, so any widening or
        // narrowing is a visible integer conversion (ADR 0019).
        let usize_ty = Ty::Int(IntTy::Usize);
        match (from, target) {
            (Ty::Int(_), Ty::Ptr(_)) if from != usize_ty => {
                self.q.diags.push(
                    Diagnostic::error(
                        "ty::pointer-cast-width",
                        format!("cannot cast `{from}` to a pointer"),
                    )
                    .label(operand.span, format!("has type `{from}`"))
                    .help(format!(
                        "only `usize` converts to pointers; write `x as usize as {target}` (ADR 0019)"
                    )),
                );
            }
            (Ty::Ptr(_), Ty::Int(_)) if target != usize_ty => {
                self.q.diags.push(
                    Diagnostic::error(
                        "ty::pointer-cast-width",
                        format!("cannot cast a pointer to `{target}`"),
                    )
                    .label(target_span, "not `usize`")
                    .help(format!(
                        "pointers convert only to `usize`; write `p as usize as {target}` (ADR 0019)"
                    )),
                );
            }
            _ => {}
        }
        target
    }

    /// A cast from or to a slice, which never invents a length or
    /// reinterprets one (ADR 0031): a slice converts to any thin pointer, to
    /// the address of its first element, and to slices of its elements; a
    /// pointer to an array of `T` converts to a slice of `T`.
    fn slice_cast(&mut self, from: Ty, target: Ty, target_span: Span) {
        // An erroneous target, such as `[u8]` or an array whose length
        // failed, was reported.
        if target == Ty::Error {
            return;
        }
        let (label, help) = match (from, from.slice_elem(), target.slice_elem()) {
            (_, Some(from_elem), Some(to_elem)) if from_elem == to_elem => return,
            (_, Some(_), Some(_)) => (
                "a slice of other elements",
                "its length counts elements, so this would reinterpret it; cast to a pointer to the first element, and make a new slice with `slice(p, n)` (ADR 0031)",
            ),
            (_, Some(_), None) if matches!(target, Ty::Ptr(_)) => return,
            (_, Some(_), None) => (
                "a slice is an address and a length, not an integer",
                "cast its address instead, as in `s as *const u8 as usize` (ADR 0031)",
            ),
            (Ty::Ptr(ptr), None, Some(elem)) if matches!(*ptr.pointee, Ty::Array(array_elem, _) if *array_elem == elem) =>
            {
                return;
            }
            (Ty::Never | Ty::Error, None, Some(_)) => return,
            (Ty::Ptr(ptr), None, Some(_)) if matches!(*ptr.pointee, Ty::Array(..)) => (
                "a slice of other elements than the array's",
                "a pointer to an array converts only to a slice of its elements; for others, make one with `slice(p, n)` (ADR 0031)",
            ),
            (Ty::Ptr(_), None, Some(_)) => (
                "a slice needs a length, which this pointer does not have",
                "only a pointer to an array of its elements converts to a slice; make one with `slice(p, n)` (ADR 0031)",
            ),
            (_, None, Some(_)) => (
                "a slice needs a length, and an address that is a pointer",
                "make one with `slice(p, n)`, from a pointer and a length (ADR 0031)",
            ),
            (_, None, None) => unreachable!("one of the types is a slice"),
        };
        self.q.diags.push(
            Diagnostic::error(
                "ty::invalid-cast",
                format!("cannot cast `{from}` to `{target}`"),
            )
            .label(target_span, label)
            .help(help),
        );
    }

    /// The place a pointer of type `ty` leads to, for `*` and `[]`. A
    /// pointer of type `!` is never computed, so neither is the place. The
    /// `[T]` a slice points to has no size, so it is no place to access
    /// (ADR 0031); `place` is the expression of the place.
    fn through_pointer(
        &mut self,
        ty: Ty,
        pointer: &Expr,
        place: &Expr,
        error: Diagnostic,
    ) -> PlaceInfo {
        match ty {
            Ty::Ptr(ptr) if matches!(*ptr.pointee, Ty::Slice(_)) => {
                self.q.diags.push(
                    unsized_ty(*ptr.pointee)
                        .label(
                            place.span,
                            format!("what the slice `{ty}` points to has type `{}`", ptr.pointee),
                        )
                        .help("the length of a slice is known only at run time; `len(s)` gives it (ADR 0031)"),
                );
                PlaceInfo::ERROR
            }
            Ty::Ptr(ptr) => PlaceInfo::through(ptr, pointer.span),
            Ty::Never => PlaceInfo {
                ty: Ty::Never,
                ..PlaceInfo::ERROR
            },
            Ty::Error => PlaceInfo::ERROR,
            _ => {
                self.q.diags.push(error);
                PlaceInfo::ERROR
            }
        }
    }

    /// Checks a read or write of a place. A volatile access is exactly one
    /// instruction (ADR 0023), so it cannot be wider than the target's
    /// widest access, nor a whole aggregate.
    fn access(&mut self, place: &PlaceInfo, span: Span) {
        let volatile = place.via.is_some_and(|ptr| ptr.volatile);
        if volatile && !place.ty.is_aggregate() && self.may_be_misaligned(place) {
            self.q.diags.push(
                Diagnostic::error(
                    "ty::volatile-packed-field",
                    format!("a volatile `{}` here may be misaligned", place.ty),
                )
                .label(span, "reached through a field of a `#[packed]` struct")
                .help("it would be split into several accesses; register blocks use the default layout, with explicit padding fields (ADR 0022, 0023)"),
            );
        }
        if volatile && place.ty.slice_elem().is_some() {
            self.q.diags.push(
                Diagnostic::error(
                    "ty::volatile-aggregate",
                    format!("a volatile `{}` cannot be accessed as a whole", place.ty),
                )
                .label(span, "a slice is an address and a length, two accesses")
                .help("a slice is an aggregate, whose bus accesses would be up to the compiler (ADR 0023, 0031)"),
            );
        } else if volatile && place.ty.is_aggregate() {
            self.q.diags.push(
                Diagnostic::error(
                    "ty::volatile-aggregate",
                    format!("a volatile `{}` cannot be accessed as a whole", place.ty),
                )
                .label(span, "access its elements or fields one at a time")
                .help("the width and number of the bus accesses would be up to the compiler (ADR 0023)"),
            );
        }
        if let (true, Ty::Int(int)) = (volatile, place.ty)
            && int.bits() > target::WIDEST_ACCESS_BITS
        {
            let half = target::WIDEST_ACCESS_BITS;
            self.q.diags.push(
                Diagnostic::error(
                    "ty::volatile-too-wide",
                    format!("a volatile `{int}` cannot be accessed in one instruction"),
                )
                .label(span, format!("the widest single access is {half} bits"))
                .help(format!(
                    "access it as `u{half}` halves, in the order the device needs (ADR 0023)"
                )),
            );
        }
    }

    /// Types a place expression: a local, a static, a byte string,
    /// `*pointer`, `base[index]` or `base.field`.
    /// Returns `None`, having typed nothing, if `expr` is not a place.
    fn place_info(&mut self, f: &Fn, expr: &Expr) -> Option<PlaceInfo> {
        let info = match &expr.kind {
            ExprKind::Local(id) => {
                let local = f.local(*id);
                PlaceInfo {
                    ty: self.norm(local.ty),
                    immutable: (!local.mutable).then_some(Immutable::Local(*id)),
                    via: None,
                    misaligned: false,
                    constant: false,
                }
            }
            ExprKind::ByteString(bytes) => PlaceInfo {
                ty: Ty::array(Ty::Int(IntTy::U8), bytes.len() as u64),
                immutable: Some(Immutable::ByteString(expr.span)),
                via: None,
                misaligned: false,
                constant: false,
            },
            ExprKind::Static(id) => {
                let def = self.hir.static_(*id);
                PlaceInfo {
                    ty: self.norm(def.ty),
                    immutable: (!def.mutable).then_some(Immutable::Static(*id)),
                    via: None,
                    misaligned: false,
                    constant: false,
                }
            }
            ExprKind::Deref(pointer) => {
                let ty = self.check(f, pointer, None);
                let error = Diagnostic::error(
                    "ty::deref-non-pointer",
                    format!("cannot dereference `{ty}`"),
                )
                .label(pointer.span, "not a pointer");
                self.through_pointer(ty, pointer, expr, error)
            }
            ExprKind::Field(base, name, name_span) => {
                let (ty, base_place) = self.base(f, base);
                self.field(expr, ty, base_place, name, *name_span)
            }
            ExprKind::Index(base, index) => {
                // An array is indexed where it is; anything else is read as
                // a value, which must be a pointer.
                let base_place = self.place_info(f, base);
                let ty = match base_place {
                    Some(info) => info.ty,
                    None => self.check(f, base, None),
                };
                self.check(f, index, Some(Ty::Int(IntTy::Usize)));
                if let Ty::Array(elem, _) = ty {
                    let base_place = base_place.unwrap_or_else(|| PlaceInfo::temporary(ty, base));
                    PlaceInfo {
                        ty: *elem,
                        ..base_place
                    }
                } else {
                    if let Some(info) = base_place {
                        self.access(&info, base.span);
                    }
                    if let Ty::Ptr(ptr) = ty
                        && !matches!(*ptr.pointee, Ty::Slice(_))
                        && self.q.layout_of(*ptr.pointee).is_none()
                    {
                        self.too_large(
                            *ptr.pointee,
                            base.span,
                            "the distance between elements does not fit in the address space",
                        );
                    }
                    let error = Diagnostic::error(
                        "ty::cannot-index",
                        format!("cannot index a value of type `{ty}`"),
                    )
                    .label(base.span, "not an array or a pointer");
                    // Unchecked, as in C (ADR 0021).
                    self.through_pointer(ty, base, expr, error)
                }
            }
            _ => return None,
        };
        self.q.tables.expr_types[expr.id.0 as usize] = info.ty;
        Some(info)
    }

    /// The type of the base of a field access or of indexing, and its
    /// place: the base's own if it is a place expression, or else a
    /// temporary holding its value.
    fn base(&mut self, f: &Fn, base: &Expr) -> (Ty, PlaceInfo) {
        match self.place_info(f, base) {
            Some(info) => (info.ty, info),
            None => {
                let ty = self.check(f, base, None);
                (ty, PlaceInfo::temporary(ty, base))
            }
        }
    }

    /// `base.name`, where `base` has type `ty` and place `base_place`.
    fn field(
        &mut self,
        expr: &Expr,
        ty: Ty,
        base_place: PlaceInfo,
        name: &str,
        name_span: Span,
    ) -> PlaceInfo {
        let hir = self.hir;
        let s = match ty {
            Ty::Struct(s) => s,
            Ty::Never => {
                return PlaceInfo {
                    ty: Ty::Never,
                    ..PlaceInfo::ERROR
                };
            }
            Ty::Error => return PlaceInfo::ERROR,
            other => {
                let mut diag =
                    Diagnostic::error("ty::no-fields", format!("`{other}` has no fields"))
                        .label(name_span, "no such field");
                if let Ty::Ptr(ptr) = other
                    && let Ty::Struct(_) = *ptr.pointee
                {
                    diag = diag.help(format!(
                        "dereference the pointer first: `(*p).{name}` (ADR 0021)"
                    ));
                }
                if other.slice_elem().is_some() && name == "len" {
                    diag = diag.help("the length of a slice `s` is `len(s)` (ADR 0031)");
                }
                self.q.diags.push(diag);
                return PlaceInfo::ERROR;
            }
        };
        let def = hir.struct_(s.id);
        let Some(index) = def.fields.iter().position(|field| field.name == name) else {
            let names: Vec<String> = def.fields.iter().map(|f| format!("`{}`", f.name)).collect();
            let mut diag = Diagnostic::error(
                "ty::unknown-field",
                format!("`{}` has no field `{name}`", s.name),
            )
            .label(name_span, "unknown field");
            diag = if names.is_empty() {
                diag.help(format!("`{}` has no fields", s.name))
            } else {
                diag.help(format!("its fields are {}", names.join(", ")))
            };
            self.q.diags.push(diag);
            return PlaceInfo::ERROR;
        };
        self.q.tables.fields.insert(expr.id, index as u32);
        PlaceInfo {
            ty: self.norm(def.fields[index].ty),
            misaligned: base_place.misaligned || def.packed,
            ..base_place
        }
    }

    /// Checks that `place` can be assigned to and returns its type.
    fn place(&mut self, f: &Fn, place: &Expr) -> Ty {
        let Some(info) = self.place_info(f, place) else {
            self.check(f, place, None);
            self.q.diags.push(
                Diagnostic::error("ty::not-assignable", "cannot assign to this expression")
                    .label(place.span, "not a variable, `*pointer` or `a[index]`"),
            );
            return Ty::Error;
        };
        match info.immutable {
            Some(Immutable::Local(id)) => {
                let local = f.local(id);
                self.q.diags.push(
                    Diagnostic::error(
                        "ty::assign-immutable",
                        format!("cannot assign to `{}`, which is not `mut`", local.name),
                    )
                    .label(place.span, "assigned here")
                    .label(
                        local.span,
                        format!("help: declare it `let mut {}`", local.name),
                    ),
                );
            }
            Some(Immutable::ConstPointer(ptr, pointer)) => {
                self.q.diags.push(
                    Diagnostic::error(
                        "ty::write-through-const",
                        "cannot write through a `*const` pointer",
                    )
                    .label(pointer, format!("has type `{}`", Ty::Ptr(ptr)))
                    .help("only `*mut` pointers can be written through"),
                );
            }
            Some(Immutable::Temporary(base)) => {
                self.q.diags.push(
                    Diagnostic::error(
                        "ty::not-assignable",
                        "cannot assign to part of a temporary value",
                    )
                    .label(base, "this value lives in no variable")
                    .help("store it in a `let mut` first"),
                );
            }
            Some(Immutable::Static(id)) => {
                let def = self.hir.static_(id);
                self.q.diags.push(
                    Diagnostic::error(
                        "ty::assign-immutable",
                        format!("cannot assign to `{}`, which is not `mut`", def.name),
                    )
                    .label(place.span, "assigned here")
                    .label(
                        def.name_span,
                        format!("help: declare it `{}`", mutable_static(def)),
                    ),
                );
            }
            Some(Immutable::ByteString(span)) => {
                self.q.diags.push(
                    Diagnostic::error("ty::assign-immutable", "cannot assign to a byte string")
                        .label(span, "in read-only memory")
                        .help("copy it into a `let mut` to change it (ADR 0031)"),
                );
            }
            None => {}
        }
        self.access(&info, place.span);
        info.ty
    }

    /// A place inside a packed struct whose type needs alignment.
    fn may_be_misaligned(&mut self, place: &PlaceInfo) -> bool {
        place.misaligned && self.q.layout_of(place.ty).is_some_and(|l| l.align > 1)
    }

    /// `&raw const place` or `&raw mut place` (ADR 0021). The address of a
    /// place is taken without accessing it.
    fn addr_of(&mut self, f: &Fn, mutable: bool, place: &Expr) -> Ty {
        let not_a_place = Diagnostic::error("ty::not-a-place", "`&raw` needs a place in memory")
            .label(place.span, "a value, not a place")
            .help("take the address of a variable, `*pointer` or `a[index]`; to point at a value, store it in a `let` first");
        let Some(info) = self.place_info(f, place) else {
            self.check(f, place, None);
            self.q.diags.push(not_a_place);
            return Ty::Error;
        };
        // Its result would be a `*const !` (ADR 0034).
        if info.ty == Ty::Never {
            self.q.diags.push(never_in_memory(
                place.span,
                "the type of a place under `&raw`",
                NeverHelp::Unit,
            ));
            return Ty::Error;
        }
        // An element or field of a constant is read where the constant's
        // data is, but has no address of its own (ADR 0029).
        if info.constant {
            self.q.diags.push(not_a_place);
            return Ty::Error;
        }
        if mutable {
            match info.immutable {
                Some(Immutable::Local(id)) => {
                    let local = f.local(id);
                    self.q.diags.push(
                        Diagnostic::error(
                            "ty::address-of-immutable",
                            format!(
                                "cannot take a `*mut` address of `{}`, which is not `mut`",
                                local.name
                            ),
                        )
                        .label(place.span, "address taken here")
                        .label(
                            local.span,
                            format!("help: declare it `let mut {}`", local.name),
                        ),
                    );
                }
                Some(Immutable::ConstPointer(ptr, pointer)) => {
                    self.q.diags.push(
                        Diagnostic::error(
                            "ty::address-of-immutable",
                            "cannot take a `*mut` address through a `*const` pointer",
                        )
                        .label(pointer, format!("has type `{}`", Ty::Ptr(ptr)))
                        .help("use `&raw const`, or a `*mut` pointer"),
                    );
                }
                Some(Immutable::Temporary(base)) => {
                    self.q.diags.push(
                        Diagnostic::error(
                            "ty::address-of-immutable",
                            "cannot take a `*mut` address of part of a temporary value",
                        )
                        .label(base, "this value lives in no variable")
                        .help("store it in a `let mut` first"),
                    );
                }
                Some(Immutable::Static(id)) => {
                    let def = self.hir.static_(id);
                    self.q.diags.push(
                        Diagnostic::error(
                            "ty::address-of-immutable",
                            format!(
                                "cannot take a `*mut` address of `{}`, which is not `mut`",
                                def.name
                            ),
                        )
                        .label(place.span, "address taken here")
                        .label(
                            def.name_span,
                            format!("help: declare it `{}`", mutable_static(def)),
                        ),
                    );
                }
                Some(Immutable::ByteString(span)) => {
                    self.q.diags.push(
                        Diagnostic::error(
                            "ty::address-of-immutable",
                            "cannot take a `*mut` address of a byte string",
                        )
                        .label(span, "in read-only memory")
                        .help("use `&raw const`, or copy it into a `let mut` (ADR 0031)"),
                    );
                }
                None => {}
            }
        }
        if self.may_be_misaligned(&info) {
            self.q.diags.push(
                Diagnostic::error(
                    "ty::packed-field-address",
                    format!("cannot take the address of a `{}` that may be misaligned", info.ty),
                )
                .label(place.span, "reached through a field of a `#[packed]` struct")
                .help(format!(
                    "a `*const {}` promises alignment, and dereferencing a misaligned one may trap; copy the value into a `let` first (ADR 0022)",
                    info.ty
                )),
            );
        }
        match info.ty {
            Ty::Error => Ty::Error,
            ty => Ty::ptr(mutable, false, ty),
        }
    }

    fn require(&mut self, expr: &Expr, ty: Ty, op: &str, allowed: Operands) {
        let ok = match ty {
            Ty::Error | Ty::Never => true,
            Ty::Int(_) => true,
            Ty::Bool => matches!(allowed, Operands::IntOrBool | Operands::Equatable),
            // A slice is an address and a length (ADR 0031).
            Ty::Ptr(_) if ty.slice_elem().is_some() => false,
            Ty::Ptr(_) => matches!(allowed, Operands::Equatable | Operands::Ordered),
            // Aggregates compare part by part (ADR 0021).
            Ty::Unit | Ty::Array(..) | Ty::Slice(_) | Ty::Struct(_) => false,
        };
        if !ok {
            let what = match allowed {
                Operands::Int => "an integer",
                Operands::IntOrBool => "an integer or `bool`",
                Operands::Equatable => "integers, pointers or `bool`",
                Operands::Ordered => "integers or pointers",
            };
            let mut diag = Diagnostic::error(
                "ty::invalid-operand",
                format!("`{op}` needs {what}, found `{ty}`"),
            )
            .label(expr.span, format!("has type `{ty}`"));
            if ty.slice_elem().is_some() {
                diag = diag.help(
                    "a slice is an address and a length; compare those: `s as *const T` and `len(s)` (ADR 0031)",
                );
            }
            self.q.diags.push(diag);
        }
    }
}

/// Lists struct layouts, then every function's locals and expression types,
/// for `--emit=typeck`.
pub fn dump(q: &mut Queries) -> String {
    let hir = q.hir;
    let mut out = String::new();
    for (i, def) in hir.structs.iter().enumerate() {
        let Some(layout) = q.struct_layout(hir::StructId(i as u32)) else {
            continue;
        };
        let fields: Vec<String> = def
            .fields
            .iter()
            .zip(&layout.offsets)
            .map(|(field, offset)| {
                let ty = q.normalize(field.ty);
                format!("{}: {ty} at {offset}", field.name)
            })
            .collect();
        let fields = if fields.is_empty() {
            "{}".to_owned()
        } else {
            format!("{{ {} }}", fields.join(", "))
        };
        let attrs = match (def.packed, def.align) {
            (true, _) => "#[packed] ".to_owned(),
            (false, Some((align, _))) => match q.array_len(align) {
                Some(align) => format!("#[align({align})] "),
                None => "#[align(?)] ".to_owned(),
            },
            (false, None) => String::new(),
        };
        writeln!(
            out,
            "{attrs}struct {} {fields} size {}, align {}",
            def.name, layout.layout.size, layout.layout.align
        )
        .unwrap();
    }
    for (_, f) in hir.fns() {
        let kind = match f.kind {
            FnKind::Plain => "",
            FnKind::Entry => "entry ",
            FnKind::Panic => "panic ",
        };
        let params: Vec<String> = f
            .params()
            .iter()
            .map(|p| format!("{}: {}", p.name, q.normalize(p.ty)))
            .collect();
        let external = if f.body.is_none() { "extern " } else { "" };
        let external = if f.comptime.is_some() {
            "comptime "
        } else {
            external
        };
        writeln!(
            out,
            "{external}{kind}fn {}({}) -> {}",
            f.name,
            params.join(", "),
            q.normalize(f.ret)
        )
        .unwrap();
        for local in &f.locals[f.param_count..] {
            let mutable = if local.mutable { "mut " } else { "" };
            let ty = q.normalize(local.ty);
            writeln!(out, "    let {mutable}{}: {ty}", local.name).unwrap();
        }
        let Some(body) = &f.body else {
            continue;
        };
        body.walk(&mut |e| {
            let ty = q.tables.expr_ty(e.id);
            let converted = match q.tables.coercion(e.id) {
                Some(to) => format!(" as {to}"),
                None => String::new(),
            };
            writeln!(
                out,
                "    expr {} {} {:?}: {ty}{converted}",
                e.id.0,
                expr_label(hir, f, e),
                e.span
            )
            .unwrap();
        });
    }
    out
}

/// The types an operator accepts.
#[derive(Clone, Copy)]
enum Operands {
    Int,
    IntOrBool,
    /// `==` and `!=`
    Equatable,
    /// `<`, `<=`, `>`, `>=`
    Ordered,
}

/// How a static would be declared to be written: `static mut NAME`, or
/// `extern static mut NAME`.
fn mutable_static(def: &hir::StaticDef) -> String {
    let external = if def.init.is_none() { "extern " } else { "" };
    format!("{external}static mut {}", def.name)
}

fn expr_label(hir: &Hir, f: &Fn, expr: &Expr) -> String {
    match &expr.kind {
        ExprKind::Bool(value) => format!("bool {value}"),
        ExprKind::Logical(op, _, _) => format!("logical {}", op.symbol()),
        ExprKind::If { .. } => "if".to_owned(),
        ExprKind::While { .. } => "while".to_owned(),
        ExprKind::Break { .. } => "break".to_owned(),
        ExprKind::Continue { .. } => "continue".to_owned(),
        ExprKind::Return(_) => "return".to_owned(),
        ExprKind::Int(value) => format!("int {value}"),
        ExprKind::Byte(value) => format!("byte {value}"),
        ExprKind::ByteString(bytes) => format!("byte string b\"{}\"", bytes.escape_ascii()),
        ExprKind::Unit => "unit".to_owned(),
        ExprKind::Const(id) => format!("const {}", hir.const_(*id).name),
        ExprKind::Static(id) => format!("static {}", hir.static_(*id).name),
        ExprKind::Comptime(_) => "comptime".to_owned(),
        ExprKind::Array(_) => "array".to_owned(),
        ExprKind::StructLit(s, _) => format!("struct {}", s.name),
        ExprKind::Field(_, name, _) => format!("field {name}"),
        ExprKind::SizeOf(ty) => format!("size_of({ty})"),
        ExprKind::AlignOf(ty) => format!("align_of({ty})"),
        ExprKind::OffsetOf(ty, name, _) => format!("offset_of({ty}, {name})"),
        ExprKind::Repeat(..) => "repeat".to_owned(),
        ExprKind::Local(id) => format!("local {}", f.local(*id).name),
        ExprKind::Call { .. } => "call".to_owned(),
        ExprKind::Unary(op, _) => format!("{op:?}").to_lowercase(),
        ExprKind::Deref(_) => "deref".to_owned(),
        ExprKind::Index(..) => "index".to_owned(),
        ExprKind::AddrOf { mutable: true, .. } => "&raw mut".to_owned(),
        ExprKind::AddrOf { mutable: false, .. } => "&raw const".to_owned(),
        ExprKind::Binary(op, _, _) => format!("binary {}", op.symbol()),
        ExprKind::Cast(_, ty, _) => format!("as {ty}"),
        ExprKind::Assign(..) => "assign".to_owned(),
        ExprKind::CompoundAssign(op, _, _) => format!("assign {}=", op.symbol()),
        ExprKind::Block(_) => "block".to_owned(),
        ExprKind::Assert(_) => "assert".to_owned(),
        ExprKind::Len(_) => "len".to_owned(),
        ExprKind::Slice(..) => "slice".to_owned(),
        ExprKind::Loop(_) => "loop".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_and_error_coerce_to_anything() {
        let u8_ = Ty::Int(IntTy::U8);
        assert!(coerces(Ty::Never, u8_));
        assert!(coerces(Ty::Error, Ty::Unit));
        assert!(coerces(u8_, Ty::Error));
        assert!(!coerces(Ty::Int(IntTy::U32), u8_));
        assert!(!coerces(Ty::Unit, Ty::Never));
    }

    #[test]
    fn mut_pointers_coerce_to_const() {
        let u8_ = Ty::Int(IntTy::U8);
        let mut_ = Ty::ptr(true, false, u8_);
        let const_ = Ty::ptr(false, false, u8_);
        assert!(coerces(mut_, const_));
        assert!(!coerces(const_, mut_));
        // Volatility never changes implicitly.
        assert!(!coerces(mut_, Ty::ptr(false, true, u8_)));
        // Only the outermost pointer converts.
        assert!(coerces(
            Ty::ptr(true, false, mut_),
            Ty::ptr(false, false, mut_)
        ));
        assert!(!coerces(
            Ty::ptr(true, false, mut_),
            Ty::ptr(true, false, const_)
        ));
    }

    #[test]
    fn array_pointers_coerce_to_slices() {
        let u8_ = Ty::Int(IntTy::U8);
        let array = Ty::array(u8_, 4);
        let slice = Ty::slice(u8_);
        let ptr = |mutable, volatile, pointee| Ty::ptr(mutable, volatile, pointee);
        assert!(coerces(ptr(false, false, array), ptr(false, false, slice)));
        assert!(coerces(ptr(true, false, array), ptr(true, false, slice)));
        assert!(coerces(ptr(true, false, array), ptr(false, false, slice)));
        assert!(coerces(ptr(true, true, array), ptr(false, true, slice)));
        assert!(coerces(ptr(true, false, slice), ptr(false, false, slice)));
        // Never to `*mut`, nor changing volatility or the elements.
        assert!(!coerces(ptr(false, false, array), ptr(true, false, slice)));
        assert!(!coerces(ptr(false, true, array), ptr(false, false, slice)));
        assert!(!coerces(ptr(false, false, array), ptr(false, true, slice)));
        let u16_slice = Ty::slice(Ty::Int(IntTy::U16));
        assert!(!coerces(
            ptr(false, false, array),
            ptr(false, false, u16_slice)
        ));
        // Only the outermost pointer converts, and never back.
        assert!(!coerces(
            ptr(false, false, ptr(false, false, array)),
            ptr(false, false, ptr(false, false, slice))
        ));
        assert!(!coerces(ptr(false, false, slice), ptr(false, false, array)));
    }
}
