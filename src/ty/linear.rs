//! Which types are linear (ADR 0094, 0101), and where a linear value may not
//! be held: a static, an array or slice, a place reached through a pointer.

use crate::diag::Diagnostic;
use crate::hir::{self, Block, EnumId, Expr, ExprKind, Fn, Stmt, StructId, Ty};
use crate::query::Queries;

use super::Checker;
use super::place::Through;

/// Whether the values of `ty` are linear (ADR 0101): a struct declared
/// `#[linear]`, or a type that holds one by value, as a field of a struct,
/// a payload value of an enum (`Option(T)` and `Result(T, E)` included, as
/// written or normalized), or the element of an array or of a slice. A
/// pointer is not linear, whatever it points to. Computed from the parts,
/// unlike copy: nothing but `#[linear]` is declared.
///
/// A type whose layout is a cycle is `ty::cycle`, reported where the layout
/// is asked for; here a struct or an enum met again while its own parts are
/// looked into counts for nothing, so that the walk ends.
pub fn is_linear(q: &Queries, ty: Ty) -> bool {
    Walk {
        q,
        arrays: true,
        open: Vec::new(),
    }
    .linear(ty)
}

/// `is_linear`, but an array or a slice never makes a type linear: one with
/// linear elements is `ty::linear-array`, reported where it is written or
/// made, so the types that hold it are not reported again (ADR 0101). It
/// decides which arrays are in error, and which statics and writes through a
/// pointer: `[Transfer; 2]` is one error, not also a static of a linear type.
fn is_linear_outside_arrays(q: &Queries, ty: Ty) -> bool {
    Walk {
        q,
        arrays: false,
        open: Vec::new(),
    }
    .linear(ty)
}

/// A struct or an enum whose parts are being looked into.
#[derive(PartialEq)]
enum Open {
    Struct(StructId),
    Enum(EnumId),
}

struct Walk<'q, 'h> {
    q: &'q Queries<'h>,
    /// Whether an array or a slice of a linear type is linear.
    arrays: bool,
    open: Vec<Open>,
}

impl Walk<'_, '_> {
    fn linear(&mut self, ty: Ty) -> bool {
        match ty {
            Ty::Struct(s) => {
                let def = self.q.hir.struct_(s.id);
                if def.linear.is_some() {
                    return true;
                }
                let this = Open::Struct(s.id);
                if self.open.contains(&this) {
                    return false;
                }
                self.open.push(this);
                let found = def.fields.iter().any(|field| self.linear(field.ty));
                self.open.pop();
                found
            }
            Ty::Enum(e) if e.payloads => {
                let this = Open::Enum(e.id);
                if self.open.contains(&this) {
                    return false;
                }
                self.open.push(this);
                let found = (self.q.enum_def(e.id).variants.iter())
                    .flat_map(|variant| variant.payload.iter())
                    .any(|payload| self.linear(payload.ty));
                self.open.pop();
                found
            }
            Ty::Applied(applied) => applied.args.iter().any(|&arg| self.linear(arg)),
            Ty::Array(elem, _) | Ty::Slice(elem) => self.arrays && self.linear(*elem),
            Ty::Unit
            | Ty::Never
            | Ty::Bool
            | Ty::Int(_)
            | Ty::Ptr(_)
            | Ty::Ref(_)
            | Ty::BitStruct(_)
            | Ty::Enum(_)
            | Ty::Peripheral(_)
            | Ty::Error => false,
        }
    }
}

/// The element type of `ty`, if it is an array or a slice whose elements are
/// linear, apart from arrays (`is_linear_outside_arrays`): the types
/// `ty::linear-array` is about.
fn linear_elements(q: &Queries, ty: Ty) -> Option<Ty> {
    match ty {
        Ty::Array(elem, _) | Ty::Slice(elem) if is_linear_outside_arrays(q, *elem) => Some(*elem),
        _ => None,
    }
}

/// Whether `ty` is an array or a slice whose elements are linear, apart from
/// arrays: the type `ty::linear-array` rejects. A body whose types reach one
/// is not lowered to MIR, where rule L3 would not hold (ADR 0103).
pub fn has_linear_elements(q: &Queries, ty: Ty) -> bool {
    linear_elements(q, ty).is_some()
}

