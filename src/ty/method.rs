//! Method calls (ADR 0114): `x.f(args)` calls the function `f` of the type of
//! `x`, with a receiver made from `x` as the first argument.

use crate::diag::Diagnostic;
use crate::hir::{self, AccessMode, Expr, Fn, FnId, ModuleId, ReceiverKind, Ty, Visibility};
use crate::span::Span;

use super::place::Unmovable;
use super::{Checker, PlaceInfo, is_copy};

/// The functions of a type, and the module they are visible from (ADR 0113):
/// where the type is declared, and whether a path written in the module of the
/// call can name the type.
struct TypeFns<'h> {
    fns: &'h [FnId],
    module: ModuleId,
    reachable: bool,
}

impl Checker<'_, '_> {
    /// The functions of the type `ty`, if it is one that can have any: a
    /// struct, a bit-field struct, an enum or a peripheral type (ADR 0113).
    /// `from` is the module the call is written in.
    fn type_fns(&self, ty: Ty, from: ModuleId) -> Option<TypeFns<'_>> {
        let hir = self.hir;
        Some(match ty {
            Ty::Struct(s) => {
                let def = hir.struct_(s.id);
                TypeFns {
                    fns: &def.fns,
                    module: def.module,
                    reachable: def.reachable_from(from),
                }
            }
            Ty::BitStruct(s) => {
                let def = hir.bit_struct(s.id);
                TypeFns {
                    fns: &def.fns,
                    module: def.module,
                    reachable: def.reachable_from(from),
                }
            }
            // An `Option(T)` or a `Result(T, E)` has none, and is reachable
            // everywhere.
            Ty::Enum(e) => {
                let def = self.q.enum_def(e.id);
                TypeFns {
                    fns: &def.fns,
                    module: def.module,
                    reachable: def.reachable_from(from),
                }
            }
            Ty::Peripheral(p) => {
                let def = hir.peripheral_type(p.id);
                TypeFns {
                    fns: &def.fns,
                    module: def.module,
                    reachable: def.reachable_from(from),
                }
            }
            _ => return None,
        })
    }

    /// `receiver.name(args)` (ADR 0114). The type is that of `receiver`, or
    /// of what it refers to when it is a reference, whose place is then
    /// `*receiver`; a raw pointer is never followed. The first argument is made
    /// from the receiver place `P` as `f`'s receiver says: `P` moved or copied,
    /// `&P` or `&mut P`, checked as those expressions are, so their codes are
    /// the ones reported. The other arguments are checked as a call's.
    pub(super) fn method_call(
        &mut self,
        f: &Fn,
        expr: &Expr,
        receiver: &Expr,
        name: &str,
        name_span: Span,
        args: &[Expr],
    ) -> Ty {
        let hir = self.hir;
        let (written, place) = self.base(f, receiver);
        let written = self.norm(written);
        let (place, through) = match written {
            Ty::Ref(r) => (self.dereferenced(receiver, r).1, true),
            _ => (place, false),
        };
        let ty = self.norm(place.ty);
        if matches!(ty, Ty::Error | Ty::Never) {
            self.unchecked_args(f, args);
            return ty;
        }
        let found = self
            .type_fns(ty, f.module)
            .and_then(|t| (t.fns.iter().copied()).find(|&id| hir.fn_(id).name == name));
        let Some(callee) = found else {
            self.unknown_method(receiver, written, ty, name, name_span, f.module);
            self.unchecked_args(f, args);
            return Ty::Error;
        };
        let target = hir.fn_(callee);
        let Some(kind) = target.owner.and_then(|owner| owner.receiver) else {
            self.not_a_method(target, name_span);
            self.unchecked_args(f, args);
            return Ty::Error;
        };
        if !self.fn_visible(target, ty, f.module) {
            self.private_method(target, ty, name_span, f.module);
            self.unchecked_args(f, args);
            return Ty::Error;
        }
        self.q.tables.methods.insert(expr.id, callee);
        self.receiver(f, receiver, &place, through, kind);
        self.call_args(f, expr, target, 1, args);
        self.norm(target.ret)
    }

    /// Checks the arguments of a call that has no function to check them
    /// against: against `{error}`, which keeps them quiet.
    fn unchecked_args(&mut self, f: &Fn, args: &[Expr]) {
        for arg in args {
            self.check(f, arg, Some(Ty::Error));
        }
    }

    /// Whether the function `target` of the type `ty` is visible in `from`
    /// (ADR 0050, 0113): a private one where a private item of the type's
    /// module would be, a `pub` one wherever the type is reachable.
    fn fn_visible(&self, target: &Fn, ty: Ty, from: ModuleId) -> bool {
        let Some(type_fns) = self.type_fns(ty, from) else {
            return true;
        };
        match target.vis {
            Visibility::Private => hir::is_within(&self.hir.modules, from, type_fns.module),
            Visibility::Pub => type_fns.reachable,
        }
    }

    /// The first argument of the call: the receiver place `place`, of the
    /// expression `receiver`, reached through the reference it is when
    /// `through`, made as `kind` says.
    fn receiver(
        &mut self,
        f: &Fn,
        receiver: &Expr,
        place: &PlaceInfo,
        through: bool,
        kind: ReceiverKind,
    ) {
        match kind {
            ReceiverKind::Value | ReceiverKind::MutValue => {
                // A peripheral type by value was reported at the parameter.
                if let Ty::Peripheral(_) = place.ty {
                    return;
                }
                // Read whole, as the place written there would be (ADR 0097).
                self.access(place, receiver, AccessMode::Read);
                if !is_copy(self.q, place.ty) {
                    let why = if through {
                        Some(Unmovable::Reference)
                    } else {
                        self.unmovable(receiver)
                    };
                    if let Some(why) = why {
                        self.report_unmovable(place.ty, receiver.span, why, None);
                    }
                }
            }
            ReceiverKind::Ref | ReceiverKind::RefMut => {
                let mutable = kind == ReceiverKind::RefMut;
                // Bits have no address, in a register too: that is reported
                // for the bit field by `borrow_place`.
                if place.bit_field.is_none()
                    && let Some(register) = place.register
                {
                    self.register_receiver(receiver, register, kind);
                    return;
                }
                let borrowed = self.borrow_place(f, mutable, receiver, place);
                if let Ty::Ref(r) = borrowed
                    && r.volatile
                {
                    self.volatile_receiver(receiver, r, kind);
                }
            }
        }
    }

    /// `ty::register-address` for a method with a `&self` or `&mut self`
    /// receiver called on a register, which keeps no address (ADR 0058, 0110).
    fn register_receiver(
        &mut self,
        receiver: &Expr,
        register: super::Register,
        kind: ReceiverKind,
    ) {
        let def = self.hir.peripheral_type(register.id);
        let reg = &def.entries[register.index];
        self.q.diags.push(
            Diagnostic::error(
                "ty::register-address",
                format!(
                    "cannot take the address of the register `{}` of `{}`",
                    reg.name, def.path
                ),
            )
            .label(
                receiver.span,
                format!("a register, but the method takes `{}`", kind.written()),
            )
            .help("a method that takes `&self` or `&mut self` is lent the address of what it is called on, and a register has none, as it must be reached with its access mode; a method that takes `self` by value reads the register once and works on the copy (ADR 0058, 0110, 0114)"),
        );
    }

    /// `ty::mismatched-types` for a method with a `&self` or `&mut self`
    /// receiver called on a volatile place: the reference made is volatile
    /// (ADR 0110), and no receiver is.
    fn volatile_receiver(&mut self, receiver: &Expr, found: hir::Ref, kind: ReceiverKind) {
        let expected = Ty::reference(found.mutable, false, *found.referent);
        self.q.diags.push(
            Diagnostic::error("ty::mismatched-types", "mismatched types")
                .label(
                    receiver.span,
                    format!(
                        "expected `{expected}`, found `{}`",
                        Ty::Ref(found)
                    ),
                )
                .help(format!(
                    "a method has no volatile receiver: `{}` is not a volatile reference, and a volatile one never becomes one that is not (ADR 0110, 0114); a method that takes `self` by value reads the place with one volatile access",
                    kind.written()
                )),
        );
    }

    /// `ty::unknown-method`: the type `ty` has no function `name`. `written`
    /// is the type of the receiver, `ty` or a reference to it.
    fn unknown_method(
        &mut self,
        receiver: &Expr,
        written: Ty,
        ty: Ty,
        name: &str,
        name_span: Span,
        from: ModuleId,
    ) {
        let hir = self.hir;
        let mut diag = Diagnostic::error(
            "ty::unknown-method",
            format!("`{ty}` has no method `{name}`"),
        )
        .label(name_span, "no such method");
        if written != ty {
            diag = diag.label(
                receiver.span,
                format!("`{ty}`, reached through this `{written}`"),
            );
        } else if let Ty::Ptr(_) = ty {
            diag = diag.label(receiver.span, format!("this is a `{ty}`"));
        }
        let length = name == "len"
            && match ty {
                Ty::Slice(_) | Ty::Array(..) => true,
                Ty::Ptr(ptr) => matches!(*ptr.pointee, Ty::Slice(_) | Ty::Array(..)),
                _ => false,
            };
        let methods: Vec<String> = match self.type_fns(ty, from) {
            Some(type_fns) => {
                let mut names: Vec<String> = Vec::new();
                for &id in type_fns.fns {
                    let target = hir.fn_(id);
                    let listed = format!("`{}`", target.name);
                    if target.owner.is_some_and(|o| o.receiver.is_some())
                        && !names.contains(&listed)
                    {
                        names.push(listed);
                    }
                }
                names
            }
            None => Vec::new(),
        };
        let help = if length {
            "the length of a slice or an array is the built-in `len(s)`, not a method (ADR 0031, 0108)".to_owned()
        } else if let Ty::Ptr(_) = ty {
            format!(
                "a raw pointer is never followed to a method, as its dereference is the one access the compiler does not check; write it: `(*p).{name}(...)` (ADR 0114)"
            )
        } else if self.has_field(ty, name) {
            format!(
                "`{name}` is a field of `{ty}`, not a function, and a field is not called; read it as `x.{name}` (ADR 0114)"
            )
        } else if self.type_fns(ty, from).is_none() {
            "only a struct, a bit-field struct, an enum or a peripheral type has functions, declared in an `impl` block (ADR 0113)".to_owned()
        } else if methods.is_empty() {
            format!(
                "`{ty}` has no methods: a method is a function of an `impl` block whose first parameter is `self`, `mut self`, `&self` or `&mut self` (ADR 0113)"
            )
        } else {
            format!("its methods are {}", methods.join(", "))
        };
        self.q.diags.push(diag.help(help));
    }

    /// Whether the type `ty` has a field, or a register, named `name`.
    fn has_field(&self, ty: Ty, name: &str) -> bool {
        let hir = self.hir;
        match ty {
            Ty::Struct(s) => hir.struct_(s.id).fields.iter().any(|f| f.name == name),
            Ty::BitStruct(s) => hir.bit_struct(s.id).fields.iter().any(|f| f.name == name),
            Ty::Peripheral(p) => hir
                .peripheral_type(p.id)
                .entries
                .iter()
                .any(|e| e.name == name),
            _ => false,
        }
    }

    /// `ty::not-a-method`: `target`, a function of a type, has no receiver.
    fn not_a_method(&mut self, target: &Fn, name_span: Span) {
        self.q.diags.push(
            Diagnostic::error(
                "ty::not-a-method",
                format!("`{}` is not a method", target.path),
            )
            .label(name_span, "called as a method")
            .label(target.name_span, "declared without a receiver")
            .help(format!(
                "a function without a receiver is called through its type: `{}(...)`; to make it a method, give it `self`, `&self` or `&mut self` as its first parameter (ADR 0113, 0114)",
                target.path
            )),
        );
    }

    /// `ty::private-method`: `target`, a function of the type `ty`, is not
    /// visible in the module `from` where it is called (ADR 0050, 0113).
    fn private_method(&mut self, target: &Fn, ty: Ty, name_span: Span, from: ModuleId) {
        let hir = self.hir;
        let diag = match target.vis {
            Visibility::Private => Diagnostic::error(
                "ty::private-method",
                format!("`{}` is private", target.path),
            ),
            Visibility::Pub => Diagnostic::error(
                "ty::private-method",
                format!("`{}` is not visible here", target.path),
            ),
        }
        .label(name_span, "not visible here");
        let diag = match target.vis {
            Visibility::Private => diag
                .label(target.name_span, "declared here without `pub`")
                .help(format!(
                    "a private function of a type is visible only in {}, which declares the type, and the modules inside it; declare it `pub` to call it elsewhere (ADR 0050, 0113)",
                    hir.module(target.module).describe()
                )),
            Visibility::Pub => diag
                .label(target.name_span, "declared `pub` here")
                .help(format!(
                    "a `pub` function of a type is visible wherever a path can name the type, and none written in {} can name `{ty}`; calling it on a value does not change that (ADR 0050, 0113)",
                    hir.module(from).describe()
                )),
        };
        self.q.diags.push(diag);
    }
}
