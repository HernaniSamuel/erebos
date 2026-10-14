//! Sizes, alignments and field offsets of types (ADR 0022): the layout part
//! of the "const evaluation" layer of ADR 0003. The type checker uses it to
//! reject types too large for the address space, and MIR building to record
//! the layouts that code generation follows.

use crate::diag::{Diagnostic, Diagnostics};
use crate::hir::{Hir, StructId, Ty};
use crate::target;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// In bytes; always a multiple of `align`.
    pub size: u64,
    /// In bytes; a power of two.
    pub align: u64,
}

/// The largest object the target can hold: half of its address space, so
/// that the distance between two bytes of an object fits in `isize`.
pub const MAX_SIZE: u64 = (1 << (target::POINTER_BITS - 1)) - 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructLayout {
    pub layout: Layout,
    /// The offset of each field, in declaration order.
    pub offsets: Vec<u64>,
    /// Asks for more than its natural alignment, through `#[align(N)]` on
    /// it or on something it contains; a packed struct could not keep it.
    pub over_aligned: bool,
}

/// The largest alignment `#[align(N)]` accepts: the address space itself.
pub const MAX_ALIGN: u64 = 1 << (target::POINTER_BITS - 1);

/// The layout of every struct of a program. A struct has none when it
/// contains itself or is too large; both are reported.
#[derive(Debug)]
pub struct Layouts {
    structs: Vec<Option<StructLayout>>,
}

impl Layouts {
    /// Lays out every struct, reporting structs that contain themselves and
    /// structs too large for the address space.
    pub fn compute(hir: &Hir, diags: &mut Diagnostics) -> Layouts {
        let mut builder = Builder {
            hir,
            diags,
            state: vec![State::Pending; hir.structs.len()],
            stack: Vec::new(),
        };
        for id in 0..hir.structs.len() {
            builder.struct_(StructId(id as u32));
        }
        let structs = builder
            .state
            .into_iter()
            .map(|state| match state {
                State::Done(layout) => layout,
                State::Pending | State::InProgress => {
                    unreachable!("every struct is visited to the end")
                }
            })
            .collect();
        Layouts { structs }
    }

    pub fn struct_(&self, id: StructId) -> Option<&StructLayout> {
        self.structs[id.0 as usize].as_ref()
    }

    /// The layout of `ty`, or `None` if it is larger than [`MAX_SIZE`]. A
    /// struct without a layout, already reported, counts as empty.
    pub fn of(&self, ty: Ty) -> Option<Layout> {
        let empty = Layout { size: 0, align: 1 };
        of(ty, &mut |id| {
            Some(self.struct_(id).map_or(empty, |s| s.layout))
        })
    }
}

/// The layout of `ty`, given the layout of each struct.
fn of(ty: Ty, struct_layout: &mut impl FnMut(StructId) -> Option<Layout>) -> Option<Layout> {
    let layout = match ty {
        // `!` has no values; `{error}` was reported.
        Ty::Unit | Ty::Never | Ty::Error => Layout { size: 0, align: 1 },
        Ty::Bool => Layout { size: 1, align: 1 },
        Ty::Int(int) => Layout {
            size: int.bytes(),
            align: int.bytes(),
        },
        Ty::Ptr(_) => Layout {
            size: target::POINTER_BYTES,
            align: target::POINTER_BYTES,
        },
        Ty::Array(elem, len) => {
            let elem = of(*elem, struct_layout)?;
            Layout {
                size: elem.size.checked_mul(len)?,
                align: elem.align,
            }
        }
        Ty::Struct(s) => struct_layout(s.id)?,
    };
    (layout.size <= MAX_SIZE).then_some(layout)
}

/// Rounds `offset` up to a multiple of `align`, a power of two.
fn align_up(offset: u64, align: u64) -> Option<u64> {
    Some(offset.checked_add(align - 1)? & !(align - 1))
}

#[derive(Clone)]
enum State {
    Pending,
    InProgress,
    Done(Option<StructLayout>),
}

struct Builder<'a> {
    hir: &'a Hir,
    diags: &'a mut Diagnostics,
    state: Vec<State>,
    /// Structs being laid out, outermost first: a struct found here again
    /// contains itself.
    stack: Vec<StructId>,
}

