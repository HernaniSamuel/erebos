//! Lowering written types.

use crate::diag::Diagnostic;
use crate::hir::{
    BuiltinEnum, IntTy, NeverInMemory, NeverPosition, PeripheralByValue, Ty, UnsizedPosition,
    UnsizedType, VolatilePeripheral,
};
use crate::span::Span;
use crate::syntax::ast;

use super::{Def, Lowerer, Named, plain_name, written};

impl Lowerer<'_> {
    /// A type that memory has: `!` there is `{error}`, recorded for the
    /// type checker (ADR 0034).
    pub(super) fn memory_ty(&mut self, ty: &ast::Ty, position: NeverPosition) -> Ty {
        if let ast::TyKind::Never = ty.kind {
            self.never_in_memory.push(NeverInMemory {
                span: ty.span,
                position,
            });
            return Ty::Error;
        }
        self.ty(ty, UnsizedPosition::Memory(position))
    }

    /// `[elem]` written where a size is needed, at `position`: `{error}`,
    /// recorded for the type checker unless its element type failed
    /// already, which was reported (ADR 0031, 0034).
    fn unsized_ty(&mut self, span: Span, elem: &ast::Ty, position: UnsizedPosition) -> Ty {
        let elem = self.memory_ty(elem, NeverPosition::SliceElement);
        if elem != Ty::Error {
            self.unsized_types.push(UnsizedType {
                span,
                ty: Ty::slice(elem),
                position,
            });
        }
        Ty::Error
    }

    /// A written type, at `position`, where `[T]` has no size. One
    /// containing `{error}` is `{error}` as a whole.
    pub(super) fn ty(&mut self, ty: &ast::Ty, position: UnsizedPosition) -> Ty {
        match &ty.kind {
            ast::TyKind::Never => Ty::Never,
            ast::TyKind::Unit => Ty::Unit,
            ast::TyKind::Slice(elem) => self.unsized_ty(ty.span, elem, position),
            // Behind a pointer, `[T]` is the pointee of a slice (ADR 0031).
            ast::TyKind::Ptr {
                mutable,
                volatile,
                pointee,
            } => {
                let pointee = match &pointee.kind {
                    ast::TyKind::Slice(elem) => {
                        match self.memory_ty(elem, NeverPosition::SliceElement) {
                            Ty::Error => Ty::Error,
                            elem => Ty::slice(elem),
                        }
                    }
                    _ => self.memory_ty(pointee, NeverPosition::Pointee),
                };
                match pointee {
                    Ty::Error => Ty::Error,
                    // Accesses through it are volatile already (ADR 0058):
                    // reported, and lowered as written without the word.
                    Ty::Peripheral(_) if *volatile => {
                        let lowered = Ty::ptr(*mutable, false, pointee);
                        self.volatile_peripherals.push(VolatilePeripheral {
                            span: ty.span,
                            ty: lowered,
                        });
                        lowered
                    }
                    pointee => Ty::ptr(*mutable, *volatile, pointee),
                }
            }
            ast::TyKind::Array { elem, len } => {
                let elem = self.memory_ty(elem, NeverPosition::Element);
                let len = self.array_len(len);
                match elem {
                    Ty::Error => Ty::Error,
                    elem => Ty::array_of(elem, len),
                }
            }
            ast::TyKind::Applied(path, args) => self.applied_ty(ty, path, args),
            ast::TyKind::Named(path) => match self.named_ty(path) {
                // Only behind a pointer, or as a register's type, which is
                // checked once types are computed (ADR 0058).
                peripheral @ Ty::Peripheral(_)
                    if !matches!(
                        position,
                        UnsizedPosition::Memory(NeverPosition::Pointee | NeverPosition::Register)
                    ) =>
                {
                    self.peripherals_by_value.push(PeripheralByValue {
                        span: ty.span,
                        ty: peripheral,
                        position,
                    });
                    Ty::Error
                }
                lowered => lowered,
            },
        }
    }

    /// A type written by its path, without type arguments; a peripheral
    /// type whatever the position, which the caller judges.
    pub(super) fn named_ty(&mut self, path: &ast::Path) -> Ty {
        if let Some(name) = plain_name(path) {
            if name.name == "bool" {
                return Ty::Bool;
            }
            if let Some(builtin) = BuiltinEnum::from_name(&name.name) {
                self.diags.push(
                    Diagnostic::error(
                        "resolve::type-argument-count",
                        format!(
                            "`{}` takes {}, written in parentheses",
                            name.name,
                            type_arguments(builtin.arity())
                        ),
                    )
                    .label(name.span, "written without them")
                    .help(format!(
                        "write the types it holds, as in `{}` (ADR 0045)",
                        builtin.written()
                    )),
                );
                return Ty::Error;
            }
            if let Some(int) = IntTy::from_name(&name.name) {
                return Ty::Int(int);
            }
        }
        match self.lookup_path(path) {
            Named::Item(Def::Struct(id)) => Ty::Struct(self.struct_tys[id.0 as usize]),
            Named::Item(Def::BitStruct(id)) => Ty::BitStruct(self.bit_struct_tys[id.0 as usize]),
            Named::Item(Def::Enum(id)) => Ty::Enum(self.enum_tys[id.0 as usize]),
            Named::Item(Def::PeripheralType(id)) => {
                Ty::Peripheral(self.peripheral_tys[id.0 as usize])
            }
            named => {
                self.not_a_type(path, named);
                Ty::Error
            }
        }
    }

    /// `name(args)`, written as `ty`: `Option(T)` or `Result(T, E)`, with
    /// as many type arguments as it takes, each a type of memory, as a
    /// payload type is (ADR 0045). Any other type takes none. One whose
    /// arguments are in error, reported, is `{error}` as a whole.
    pub(super) fn applied_ty(&mut self, ty: &ast::Ty, path: &ast::Path, args: &[ast::Ty]) -> Ty {
        let lowered: Vec<Ty> = args
            .iter()
            .map(|arg| self.memory_ty(arg, NeverPosition::TypeArgument))
            .collect();
        let builtin = plain_name(path).and_then(|name| BuiltinEnum::from_name(&name.name));
        if builtin.is_none() && !self.names_type(path) {
            return Ty::Error;
        }
        let name = written(path);
        let takes = builtin.map_or(0, BuiltinEnum::arity);
        let Some(builtin) = builtin.filter(|_| args.len() == takes) else {
            let given = match args.len() {
                0 => "none were given".to_owned(),
                1 => "1 was given".to_owned(),
                n => format!("{n} were given"),
            };
            let help = match builtin {
                Some(builtin) => format!("it is written `{}` (ADR 0045)", builtin.written()),
                None => {
                    "only `Option(T)` and `Result(T, E)` take type arguments (ADR 0045)".to_owned()
                }
            };
            // `u32()`: no arguments, but parentheses all the same.
            let message = if builtin.is_none() && args.is_empty() {
                format!("`{name}` takes no type arguments, and is written without parentheses")
            } else {
                format!("`{name}` takes {}, but {given}", type_arguments(takes))
            };
            self.diags.push(
                Diagnostic::error("resolve::type-argument-count", message)
                    .label(ty.span, "in this type")
                    .help(help),
            );
            return Ty::Error;
        };
        if lowered.contains(&Ty::Error) {
            return Ty::Error;
        }
        let applied = Ty::applied(builtin, lowered);
        self.applications.push((ty.span, applied));
        applied
    }

    /// Whether `path` names a type that takes no type arguments: `bool`,
    /// an integer type, a struct, a bit-field struct, an enum or a
    /// peripheral type. Anything
    /// else is reported.
    fn names_type(&mut self, path: &ast::Path) -> bool {
        if let Some(name) = plain_name(path)
            && (name.name == "bool" || IntTy::from_name(&name.name).is_some())
        {
            return true;
        }
        match self.lookup_path(path) {
            Named::Item(
                Def::Struct(_) | Def::BitStruct(_) | Def::Enum(_) | Def::PeripheralType(_),
            ) => true,
            named => {
                self.not_a_type(path, named);
                false
            }
        }
    }

    /// Reports `path`, which `named` shows not to be a type, unless that
    /// was reported.
    fn not_a_type(&mut self, path: &ast::Path, named: Named) {
        let diag = Diagnostic::error(
            "resolve::unknown-type",
            format!("unknown type `{}`", written(path)),
        );
        let diag = match named {
            Named::Failed => return,
            Named::Item(Def::Module(_)) => return self.misused_module(path, "a type"),
            Named::Missing(module) => self.missing(diag, path, module, "not a type"),
            Named::Item(def) => diag.label(path.span, format!("{}, not a type", def.what())),
            Named::Variant(..) => diag.label(path.span, "a variant, not a type"),
        };
        self.diags.push(diag);
    }
}

/// "1 type argument", or as many, or "no type arguments".
fn type_arguments(count: usize) -> String {
    match count {
        0 => "no type arguments".to_owned(),
        1 => "1 type argument".to_owned(),
        n => format!("{n} type arguments"),
    }
}
