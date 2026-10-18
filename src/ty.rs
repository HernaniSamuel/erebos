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
    self, Arm, BinOp, Block, EnumId, EnumTy, Expr, ExprId, ExprKind, Fn, FnKind, Hir, IntTy,
    LocalId, NeverPosition, Pattern, PatternKind, Ptr, StaticId, Stmt, Ty, UnOp, UnsizedPosition,
};
use crate::layout;
use crate::query::Queries;
use crate::span::Span;
use crate::target;

use usefulness::{Column, Enums, Pat, Search, Variant};

mod usefulness;

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
        NeverPosition::Payload => "a payload type".to_owned(),
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

/// `ty::unsized` for `place`, the `[T]` of type `ty` a slice points to,
/// `accessed` whole: it can only be indexed or passed to `&raw` (ADR 0031).
fn whole_slice(ty: Ty, place: Span, accessed: &str) -> Diagnostic {
    unsized_ty(ty)
        .label(place, format!("what a slice points to, {accessed} whole"))
        .help("it can only be indexed, as in `(*s)[i]`, or passed to `&raw`; its length, known only at run time, is `len(s)` (ADR 0031)")
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

/// Checks one function; `query` decides when. Returns how many of the
/// diagnostics pushed meanwhile come from evaluating the constants its
/// patterns use: they are errors of those constants, not of the function
/// (ADR 0032, 0037).
pub fn check_fn(q: &mut Queries, f: &Fn) -> usize {
    let hir = q.hir;
    let mut checker = Checker {
        q,
        hir,
        ret: Ty::Error,
        loops: Vec::new(),
        equality: None,
        elsewhere: 0,
    };
    checker.fn_(f);
    checker.elsewhere
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
    /// The `==` or `!=` whose operands are being checked: a literal there
    /// compared with an enum gets a help converting the enum (ADR 0035).
    equality: Option<Equality>,
    /// The diagnostics pushed while evaluating the constants of patterns,
    /// which belong to those constants.
    elsewhere: usize,
}

/// A `==` or `!=`: its operator and the ids of its operands, in order.
#[derive(Clone, Copy)]
struct Equality {
    op: BinOp,
    lhs: ExprId,
    rhs: ExprId,
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
            ExprKind::Int(value) => self.int_literal(f, expr, *value, false, expected),
            ExprKind::Byte(_) => Ty::Int(IntTy::U8),
            ExprKind::Bool(_) => Ty::Bool,
            ExprKind::Unit => Ty::Unit,
            ExprKind::Array(elems) => self.array(f, expr, elems, expected),
            ExprKind::Repeat(value, len) => {
                let want = element_of(expected);
                let ty = self.check(f, value, want);
                match (ty, self.q.array_len(*len)) {
                    (Ty::Error, _) | (_, None) => Ty::Error,
                    // Like a block that does not finish (ADR 0034).
                    (Ty::Never, _) => Ty::Never,
                    // The operand converts once, before it is copied, like
                    // each element of an array literal (ADR 0024, 0031).
                    (ty, Some(len)) => match want {
                        Some(want) if want != Ty::Error && coerces(ty, want) => {
                            Ty::array(want, len)
                        }
                        _ => Ty::array(ty, len),
                    },
                }
            }
            ExprKind::Local(local) => self.local_ty(f, *local),
            ExprKind::Const(id) => self.norm(self.hir.const_(*id).ty),
            ExprKind::Variant(id, index, args) => {
                self.variant(f, expr, *id, *index, args.as_deref())
            }
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
                    other => {
                        let diag =
                            Diagnostic::error("ty::no-fields", format!("`{other}` has no fields"))
                                .label(*name_span, "no such field");
                        self.q.diags.push(payload_values_help(diag, *other));
                    }
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
                let equality = matches!(op, BinOp::Eq | BinOp::Ne).then_some(Equality {
                    op: *op,
                    lhs: lhs.id,
                    rhs: rhs.id,
                });
                let outer = std::mem::replace(&mut self.equality, equality);
                let ty = self.same_type_operands(f, lhs, rhs, None, comparable);
                self.equality = outer;
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
                if let Ty::Slice(_) = info.ty {
                    self.q.diags.push(whole_slice(info.ty, expr.span, "read"));
                    return Ty::Error;
                }
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
            ExprKind::Assert(cond, message) => {
                self.check(f, cond, Some(Ty::Bool));
                // Any `*const [u8]`, or what converts to one (ADR 0031).
                if let Some(message) = message {
                    let bytes = Ty::ptr(false, false, Ty::slice(Ty::Int(IntTy::U8)));
                    self.check(f, message, Some(bytes));
                }
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
            ExprKind::Match { scrutinee, arms } => self.match_(f, scrutinee, arms, expected),
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

    /// `match scrutinee { arms }` (ADR 0037, 0040). The scrutinee is typed
    /// on its own; each pattern against its type; each arm's expression as
    /// a branch of `if` is, the first arm whose type is not `!` giving the
    /// type without an expected one. Then the arms must cover every value,
    /// and each must match some value the arms before it leave.
    fn match_(&mut self, f: &Fn, scrutinee: &Expr, arms: &[Arm], expected: Option<Ty>) -> Ty {
        let ty = self.check(f, scrutinee, None);
        let matched = match ty {
            Ty::Int(_) | Ty::Bool | Ty::Error => ty,
            Ty::Enum(e) if !e.payloads => ty,
            // An enum with a payload type in error was reported, once
            // (ADR 0039).
            Ty::Enum(_) if self.q.is_erroneous(ty) => Ty::Error,
            Ty::Enum(_) => ty,
            other => {
                self.q.diags.push(
                    Diagnostic::error(
                        "ty::match-type",
                        format!("cannot `match` on a value of type `{other}`"),
                    )
                    .label(scrutinee.span, format!("has type `{other}`"))
                    .help("a `match` takes an integer, a `bool` or an enum (ADR 0037)"),
                );
                Ty::Error
            }
        };
        // The checks need the values of an enum only at the positions where
        // a pattern names one of its values, or for a `match` with no arms,
        // which lists the missing variants; arms that are only `_` and
        // `let` there need none, so such a `match` may run while the
        // enum's values are computed. A `match` that needs the values of an
        // enum with a value that failed or repeats, which was reported, is
        // not checked for exhaustiveness or dead arms (ADR 0038, 0040).
        // Every enum needed is asked about, so that MIR building, which
        // asks about the same, adds no dependence. Their failures are the
        // enums', not the function's.
        let needed = self.hir.enums_needed(ty, arms);
        let mut ok = true;
        for &id in &needed {
            let before = self.q.diags.len();
            ok &= self.q.enum_ok(id);
            self.elsewhere += self.q.diags.len() - before;
        }
        // The variants of the enums the checks need; `None` if one of them
        // has a value that failed or repeats.
        let enums = ok.then(|| {
            let mut enums = Enums::new();
            for &id in &needed {
                let variants = self.variants(id);
                enums.insert(id, variants);
            }
            enums
        });
        // What each arm matches; `None` once one cannot be known, which
        // stops the checks of the whole `match`.
        let mut values: Option<Vec<Pat>> = Some(Vec::new());
        let mut first = None;
        let mut never = true;
        for arm in arms {
            match (&mut values, self.pattern(f, &arm.pattern, matched)) {
                (Some(list), Some(pattern)) => list.push(pattern),
                _ => values = None,
            }
            let arm_ty = self.check(f, &arm.body, expected.or(first));
            if arm_ty != Ty::Never {
                never = false;
                first = first.or(Some(arm_ty));
            }
        }
        if let Some(values) = values
            && let Some(enums) = enums
            && matched != Ty::Error
        {
            self.coverage(scrutinee, matched, arms, &values, &enums);
        }
        match expected.or(first) {
            // Without an arm that finishes, neither does the `match`.
            _ if never => Ty::Never,
            Some(ty) => ty,
            None => unreachable!("an arm that finishes gives a type"),
        }
    }

    /// Checks a pattern against `ty`, the type of its position (`{error}`
    /// if it has none to match), and returns the values it matches; `None`
    /// if they cannot be known: after an error, reported, or for a
    /// constant that failed to evaluate, reported where it failed
    /// (ADR 0037, 0040). Every alternative and payload pattern is checked,
    /// whatever the others give.
    fn pattern(&mut self, f: &Fn, pattern: &Pattern, ty: Ty) -> Option<Pat> {
        let mismatch = |found: &str| {
            Diagnostic::error("ty::mismatched-types", "mismatched types")
                .label(pattern.span, format!("expected `{ty}`, found {found}"))
        };
        // A literal against an enum: an enum is not its integer (ADR 0035).
        let enum_help = |e: EnumTy| {
            if e.payloads {
                // No constant of it is a pattern (ADR 0040).
                return format!(
                    "an enum with payloads is matched with its variant patterns, such as `{}` (ADR 0040)",
                    variant_pattern_example(self.hir, e)
                );
            }
            let variant = such_as_variant(self.hir, e);
            format!(
                "an enum is matched with its variants{variant}, or with constants of its type (ADR 0037)"
            )
        };
        let bool_help = "a `bool` is matched with `true` and `false` (ADR 0037)";
        // A struct, an array, a pointer, a slice or `()` (ADR 0040).
        let other_help = format!("a `{ty}` is matched only with `_` or a `let` (ADR 0040)");
        match &pattern.kind {
            PatternKind::Wildcard => Some(Pat::Wild),
            PatternKind::Binding {
                local,
                ty: written,
                ty_span,
            } => {
                let local_ty = self.local_ty(f, *local);
                let written = self.norm(*written);
                if local_ty == Ty::Error || written == Ty::Error {
                    return None;
                }
                // A later alternative's `let` of a variable that the first
                // alternative declared, with its type (ADR 0041).
                if written != local_ty {
                    let name = &f.local(*local).name;
                    self.q.diags.push(
                        Diagnostic::error("ty::mismatched-types", "mismatched types")
                            .label(*ty_span, format!("expected `{local_ty}`, found `{written}`"))
                            .help(format!(
                                "the alternatives bind one variable `{name}`, which has one type, the one its first `let` writes, `{local_ty}` (ADR 0041)"
                            )),
                    );
                    return None;
                }
                if ty == Ty::Error {
                    return None;
                }
                if local_ty != ty {
                    self.q.diags.push(
                        Diagnostic::error("ty::mismatched-types", "mismatched types")
                            .label(*ty_span, format!("expected `{ty}`, found `{local_ty}`"))
                            .help(format!(
                                "the variable holds the value matched, a `{ty}`, which a pattern never converts (ADR 0037)"
                            )),
                    );
                    return None;
                }
                Some(Pat::Wild)
            }
            PatternKind::Int { value, negated } => {
                let int = match ty {
                    Ty::Int(int) => int,
                    Ty::Error => return None,
                    Ty::Enum(e) => {
                        let help = enum_help(e);
                        self.q.diags.push(mismatch("an integer").help(help));
                        return None;
                    }
                    Ty::Bool => {
                        self.q.diags.push(mismatch("an integer").help(bool_help));
                        return None;
                    }
                    _ => {
                        self.q.diags.push(mismatch("an integer").help(other_help));
                        return None;
                    }
                };
                if *negated && !int.is_signed() {
                    self.q.diags.push(
                        Diagnostic::error(
                            "ty::negate-unsigned",
                            format!("cannot negate a value of unsigned type `{int}`"),
                        )
                        .label(pattern.span, format!("`{int}` has no negative values")),
                    );
                    return None;
                }
                // `-128` for `i8`, whose magnitude is one past the max.
                let limit = if *negated { int.max() + 1 } else { int.max() };
                if *value > limit {
                    let sign = if *negated { "-" } else { "" };
                    self.q.diags.push(
                        Diagnostic::error(
                            "ty::literal-out-of-range",
                            format!("literal out of range for `{int}`"),
                        )
                        .label(
                            pattern.span,
                            format!("`{sign}{value}` does not fit in `{int}`"),
                        ),
                    );
                    return None;
                }
                let bits = if *negated {
                    value.wrapping_neg()
                } else {
                    *value
                };
                Some(Pat::Value(bits & int.mask(), Vec::new()))
            }
            PatternKind::Byte(value) => match ty {
                Ty::Int(IntTy::U8) => Some(Pat::Value(u64::from(*value), Vec::new())),
                Ty::Error => None,
                Ty::Int(_) => {
                    self.q.diags.push(mismatch("`u8`").help(format!(
                        "a byte literal is a `u8` (ADR 0016); write this value as an integer literal, `{value}`"
                    )));
                    None
                }
                Ty::Enum(e) => {
                    let help = enum_help(e);
                    self.q.diags.push(mismatch("`u8`").help(help));
                    None
                }
                Ty::Bool => {
                    self.q.diags.push(mismatch("`u8`").help(bool_help));
                    None
                }
                _ => {
                    self.q.diags.push(mismatch("`u8`").help(other_help));
                    None
                }
            },
            PatternKind::Bool(value) => match ty {
                Ty::Bool => Some(Pat::Value(u64::from(*value), Vec::new())),
                Ty::Error => None,
                Ty::Enum(e) => {
                    let help = enum_help(e);
                    self.q.diags.push(mismatch("`bool`").help(help));
                    None
                }
                Ty::Int(int) => {
                    let literals = if int == IntTy::U8 {
                        "integer and byte literals"
                    } else {
                        "integer literals"
                    };
                    self.q.diags.push(mismatch("`bool`").help(format!(
                        "a `{int}` is matched with {literals}, or with constants of its type (ADR 0037)"
                    )));
                    None
                }
                _ => {
                    self.q.diags.push(mismatch("`bool`").help(other_help));
                    None
                }
            },
            PatternKind::Const(id) => {
                let def = self.hir.const_(*id);
                let const_ty = self.norm(def.ty);
                if const_ty == Ty::Error {
                    return None;
                }
                // Whatever the type of its position, and alone (ADR 0040).
                if !matches!(
                    const_ty,
                    Ty::Int(_)
                        | Ty::Bool
                        | Ty::Enum(EnumTy {
                            payloads: false,
                            ..
                        })
                ) {
                    let help = match const_ty {
                        Ty::Enum(e) => format!(
                            "match a `{const_ty}` with its variant patterns instead, such as `{}` (ADR 0040)",
                            variant_pattern_example(self.hir, e)
                        ),
                        _ => "patterns compare only integers, `bool` and enums without payloads; bind the value with a `let` and test it instead (ADR 0040)".to_owned(),
                    };
                    self.q.diags.push(
                        Diagnostic::error(
                            "ty::constant-pattern-type",
                            format!("a constant of type `{const_ty}` cannot be a pattern"),
                        )
                        .label(
                            pattern.span,
                            format!("`{}` has type `{const_ty}`", def.name),
                        )
                        .help(help),
                    );
                    return None;
                }
                if ty == Ty::Error {
                    return None;
                }
                if const_ty != ty {
                    self.q.diags.push(mismatch(&format!("`{const_ty}`")).help(
                        "a constant in a pattern has the type of the value matched, which a pattern never converts (ADR 0037)",
                    ));
                    return None;
                }
                // Whatever its value is (ADR 0037). A failure is the
                // constant's, reported once wherever it is first needed, so
                // the function still type checks, and gets the checks of
                // ADR 0032, as when the constant is used in an expression.
                let before = self.q.diags.len();
                let value = self.q.const_value(*id);
                self.elsewhere += self.q.diags.len() - before;
                let value = value?;
                let bits = match ty {
                    Ty::Int(int) => value.int(int.bytes() as usize),
                    // A variant or not (ADR 0036).
                    Ty::Enum(e) => value.int(e.int.bytes() as usize),
                    _ => u64::from(value.bytes[0] != 0),
                };
                Some(Pat::Value(bits, Vec::new()))
            }
            PatternKind::Variant(id, index, subs) => {
                self.variant_pattern(f, pattern, ty, *id, *index, subs.as_deref())
            }
            PatternKind::Or(alternatives) => {
                let mut matched = Some(Vec::new());
                for alternative in alternatives {
                    match (&mut matched, self.pattern(f, alternative, ty)) {
                        (Some(list), Some(alternative)) => list.push(alternative),
                        _ => matched = None,
                    }
                }
                matched.map(Pat::Or)
            }
        }
    }

    /// `Enum::Variant`, or `Enum::Variant(subs)` when `subs` are written,
    /// against `ty`: as many patterns as the variant's payload has, each
    /// checked against the type its declaration gives that value, even
    /// when the variant does not fit its position (ADR 0040).
    fn variant_pattern(
        &mut self,
        f: &Fn,
        pattern: &Pattern,
        ty: Ty,
        id: EnumId,
        index: u32,
        subs: Option<&[Pattern]>,
    ) -> Option<Pat> {
        let hir = self.hir;
        let def = hir.enum_(id);
        let variant = &def.variants[index as usize];
        let variant_ty = Ty::Enum(def.ty);
        let mut ok = ty != Ty::Error;
        if ok && variant_ty != ty {
            self.q.diags.push(
                Diagnostic::error("ty::mismatched-types", "mismatched types")
                    .label(
                        pattern.span,
                        format!("expected `{ty}`, found `{variant_ty}`"),
                    )
                    .help(
                        "a variant in a pattern has the type of the value matched, which a pattern never converts (ADR 0037)",
                    ),
            );
            ok = false;
        }
        if let Some(diag) = payload_count(
            hir,
            id,
            index,
            subs.map(<[Pattern]>::len),
            pattern.span,
            true,
        ) {
            self.q.diags.push(diag);
            ok = false;
        }
        // A payload type in error was reported, once (ADR 0039); extra
        // patterns were reported above. `{error}` keeps both quiet.
        let erroneous = self.q.is_erroneous(variant_ty);
        let mut payload = Some(Vec::new());
        for (i, sub) in subs.unwrap_or_default().iter().enumerate() {
            let sub_ty = match variant.payload.get(i) {
                Some(value) if !erroneous => self.norm(value.ty),
                _ => Ty::Error,
            };
            match (&mut payload, self.pattern(f, sub, sub_ty)) {
                (Some(list), Some(sub)) => list.push(sub),
                _ => payload = None,
            }
        }
        if !ok {
            return None;
        }
        // A failure is the enum's, reported once (ADR 0035).
        let before = self.q.diags.len();
        let value = self.q.discriminant(id, index);
        self.elsewhere += self.q.diags.len() - before;
        Some(Pat::Value(value?, payload?))
    }

    /// Exhaustiveness and dead arms (ADR 0037, 0040) of a `match` on `ty`,
    /// an integer, `bool` or enum, whose arms match `values`, one per arm.
    /// `enums` has the variants of the enums whose values the `match`
    /// needs; none of them failed or repeats (ADR 0038, 0040). The values
    /// of an enum are all those of its integer type (ADR 0036): for dead
    /// arms, those that are no variant remain after every variant, but a
    /// `match` is exhaustive with every variant.
    fn coverage(&mut self, scrutinee: &Expr, ty: Ty, arms: &[Arm], values: &[Pat], enums: &Enums) {
        let column = column(ty);
        let columns = [column];
        let search = Search::new(enums);
        // Whether `rows`, one pattern each, leave no value that `pattern`
        // matches.
        let covered = |rows: &[Pat], pattern: &Pat| {
            let rows: Vec<Vec<&Pat>> = rows.iter().map(|row| vec![row]).collect();
            !usefulness::useful(&search, &columns, &rows, &[pattern])
        };
        for (i, arm) in arms.iter().enumerate() {
            let earlier = &values[..i];
            let pattern = &values[i];
            if covered(earlier, pattern) {
                let mut diag = Diagnostic::error("ty::unreachable-pattern", "unreachable arm")
                    .label(
                        arm.pattern.span,
                        "matches no value that the arms before it leave",
                    );
                if let Some(k) = (0..i).find(|&k| covered(&values[k..=k], pattern)) {
                    diag = diag.label(
                        arms[k].pattern.span,
                        "every value it matches is matched here",
                    );
                }
                self.q.diags.push(diag.help(
                    "a `match` takes the first arm that matches, so this one never runs; remove it (ADR 0037)",
                ));
                continue;
            }
            let arm_context = Alternatives {
                covered: &covered,
                arms,
                earlier,
            };
            self.dead_alternatives(
                &arm_context,
                earlier,
                pattern,
                &mut Vec::new(),
                pattern,
                &arm.pattern,
            );
        }
        let rows: Vec<Vec<&Pat>> = values.iter().map(|value| vec![value]).collect();
        // Three are shown, and a fourth tells whether there are more: the
        // search stops there, since the witnesses can be exponentially many
        // (ADR 0043).
        let missing = usefulness::missing(&search, &columns, &rows, arms.is_empty(), 4);
        if missing.is_empty() {
            return;
        }
        let diag = match ty {
            Ty::Bool | Ty::Enum(_) => {
                let shown: Vec<String> = missing
                    .iter()
                    .take(3)
                    .map(|witness| self.witness(enums, column, &witness[0]))
                    .collect();
                let listed: Vec<String> = shown.iter().map(|name| format!("`{name}`")).collect();
                let listed = if missing.len() > shown.len() {
                    format!("{} and more", listed.join(", "))
                } else {
                    and_list(&listed)
                };
                let arms: Vec<String> = shown
                    .iter()
                    .map(|name| format!("`{name} => ...`"))
                    .collect();
                Diagnostic::error(
                    "ty::non-exhaustive-match",
                    format!("non-exhaustive `match`: {listed} not covered"),
                )
                .label(scrutinee.span, format!("missing: {listed}"))
                .help(format!("add {}, or a `_` arm (ADR 0037)", and_list(&arms)))
            }
            _ => Diagnostic::error(
                "ty::non-exhaustive-match",
                format!("non-exhaustive `match`: not every `{ty}` is covered"),
            )
            .label(scrutinee.span, "missing: a `_` or `let` arm")
            .help(format!(
                "a `match` on an integer needs an arm for every value, such as `_ => ...` or `let n: {ty} => ...` (ADR 0037)"
            )),
        };
        self.q.diags.push(diag);
    }

    /// Reports the alternatives of the or-patterns in `pattern`, at `path`
    /// in `whole`, that match no value the arms and alternatives before
    /// them leave, at any depth (ADR 0037, 0040): `context` holds the
    /// earlier arms, and the alternatives before the one `whole` takes of
    /// each or-pattern enclosing `pattern`, each as a whole arm's pattern.
    /// An alternative is tried in place of its or-pattern, within `whole`;
    /// a dead one is reported alone, without its own or-patterns.
    fn dead_alternatives(
        &mut self,
        arm: &Alternatives,
        context: &[Pat],
        whole: &Pat,
        path: &mut Vec<usize>,
        pattern: &Pat,
        written: &Pattern,
    ) {
        match (pattern, &written.kind) {
            (Pat::Value(_, subs), PatternKind::Variant(_, _, Some(written_subs))) => {
                for (i, (sub, written_sub)) in subs.iter().zip(written_subs).enumerate() {
                    path.push(i);
                    self.dead_alternatives(arm, context, whole, path, sub, written_sub);
                    path.pop();
                }
            }
            (Pat::Or(alternatives), PatternKind::Or(written_alternatives)) => {
                let tried: Vec<Pat> = alternatives
                    .iter()
                    .map(|alternative| whole.replaced(path, alternative))
                    .collect();
                let mut before = context.to_vec();
                for (j, alternative) in alternatives.iter().enumerate() {
                    let candidate = &tried[j];
                    if (arm.covered)(&before, candidate) {
                        let earlier_arm = (0..arm.earlier.len())
                            .find(|&k| (arm.covered)(&arm.earlier[k..=k], candidate))
                            .map(|k| arm.arms[k].pattern.span);
                        let earlier_alternative = (0..j)
                            .find(|&m| (arm.covered)(&tried[m..=m], candidate))
                            .map(|m| written_alternatives[m].span);
                        let mut diag =
                            Diagnostic::error("ty::unreachable-pattern", "unreachable pattern")
                                .label(
                                    written_alternatives[j].span,
                                    "matches no value that the patterns before it leave",
                                );
                        if let Some(span) = earlier_arm.or(earlier_alternative) {
                            diag = diag.label(span, "every value it matches is matched here");
                        }
                        self.q
                            .diags
                            .push(diag.help("remove this alternative (ADR 0037)"));
                    } else {
                        self.dead_alternatives(
                            arm,
                            &before,
                            candidate,
                            path,
                            alternative,
                            &written_alternatives[j],
                        );
                    }
                    before.push(candidate.clone());
                }
            }
            _ => {}
        }
    }

    /// The variants of the enum `id`, whose every value was computed, none
    /// twice, for the checks of a `match` (ADR 0038, 0040).
    fn variants(&mut self, id: EnumId) -> Vec<Variant> {
        let def = self.hir.enum_(id);
        let mut variants = Vec::with_capacity(def.variants.len());
        for (index, variant) in def.variants.iter().enumerate() {
            let value = self
                .q
                .discriminant(id, index as u32)
                .expect("an enum checked for coverage has every value");
            let mut payload = Vec::with_capacity(variant.payload.len());
            for value in &variant.payload {
                let ty = self.norm(value.ty);
                payload.push(column(ty));
            }
            variants.push(Variant { value, payload });
        }
        variants
    }

    /// A value a `match` misses, at a position of type `column`, as a
    /// pattern would write it.
    fn witness(&self, enums: &Enums, column: Column, witness: &Pat) -> String {
        let Pat::Value(value, subs) = witness else {
            return "_".to_owned();
        };
        match column {
            Column::Bool => (*value != 0).to_string(),
            Column::Int(int) => int.display_bits(*value),
            Column::Enum(e) => {
                let def = self.hir.enum_(e.id);
                let variants = &enums[&e.id];
                let index = variants
                    .iter()
                    .position(|variant| variant.value == *value)
                    .expect("a value a `match` misses at an enum position is a variant");
                let name = format!("{}::{}", def.name, def.variants[index].name);
                if subs.is_empty() {
                    return name;
                }
                let subs: Vec<String> = variants[index]
                    .payload
                    .iter()
                    .zip(subs)
                    .map(|(&column, sub)| self.witness(enums, column, sub))
                    .collect();
                format!("{name}({})", subs.join(", "))
            }
            Column::Other => unreachable!("no value is missing where only `_` and `let` match"),
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

    /// `Enum::Variant`, or `Enum::Variant(args)` when `args` are written:
    /// as many values as the variant's payload has, each checked against
    /// its type as an argument is against its parameter's (ADR 0039).
    fn variant(
        &mut self,
        f: &Fn,
        expr: &Expr,
        id: hir::EnumId,
        index: u32,
        args: Option<&[Expr]>,
    ) -> Ty {
        let hir = self.hir;
        let def = hir.enum_(id);
        let variant = &def.variants[index as usize];
        if let Some(diag) = payload_count(hir, id, index, args.map(<[Expr]>::len), expr.span, false)
        {
            self.q.diags.push(diag);
        }
        for (i, arg) in args.unwrap_or_default().iter().enumerate() {
            // Extra values were reported above; `{error}` keeps them quiet.
            let expected = variant.payload.get(i).map_or(Ty::Error, |value| value.ty);
            let expected = self.norm(expected);
            self.check(f, arg, Some(expected));
        }
        Ty::Enum(def.ty)
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

    /// The text of the expression `id` of `f` as written, if it is a name,
    /// a path `Enum::Variant`, or fields and derefs of those, as in
    /// `(*regs).mode`: what a help may quote of it. Looked up only for a
    /// help.
    fn simple_text(&self, f: &Fn, id: ExprId) -> Option<String> {
        let mut found = None;
        if let Some(body) = &f.body {
            body.walk(&mut |e| {
                if e.id == id {
                    found = Some(e);
                }
            });
        }
        self.path_text(f, found?)
    }

    fn path_text(&self, f: &Fn, expr: &Expr) -> Option<String> {
        match &expr.kind {
            ExprKind::Local(id) => Some(f.local(*id).name.clone()),
            ExprKind::Const(id) => Some(self.hir.const_(*id).name.clone()),
            ExprKind::Static(id) => Some(self.hir.static_(*id).name.clone()),
            ExprKind::Variant(id, index, None) => {
                let def = self.hir.enum_(*id);
                Some(format!(
                    "{}::{}",
                    def.name, def.variants[*index as usize].name
                ))
            }
            // `*p.f` is `*(p.f)`, so a deref under a field is in
            // parentheses.
            ExprKind::Field(base, name, _) => match &base.kind {
                ExprKind::Deref(_) => Some(format!("({}).{name}", self.path_text(f, base)?)),
                _ => Some(format!("{}.{name}", self.path_text(f, base)?)),
            },
            ExprKind::Deref(pointer) => Some(format!("*{}", self.path_text(f, pointer)?)),
            _ => None,
        }
    }

    /// `negated` covers `-128` for `i8`, whose magnitude is one past the max.
    fn int_literal(
        &mut self,
        f: &Fn,
        expr: &Expr,
        value: u64,
        negated: bool,
        expected: Option<Ty>,
    ) -> Ty {
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
                let mut diag = Diagnostic::error("ty::mismatched-types", "mismatched types")
                    .label(expr.span, format!("expected `{other}`, found an integer"));
                // An enum is not its integer (ADR 0035).
                if let Ty::Enum(e) = other {
                    let variant = such_as_variant(self.hir, e);
                    // Compared with `==` or `!=`, the integer can be
                    // compared instead, unless the literal is negative
                    // and the integer type unsigned, or the enum has
                    // payloads, which converts to nothing (ADR 0039).
                    // The operands keep their order.
                    let convert = match self.equality {
                        Some(Equality { op, lhs, rhs })
                            if (expr.id == lhs || expr.id == rhs)
                                && (!negated || e.int.is_signed())
                                && !e.payloads =>
                        {
                            // The enum is the other operand.
                            let enum_id = if expr.id == lhs { rhs } else { lhs };
                            let operand = self
                                .simple_text(f, enum_id)
                                .unwrap_or_else(|| "...".to_owned());
                            let sign = if negated { "-" } else { "" };
                            let converted = format!("{operand} as {}", e.int);
                            let literal = format!("{sign}{value}");
                            let (left, right) = if expr.id == lhs {
                                (literal, converted)
                            } else {
                                (converted, literal)
                            };
                            format!(
                                ", or compare its integer instead, as in `{left} {} {right}`",
                                op.symbol()
                            )
                        }
                        _ => String::new(),
                    };
                    diag = diag.help(format!(
                        "write a variant of `{other}`{variant}{convert} (ADR 0035)"
                    ));
                }
                self.q.diags.push(diag);
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
                let ty = self.int_literal(f, expr, *value, true, expected);
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

    /// Integers and pointers convert to each other with `as` (ADR 0013),
    /// and enums to integers (ADR 0035). A literal operand takes the target
    /// type, or `usize` for a pointer target (ADR 0017).
    fn cast(&mut self, f: &Fn, operand: &Expr, target: Ty, target_span: Span) -> Ty {
        let literal_ty = match target {
            Ty::Int(_) => Some(target),
            Ty::Ptr(_) => Some(Ty::Int(IntTy::Usize)),
            // Reported, such as `as *const !` (ADR 0034), or below, `as !`
            // and `as Mode`, which a literal has no type to take for.
            Ty::Error | Ty::Never | Ty::Enum(_) => Some(Ty::Error),
            _ => None,
        };
        let expected = if needs_expected_type(operand) {
            literal_ty
        } else {
            None
        };
        let from = self.check(f, operand, expected);
        let castable = |ty: Ty| matches!(ty, Ty::Int(_) | Ty::Ptr(_) | Ty::Error | Ty::Never);
        // Its result would be a value of type `!`, which never exists
        // (ADR 0034), whatever is cast.
        if target == Ty::Never {
            self.q.diags.push(
                Diagnostic::error("ty::invalid-cast", "cannot cast to `!`")
                    .label(target_span, "no value of type `!` exists")
                    .help("`!` is the type of expressions that do not finish (ADR 0034)"),
            );
            return Ty::Error;
        }
        // Nor to an enum with payloads, an aggregate (ADR 0039).
        if let Ty::Enum(e) = target
            && e.payloads
        {
            self.q.diags.push(
                Diagnostic::error("ty::invalid-cast", format!("cannot cast to `{target}`"))
                    .label(target_span, "nothing converts to an enum")
                    .help(format!(
                        "a value of an enum with payloads is built from one of its variants{} (ADR 0039)",
                        such_as_variant(self.hir, e)
                    )),
            );
            return Ty::Error;
        }
        // Nothing converts to an enum, so a program that only writes
        // variants never holds a value that is no variant (ADR 0035, 0036).
        if let Ty::Enum(e) = target {
            let variant = match self.hir.enum_(e.id).variants.first() {
                Some(variant) => format!("`N => {}::{}`", e.name, variant.name),
                None => "`N => ...`".to_owned(),
            };
            self.q.diags.push(
                Diagnostic::error("ty::invalid-cast", format!("cannot cast to `{target}`"))
                    .label(target_span, "nothing converts to an enum")
                    .help(format!(
                        "decode the integer with a `match`, with an arm such as {variant} for each value (ADR 0035)"
                    )),
            );
            return Ty::Error;
        }
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
        // An aggregate, which converts to nothing (ADR 0039).
        if let Ty::Enum(EnumTy { payloads: true, .. }) = from {
            self.q.diags.push(
                Diagnostic::error("ty::invalid-cast", format!("cannot cast `{from}`"))
                    .label(operand.span, "an enum with payloads converts to nothing")
                    .help("it is an aggregate, not an integer; its variant is read with a `match` (ADR 0039)"),
            );
            return Ty::Error;
        }
        // As its integer type would convert, to integers only (ADR 0035).
        if let Ty::Enum(_) = from {
            if !matches!(target, Ty::Int(_) | Ty::Error) {
                self.q.diags.push(
                    Diagnostic::error(
                        "ty::invalid-cast",
                        format!("cannot cast `{from}` to `{target}`"),
                    )
                    .label(target_span, "an enum converts only to integer types"),
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
    /// pointer of type `!` is never computed, so neither is the place.
    /// What a slice points to is a place of type `[T]`, which can only be
    /// indexed or passed to `&raw` (ADR 0031).
    fn through_pointer(&mut self, ty: Ty, pointer: &Expr, error: Diagnostic) -> PlaceInfo {
        match ty {
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
            // An enum with payloads has no fields to access (ADR 0039).
            let (label, help) = match place.ty {
                Ty::Enum(_) => (
                    "an enum with payloads is an aggregate, without fields to access one at a time",
                    "the width and number of the bus accesses would be up to the compiler; device registers are described with structs, integers and enums without payloads (ADR 0023, 0039)",
                ),
                _ => (
                    "access its elements or fields one at a time",
                    "the width and number of the bus accesses would be up to the compiler (ADR 0023)",
                ),
            };
            self.q.diags.push(
                Diagnostic::error(
                    "ty::volatile-aggregate",
                    format!("a volatile `{}` cannot be accessed as a whole", place.ty),
                )
                .label(span, label)
                .help(help),
            );
        }
        // An enum without payloads is accessed as its integer type
        // (ADR 0023, 0035).
        if let (
            true,
            Ty::Int(int)
            | Ty::Enum(EnumTy {
                int,
                payloads: false,
                ..
            }),
        ) = (volatile, place.ty)
            && int.bits() > target::WIDEST_ACCESS_BITS
        {
            let half = target::WIDEST_ACCESS_BITS;
            // Nothing converts to an enum (ADR 0035), so its halves would
            // not make one.
            let help = match place.ty {
                Ty::Enum(_) => format!(
                    "an enum is accessed as its integer type, `{int}`; declare it on an integer type of at most {half} bits if its values fit (ADR 0023, 0035)"
                ),
                _ => format!(
                    "access it as `u{half}` halves, in the order the device needs (ADR 0023)"
                ),
            };
            self.q.diags.push(
                Diagnostic::error(
                    "ty::volatile-too-wide",
                    format!(
                        "a volatile `{}` cannot be accessed in one instruction",
                        place.ty
                    ),
                )
                .label(span, format!("the widest single access is {half} bits"))
                .help(help),
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
                self.through_pointer(ty, pointer, error)
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
                } else if let (Ty::Slice(elem), Some(base_place)) = (ty, base_place) {
                    // `(*s)[i]`, checked against the length (ADR 0031): the
                    // element is reached through the slice, so it is
                    // volatile if the slice is.
                    if self.q.layout_of(*elem).is_none() {
                        self.too_large(
                            *elem,
                            base.span,
                            "the distance between elements does not fit in the address space",
                        );
                    }
                    PlaceInfo {
                        ty: *elem,
                        ..base_place
                    }
                } else if let Ty::Ptr(ptr) = ty
                    && let Ty::Slice(elem) = *ptr.pointee
                {
                    if let Some(info) = base_place {
                        self.access(&info, base.span);
                    }
                    // Indexing a pointer steps over whole pointees, and `[T]`
                    // has no size (ADR 0021, 0031).
                    let first = Ty::ptr(ptr.mutable, ptr.volatile, *elem);
                    self.q.diags.push(
                        Diagnostic::error(
                            "ty::slice-pointer-index",
                            format!("cannot index the slice `{ty}` itself"),
                        )
                        .label(base.span, "a slice, not a pointer to its elements")
                        .help(format!(
                            "index what it points to, checked against its length: `(*s)[i]`; or, unchecked, a pointer to its first element: `(s as {first})[i]` (ADR 0031)"
                        )),
                    );
                    PlaceInfo::ERROR
                } else {
                    if let Some(info) = base_place {
                        self.access(&info, base.span);
                    }
                    if let Ty::Ptr(ptr) = ty
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
                    self.through_pointer(ty, base, error)
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
                if (other.slice_elem().is_some() || matches!(other, Ty::Slice(_))) && name == "len"
                {
                    diag = diag.help("the length of a slice `s` is `len(s)` (ADR 0031)");
                }
                self.q.diags.push(payload_values_help(diag, other));
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
        if let Ty::Slice(_) = info.ty {
            self.q
                .diags
                .push(whole_slice(info.ty, place.span, "written"));
            return Ty::Error;
        }
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
            // An aggregate, compared with a `match` (ADR 0039).
            Ty::Enum(EnumTy { payloads: true, .. }) => false,
            // Only told apart, never ordered or computed with (ADR 0035).
            Ty::Enum(_) => matches!(allowed, Operands::Equatable),
            // Aggregates compare part by part (ADR 0021).
            Ty::Unit | Ty::Array(..) | Ty::Slice(_) | Ty::Struct(_) => false,
        };
        if !ok {
            let payloads = matches!(ty, Ty::Enum(EnumTy { payloads: true, .. }));
            let what = match allowed {
                Operands::Int => "an integer",
                Operands::IntOrBool => "an integer or `bool`",
                Operands::Equatable if payloads => {
                    "integers, pointers, `bool` or enums without payloads"
                }
                Operands::Equatable => "integers, pointers, `bool` or enums",
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
            match ty {
                Ty::Enum(EnumTy { payloads: true, .. }) => {
                    diag = diag.help(
                        "an enum with payloads is an aggregate, which operators do not apply to; tell its variants apart with a `match` (ADR 0039)",
                    );
                }
                Ty::Enum(e) => {
                    diag = diag.help(format!(
                        "an enum only compares with `==` and `!=`; for anything else, convert it to its integer first, as in `x as {}` (ADR 0035)",
                        e.int
                    ));
                }
                _ => {}
            }
            self.q.diags.push(diag);
        }
    }
}

/// Lists struct layouts, enums with their values and, with payloads, their
/// layouts, then every function's locals and expression types, for
/// `--emit=typeck`.
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
    for (i, def) in hir.enums.iter().enumerate() {
        let id = EnumId(i as u32);
        // Each payload value with its offset, and the enum's size, if it
        // has a layout (ADR 0039).
        let layout = if def.ty.payloads {
            q.enum_layout(id)
        } else {
            None
        };
        let variants: Vec<String> = def
            .variants
            .iter()
            .enumerate()
            .map(|(index, variant)| {
                let payload: Vec<String> = variant
                    .payload
                    .iter()
                    .enumerate()
                    .map(|(j, value)| {
                        let ty = q.normalize(value.ty);
                        match &layout {
                            Some(layout) => format!("{ty} at {}", layout.offsets[index][j]),
                            None => ty.to_string(),
                        }
                    })
                    .collect();
                let payload = if payload.is_empty() {
                    String::new()
                } else {
                    format!("({})", payload.join(", "))
                };
                match q.discriminant(id, index as u32) {
                    Some(value) => format!(
                        "{}{payload} = {}",
                        variant.name,
                        def.ty.int.display_bits(value)
                    ),
                    None => format!("{}{payload} = ?", variant.name),
                }
            })
            .collect();
        let size = match &layout {
            Some(layout) => format!(
                " size {}, align {}",
                layout.layout.size, layout.layout.align
            ),
            None => String::new(),
        };
        writeln!(
            out,
            "enum {}: {} {{ {} }}{size}",
            def.name,
            def.ty.int,
            variants.join(", ")
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

/// `ty::wrong-payload-count` for the variant `index` of `id`, written at
/// `span` with `given` values, or patterns if `pattern`, or without
/// parentheses for `None`; `None` if their number is the payload's
/// (ADR 0039, 0040).
fn payload_count(
    hir: &Hir,
    id: EnumId,
    index: u32,
    given: Option<usize>,
    span: Span,
    pattern: bool,
) -> Option<Diagnostic> {
    let def = hir.enum_(id);
    let variant = &def.variants[index as usize];
    let carried = variant.payload.len();
    let counted = match given {
        None => carried == 0,
        Some(given) => carried > 0 && given == carried,
    };
    if counted {
        return None;
    }
    let name = format!("{}::{}", def.name, variant.name);
    let carries = match carried {
        0 => "no values".to_owned(),
        1 => "1 value".to_owned(),
        n => format!("{n} values"),
    };
    let were_given = |n: usize| match n {
        0 => "none were given".to_owned(),
        1 => "1 was given".to_owned(),
        n => format!("{n} were given"),
    };
    let (label, adr) = if pattern {
        ("in this pattern", "ADR 0040")
    } else {
        ("in this variant", "ADR 0039")
    };
    let wildcards = vec!["_"; carried].join(", ");
    let diag = match given {
        // There are no function values (ADR 0039).
        None if !pattern => {
            let values = vec!["..."; carried].join(", ");
            Diagnostic::error(
                "ty::wrong-payload-count",
                format!("`{name}` carries {carries}, but none are given"),
            )
            .label(span, "built without its values")
            .help(format!(
                "a variant with a payload is built with its values, as in `{name}({values})`; it is not a function (ADR 0039)"
            ))
        }
        None => Diagnostic::error(
            "ty::wrong-payload-count",
            format!("`{name}` carries {carries}, but no patterns are given for them"),
        )
        .label(span, "matched without its values")
        .help(format!(
            "a variant with a payload is matched with a pattern for each value, as in `{name}({wildcards})` (ADR 0040)"
        )),
        Some(given) if carried == 0 => {
            let message = if given == 0 {
                format!("`{name}` carries no values, but is written with parentheses")
            } else {
                format!("`{name}` carries no values, but {}", were_given(given))
            };
            let named = if pattern { "matched" } else { "named" };
            Diagnostic::error("ty::wrong-payload-count", message)
                .label(span, label)
                .help(format!(
                    "a variant without payload is {named} without parentheses: `{name}` ({adr})"
                ))
        }
        Some(given) => {
            let diag = Diagnostic::error(
                "ty::wrong-payload-count",
                format!("`{name}` carries {carries}, but {}", were_given(given)),
            )
            .label(span, label);
            if pattern {
                diag.help(format!(
                    "a pattern is written for each value, `_` for one not tested, as in `{name}({wildcards})`; there is no `..` (ADR 0040)"
                ))
            } else {
                diag
            }
        }
    };
    Some(diag.label(variant.span, "declared here"))
}

/// A variant pattern of `e`, naming its first variant, with `_` for each
/// of its payload values.
fn variant_pattern_example(hir: &Hir, e: EnumTy) -> String {
    match hir.enum_(e.id).variants.first() {
        Some(variant) if variant.payload.is_empty() => format!("{}::{}", e.name, variant.name),
        Some(variant) => format!(
            "{}::{}({})",
            e.name,
            variant.name,
            vec!["_"; variant.payload.len()].join(", ")
        ),
        None => e.name.to_owned(),
    }
}

/// The column of a position of type `ty`, as the checks of a `match` see
/// it (ADR 0040).
fn column(ty: Ty) -> Column {
    match ty {
        Ty::Bool => Column::Bool,
        Ty::Int(int) => Column::Int(int),
        Ty::Enum(e) => Column::Enum(e),
        _ => Column::Other,
    }
}

/// An arm whose or-patterns are checked for dead alternatives: `covered`
/// tells whether some rows leave no value a pattern matches, and `earlier`
/// holds what the `arms` before it match.
struct Alternatives<'a> {
    covered: &'a dyn std::ops::Fn(&[Pat], &Pat) -> bool,
    arms: &'a [Arm],
    earlier: &'a [Pat],
}

/// `, such as `E::V``, naming the first variant of `e` as it is written,
/// with `(...)` if it has a payload; empty without variants.
fn such_as_variant(hir: &Hir, e: EnumTy) -> String {
    match hir.enum_(e.id).variants.first() {
        Some(variant) if variant.payload.is_empty() => {
            format!(", such as `{}::{}`", e.name, variant.name)
        }
        Some(variant) => format!(", such as `{}::{}(...)`", e.name, variant.name),
        None => String::new(),
    }
}

/// `diag`, a `ty::no-fields` for a value of type `ty`, with a help on
/// payload values if `ty` is an enum with payloads (ADR 0039).
fn payload_values_help(diag: Diagnostic, ty: Ty) -> Diagnostic {
    match ty {
        Ty::Enum(EnumTy { payloads: true, .. }) => diag.help(
            "the values of an enum's payload have no names, and are reached only by patterns (ADR 0039, 0040)",
        ),
        _ => diag,
    }
}

/// `a`, `a and b`, or `a, b and c`.
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [only] => only.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
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
        ExprKind::Variant(id, index, _) => {
            let def = hir.enum_(*id);
            format!(
                "variant {}::{}",
                def.name, def.variants[*index as usize].name
            )
        }
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
        ExprKind::Assert(..) => "assert".to_owned(),
        ExprKind::Len(_) => "len".to_owned(),
        ExprKind::Slice(..) => "slice".to_owned(),
        ExprKind::Loop(_) => "loop".to_owned(),
        ExprKind::Match { .. } => "match".to_owned(),
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