impl Builder<'_> {
    fn struct_(&mut self, id: StructId) -> Option<Layout> {
        match &self.state[id.0 as usize] {
            State::Done(layout) => return layout.as_ref().map(|s| s.layout),
            State::InProgress => {
                self.report_cycle(id);
                return None;
            }
            State::Pending => {}
        }
        self.state[id.0 as usize] = State::InProgress;
        self.stack.push(id);

        // C rules (ADR 0022): fields in declaration order, each at the next
        // offset aligned for it; the size is rounded up to the alignment.
        // `#[packed]` drops the padding and the alignment.
        let hir = self.hir;
        let def = hir.struct_(id);
        let mut offsets = Vec::with_capacity(def.fields.len());
        let mut end = Some(0);
        let mut align = 1;
        let mut complete = true;
        let mut over_aligned = def.align.is_some();
        for field in &def.fields {
            let Some(field_layout) = of(field.ty, &mut |id| self.struct_(id)) else {
                complete = false;
                continue;
            };
            if self.over_aligned(field.ty) {
                over_aligned = true;
                if def.packed {
                    self.diags.push(
                        Diagnostic::error(
                            "ty::aligned-in-packed",
                            format!("a packed struct cannot contain `{}`", field.ty),
                        )
                        .label(field.span, "asks for an alignment through `#[align]`")
                        .label(def.name_span, "packed: alignment 1")
                        .help("the alignment it asks for could not be kept (ADR 0022)"),
                    );
                }
            }
            let field_align = if def.packed { 1 } else { field_layout.align };
            align = align.max(field_align);
            let offset = end.and_then(|end| align_up(end, field_align));
            offsets.push(offset.unwrap_or(0));
            end = offset.and_then(|offset| offset.checked_add(field_layout.size));
        }
        if let Some((wanted, span)) = def.align {
            let problem = if !wanted.is_power_of_two() {
                Some("an alignment is a power of two".to_owned())
            } else if wanted > MAX_ALIGN {
                Some(format!("alignments go up to {MAX_ALIGN}"))
            } else if wanted < align {
                Some(format!(
                    "`{}` already needs alignment {align}; `#[align]` only raises it",
                    def.name
                ))
            } else {
                None
            };
            match problem {
                Some(problem) => self.diags.push(
                    Diagnostic::error(
                        "ty::invalid-align",
                        format!("invalid alignment {wanted}"),
                    )
                    .label(span, problem)
                    .help("`#[align(N)]` takes a power of two not below the natural alignment (ADR 0022)"),
                ),
                None => align = wanted,
            }
        }
        let size = end.and_then(|end| align_up(end, align));

        self.stack.pop();
        let layout = match (complete, size) {
            (true, Some(size)) if size <= MAX_SIZE => Some(StructLayout {
                layout: Layout { size, align },
                offsets,
                over_aligned,
            }),
            (true, _) => {
                self.diags.push(
                    Diagnostic::error("ty::too-large", format!("`{}` is too large", def.name))
                        .label(def.name_span, "does not fit in the address space")
                        .help(format!(
                            "objects are limited to {MAX_SIZE} bytes on this target"
                        )),
                );
                None
            }
            // A field without a layout was reported where it was found.
            (false, _) => None,
        };
        self.state[id.0 as usize] = State::Done(layout.clone());
        layout.map(|s| s.layout)
    }

    /// Whether `ty` contains a struct with `#[align(N)]`.
    fn over_aligned(&self, ty: Ty) -> bool {
        match ty {
            Ty::Struct(s) => matches!(
                &self.state[s.id.0 as usize],
                State::Done(Some(layout)) if layout.over_aligned
            ),
            Ty::Array(elem, _) => self.over_aligned(*elem),
            _ => false,
        }
    }

    /// `id` was found while laying itself out: the structs on the stack
    /// from `id` onwards contain each other by value.
    fn report_cycle(&mut self, id: StructId) {
        let start = self
            .stack
            .iter()
            .position(|&s| s == id)
            .expect("a struct in progress is on the stack");
        let cycle = &self.stack[start..];
        let name = |id: &StructId| self.hir.struct_(*id).name.as_str();
        let first = self.hir.struct_(id);
        let message = if cycle.len() == 1 {
            format!("`{}` contains itself", first.name)
        } else {
            let mut path: Vec<&str> = cycle.iter().map(name).collect();
            path.push(&first.name);
            format!("these structs contain each other: {}", path.join(" -> "))
        };
        let mut diag = Diagnostic::error("ty::infinite-size", message)
            .label(first.name_span, "would have infinite size")
            .help(format!(
                "contain a pointer instead, such as `*mut {}` (ADR 0021)",
                first.name
            ));
        for other in &cycle[1..] {
            diag = diag.label(self.hir.struct_(*other).name_span, "part of the cycle");
        }
        self.diags.push(diag);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hir::IntTy;

    fn scalar(ty: Ty) -> Option<Layout> {
        of(ty, &mut |_| unreachable!("no structs"))
    }

    #[test]
    fn scalars_and_arrays() {
        let u64_ = Ty::Int(IntTy::U64);
        assert_eq!(scalar(u64_), Some(Layout { size: 8, align: 8 }));
        assert_eq!(scalar(Ty::Bool), Some(Layout { size: 1, align: 1 }));
        assert_eq!(
            scalar(Ty::array(u64_, 3)),
            Some(Layout { size: 24, align: 8 })
        );
        assert_eq!(
            scalar(Ty::array(u64_, 0)),
            Some(Layout { size: 0, align: 8 })
        );
        assert_eq!(scalar(Ty::array(Ty::Bool, MAX_SIZE + 1)), None);
        assert_eq!(scalar(Ty::array(Ty::array(u64_, u64::MAX), 2)), None);
    }

    #[test]
    fn align_up_rounds_to_powers_of_two() {
        assert_eq!(align_up(0, 4), Some(0));
        assert_eq!(align_up(1, 4), Some(4));
        assert_eq!(align_up(8, 8), Some(8));
        assert_eq!(align_up(u64::MAX, 2), None);
    }
}