/// Appends to `out`, once each, the arrays and slices of linear elements in
/// `ty` and in the types it reaches: through arrays, fields, payload values
/// and pointees (ADR 0101). Types are normalized on the way.
fn arrays_reached(q: &mut Queries, ty: Ty, out: &mut Vec<Ty>) {
    Reach {
        q,
        open: Vec::new(),
        out,
    }
    .visit(ty);
}

struct Reach<'q, 'h, 'o> {
    q: &'q mut Queries<'h>,
    open: Vec<Open>,
    out: &'o mut Vec<Ty>,
}

impl Reach<'_, '_, '_> {
    fn visit(&mut self, ty: Ty) {
        let ty = self.q.normalize(ty);
        match ty {
            Ty::Array(elem, _) | Ty::Slice(elem) => {
                self.visit(*elem);
                if linear_elements(self.q, ty).is_some() && !self.out.contains(&ty) {
                    self.out.push(ty);
                }
            }
            Ty::Ptr(ptr) => self.visit(*ptr.pointee),
            Ty::Struct(s) => {
                let this = Open::Struct(s.id);
                if self.open.contains(&this) {
                    return;
                }
                self.open.push(this);
                let fields: Vec<Ty> = (self.q.hir.struct_(s.id).fields.iter())
                    .map(|field| field.ty)
                    .collect();
                for field in fields {
                    self.visit(field);
                }
                self.open.pop();
            }
            Ty::Enum(e) if e.payloads => {
                let this = Open::Enum(e.id);
                if self.open.contains(&this) {
                    return;
                }
                self.open.push(this);
                let payloads: Vec<Ty> = (self.q.enum_def(e.id).variants.iter())
                    .flat_map(|variant| variant.payload.iter())
                    .map(|payload| payload.ty)
                    .collect();
                for payload in payloads {
                    self.visit(payload);
                }
                self.open.pop();
            }
            _ => {}
        }
    }
}

/// `ty::linear-array` for `ty`, an array or a slice of `elem`, without
/// labels.
fn array_diag(ty: Ty, elem: Ty) -> Diagnostic {
    Diagnostic::error(
        "ty::linear-array",
        format!("`{ty}` has elements of the linear type `{elem}`"),
    )
}

/// What `ty::linear-array` suggests instead.
const ARRAY_HELP: &str = "no element of an array or a slice is ever moved out (ADR 0097, 0100), so none could be consumed, only forgotten; keep each linear value in a variable of its own, or in a field of a struct (ADR 0101)";

/// Reports, once per type, at the first place it is written, the arrays and the slices behind a
/// pointer whose elements are linear, which HIR lowering recorded
/// (ADR 0101). A type with such an array inside is reported at the array
/// only: `[[Transfer; 2]; 2]` once. The types are remembered, so that no
/// expression is reported for making one of them (`report_made_arrays`).
pub fn report_linear_arrays(q: &mut Queries) {
    let hir = q.hir;
    for &(span, written) in &hir.array_types {
        // Not normalized before it is known to be an error: evaluating the
        // lengths of every array written would change when, and where, a
        // length that cycles is reported.
        if linear_elements(q, written).is_none() {
            continue;
        }
        let ty = q.normalize(written);
        let Some(elem) = linear_elements(q, ty) else {
            continue;
        };
        // Once per type, at the first place it is written (ADR 0101).
        if q.linear_arrays.contains(&ty) {
            continue;
        }
        q.linear_arrays.push(ty);
        q.diags.push(
            array_diag(ty, elem)
                .label(span, format!("its elements are linear, of type `{elem}`"))
                .help(ARRAY_HELP),
        );
    }
}

