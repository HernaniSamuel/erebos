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

use crate::diag::Diagnostic;
use crate::hir::{
    self, AccessMode, BinOp, BitStructId, BitStructTy, Block, EnumId, EnumTy, Expr, ExprId,
    ExprKind, Fn, FnKind, Hir, IntTy, LocalId, ModuleId, NeverPosition, PatternId,
    PeripheralTypeId, Ptr, StaticId, Stmt, Ty, UnsizedPosition,
};
use crate::layout;
use crate::query::Queries;
use crate::span::Span;

pub use dump::dump;

mod dump;
mod expr;
mod pattern;
mod place;
mod usefulness;

/// What type checking found out about expressions, for every function
/// checked so far.
pub struct Tables {
    expr_types: Vec<Ty>,
    /// The field index of each field access, or the register index of
    /// each register access (ADR 0058).
    fields: HashMap<ExprId, u32>,
    /// The bit-field struct and the field index of each access to a bit
    /// field, read or written (ADR 0059).
    bit_fields: HashMap<ExprId, (BitStructId, u32)>,
    /// The slice type each pointer to an array is converted to, where it
    /// is (ADR 0031): unlike the other conversion (ADR 0024), it changes
    /// the representation.
    coercions: HashMap<ExprId, Ty>,
    /// The enum and variant index each `.V` names (ADR 0044).
    variants: HashMap<ExprId, (EnumId, u32)>,
    /// Likewise for each `.V` pattern.
    pattern_variants: HashMap<PatternId, (EnumId, u32)>,
}

impl Tables {
    pub fn new(expr_count: u32) -> Tables {
        Tables {
            // Overwritten for every expression checked; the placeholder is
            // never read.
            expr_types: vec![Ty::Error; expr_count as usize],
            fields: HashMap::new(),
            bit_fields: HashMap::new(),
            coercions: HashMap::new(),
            variants: HashMap::new(),
            pattern_variants: HashMap::new(),
        }
    }

    pub fn expr_ty(&self, id: ExprId) -> Ty {
        self.expr_types[id.0 as usize]
    }

    /// The index of the field that the field access `id` reads, or of the
    /// register it reaches when its base is of a peripheral type.
    pub fn field(&self, id: ExprId) -> u32 {
        self.fields[&id]
    }

    /// The bit-field struct and the index of the field that the field
    /// access `id` reads or writes, if its base is of a bit-field struct
    /// type (ADR 0059).
    pub fn bit_field(&self, id: ExprId) -> Option<(BitStructId, u32)> {
        self.bit_fields.get(&id).copied()
    }

    /// The slice type that the value of `id`, a pointer to an array, is
    /// converted to, if it is (ADR 0031).
    pub fn coercion(&self, id: ExprId) -> Option<Ty> {
        self.coercions.get(&id).copied()
    }

    /// The enum and the index of the variant that the `.V` `id` names,
    /// resolved against its expected type (ADR 0044).
    pub fn variant(&self, id: ExprId) -> (EnumId, u32) {
        self.variants[&id]
    }

    /// The enum and the index of the variant that the `.V` pattern `id`
    /// names, resolved against the type of its position (ADR 0044).
    pub fn pattern_variant(&self, id: PatternId) -> (EnumId, u32) {
        self.pattern_variants[&id]
    }
}

