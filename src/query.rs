//! On-demand compilation (ADR 0028). Type checking, layouts and MIR are
//! computed per item when first needed, remembered, and protected against
//! cycles; the driver then asks for every item, so the whole program is
//! still compiled.

use crate::diag::{Diagnostic, Diagnostics};
use crate::hir::{self, FnId, Hir, StructId};
use crate::layout::{self, Layout, MAX_ALIGN, MAX_SIZE, StructLayout};
use crate::mir::{self, BodyId, Field, Mir, Ptr, StructRef};
use crate::ty::{self, Tables};

/// The progress of one item through a query.
enum State<T> {
    NotStarted,
    InProgress,
    Done(T),
}

pub struct Queries<'h> {
    pub hir: &'h Hir,
    pub diags: Diagnostics,
    /// What type checking found out about expressions so far.
    pub tables: Tables,
    /// Type checking of each function.
    checked: Vec<State<()>>,
    /// The layout of each struct; `None` when it contains itself or is too
    /// large, both reported.
    layouts: Vec<State<Option<StructLayout>>>,
    /// Structs being laid out, outermost first: one found here again
    /// contains itself.
    layout_stack: Vec<StructId>,
    /// The MIR type of each struct.
    mir_structs: Vec<Option<StructRef>>,
    /// The MIR of each function.
    bodies: Vec<Option<mir::Body>>,
}

impl<'h> Queries<'h> {
    pub fn new(hir: &'h Hir) -> Queries<'h> {
        Queries {
            hir,
            diags: Diagnostics::default(),
            tables: Tables::new(hir.expr_count),
            checked: hir.fns.iter().map(|_| State::NotStarted).collect(),
            layouts: hir.structs.iter().map(|_| State::NotStarted).collect(),
            layout_stack: Vec::new(),
            mir_structs: vec![None; hir.structs.len()],
            bodies: hir.fns.iter().map(|_| None).collect(),
        }
    }

    /// Checks every item, in source order, whether or not something else
    /// needed it.
    pub fn check_all(&mut self) {
        for id in 0..self.hir.structs.len() {
            self.struct_layout(StructId(id as u32));
        }
        for id in 0..self.hir.fns.len() {
            self.typeck_fn(FnId(id as u32));
        }
    }

    pub fn typeck_fn(&mut self, id: FnId) {
        let state = &mut self.checked[id.0 as usize];
        if !matches!(state, State::NotStarted) {
            return;
        }
        *state = State::InProgress;
        ty::check_fn(self, self.hir.fn_(id));
        self.checked[id.0 as usize] = State::Done(());
    }

    /// The layout of `ty`, or `None` if it is larger than the target can
    /// hold. A struct without a layout, already reported, counts as empty.
    pub fn layout_of(&mut self, ty: hir::Ty) -> Option<Layout> {
        let empty = Layout { size: 0, align: 1 };
        layout::of(ty, &mut |id| {
            Some(self.struct_layout(id).map_or(empty, |s| s.layout))
        })
    }

    /// Lays out a struct with C rules (ADR 0022), reporting structs that
    /// contain themselves, too large ones, and misused attributes.
    pub fn struct_layout(&mut self, id: StructId) -> Option<StructLayout> {
        match &self.layouts[id.0 as usize] {
            State::Done(layout) => return layout.clone(),
            State::InProgress => {
                self.report_containment_cycle(id);
                return None;
            }
            State::NotStarted => {}
        }
        self.layouts[id.0 as usize] = State::InProgress;
        self.layout_stack.push(id);

        // Fields in declaration order, each at the next offset aligned for
        // it; the size is rounded up to the alignment. `#[packed]` drops
        // the padding and the alignment.
        let hir = self.hir;
        let def = hir.struct_(id);
        let mut offsets = Vec::with_capacity(def.fields.len());
        let mut end = Some(0);
        let mut align = 1;
        let mut complete = true;
        let mut over_aligned = def.align.is_some();
        for field in &def.fields {
            let field_layout =
                layout::of(field.ty, &mut |id| self.struct_layout(id).map(|s| s.layout));
            let Some(field_layout) = field_layout else {
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
            let offset = end.and_then(|end| layout::align_up(end, field_align));
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
                    Diagnostic::error("ty::invalid-align", format!("invalid alignment {wanted}"))
                        .label(span, problem)
                        .help("`#[align(N)]` takes a power of two not below the natural alignment (ADR 0022)"),
                ),
                None => align = wanted,
            }
        }
        let size = end.and_then(|end| layout::align_up(end, align));

        self.layout_stack.pop();
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
        self.layouts[id.0 as usize] = State::Done(layout.clone());
        layout
    }