/// Reports each static, `extern static` included, whose type is linear, at
/// its type (ADR 0101). An array of linear elements, alone or inside the
/// type, is `ty::linear-array`.
pub fn report_linear_statics(q: &mut Queries) {
    let hir = q.hir;
    for def in &hir.statics {
        if !is_linear_outside_arrays(q, def.ty) {
            continue;
        }
        let ty = q.normalize(def.ty);
        let what = if def.mutable { "static mut" } else { "static" };
        q.diags.push(
            Diagnostic::error(
                "ty::linear-static",
                format!("the {what} `{}` is of the linear type `{ty}`", def.path),
            )
            .label(def.ty_span, format!("`{ty}` is linear"))
            .help("the value a static holds is never moved out (ADR 0094), so it could only be overwritten, which forgets it; keep it in a variable and pass it on; how a context owns what a static holds is decided in M5.6 (ADR 0101)"),
        );
    }
}

impl Checker<'_, '_> {
    /// `end(operand)`, built in (ADR 0102): of type `()`. The operand is read
    /// whole, so it is moved as any value is (`ty::move-out-of-place` for a
    /// place it cannot be moved out of), and must be a value of a `#[linear]`
    /// struct (`ty::end-type`) whose fields are all visible in the module of
    /// `f`, as for a struct literal (`ty::end-private-field`, ADR 0050).
    pub(super) fn end(&mut self, f: &Fn, expr: &Expr, operand: &Expr) {
        let ty = self.check(f, operand, None);
        if matches!(ty, Ty::Never | Ty::Error) {
            return;
        }
        let hir = self.hir;
        let linear_struct = match ty {
            Ty::Struct(s) if hir.struct_(s.id).linear.is_some() => s,
            _ => {
                let help = if is_linear(self.q, ty) {
                    format!(
                        "`{ty}` holds a linear value but is not a `#[linear]` struct itself: take the value out of it, with `match` or by moving the field, and end that; only a `#[linear]` struct is ended (ADR 0102)"
                    )
                } else if let Ty::Struct(_) = ty {
                    format!(
                        "`{ty}` is not marked `#[linear]`, so its values need no ending: they can be let go; mark the struct `#[linear]` if each value must be consumed (ADR 0101, 0102)"
                    )
                } else {
                    "`end(e)` ends a value of a `#[linear]` struct, the only values that must be consumed (ADR 0101, 0102)".to_owned()
                };
                self.q.diags.push(
                    Diagnostic::error(
                        "ty::end-type",
                        format!("`end` takes a value of a `#[linear]` struct, found `{ty}`"),
                    )
                    .label(operand.span, format!("`{ty}` is not a `#[linear]` struct"))
                    .help(help),
                );
                return;
            }
        };
        let def = hir.struct_(linear_struct.id);
        let hidden = (def.fields.iter().enumerate())
            .find(|(index, _)| !hir.field_visible(linear_struct.id, *index, f.module));
        let Some((_, field)) = hidden else {
            return;
        };
        let mut diag = Diagnostic::error(
            "ty::end-private-field",
            format!(
                "cannot end a `{ty}` here: its field `{}` is not visible",
                field.name
            ),
        )
        .label(expr.span, "cannot end it here");
        diag = match field.vis {
            hir::Visibility::Private => diag
                .label(field.span, "declared here without `pub`")
                .help(format!(
                    "whoever may build a value may end it (ADR 0102), and a private field is visible only in {} and the modules inside it: end it there, with a function that takes the value, or declare the field `pub` (ADR 0050)",
                    hir.module(def.module).describe()
                )),
            hir::Visibility::Pub => diag
                .label(def.name_span, format!("`{ty}` is declared here"))
                .help(format!(
                    "a `pub` field is visible wherever a path can name its struct, and none written in {} can name `{ty}`; whoever may build a value may end it (ADR 0050, 0102)",
                    hir.module(f.module).describe()
                )),
        };
        self.q.diags.push(diag);
    }

