//! Lowering `match` arms and patterns (ADR 0037, 0040).

use std::collections::HashMap;

use crate::diag::Diagnostic;
use crate::hir::{Arm, ConstId, EnumId, Pattern, PatternId, PatternKind, UnsizedPosition};
use crate::syntax::ast;

use super::{Binding, Bound, Def, Later, Lookup, Lowerer, Named, plain_name, written};

impl Lowerer<'_> {
    /// An arm of a `match`, in a scope of its own: the variables its
    /// pattern binds exist only in its expression (ADR 0037, 0040).
    pub(super) fn arm(&mut self, arm: &ast::Arm) -> Arm {
        self.scopes.push(HashMap::new());
        let mut bound = Vec::new();
        let pattern = self.pattern(&arm.pattern, &[], &mut bound);
        // The variables exist in the arm's expression, not in the rest of
        // the pattern, whose names resolve as outside it.
        let scope = self
            .scopes
            .last_mut()
            .expect("an arm has a scope of its own");
        for bound in bound {
            scope.insert(bound.name, bound.local);
        }
        let body = self.expr(&arm.body);
        self.scopes.pop();
        Arm { pattern, body }
    }

    /// A pattern. Each variable it binds goes in `bound`, after those
    /// bound before it in the arm's pattern, to be visible in the arm's
    /// expression; one name bound twice is reported against the others
    /// (ADR 0040). The alternatives of an or-pattern bind one variable
    /// per name, which the first declares (ADR 0041); `later` are the
    /// or-patterns around the pattern that it is in a later alternative
    /// of, innermost last.
    fn pattern(
        &mut self,
        pattern: &ast::Pattern,
        later: &[Later],
        bound: &mut Vec<Bound>,
    ) -> Pattern {
        let kind = match &pattern.kind {
            ast::PatternKind::Wildcard => PatternKind::Wildcard,
            ast::PatternKind::Binding { mutable, name, ty } => {
                // A `let` the first alternative of an or-pattern around it
                // compared with the same variable was reported there.
                let reported = |index: usize| {
                    later
                        .iter()
                        .any(|or| index < or.before && or.first.iter().any(|v| v.name == name.name))
                };
                if let Some((_, first)) = bound
                    .iter()
                    .enumerate()
                    .find(|&(index, bound)| bound.name == name.name && !reported(index))
                {
                    self.diags.push(
                        Diagnostic::error(
                            "resolve::duplicate-binding",
                            format!("`{}` is bound twice in this pattern", name.name),
                        )
                        .label(first.name_span, "first bound here")
                        .label(name.span, "bound again here")
                        .help("each variable of a pattern has a name of its own (ADR 0040)"),
                    );
                }
                let local_ty = self.ty(ty, UnsizedPosition::Variable);
                // A later alternative's `let` is the variable the first
                // alternative declared, whose type type checking compares
                // with the one written here (ADR 0041).
                let shared = later
                    .iter()
                    .rev()
                    .find_map(|or| or.first.iter().find(|v| v.name == name.name));
                let local = match shared {
                    Some(shared) => shared.local,
                    None => self.new_local(name, local_ty, *mutable),
                };
                bound.push(Bound {
                    name: name.name.clone(),
                    local,
                    mutable: *mutable,
                    name_span: name.span,
                    let_span: pattern.span,
                    reported: false,
                });
                PatternKind::Binding {
                    local,
                    ty: local_ty,
                    ty_span: ty.span,
                }
            }
            // Rejected literals were reported by the lexer, and the pipeline
            // stops before HIR when there are syntax errors.
            ast::PatternKind::Int { value, negated } => PatternKind::Int {
                value: value.unwrap_or(0),
                negated: *negated,
            },
            ast::PatternKind::Byte(value) => PatternKind::Byte(value.unwrap_or(0)),
            ast::PatternKind::Bool(value) => PatternKind::Bool(*value),
            ast::PatternKind::Name(path) => self.pattern_path(path),
            ast::PatternKind::PathArgs(path, patterns) => {
                let found = self.pattern_variant(path);
                let patterns = patterns
                    .iter()
                    .map(|sub| self.pattern(sub, later, bound))
                    .collect();
                match found {
                    Some((id, index)) => PatternKind::Variant(id, index, Some(patterns)),
                    // Reported; lowering errors stop the pipeline.
                    None => PatternKind::Wildcard,
                }
            }
            // Its enum is the type of its position, known to type checking
            // (ADR 0044); what it binds, it binds as a variant pattern.
            ast::PatternKind::ContextVariant(variant, patterns) => {
                let id = PatternId(self.next_pattern);
                self.next_pattern += 1;
                let patterns = patterns.as_ref().map(|patterns| {
                    patterns
                        .iter()
                        .map(|sub| self.pattern(sub, later, bound))
                        .collect()
                });
                PatternKind::ContextVariant(id, variant.name.clone(), variant.span, patterns)
            }
            ast::PatternKind::Or(alternatives) => {
                PatternKind::Or(self.or_pattern(alternatives, later, bound))
            }
        };
        Pattern {
            kind,
            span: pattern.span,
        }
    }

    /// The alternatives of an or-pattern (ADR 0041). Each starts from the
    /// variables bound before the or-pattern; the first declares one
    /// variable per name it binds, and the others' `let`s of those names
    /// are those variables. A name that some alternatives do not bind, or
    /// bind with `let mut` while others bind it with `let`, is
    /// `resolve::inconsistent-binding`, once per name, unless an inner
    /// or-pattern reported it; either way it counts as bound by the
    /// or-pattern, which adds to `bound` each name that some alternative
    /// binds, as the first one binding it does.
    fn or_pattern(
        &mut self,
        alternatives: &[ast::Pattern],
        later: &[Later],
        bound: &mut Vec<Bound>,
    ) -> Vec<Pattern> {
        let before = bound.len();
        let mut lowered = Vec::with_capacity(alternatives.len());
        // The variables each alternative binds, in the order written.
        let mut per_alternative: Vec<Vec<Bound>> = Vec::with_capacity(alternatives.len());
        let mut first = Vec::new();
        for (i, alternative) in alternatives.iter().enumerate() {
            let mut alternative_bound = bound.clone();
            let pattern = if i == 0 {
                self.pattern(alternative, later, &mut alternative_bound)
            } else {
                let mut inner = later.to_vec();
                inner.push(Later {
                    before,
                    first: &first,
                });
                self.pattern(alternative, &inner, &mut alternative_bound)
            };
            lowered.push(pattern);
            let variables = alternative_bound.split_off(before);
            if i == 0 {
                first = variables.clone();
            }
            per_alternative.push(variables);
        }
        // Each name once, as the first alternative binding it binds it.
        let mut names: Vec<Bound> = Vec::new();
        for variable in per_alternative.iter().flatten() {
            if !names.iter().any(|named| named.name == variable.name) {
                names.push(variable.clone());
            }
        }
        for named in &mut names {
            let found: Vec<Option<&Bound>> = per_alternative
                .iter()
                .map(|variables| variables.iter().find(|v| v.name == named.name))
                .collect();
            if found.iter().flatten().any(|v| v.reported) {
                named.reported = true;
                continue;
            }
            let name = &named.name;
            if let Some(missing) = found.iter().position(Option::is_none) {
                let binding = found
                    .iter()
                    .position(Option::is_some)
                    .expect("some alternative binds each name");
                self.diags.push(
                    Diagnostic::error(
                        "resolve::inconsistent-binding",
                        format!("`{name}` is not bound by every alternative"),
                    )
                    .label(alternatives[binding].span, format!("binds `{name}`"))
                    .label(alternatives[missing].span, format!("does not bind `{name}`"))
                    .help(format!(
                        "the arm's expression has `{name}` whichever alternative matched, so every alternative binds it, with the same type (ADR 0041)"
                    )),
                );
                named.reported = true;
                continue;
            }
            let first = found[0].expect("every alternative binds the name");
            if let Some(other) = found.iter().flatten().find(|v| v.mutable != first.mutable) {
                let how = |v: &Bound| {
                    if v.mutable {
                        "bound with `let mut` here"
                    } else {
                        "bound with `let` here"
                    }
                };
                self.diags.push(
                    Diagnostic::error(
                        "resolve::inconsistent-binding",
                        format!("`{name}` is bound with `let mut` by only some alternatives"),
                    )
                    .label(first.let_span, how(first))
                    .label(other.let_span, how(other))
                    .help(format!(
                        "the alternatives bind one variable `{name}`, which is mutable or not; write `let mut` in every alternative or in none (ADR 0041)"
                    )),
                );
                named.reported = true;
            }
        }
        bound.extend(names);
        lowered
    }

    /// A path in a pattern without patterns of its own: a constant or a
    /// variant, resolved as in an expression (ADR 0037, 0049).
    fn pattern_path(&mut self, path: &ast::Path) -> PatternKind {
        if let Some(name) = plain_name(path) {
            return match self.pattern_const(name) {
                Some(id) => PatternKind::Const(id),
                // Reported; lowering errors stop the pipeline.
                None => PatternKind::Wildcard,
            };
        }
        match self.lookup_path(path) {
            Named::Variant(id, index) => return PatternKind::Variant(id, index, None),
            Named::Item(Def::Const(id)) => return PatternKind::Const(id),
            Named::Failed => {}
            Named::Item(Def::Module(_)) => self.misused_module(path, "a constant"),
            Named::Item(def) => {
                let diag = self.not_a_constant(path, def);
                self.diags.push(diag);
            }
            Named::Missing(module) => {
                let diag = Diagnostic::error(
                    "resolve::unknown-name",
                    format!("cannot find `{}`", written(path)),
                );
                let diag = self.missing(diag, path, module, "not found");
                self.diags.push(diag);
            }
        }
        // Reported; lowering errors stop the pipeline.
        PatternKind::Wildcard
    }

    /// The variant named by the path of `path(p, ...)`, a pattern with
    /// patterns of its own, which only a variant has (ADR 0040).
    fn pattern_variant(&mut self, path: &ast::Path) -> Option<(EnumId, u32)> {
        let diag = match self.lookup_path(path) {
            Named::Variant(id, index) => return Some((id, index)),
            Named::Failed => return None,
            Named::Item(Def::Module(_)) => {
                self.misused_module(path, "a variant");
                return None;
            }
            Named::Item(def) => Diagnostic::error(
                "resolve::unknown-variant",
                format!("`{}` is not a variant", written(path)),
            )
            .label(path.span, format!("{}, not a variant", def.what()))
            .help("patterns in parentheses match the values of a variant's payload, as in `E::V(p)` (ADR 0040)"),
            Named::Missing(module) => {
                let diag = Diagnostic::error(
                    "resolve::unknown-name",
                    format!("cannot find `{}`", written(path)),
                );
                self.missing(diag, path, module, "not found")
            }
        };
        self.diags.push(diag);
        None
    }

    /// `resolve::not-a-constant` for `path` in a pattern, which names
    /// `def` (ADR 0037).
    fn not_a_constant(&self, path: &ast::Path, def: Def) -> Diagnostic {
        let written = written(path);
        let diag = Diagnostic::error(
            "resolve::not-a-constant",
            format!("`{written}` is {}, not a constant", def.what()),
        )
        .label(path.span, "a name in a pattern is a constant");
        // To compare with anything else, the value is bound and tested.
        let compare = format!(
            "to compare with it, bind the value and test it: `let x: T => if x == {written} {{ ... }}` (ADR 0037)"
        );
        match def {
            Def::Static(_) => diag.help(format!(
                "a static is memory, read at run time (ADR 0030); {compare}"
            )),
            Def::Enum(id) => {
                // An empty enum was reported; any name shows the form.
                let variant = self.enum_decls[id.0 as usize]
                    .0
                    .variants
                    .first()
                    .map_or("Variant", |v| v.name.name.as_str());
                diag.help(format!(
                    "a variant is matched with its enum's name, as in `{written}::{variant}` (ADR 0035, 0037)"
                ))
            }
            _ => diag.help("a pattern matches a value: a literal, a constant or `_` (ADR 0037)"),
        }
    }

    /// A bare name in a pattern, which is always a constant (ADR 0037). It
    /// is looked up as in an expression, so a local variable shadows a
    /// constant of the same name. `core` names `core`'s root, as in a path
    /// (ADR 0052).
    fn pattern_const(&mut self, name: &ast::Ident) -> Option<ConstId> {
        let path = ast::Path {
            segments: vec![ast::Ident {
                name: name.name.clone(),
                span: name.span,
            }],
            span: name.span,
        };
        let binding = if name.name == "core" {
            Some(Binding::Def(Def::Module(self.core_root)))
        } else {
            self.namespaces[self.module.0 as usize]
                .get(&name.name)
                .map(|entry| entry.binding)
        };
        let def = match binding {
            Some(binding) => match self.binding_def(binding) {
                Some(def) => Lookup::Found(def),
                None => Lookup::Failed,
            },
            None => Lookup::Missing,
        };
        let diag = if let Some(local) = self.find_local(&name.name) {
            let declared = self.locals[local.0 as usize].span;
            let compare = format!(
                "to compare with it, bind the value and test it: `let x: T => if x == {} {{ ... }}` (ADR 0037)",
                name.name
            );
            let diag = Diagnostic::error(
                "resolve::not-a-constant",
                format!("`{}` is a variable, not a constant", name.name),
            )
            .label(name.span, "a name in a pattern is a constant")
            .label(declared, "the variable is declared here");
            if matches!(def, Lookup::Found(Def::Const(_))) {
                diag.help(format!(
                    "the variable shadows the constant `{}` here (ADR 0029); {compare}",
                    name.name
                ))
            } else {
                diag.help(compare)
            }
        } else {
            match def {
                Lookup::Found(Def::Const(id)) => return Some(id),
                Lookup::Found(Def::Module(_)) => {
                    self.misused_module(&path, "a constant");
                    return None;
                }
                Lookup::Found(def) => self.not_a_constant(&path, def),
                Lookup::Failed => return None,
                Lookup::Missing => {
                    let diag = Diagnostic::error(
                        "resolve::unknown-name",
                        format!("cannot find `{}` in this scope", name.name),
                    );
                    let mut diag = self.missing(diag, &path, self.module, "not found");
                    // A constant of a parent module, if there is one, is
                    // the other reading, given after this one.
                    let binding = format!(
                        "a name in a pattern is always a constant; to bind the value to a new variable, write `let {}: T` (ADR 0037)",
                        name.name
                    );
                    diag.help = Some(match diag.help.take() {
                        Some(parent) => format!("{binding}; {parent}"),
                        None => binding,
                    });
                    diag
                }
            }
        };
        self.diags.push(diag);
        None
    }
}