    /// Whether `ty` contains a struct with `#[align(N)]`, among the structs
    /// already laid out.
    fn over_aligned(&self, ty: hir::Ty) -> bool {
        match ty {
            hir::Ty::Struct(s) => matches!(
                &self.layouts[s.id.0 as usize],
                State::Done(Some(layout)) if layout.over_aligned
            ),
            hir::Ty::Array(elem, _) => self.over_aligned(*elem),
            _ => false,
        }
    }

    /// `id` was found while laying itself out: the structs on the stack
    /// from `id` onwards contain each other by value.
    fn report_containment_cycle(&mut self, id: StructId) {
        let start = self
            .layout_stack
            .iter()
            .position(|&s| s == id)
            .expect("a struct in progress is on the stack");
        let hir = self.hir;
        let cycle = &self.layout_stack[start..];
        let first = hir.struct_(id);
        let message = if cycle.len() == 1 {
            format!("`{}` contains itself", first.name)
        } else {
            let mut path: Vec<&str> = cycle
                .iter()
                .map(|s| hir.struct_(*s).name.as_str())
                .collect();
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
            diag = diag.label(hir.struct_(*other).name_span, "part of the cycle");
        }
        self.diags.push(diag);
    }

    /// The MIR type of a struct, with its layout. Its fields are set after
    /// it exists, since they may point back to it.
    pub fn mir_struct(&mut self, id: StructId) -> StructRef {
        if let Some(s) = self.mir_structs[id.0 as usize] {
            return s;
        }
        let hir = self.hir;
        let def = hir.struct_(id);
        let layout = self
            .struct_layout(id)
            .expect("MIR is only built for well-typed programs");
        let s = StructRef::new(def.name.clone(), layout.layout, def.packed);
        self.mir_structs[id.0 as usize] = Some(s);
        let fields = def
            .fields
            .iter()
            .zip(&layout.offsets)
            .map(|(field, &offset)| Field {
                name: field.name.clone(),
                ty: self.mir_ty(field.ty),
                offset,
            })
            .collect();
        s.def().set_fields(fields);
        s
    }

    pub fn mir_ty(&mut self, ty: hir::Ty) -> mir::Ty {
        match ty {
            hir::Ty::Unit => mir::Ty::Unit,
            hir::Ty::Never => mir::Ty::Never,
            hir::Ty::Bool => mir::Ty::Bool,
            hir::Ty::Int(int) => mir::Ty::Int(int),
            hir::Ty::Ptr(ptr) => mir::Ty::Ptr(Ptr {
                mutable: ptr.mutable,
                volatile: ptr.volatile,
                // Leaked like HIR pointee types; see `hir::Ty::ptr`.
                pointee: Box::leak(Box::new(self.mir_ty(*ptr.pointee))),
            }),
            hir::Ty::Array(elem, len) => {
                mir::Ty::Array(Box::leak(Box::new(self.mir_ty(*elem))), len)
            }
            hir::Ty::Struct(s) => mir::Ty::Struct(self.mir_struct(s.id)),
            hir::Ty::Error => unreachable!("MIR is only built for well-typed programs"),
        }
    }

    /// The MIR of a function, built from its checked HIR.
    pub fn mir_body(&mut self, id: FnId) -> &mir::Body {
        if self.bodies[id.0 as usize].is_none() {
            self.typeck_fn(id);
            let body = mir::build_body(self, self.hir.fn_(id));
            self.bodies[id.0 as usize] = Some(body);
        }
        self.bodies[id.0 as usize]
            .as_ref()
            .expect("built just above")
    }

    /// The MIR of the whole program, in source order. The program must have
    /// passed type checking without errors.
    pub fn into_mir(mut self) -> Mir {
        let structs = (0..self.hir.structs.len())
            .map(|id| self.mir_struct(StructId(id as u32)))
            .collect();
        for id in 0..self.hir.fns.len() {
            self.mir_body(FnId(id as u32));
        }
        Mir {
            structs,
            bodies: self
                .bodies
                .into_iter()
                .map(|body| body.expect("every body was built"))
                .collect(),
            entry: BodyId(self.hir.entry.0),
            panic: BodyId(self.hir.panic.0),
        }
    }
}