    /// `ty::linear-through-pointer` for the assignment `expr`, if it moves a
    /// value of the linear type `value` into a place reached through a pointer
    /// or a reference (`*p = v`, `p[i] = v`, `(*p).f = v`, `*r = v`): the
    /// value could never be moved back out (ADR 0097, 0101, 0105). An array of
    /// linear elements was reported at its type, and does not count here.
    pub(super) fn check_linear_write(&mut self, expr: &Expr, through: Option<Through>, value: Ty) {
        let Some(through) = through else { return };
        if !is_linear_outside_arrays(self.q, value) {
            return;
        }
        let (through, the) = match through {
            Through::Pointer => ("a pointer", "the pointer"),
            Through::Reference => ("a reference", "the reference"),
        };
        self.q.diags.push(
            Diagnostic::error(
                "ty::linear-through-pointer",
                format!("cannot move a value of the linear type `{value}` into a place reached through {through}"),
            )
            .label(expr.span, format!("`{value}` is linear"))
            .help(format!("a value put there could never be moved out again (ADR 0097), so it would be forgotten; keep the value in a variable, or write its copy fields through {the} (ADR 0101)")),
        );
    }

    /// Reports the arrays and slices of linear elements that the type of an
    /// expression of `body` reaches and that nothing else wrote or made
    /// already: the expression is where the type arises, and is reported once
    /// per type (ADR 0101). The operands come first, so an expression whose
    /// type came from an operand is not reported, nor one whose type came
    /// from a declaration (a variable, a call): it was written, and reported
    /// at the type.
    pub(super) fn report_made_arrays(&mut self, body: &Block) {
        // An array of linear elements needs a linear struct.
        if !self.q.hir.structs.iter().any(|s| s.linear.is_some()) {
            return;
        }
        self.made_in_block(body);
    }

    fn made_in_block(&mut self, block: &Block) {
        for stmt in &block.stmts {
            match stmt {
                Stmt::Let { init, .. } => self.made(init),
                Stmt::Expr { expr, .. } => self.made(expr),
            }
        }
        if let Some(tail) = &block.tail {
            self.made(tail);
        }
    }

    fn made(&mut self, expr: &Expr) {
        let mut operands: Vec<&Expr> = Vec::new();
        let mut blocks: Vec<&Block> = Vec::new();
        operands_of(expr, &mut operands, &mut blocks);
        for operand in operands {
            self.made(operand);
        }
        for block in blocks {
            self.made_in_block(block);
        }
        let ty = self.q.tables.expr_ty(expr.id);
        let mut reached = Vec::new();
        arrays_reached(self.q, ty, &mut reached);
        for array in reached {
            // Every type reached has linear elements.
            let Some(elem) = linear_elements(self.q, array) else {
                continue;
            };
            if self.q.linear_arrays.contains(&array) {
                continue;
            }
            self.q.linear_arrays.push(array);
            self.q.diags.push(
                array_diag(array, elem)
                    .label(
                        expr.span,
                        format!("makes `{array}`, whose elements are linear"),
                    )
                    .help(ARRAY_HELP),
            );
        }
    }
}

