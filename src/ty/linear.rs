//! Which types are linear (ADR 0094, 0101).

use crate::hir::{EnumId, StructId, Ty};
use crate::query::Queries;

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
            Ty::Array(elem, _) | Ty::Slice(elem) => self.linear(*elem),
            Ty::Unit
            | Ty::Never
            | Ty::Bool
            | Ty::Int(_)
            | Ty::Ptr(_)
            | Ty::BitStruct(_)
            | Ty::Enum(_)
            | Ty::Peripheral(_)
            | Ty::Error => false,
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
}
