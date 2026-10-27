//! Checking `match` and its patterns (ADR 0037, 0040): the type of each
//! pattern, then exhaustiveness and dead arms, by [`super::usefulness`].

use crate::diag::Diagnostic;
use crate::hir::{self, Arm, EnumId, EnumTy, Expr, Fn, IntTy, ModuleId, Pattern, PatternKind, Ty};
use crate::span::Span;

use super::usefulness::{self, Column, Enums, Pat, Search, Variant};
use super::{Checker, payload_count, such_as_variant, unknown_variant};

impl Checker<'_, '_> {
    /// `match scrutinee { arms }` (ADR 0037, 0040). The scrutinee is typed
    /// on its own; each pattern against its type; each arm's expression as
    /// a branch of `if` is, the first arm whose type is not `!` giving the
    /// type without an expected one. Then the arms must cover every value,
    /// and each must match some value the arms before it leave.
    pub(super) fn match_(
        &mut self,
        f: &Fn,
        scrutinee: &Expr,
        arms: &[Arm],
        expected: Option<Ty>,
    ) -> Ty {
        let ty = self.check(f, scrutinee, None);
        let matched = match ty {
            Ty::Int(_) | Ty::Bool | Ty::Error => ty,
            Ty::Enum(e) if !e.payloads => ty,
            // An enum with a payload type in error was reported, once
            // (ADR 0039).
            Ty::Enum(_) if self.q.is_erroneous(ty) => Ty::Error,
            Ty::Enum(_) => ty,
            other => {
                let help = match other {
                    // Its fields are values to match on (ADR 0059).
                    Ty::BitStruct(s) => format!(
                        "a `match` takes an integer, a `bool` or an enum (ADR 0037); match on one of its fields, or on its integer made with `as`, as in `x as {}` (ADR 0059)",
                        s.int
                    ),
                    _ => "a `match` takes an integer, a `bool` or an enum (ADR 0037)".to_owned(),
                };
                self.q.diags.push(
                    Diagnostic::error(
                        "ty::match-type",
                        format!("cannot `match` on a value of type `{other}`"),
                    )
                    .label(scrutinee.span, format!("has type `{other}`"))
                    .help(help),
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
        let needed = self.q.enums_needed(ty, arms);
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
            self.coverage(f.module, scrutinee, matched, arms, &values, &enums);
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
            let def = self.q.enum_def(e.id);
            if e.payloads {
                // No constant of it is a pattern (ADR 0040).
                return format!(
                    "an enum with payloads is matched with its variant patterns, such as `{}` (ADR 0040)",
                    variant_pattern_example(def)
                );
            }
            let variant = such_as_variant(def);
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
                            variant_pattern_example(self.q.enum_def(e.id))
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
                            format!("`{}` has type `{const_ty}`", def.path),
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
            PatternKind::ContextVariant(..) => self.context_variant_pattern(f, pattern, ty),
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
        let def = self.q.enum_def(id);
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
        if let Some(diag) =
            payload_count(def, index, subs.map(<[Pattern]>::len), pattern.span, true)
        {
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

    /// `.V`, or `.V(subs)` when `subs` are written, against `ty`: the
    /// variant `V` of the enum `ty` is, then checked as `E::V` is
    /// (ADR 0044). When it names no variant, which is reported unless `ty`
    /// is erroneous, its patterns are checked against `{error}`, so that
    /// they report nothing more.
    fn context_variant_pattern(&mut self, f: &Fn, pattern: &Pattern, ty: Ty) -> Option<Pat> {
        let PatternKind::ContextVariant(id, name, name_span, subs) = &pattern.kind else {
            unreachable!("a `.V` pattern")
        };
        let (id, name, name_span, subs) = (*id, name.as_str(), *name_span, subs.as_deref());
        let found = match ty {
            Ty::Enum(e) => {
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
            Ty::Error => None,
            other => {
                self.q.diags.push(
                    Diagnostic::error("ty::mismatched-types", "mismatched types")
                        .label(
                            pattern.span,
                            format!("expected `{other}`, found variant `.{name}`"),
                        )
                        .help(format!(
                            "`.{name}` names a variant of the enum matched where it appears, and `{other}` is not an enum (ADR 0044)"
                        )),
                );
                None
            }
        };
        let Some((enum_id, index)) = found else {
            for sub in subs.unwrap_or_default() {
                self.pattern(f, sub, Ty::Error);
            }
            return None;
        };
        self.reachable_variant(f, enum_id, name, name_span, true);
        self.q.tables.pattern_variants.insert(id, (enum_id, index));
        self.variant_pattern(f, pattern, ty, enum_id, index, subs)
    }

    /// Reports `.name`, written at `span` in the body `f`, in a pattern if
    /// `pattern`, which names a variant of the enum `id`, if that enum is
    /// not reachable from the module of `f`: only there may its variants be
    /// named (ADR 0050). It still names the variant, so nothing more is
    /// reported about it.
    pub(super) fn reachable_variant(
        &mut self,
        f: &Fn,
        id: EnumId,
        name: &str,
        span: Span,
        pattern: bool,
    ) {
        let def = self.q.enum_def(id);
        if def.reachable_from(f.module) {
            return;
        }
        let module = self.hir.module(f.module).describe();
        let help = if pattern {
            "match its values with `_`, which names no variant, or offer the enum with `pub` or `pub use`"
        } else {
            "make its values with a function of its module, or offer the enum with `pub` or `pub use`"
        };
        let mut diag = Diagnostic::error(
            "ty::private-variant",
            format!(
                "`.{name}` names a variant of `{}`, which cannot be named here",
                def.name
            ),
        )
        .label(span, "not visible here");
        if let Some(name_span) = def.name_span {
            diag = diag.label(name_span, format!("`{}` is declared here", def.name));
        }
        self.q.diags.push(diag.help(format!(
            "the variants of an enum are visible only where a path can name the enum, and none written in {module} can name `{}`: {help} (ADR 0050)",
            def.name
        )));
    }

    /// Exhaustiveness and dead arms (ADR 0037, 0040) of a `match` on `ty`,
    /// an integer, `bool` or enum, whose arms match `values`, one per arm.
    /// `enums` has the variants of the enums whose values the `match`
    /// needs; none of them failed or repeats (ADR 0038, 0040). The values
    /// of an enum are all those of its integer type (ADR 0036): for dead
    /// arms, those that are no variant remain after every variant, but a
    /// `match` is exhaustive with every variant.
    /// The `match` is written in `module`: a missing value that names a
    /// variant of an enum not reachable there cannot be written there, and
    /// the help says how to cover it (ADR 0050).
    fn coverage(
        &mut self,
        module: ModuleId,
        scrutinee: &Expr,
        ty: Ty,
        arms: &[Arm],
        values: &[Pat],
        enums: &Enums,
    ) {
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
                let hidden = missing
                    .iter()
                    .take(3)
                    .find_map(|witness| self.hidden_enum(enums, column, &witness[0], module));
                let help = match hidden {
                    Some(name) => format!(
                        "`{name}` cannot be named in {}, so neither can its variants: cover them with `_`, which names no variant (ADR 0037, 0050, 0054)",
                        self.hir.module(module).describe()
                    ),
                    None => format!("add {}, or a `_` arm (ADR 0037)", and_list(&arms)),
                };
                Diagnostic::error(
                    "ty::non-exhaustive-match",
                    format!("non-exhaustive `match`: {listed} not covered"),
                )
                .label(scrutinee.span, format!("missing: {listed}"))
                .help(help)
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
            (
                Pat::Value(_, subs),
                PatternKind::Variant(_, _, Some(written_subs))
                | PatternKind::ContextVariant(_, _, _, Some(written_subs)),
            ) => {
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
        let def = self.q.enum_def(id);
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

    /// The name of the first enum, in `witness` at a position of type
    /// `column`, that is not reachable from `module`, whose variants cannot
    /// be written there (ADR 0050); `None` if there is none.
    fn hidden_enum(
        &self,
        enums: &Enums,
        column: Column,
        witness: &Pat,
        module: ModuleId,
    ) -> Option<String> {
        let (Pat::Value(value, subs), Column::Enum(e)) = (witness, column) else {
            return None;
        };
        let def = self.q.enum_def(e.id);
        if !def.reachable_from(module) {
            return Some(def.name.clone());
        }
        let variants = &enums[&e.id];
        let variant = variants
            .iter()
            .find(|variant| variant.value == *value)
            .expect("a value a `match` misses at an enum position is a variant");
        variant
            .payload
            .iter()
            .zip(subs)
            .find_map(|(&column, sub)| self.hidden_enum(enums, column, sub, module))
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
            // A variant of `Option` or `Result` is shown as `.V`
            // (ADR 0045).
            Column::Enum(e) => {
                let def = self.q.enum_def(e.id);
                let variants = &enums[&e.id];
                let index = variants
                    .iter()
                    .position(|variant| variant.value == *value)
                    .expect("a value a `match` misses at an enum position is a variant");
                let name = def.variant_path(index);
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
}

/// A variant pattern of `def`, naming its first variant, with `_` for
/// each of its payload values.
fn variant_pattern_example(def: &hir::EnumDef) -> String {
    match def.variants.first() {
        Some(variant) if variant.payload.is_empty() => def.variant_path(0),
        Some(variant) => format!(
            "{}({})",
            def.variant_path(0),
            vec!["_"; variant.payload.len()].join(", ")
        ),
        None => def.name.clone(),
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

/// `a`, `a and b`, or `a, b and c`.
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [only] => only.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}