/// Reports each `!` written where memory would have its type, which HIR
/// lowering made `{error}` (ADR 0034).
pub fn report_never_in_memory(q: &mut Queries) {
    let hir = q.hir;
    for never in &hir.never_in_memory {
        let help = match never.position {
            NeverPosition::Static => NeverHelp::Static,
            NeverPosition::Register => NeverHelp::Register,
            NeverPosition::BitField => NeverHelp::BitField,
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
        let what = position(found.position);
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

/// Reports each peripheral type written where a value would have it, which
/// HIR lowering made `{error}` (ADR 0058).
pub fn report_peripherals_by_value(q: &mut Queries) {
    let hir = q.hir;
    for found in &hir.peripherals_by_value {
        let ty = found.ty;
        q.diags.push(
            peripheral_by_value(ty)
                .label(found.span, format!("`{ty}` as {}", position(found.position)))
                .help(format!(
                    "a peripheral type describes a device's registers, reached one at a time from a peripheral or through a pointer, such as `*mut {ty}` (ADR 0058)"
                )),
        );
    }
}

/// Reports each pointer to a peripheral type written `volatile`, which HIR
/// lowering made the pointer without it (ADR 0058).
pub fn report_volatile_peripherals(q: &mut Queries) {
    let hir = q.hir;
    for found in &hir.volatile_peripherals {
        let Ty::Ptr(ptr) = found.ty else {
            unreachable!("lowered as a pointer to a peripheral type")
        };
        q.diags.push(
            Diagnostic::error(
                "ty::volatile-peripheral",
                format!(
                    "a pointer to the peripheral type `{}` cannot be `volatile`",
                    ptr.pointee
                ),
            )
            .label(found.span, "its accesses are volatile without the word")
            .help(format!(
                "accesses through a pointer to a peripheral type reach its registers, which are always accessed volatile: write `{}` (ADR 0058)",
                found.ty
            )),
        );
    }
}

/// How diagnostics name a position where `[T]` would need a size, or a
/// peripheral type would be the type of a value.
fn position(position: UnsizedPosition) -> String {
    match position {
        UnsizedPosition::Memory(position) => memory_position(position),
        UnsizedPosition::Variable => "the type of a variable".to_owned(),
        UnsizedPosition::Parameter => "the type of a parameter".to_owned(),
        UnsizedPosition::Return => "a return type".to_owned(),
        UnsizedPosition::Cast => "the target of a cast".to_owned(),
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
        NeverPosition::TypeArgument => "a type argument".to_owned(),
        NeverPosition::Static => "the type of a static".to_owned(),
        NeverPosition::Const => "the type of a constant".to_owned(),
        NeverPosition::Register => "the type of a register".to_owned(),
        NeverPosition::BitField => "the type of a bit field".to_owned(),
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

/// `ty::peripheral-by-value` for `ty`, a peripheral type (ADR 0058),
/// without labels.
fn peripheral_by_value(ty: Ty) -> Diagnostic {
    Diagnostic::error(
        "ty::peripheral-by-value",
        format!("`{ty}` is a peripheral type, which no value has"),
    )
}

/// `ty::peripheral-by-value` for `place`, a peripheral or what a pointer to
/// the peripheral type `ty` points to, `accessed` whole: a device is reached
/// one register at a time (ADR 0058).
fn whole_peripheral(ty: Ty, place: Span, accessed: &str) -> Diagnostic {
    peripheral_by_value(ty)
        .label(place, format!("a device, {accessed} as a whole"))
        .help(format!(
            "its registers are read and written one at a time, as in `P.NAME` or `(*u).NAME`; a function is given the device as a `*mut {ty}`, such as `&raw mut P` (ADR 0058)"
        ))
}

/// What `ty::never-in-memory` suggests instead.
#[derive(Clone, Copy)]
enum NeverHelp {
    Unit,
    /// A static's address must be a real one.
    Static,
    /// A register holds an integer of its width (ADR 0058).
    Register,
    /// A bit field holds a value of a few bits (ADR 0059).
    BitField,
}

fn never_in_memory(span: Span, what: &str, help: NeverHelp) -> Diagnostic {
    let instead = match help {
        NeverHelp::Unit => "for a type without data, use `()` (ADR 0034)",
        NeverHelp::Static => {
            "use a type of non-zero size, such as `u8`, whose address is a real one (ADR 0022, 0034)"
        }
        NeverHelp::Register => "a register is a `u8`, a `u16` or a `u32` (ADR 0058)",
        NeverHelp::BitField => {
            "a bit field is a `bool`, an integer type or an enum without payloads (ADR 0059)"
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

/// Expressions that cannot be typed on their own: integer literals,
/// variants named by the expected type (ADR 0044), and operators applied
/// only to them.
fn needs_expected_type(expr: &Expr) -> bool {
    untyped(expr, true)
}

/// Whether the operand of `as` takes the type a literal there takes
/// (ADR 0017): as `needs_expected_type`, but a `.V` takes none, since
/// nothing converts to an enum (ADR 0044).
fn takes_literal_type(expr: &Expr) -> bool {
    untyped(expr, false)
}

/// `needs_expected_type`, where `.V` counts only with `variants`.
fn untyped(expr: &Expr, variants: bool) -> bool {
    match &expr.kind {
        ExprKind::Int(_) => true,
        ExprKind::ContextVariant(..) => variants,
        ExprKind::Unary(_, operand) | ExprKind::Comptime(operand) => untyped(operand, variants),
        // A comparison is a `bool` whatever its operands are.
        ExprKind::Binary(op, _, _, _) if op.is_comparison() => false,
        ExprKind::Binary(op, lhs, _, _) if op.is_shift() => untyped(lhs, variants),
        ExprKind::Binary(_, lhs, rhs, _) => untyped(lhs, variants) && untyped(rhs, variants),
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
    /// The register the place is, or is part of (ADR 0058): its access
    /// mode decides whether it may be read and written, and `&raw` of it
    /// is an error.
    register: Option<Register>,
    /// The place is a bit field of a value of this bit-field struct type
    /// (ADR 0059): it has no address, and is read and written with the
    /// whole value, which volatile accesses are checked as.
    bit_field: Option<BitStructTy>,
}

/// The register `index` of the peripheral type `id`.
#[derive(Clone, Copy)]
struct Register {
    id: PeripheralTypeId,
    index: usize,
}

impl PlaceInfo {
    /// The place of an expression that failed to type; already reported.
    const ERROR: PlaceInfo = PlaceInfo {
        ty: Ty::Error,
        immutable: None,
        via: None,
        misaligned: false,
        constant: false,
        register: None,
        bit_field: None,
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
            register: None,
            bit_field: None,
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
            register: None,
            bit_field: None,
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
    /// function is called by no one and takes none, and the panic function
    /// takes none or one `*const core::PanicInfo` (ADR 0053).
    fn params(&mut self, f: &Fn) {
        if f.kind == FnKind::Entry
            && let Some(first) = f.params().first()
        {
            self.q.diags.push(
                Diagnostic::error(
                    "ty::special-fn-params",
                    "the entry function takes no parameters",
                )
                .label(first.span, "parameter here"),
            );
        }
        if f.kind == FnKind::Panic {
            self.panic_params(f);
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

    /// The panic function takes no parameter, or one through which it
    /// learns where and why the program panicked: exactly a
    /// `*const core::PanicInfo`, since the information is read-only
    /// (ADR 0053).
    fn panic_params(&mut self, f: &Fn) {
        let label = match f.params() {
            [] => return,
            [param] => {
                let ty = self.norm(param.ty);
                let info = self.q.hir.panic_info;
                match ty {
                    Ty::Ptr(Ptr {
                        mutable: false,
                        volatile: false,
                        pointee: Ty::Struct(s),
                    }) if s.id == info => return,
                    // Reported where the type is written, or as a
                    // parameter's type below.
                    Ty::Error | Ty::Unit | Ty::Never => return,
                    _ => (param.span, format!("has type `{ty}`")),
                }
            }
            [_, second, ..] => (second.span, "a second parameter".to_owned()),
        };
        self.q.diags.push(
            Diagnostic::error(
                "ty::panic-param",
                "the panic function takes no parameter or one `*const core::PanicInfo`",
            )
            .label(label.0, label.1)
            .help(
                "declare it `panic fn on_panic() -> !`, or `panic fn on_panic(info: *const core::PanicInfo) -> !` to learn where and why the program panicked, from read-only data (ADR 0053)",
            ),
        );
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
            ExprKind::ContextVariant(name, name_span, args) => {
                self.context_variant(f, expr, name, *name_span, args.as_deref(), expected)
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
                // An `Option(T)` is the enum it is (ADR 0045).
                match self.norm(*ty) {
                    // The struct is not accessed: any place will do.
                    ty @ Ty::Struct(_) => {
                        self.field(f, expr, ty, PlaceInfo::ERROR, name, *name_span);
                    }
                    Ty::Error => {}
                    Ty::BitStruct(s) => self.bit_field_offset(f, s, name, *name_span),
                    other => {
                        let diag =
                            Diagnostic::error("ty::no-fields", format!("`{other}` has no fields"))
                                .label(*name_span, "no such field");
                        self.q.diags.push(payload_values_help(diag, other));
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
            // Each value is one of its field's type, so a literal and `.V`
            // take that type (ADR 0059); the fields left out by `..` were
            // filled in by name resolution.
            ExprKind::BitStructLit(s, fields, _) => {
                let hir = self.hir;
                let def = hir.bit_struct(s.id);
                for (index, value) in fields {
                    let ty = self.norm(def.fields[*index as usize].ty);
                    self.check(f, value, Some(ty));
                }
                Ty::BitStruct(*s)
            }
            ExprKind::Call { callee, args } => self.call(f, expr, *callee, args),
            ExprKind::Unary(op, operand) => self.unary(f, expr, *op, operand, expected),
            ExprKind::Binary(op, lhs, rhs, _) if op.is_shift() => {
                let lhs_ty = self.check(f, lhs, expected);
                self.require(lhs, lhs_ty, op.symbol(), Operands::Int);
                self.shift_amount(f, rhs, lhs_ty, op.symbol());
                lhs_ty
            }
            ExprKind::Binary(op, lhs, rhs, _) if op.is_comparison() => {
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
            ExprKind::Binary(op, lhs, rhs, _) => {
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
            | ExprKind::Peripheral(_)
            | ExprKind::ByteString(_) => {
                let info = self.place_info(f, expr).expect(
                    "dereferences, indexing, fields, statics, peripherals and byte strings are places",
                );
                if let Ty::Slice(_) = info.ty {
                    self.q.diags.push(whole_slice(info.ty, expr.span, "read"));
                    return Ty::Error;
                }
                if let Ty::Peripheral(_) = info.ty {
                    self.q
                        .diags
                        .push(whole_peripheral(info.ty, expr.span, "read"));
                    return Ty::Error;
                }
                self.access(&info, expr.span, AccessMode::Read);
                info.ty
            }
            ExprKind::AddrOf { mutable, place } => self.addr_of(f, *mutable, place),
            ExprKind::Try(operand, _) => self.try_(f, expr, operand),
            ExprKind::Assign(place, value) => {
                let place_ty = self.place(f, place, AccessMode::Write);
                self.check(f, value, Some(place_ty));
                Ty::Unit
            }
            ExprKind::CompoundAssign(op, place, value, _) => {
                // Read, then written (ADR 0023, 0058).
                let place_ty = self.place(f, place, AccessMode::ReadWrite);
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
            ExprKind::Assert(cond, message, _) => {
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
            ExprKind::Match {
                scrutinee, arms, ..
            } => self.match_(f, scrutinee, arms, expected),
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
            // Aggregates compare part by part (ADR 0021); an `Option(T)`
            // is checked as the enum it is (ADR 0045).
            Ty::Unit | Ty::Array(..) | Ty::Slice(_) | Ty::Struct(_) | Ty::Applied(_) => false,
            // A device, reached one register at a time (ADR 0058).
            Ty::Peripheral(_) => false,
            // Compared field by field, or as an integer made with `as`
            // (ADR 0059).
            Ty::BitStruct(_) => false,
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
                Ty::BitStruct(s) => {
                    diag = diag.help(format!(
                        "operators do not apply to a bit-field struct: use them on its fields, or on its integer made with `as`, as in `x as {}`, which its reserved bits are part of (ADR 0059)",
                        s.int
                    ));
                }
                _ => {}
            }
            self.q.diags.push(diag);
        }
    }
}

/// `ty::wrong-payload-count` for the variant `index` of `def`, written at
/// `span` with `given` values, or patterns if `pattern`, or without
/// parentheses for `None`; `None` if their number is the payload's
/// (ADR 0039, 0040).
fn payload_count(
    def: &hir::EnumDef,
    index: u32,
    given: Option<usize>,
    span: Span,
    pattern: bool,
) -> Option<Diagnostic> {
    let variant = &def.variants[index as usize];
    let carried = variant.payload.len();
    let counted = match given {
        None => carried == 0,
        Some(given) => carried > 0 && given == carried,
    };
    if counted {
        return None;
    }
    let name = def.variant_path(index as usize);
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
    // `Option` and `Result` are declared nowhere (ADR 0045).
    Some(match variant.span {
        Some(span) => diag.label(span, "declared here"),
        None => diag,
    })
}

/// `ty::unknown-variant` for `.name`, written at `span` in `module`, where
/// a value of the enum `def` is expected (ADR 0044). The help lists its
/// variants only where they may be named (ADR 0050).
fn unknown_variant(def: &hir::EnumDef, name: &str, span: Span, module: ModuleId) -> Diagnostic {
    // A variant declared twice was reported, and is listed once.
    let mut names: Vec<String> = Vec::new();
    for variant in &def.variants {
        let listed = format!("`{}`", variant.name);
        if !names.contains(&listed) {
            names.push(listed);
        }
    }
    let mut diag = Diagnostic::error(
        "ty::unknown-variant",
        format!("`{}` has no variant `{name}`", def.name),
    )
    .label(span, "unknown variant");
    // `Option` and `Result` are declared nowhere (ADR 0045).
    if let Some(name_span) = def.name_span {
        diag = diag.label(name_span, format!("`{}` is declared here", def.name));
    }
    if !def.reachable_from(module) {
        diag.help(format!(
            "`{}` is not reachable here, so none of its variants can be named (ADR 0050)",
            def.name
        ))
    } else if names.is_empty() {
        diag.help(format!("`{}` has no variants", def.name))
    } else {
        diag.help(format!("its variants are {}", names.join(", ")))
    }
}

/// `, such as `E::V``, naming the first variant of `def` as it is
/// written, with `(...)` if it has a payload; empty without variants.
fn such_as_variant(def: &hir::EnumDef) -> String {
    match def.variants.first() {
        Some(variant) if variant.payload.is_empty() => {
            format!(", such as `{}`", def.variant_path(0))
        }
        Some(_) => format!(", such as `{}(...)`", def.variant_path(0)),
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
