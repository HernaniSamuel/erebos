//! Checking the expressions that have rules of their own: arrays,
//! variants, calls, `?`, slices, loops, literals, operators and casts.

use crate::diag::Diagnostic;
use crate::hir::{self, EnumTy, Expr, ExprId, ExprKind, Fn, Hir, IntTy, ModuleId, Ty, UnOp};
use crate::span::Span;

use super::{
    Checker, Equality, LoopCtx, Operands, coerces, element_of, needs_expected_type, payload_count,
    peripheral_by_value, such_as_variant, takes_literal_type, unknown_variant, unsized_ty,
};

impl Checker<'_, '_> {
    /// `[a, b, c]`: the elements share one type, which the expected array
    /// type gives or, without one, the first element of a type other than
    /// `!` typed on its own. An element of type `!` makes the whole array
    /// one, like a block that does not finish (ADR 0034).
    pub(super) fn array(
        &mut self,
        f: &Fn,
        expr: &Expr,
        elems: &[Expr],
        expected: Option<Ty>,
    ) -> Ty {
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
    pub(super) fn variant(
        &mut self,
        f: &Fn,
        expr: &Expr,
        id: hir::EnumId,
        index: u32,
        args: Option<&[Expr]>,
    ) -> Ty {
        let def = self.q.enum_def(id);
        let variant = &def.variants[index as usize];
        if let Some(diag) = payload_count(def, index, args.map(<[Expr]>::len), expr.span, false) {
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

    /// `.V`, or `.V(args)` when `args` are written: the variant `V` of the
    /// enum that is the expected type, then checked as `E::V` is
    /// (ADR 0044). When it names no variant, which is reported unless the
    /// expected type is erroneous, its values are checked against
    /// `{error}`, so that they report nothing more.
    pub(super) fn context_variant(
        &mut self,
        f: &Fn,
        expr: &Expr,
        name: &str,
        name_span: Span,
        args: Option<&[Expr]>,
        expected: Option<Ty>,
    ) -> Ty {
        let hir = self.hir;
        let found = match expected {
            Some(Ty::Enum(e)) => {
                let def = self.q.enum_def(e.id);
                match def.variants.iter().position(|v| v.name == name) {
                    Some(index) => Some((e.id, index as u32)),
                    None => {
                        self.q
                            .diags
                            .push(unknown_variant(def, name, name_span, f.module));
                        None
                    }
                }
            }
            Some(Ty::Error) => None,
            Some(other) => {
                self.q.diags.push(
                    Diagnostic::error("ty::mismatched-types", "mismatched types")
                        .label(
                            expr.span,
                            format!("expected `{other}`, found variant `.{name}`"),
                        )
                        .help(format!(
                            "`.{name}` names a variant of the enum expected where it appears, and `{other}` is not an enum (ADR 0044)"
                        )),
                );
                None
            }
            None => {
                self.q
                    .diags
                    .push(untyped_variant(hir, name, expr.span, f.module));
                None
            }
        };
        let Some((id, index)) = found else {
            for arg in args.unwrap_or_default() {
                self.check(f, arg, Some(Ty::Error));
            }
            return Ty::Error;
        };
        self.reachable_variant(f, id, name, name_span, false);
        self.q.tables.variants.insert(expr.id, (id, index));
        self.variant(f, expr, id, index, args)
    }

    /// `operand?` (ADR 0046): the operand is typed on its own, and must be
    /// an `Option(T)` or a `Result(T, E)`, in a function returning an
    /// `Option(U)`, or a `Result(U, E)` with the same `E`, which is never
    /// converted (ADR 0008). Its type is `T`. An operand of type `!` gives
    /// `!`, and an erroneous one `{error}`, without checking the return
    /// type; an erroneous return type reports nothing either.
    pub(super) fn try_(&mut self, f: &Fn, expr: &Expr, operand: &Expr) -> Ty {
        let ty = self.check(f, operand, None);
        let (e, builtin) = match ty {
            Ty::Never | Ty::Error => return ty,
            _ if self.q.is_erroneous(ty) => return Ty::Error,
            Ty::Enum(e) if let Some(builtin) = self.q.builtin_enum(e.id) => (e, builtin),
            other => {
                let diag = Diagnostic::error(
                    "ty::try-operand",
                    format!("`?` takes an `Option` or a `Result`, not `{other}`"),
                )
                .label(operand.span, format!("has type `{other}`"))
                .help("`?` gives the value of an `Option(T)` or a `Result(T, E)`, and returns its `.None` or `.Err(x)` from the function (ADR 0046)");
                self.q.diags.push(diag);
                return Ty::Error;
            }
        };
        let def = self.q.enum_def(e.id);
        let (success, failure) = builtin.try_variants();
        let value_ty = def.variants[success as usize].payload[0].ty;
        // The error type of a `Result`; an `Option` fails without a value.
        let error = def.variants[failure as usize]
            .payload
            .first()
            .map(|payload| payload.ty);
        let ret = self.ret;
        if ret == Ty::Error || self.q.is_erroneous(ret) {
            return self.norm(value_ty);
        }
        // The same `E`, identical: no conversion applies (ADR 0008).
        let ret_error = match ret {
            Ty::Enum(r) if self.q.builtin_enum(r.id) == Some(builtin) => Some(
                self.q.enum_def(r.id).variants[failure as usize]
                    .payload
                    .first()
                    .map(|payload| payload.ty),
            ),
            _ => None,
        };
        if ret_error != Some(error) {
            let (message, help) = match (builtin, error, ret_error) {
                (hir::BuiltinEnum::Result, Some(error), Some(_)) => (
                    "`?` on a `Result` whose error type is not the function's".to_owned(),
                    format!(
                        "errors are never converted (ADR 0008): on `.Err(x)`, `?` returns `.Err(x)` as it is, so the function must return a `Result(U, {error})`; convert the error with a `match` (ADR 0046)"
                    ),
                ),
                (hir::BuiltinEnum::Result, Some(error), None) => (
                    "`?` on a `Result` in a function that does not return a `Result`".to_owned(),
                    format!(
                        "on `.Err(x)`, `?` returns `.Err(x)`, so the function must return a `Result(U, {error})`, for any `U` (ADR 0046)"
                    ),
                ),
                _ => (
                    "`?` on an `Option` in a function that does not return an `Option`".to_owned(),
                    "on `.None`, `?` returns `.None`, so the function must return an `Option(U)`, for any `U` (ADR 0046)".to_owned(),
                ),
            };
            let (at, what) = match f.ret_span {
                Some(span) => (span, format!("the function returns `{ret}`")),
                None => (f.name_span, format!("returns `{ret}`")),
            };
            self.q.diags.push(
                Diagnostic::error("ty::try-return", message)
                    .label(expr.span, format!("`?` on `{ty}`"))
                    .label(at, what)
                    .help(help),
            );
        }
        self.norm(value_ty)
    }

    /// Each argument is checked against its parameter's type.
    pub(super) fn call(&mut self, f: &Fn, expr: &Expr, callee: hir::FnId, args: &[Expr]) -> Ty {
        let target = self.hir.fn_(callee);
        let params = target.params();
        if args.len() != params.len() {
            let plural = if params.len() == 1 { "" } else { "s" };
            self.q.diags.push(
                Diagnostic::error(
                    "ty::wrong-arg-count",
                    format!(
                        "`{}` takes {} argument{plural}, but {} {} given",
                        target.path,
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
    pub(super) fn len(&mut self, f: &Fn, slice: &Expr) {
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
    pub(super) fn make_slice(&mut self, f: &Fn, pointer: &Expr, len: &Expr) -> Ty {
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
                // `*mut [Uart]` written is reported in HIR lowering; this
                // is the one way to make it from a type not written so.
                Ty::Peripheral(_) => {
                    let raw = if ptr.mutable {
                        "&raw mut"
                    } else {
                        "&raw const"
                    };
                    self.q.diags.push(
                        peripheral_by_value(*ptr.pointee)
                            .label(
                                pointer.span,
                                format!("a `{ty}`, so its elements would be `{}`", ptr.pointee),
                            )
                            .help(format!(
                                "a peripheral type has no size, so there is no slice of devices; each one is reached through its own `{ty}`, such as `{raw} P` (ADR 0058)"
                            )),
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

    pub(super) fn in_loop(
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
    pub(super) fn break_(&mut self, f: &Fn, expr: &Expr, target: ExprId, value: Option<&Expr>) {
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
            ExprKind::Const(id) => Some(self.hir.const_(*id).path.clone()),
            ExprKind::Static(id) => Some(self.hir.static_(*id).path.clone()),
            ExprKind::Variant(id, index, None) => {
                Some(self.q.enum_def(*id).variant_path(*index as usize))
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
    pub(super) fn int_literal(
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
                    let variant = such_as_variant(self.q.enum_def(e.id));
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

    pub(super) fn unary(
        &mut self,
        f: &Fn,
        expr: &Expr,
        op: UnOp,
        operand: &Expr,
        expected: Option<Ty>,
    ) -> Ty {
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
    pub(super) fn same_type_operands(
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
    pub(super) fn shift_amount(&mut self, f: &Fn, amount: &Expr, shifted: Ty, op: &str) {
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
    pub(super) fn cast(&mut self, f: &Fn, operand: &Expr, target: Ty, target_span: Span) -> Ty {
        let literal_ty = match target {
            Ty::Int(_) => Some(target),
            Ty::Ptr(_) => Some(Ty::Int(IntTy::Usize)),
            // Reported, such as `as *const !` (ADR 0034), or below, `as !`
            // and `as Mode`, which a literal has no type to take for.
            Ty::Error | Ty::Never | Ty::Enum(_) => Some(Ty::Error),
            _ => None,
        };
        let expected = if takes_literal_type(operand) {
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
                        such_as_variant(self.q.enum_def(e.id))
                    )),
            );
            return Ty::Error;
        }
        // Nothing converts to an enum, so a program that only writes
        // variants never holds a value that is no variant (ADR 0035, 0036).
        if let Ty::Enum(e) = target {
            let def = self.q.enum_def(e.id);
            let variant = match def.variants.first() {
                Some(_) => format!("`N => {}`", def.variant_path(0)),
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
}

/// `ty::untyped-variant` for `.name`, written at `span` in `module` where no
/// type is expected (ADR 0044). The help names the enum when exactly one
/// declared enum reachable from `module` has a variant `name` (ADR 0050),
/// and writes the type for the variants of
/// `Option` and `Result` (ADR 0045), both in one help when both apply.
fn untyped_variant(hir: &Hir, name: &str, span: Span, module: ModuleId) -> Diagnostic {
    let mut having = hir
        .enums
        .iter()
        .filter(|def| def.reachable_from(module))
        .filter_map(|def| {
            def.variants
                .iter()
                .find(|variant| variant.name == name)
                .map(|variant| (def, variant))
        });
    let named = match (having.next(), having.next()) {
        (Some((def, variant)), None) => {
            let values = if variant.payload.is_empty() {
                ""
            } else {
                "(...)"
            };
            Some(format!("`{}::{name}{values}`", def.name))
        }
        _ => None,
    };
    let built_in = match name {
        "None" => Some(("Option", "let x: Option(u32) = .None;")),
        "Some" => Some(("Option", "let x: Option(u32) = .Some(1);")),
        "Ok" => Some(("Result", "let x: Result(u32, E) = .Ok(1);")),
        "Err" => Some(("Result", "let x: Result(u32, E) = .Err(e);")),
        _ => None,
    };
    let help = match (named, built_in) {
        (Some(named), None) => format!("name its enum: {named} (ADR 0044)"),
        (None, Some((built_in, example))) => format!(
            "`.{name}` is a variant of `{built_in}` where one is expected; write its type there, as in `{example}` (ADR 0044, 0045)"
        ),
        (Some(named), Some((built_in, example))) => format!(
            "name its enum: {named}; or, for the variant of `{built_in}`, write its type where the value is expected, as in `{example}` (ADR 0044, 0045)"
        ),
        (None, None) => format!(
            "`.{name}` takes its enum from the type expected where it appears; name the enum, as in `E::{name}`, or give the value a type first, as in `let x: E = .{name};` (ADR 0044)"
        ),
    };
    Diagnostic::error(
        "ty::untyped-variant",
        format!("cannot tell the enum of `.{name}`"),
    )
    .label(span, "no type is expected here")
    .help(help)
}