/// The expressions directly inside `expr`, and its blocks, in source order.
fn operands_of<'a>(expr: &'a Expr, operands: &mut Vec<&'a Expr>, blocks: &mut Vec<&'a Block>) {
    match &expr.kind {
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
        | ExprKind::Peripheral(_)
        | ExprKind::Variant(_, _, None)
        | ExprKind::ContextVariant(_, _, None)
        | ExprKind::Local(_)
        | ExprKind::Continue { .. }
        | ExprKind::Break { value: None, .. }
        | ExprKind::Return(None) => {}
        ExprKind::Break { value: Some(e), .. }
        | ExprKind::Return(Some(e))
        | ExprKind::Repeat(e, _)
        | ExprKind::Field(e, _, _)
        | ExprKind::Assert(e, None, _)
        | ExprKind::Len(e)
        | ExprKind::End(e)
        | ExprKind::Comptime(e)
        | ExprKind::Try(e, _)
        | ExprKind::Unary(_, e)
        | ExprKind::Deref(e)
        | ExprKind::Cast(e, _, _)
        | ExprKind::AddrOf { place: e, .. }
        | ExprKind::Ref { place: e, .. } => operands.push(e),
        ExprKind::Call { args, .. }
        | ExprKind::Array(args)
        | ExprKind::Variant(_, _, Some(args))
        | ExprKind::ContextVariant(_, _, Some(args)) => operands.extend(args),
        ExprKind::MethodCall { receiver, args, .. } => {
            operands.push(receiver);
            operands.extend(args);
        }
        ExprKind::StructLit(_, fields) => operands.extend(fields.iter().map(|(_, value)| value)),
        ExprKind::BitStructLit(_, fields, _) => {
            operands.extend(fields.iter().map(|(_, _, value)| value));
        }
        ExprKind::Subslice { base, from, to, .. } => {
            operands.push(base);
            operands.extend(from.as_deref());
            operands.extend(to.as_deref());
        }
        ExprKind::Binary(_, a, b, _)
        | ExprKind::Index(a, b, _)
        | ExprKind::Slice(a, b)
        | ExprKind::Assert(a, Some(b), _)
        | ExprKind::Logical(_, a, b)
        | ExprKind::Assign(a, b)
        | ExprKind::CompoundAssign(_, a, b, _) => {
            operands.push(a);
            operands.push(b);
        }
        ExprKind::Block(b) | ExprKind::Critical(b) | ExprKind::Loop(b) => blocks.push(b),
        // Each expression inside an operand, however deep: the operands of
        // `asm` have no types of their own to look at.
        ExprKind::Asm(asm) => {
            for operand in &asm.operands {
                operand.walk(&mut |e| operands.push(e));
            }
        }
        ExprKind::If { cond, then, else_ } => {
            operands.push(cond);
            blocks.push(then);
            operands.extend(else_.as_deref());
        }
        ExprKind::While { cond, body } => {
            operands.push(cond);
            blocks.push(body);
        }
        ExprKind::Match {
            scrutinee, arms, ..
        } => {
            operands.push(scrutinee);
            operands.extend(arms.iter().map(|arm| &arm.body));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hir::{Applied, BuiltinEnum, Hir, IntTy, StructTy};
    use crate::target::DEFAULT;

    /// The program the tests ask about: `Transfer` is the marked struct.
    const SOURCE: &str = "\
#[linear]
pub struct Transfer { channel: u32 }
#[linear]
struct Token {}
struct Plain { a: u8 }
struct Holder { n: u8, t: Transfer }
struct Outer { inner: Holder }
enum Slot: u8 { Full(u8, Transfer) = 0, Empty = 1 }
enum Free: u8 { Value(u8) = 0, Nothing = 1 }
struct InSlot { slot: Slot }
struct Loop { again: Loop, n: u8 }
struct LoopHolding { again: LoopHolding, t: Transfer }
struct Ring { next: Link }
struct Link { back: Ring }
struct Aim { to: *const Transfer, many: *const [Transfer] }
struct Table { slots: [Transfer; 2], n: u8 }
struct Rows { grid: [[Transfer; 2]; 3] }
struct Chain { next: *const Chain, again: [Transfer; 4] }
enum Cell: u8 { Row([Transfer; 5]) = 0, Gap = 1 }
entry fn main() -> ! { loop {} }
panic fn on_panic() -> ! { loop {} }
";

    fn hir() -> Hir {
        crate::driver::hir_of(SOURCE).expect("the program is well formed")
    }

    fn struct_ty(hir: &Hir, name: &str) -> Ty {
        let (index, def) = (hir.structs.iter().enumerate())
            .find(|(_, def)| def.name == name)
            .unwrap_or_else(|| panic!("no struct {name}"));
        Ty::Struct(StructTy {
            id: StructId(index as u32),
            name: Box::leak(def.name.clone().into_boxed_str()),
        })
    }

    fn enum_ty(hir: &Hir, name: &str) -> Ty {
        let def = hir
            .enums
            .iter()
            .find(|def| def.name == name)
            .unwrap_or_else(|| panic!("no enum {name}"));
        Ty::Enum(def.ty)
    }

    fn applied(builtin: BuiltinEnum, args: &[Ty]) -> Ty {
        Ty::Applied(Applied {
            builtin,
            args: Box::leak(args.to_vec().into_boxed_slice()),
        })
    }

    #[test]
    fn a_marked_struct_is_linear_and_a_plain_one_is_not() {
        let hir = hir();
        let q = Queries::new(&hir, DEFAULT);
        assert!(is_linear(&q, struct_ty(&hir, "Transfer")));
        assert!(!is_linear(&q, struct_ty(&hir, "Plain")));
        assert!(!is_linear(&q, Ty::Int(IntTy::U32)));
        assert!(!is_linear(&q, Ty::Unit));
    }

    #[test]
    fn a_marked_struct_without_fields_is_linear() {
        let hir = hir();
        let q = Queries::new(&hir, DEFAULT);
        assert!(is_linear(&q, struct_ty(&hir, "Token")));
    }

    #[test]
    fn a_struct_or_an_enum_holding_a_linear_value_is_linear() {
        let hir = hir();
        let q = Queries::new(&hir, DEFAULT);
        assert!(is_linear(&q, struct_ty(&hir, "Holder")));
        // Through two structs.
        assert!(is_linear(&q, struct_ty(&hir, "Outer")));
        // An enum with a linear payload value, and a struct holding it.
        assert!(is_linear(&q, enum_ty(&hir, "Slot")));
        assert!(is_linear(&q, struct_ty(&hir, "InSlot")));
        // An enum without a linear payload value.
        assert!(!is_linear(&q, enum_ty(&hir, "Free")));
    }

    #[test]
    fn option_and_result_of_a_linear_type_are_linear() {
        let hir = hir();
        let mut q = Queries::new(&hir, DEFAULT);
        let transfer = struct_ty(&hir, "Transfer");
        let plain = struct_ty(&hir, "Plain");
        let u8_ty = Ty::Int(IntTy::U8);
        let cases = [
            (applied(BuiltinEnum::Option, &[transfer]), true),
            (applied(BuiltinEnum::Option, &[plain]), false),
            (applied(BuiltinEnum::Result, &[transfer, u8_ty]), true),
            (applied(BuiltinEnum::Result, &[u8_ty, transfer]), true),
            (applied(BuiltinEnum::Result, &[u8_ty, plain]), false),
            // Nested.
            (
                applied(
                    BuiltinEnum::Option,
                    &[applied(BuiltinEnum::Option, &[transfer])],
                ),
                true,
            ),
        ];
        for (ty, linear) in cases {
            // As written, and as the enum it is once normalized.
            assert_eq!(is_linear(&q, ty), linear, "{ty}");
            let normal = q.normalize(ty);
            assert_eq!(is_linear(&q, normal), linear, "{normal}");
        }
    }

    #[test]
    fn an_array_or_a_slice_of_a_linear_type_is_linear() {
        let hir = hir();
        let q = Queries::new(&hir, DEFAULT);
        let transfer = struct_ty(&hir, "Transfer");
        let plain = struct_ty(&hir, "Plain");
        assert!(is_linear(&q, Ty::array(transfer, 2)));
        assert!(is_linear(&q, Ty::array(Ty::array(transfer, 2), 2)));
        assert!(is_linear(&q, Ty::slice(transfer)));
        assert!(!is_linear(&q, Ty::array(plain, 2)));
        assert!(!is_linear(&q, Ty::slice(plain)));
    }

    #[test]
    fn a_pointer_to_a_linear_type_is_not_linear() {
        let hir = hir();
        let q = Queries::new(&hir, DEFAULT);
        let transfer = struct_ty(&hir, "Transfer");
        assert!(!is_linear(&q, Ty::ptr(false, false, transfer)));
        assert!(!is_linear(&q, Ty::ptr(true, true, transfer)));
        assert!(!is_linear(
            &q,
            Ty::ptr(false, false, Ty::array(transfer, 4))
        ));
        assert!(!is_linear(&q, Ty::ptr(false, false, Ty::slice(transfer))));
        assert!(!is_linear(&q, struct_ty(&hir, "Aim")));
    }

    #[test]
    fn a_cycle_of_containment_ends() {
        let hir = hir();
        let q = Queries::new(&hir, DEFAULT);
        assert!(!is_linear(&q, struct_ty(&hir, "Loop")));
        assert!(is_linear(&q, struct_ty(&hir, "LoopHolding")));
        assert!(!is_linear(&q, struct_ty(&hir, "Ring")));
        assert!(!is_linear(&q, struct_ty(&hir, "Link")));
    }

    #[test]
    fn an_array_of_linear_elements_does_not_make_a_type_linear_outside_arrays() {
        let hir = hir();
        let q = Queries::new(&hir, DEFAULT);
        let transfer = struct_ty(&hir, "Transfer");
        // As `is_linear` counts them, except for arrays and slices.
        assert!(is_linear_outside_arrays(&q, transfer));
        assert!(is_linear_outside_arrays(&q, struct_ty(&hir, "Holder")));
        assert!(is_linear_outside_arrays(
            &q,
            applied(BuiltinEnum::Option, &[transfer])
        ));
        assert!(!is_linear_outside_arrays(&q, Ty::array(transfer, 2)));
        assert!(!is_linear_outside_arrays(&q, Ty::slice(transfer)));
        assert!(!is_linear_outside_arrays(&q, struct_ty(&hir, "Table")));
        assert!(!is_linear_outside_arrays(&q, enum_ty(&hir, "Cell")));
        assert!(!is_linear_outside_arrays(&q, struct_ty(&hir, "Plain")));
    }

    #[test]
    fn the_elements_of_an_array_are_linear_without_counting_arrays() {
        let hir = hir();
        let q = Queries::new(&hir, DEFAULT);
        let transfer = struct_ty(&hir, "Transfer");
        let holder = struct_ty(&hir, "Holder");
        assert_eq!(linear_elements(&q, Ty::array(transfer, 2)), Some(transfer));
        assert_eq!(linear_elements(&q, Ty::slice(holder)), Some(holder));
        // The inner array is the one in error, not the outer one.
        assert_eq!(
            linear_elements(&q, Ty::array(Ty::array(transfer, 2), 2)),
            None
        );
        assert_eq!(
            linear_elements(&q, Ty::array(struct_ty(&hir, "Plain"), 2)),
            None
        );
        assert_eq!(linear_elements(&q, transfer), None);
    }

    fn reached(q: &mut Queries, ty: Ty) -> Vec<Ty> {
        let mut out = Vec::new();
        arrays_reached(q, ty, &mut out);
        out
    }

    #[test]
    fn the_arrays_a_type_reaches_are_found_through_fields_payloads_and_pointees() {
        let hir = hir();
        let mut q = Queries::new(&hir, DEFAULT);
        let transfer = struct_ty(&hir, "Transfer");
        let two = Ty::array(transfer, 2);
        assert_eq!(reached(&mut q, two), [two]);
        assert_eq!(reached(&mut q, Ty::ptr(false, false, two)), [two]);
        assert_eq!(reached(&mut q, Ty::slice(transfer)), [Ty::slice(transfer)]);
        // A field, and a payload value.
        assert_eq!(reached(&mut q, struct_ty(&hir, "Table")), [two]);
        assert_eq!(
            reached(&mut q, enum_ty(&hir, "Cell")),
            [Ty::array(transfer, 5)]
        );
        // `Option` of one, as written and normalized.
        let option = applied(BuiltinEnum::Option, &[two]);
        assert_eq!(reached(&mut q, option), [two]);
        // Only the innermost array of a nested one.
        assert_eq!(reached(&mut q, struct_ty(&hir, "Rows")), [two]);
        // Types that reach none.
        assert!(reached(&mut q, transfer).is_empty());
        assert!(reached(&mut q, Ty::array(Ty::Int(IntTy::U8), 2)).is_empty());
        assert!(reached(&mut q, struct_ty(&hir, "Holder")).is_empty());
    }

    #[test]
    fn the_walk_through_pointers_to_the_struct_itself_ends() {
        let hir = hir();
        let mut q = Queries::new(&hir, DEFAULT);
        let transfer = struct_ty(&hir, "Transfer");
        let found = reached(&mut q, struct_ty(&hir, "Chain"));
        assert_eq!(found, [Ty::array(transfer, 4)]);
    }
}
