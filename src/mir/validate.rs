//! Validation of MIR (ADR 0095): the rules every consumer of MIR relies on,
//! checked where MIR is made. A violation is a bug of MIR building, never of
//! the program, so a run that finds one panics with the whole list.
//!
//! The checks read MIR only, compute their own types and never call what
//! panics on bad input (`Body::place_ty`, `Body::params`). A rule whose input
//! another rule found invalid is skipped for that input, so that one fault is
//! one violation. The `match`es on `Statement`, `Terminator`, `Rvalue`,
//! `Operand`, `PlaceElem` and `AsmOperandKind` name every variant and have
//! no `_` arm, so that a new variant fails to compile here. The rules M1 to M4
//! of ADR 0098 come after the rules of types, and L1 to L4 of ADR 0103 after
//! them. R1 of ADR 0107 is checked through the types of locals and statics,
//! R2 and R3 where the places they concern are: those written, and those a
//! `Ref` borrows. R4 of ADR 0112 is checked once per body, over its statements.

use std::collections::HashSet;
use std::fmt;
use std::ptr;

use super::{
    AsmOperandKind, AsmPiece, AsmSym, BasicBlock, BinOp, Body, BodyId, Const, ConstData, DataId,
    InlineAsm, IntTy, Local, Mir, Operand, PanicCause, PanicInfoAt, PanicSite, PeripheralDef,
    PeripheralRef, PeripheralTypeRef, Place, PlaceElem, Ptr, Relocation, Rvalue, Statement,
    StaticRef, Terminator, Ty, UnOp, VectorWord,
};
use crate::target;

/// Where in a body a rule is broken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Location {
    /// Not in a body but in what the violation's `body` names: a static,
    /// read-only data, a handler or the program as a whole (P2, P6, P7).
    Program,
    /// The body as a whole: its locals, its list of blocks.
    Body,
    /// A block as a whole: its spans (S4), and the statements of `bb0` of a
    /// naked body (S3).
    Block(BasicBlock),
    /// The statement at an index of a block.
    Statement(BasicBlock, usize),
    /// The terminator of a block.
    Terminator(BasicBlock),
}

impl Location {
    /// The order of ADR 0095: by block, then by statement, the terminator
    /// last; what concerns the body or a whole block comes before them.
    fn key(self) -> (u32, usize) {
        match self {
            Location::Program | Location::Body => (0, 0),
            Location::Block(block) => (block.0 + 1, 0),
            Location::Statement(block, index) => (block.0 + 1, index + 1),
            Location::Terminator(block) => (block.0 + 1, usize::MAX),
        }
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Location::Program => f.write_str("program"),
            Location::Body => f.write_str("body"),
            Location::Block(block) => write!(f, "{block}"),
            Location::Statement(block, index) => write!(f, "{block}, statement {index}"),
            Location::Terminator(block) => write!(f, "{block}, terminator"),
        }
    }
}

/// A rule of ADR 0095 that a body breaks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    /// The name of the body.
    pub body: String,
    pub at: Location,
    /// The rule's id in the ADR, such as `S5`.
    pub rule: &'static str,
    /// What is wrong, briefly.
    pub text: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // What a program-level violation concerns is already described,
        // as in "static `x`".
        if self.at == Location::Program {
            return write!(f, "{}: {}: {}", self.body, self.rule, self.text);
        }
        write!(
            f,
            "`{}`, {}: {}: {}",
            self.body, self.at, self.rule, self.text
        )
    }
}

#[cfg(test)]
thread_local! {
    /// How many bodies `check_body` has looked at on this thread, for the
    /// tests that show the validator runs where ADR 0095 says.
    static BODIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// How many programs `check_program` has looked at on this thread.
    static PROGRAMS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many bodies `check_body` has looked at on this thread.
#[cfg(test)]
pub fn bodies_checked() -> usize {
    BODIES.with(std::cell::Cell::get)
}

/// How many programs `check_program` has looked at on this thread.
#[cfg(test)]
pub fn programs_checked() -> usize {
    PROGRAMS.with(std::cell::Cell::get)
}

/// Panics with every violation, if there is one. The only panic of the
/// validator (ADR 0095, "How it fails").
pub fn assert_valid(violations: &[Violation]) {
    if violations.is_empty() {
        return;
    }
    panic!("{}", report(violations));
}

/// The message of the panic.
fn report(violations: &[Violation]) -> String {
    let s = if violations.len() == 1 { "" } else { "s" };
    let mut text = format!(
        "invalid MIR, a bug of the compiler (ADR 0095): {} violation{s}",
        violations.len()
    );
    for violation in violations {
        text += &format!(
            "
  {violation}"
        );
    }
    text
}

/// Checks the rules of one body at the end of building it, and panics with
/// the list of violations if it breaks any.
pub fn assert_valid_body(body: &Body) {
    assert_valid(&check_body(body));
}

/// Checks the rules of the program, and panics with the list of violations
/// if it breaks any (ADR 0095, points 2 and 3).
pub fn assert_valid_program(mir: &Mir) {
    assert_valid(&check_program(mir));
}

/// Every violation of the rules of one body, in the order of ADR 0095. The
/// rules that need the rest of the program (P2 to P4 and P8) are the
/// program's: see [`check_program`].
pub fn check_body(body: &Body) -> Vec<Violation> {
    sorted(Checker::run(body, None))
}

/// The fixed order of ADR 0095 for the violations of one body: by block,
/// statement (the terminator last) and rule id. The sort is stable.
fn sorted(mut found: Vec<Violation>) -> Vec<Violation> {
    found.sort_by_key(|violation| (violation.at.key(), violation.rule));
    found
}

/// Every violation of the rules of the program (ADR 0095): those of what
/// concerns the program as a whole, a static or read-only data first, by rule
/// id, then those of each body in the order of `Mir::bodies`, in the order of
/// [`check_body`]. P1 is `check_body` on every body, which also checks, now
/// that the other bodies are known, what a body names (P2), its calls (P3,
/// P4) and its panics (P8).
pub fn check_program(mir: &Mir) -> Vec<Violation> {
    #[cfg(test)]
    PROGRAMS.with(|count| count.set(count.get() + 1));
    let program = Program {
        mir,
        peripherals: mir
            .peripherals
            .iter()
            .map(|&p| ptr::from_ref(p.def()))
            .collect(),
    };
    let mut rules = Rules {
        mir,
        whole: Vec::new(),
        bodies: mir
            .bodies
            .iter()
            .map(|body| Checker::run(body, Some(&program)))
            .collect(),
    };
    rules.roles();
    rules.vector_table();
    rules.statics();
    rules.data();
    let mut found = rules.whole;
    found.sort_by_key(|violation| violation.rule);
    found.extend(rules.bodies.into_iter().flat_map(sorted));
    found
}

/// What the rules of the program that look inside a body need to know of the
/// rest of it.
struct Program<'a> {
    mir: &'a Mir,
    /// The peripherals of `Mir::peripherals`, by the address of their leaked
    /// definition.
    peripherals: HashSet<*const PeripheralDef>,
}

struct Checker<'a> {
    body: &'a Body,
    /// The program the body is in, for the rules that look outside it (P2 to
    /// P4, P8); `None` when only the body is known.
    program: Option<&'a Program<'a>>,
    at: Location,
    found: Vec<Violation>,
}

impl<'a> Checker<'a> {
    /// Every violation of the rules of one body, in the order found.
    fn run(body: &'a Body, program: Option<&'a Program<'a>>) -> Vec<Violation> {
        #[cfg(test)]
        BODIES.with(|count| count.set(count.get() + 1));
        let mut checker = Checker {
            body,
            program,
            at: Location::Body,
            found: Vec::new(),
        };
        checker.shape();
        checker.data_of_locals();
        for (block, data) in body.blocks() {
            checker.at = Location::Block(block);
            if data.spans.len() != data.statements.len() {
                checker.violation(
                    "S4",
                    format!(
                        "{} statements but {} spans",
                        data.statements.len(),
                        data.spans.len()
                    ),
                );
            }
            for (index, statement) in data.statements.iter().enumerate() {
                checker.at = Location::Statement(block, index);
                checker.statement(statement);
            }
            checker.at = Location::Terminator(block);
            checker.terminator(&data.terminator);
        }
        checker.critical();
        checker.at = Location::Body;
        checker.copy_kinds();
        checker.linear_kinds();
        checker.reference_positions();
        checker.address_temporaries();
        checker.found
    }

    fn violation(&mut self, rule: &'static str, text: String) {
        self.found.push(Violation {
            body: self.body.name.clone(),
            at: self.at,
            rule,
            text,
        });
    }

    /// S1 to S3: the locals and the blocks of the body as a whole.
    fn shape(&mut self) {
        let body = self.body;
        let wanted = 1 + body.param_count;
        match body.locals.first() {
            None => self.violation("S1", "the body has no locals, not even `_0`".to_owned()),
            Some(decl) if decl.ty != body.ret => self.violation(
                "S1",
                format!(
                    "`_0` has type `{}` but the body returns `{}`",
                    decl.ty, body.ret
                ),
            ),
            Some(_) => {}
        }
        if !body.locals.is_empty() && body.locals.len() < wanted {
            self.violation(
                "S1",
                format!(
                    "{} parameters but only {} locals besides `_0`",
                    body.param_count,
                    body.locals.len() - 1
                ),
            );
        }
        if body.external {
            if !body.blocks.is_empty() {
                self.violation(
                    "S2",
                    format!("an external body has {} blocks", body.blocks.len()),
                );
            }
            if body.locals.len() > wanted {
                self.violation(
                    "S2",
                    format!(
                        "an external body has {} locals besides `_0` and its parameters",
                        body.locals.len() - wanted
                    ),
                );
            }
        } else if body.blocks.is_empty() {
            self.violation("S2", "the body has no blocks".to_owned());
        }
        // A naked body with no block at all was reported by S2.
        if body.naked && (body.external || !body.blocks.is_empty()) {
            self.naked();
        }
    }

    /// S3. That the assembly ends in `noreturn` is S9's.
    fn naked(&mut self) {
        let body = self.body;
        let Some(first) = body.blocks.first() else {
            self.violation("S3", "a naked body has no block".to_owned());
            return;
        };
        if body.blocks.len() != 1 {
            self.violation(
                "S3",
                format!("a naked body has {} blocks, not one", body.blocks.len()),
            );
        }
        let start = BasicBlock::START;
        if !first.statements.is_empty() {
            self.at = Location::Block(start);
            self.violation(
                "S3",
                format!("a naked body has {} statements", first.statements.len()),
            );
        }
        if first.terminator.asm().is_none() {
            self.at = Location::Terminator(start);
            self.violation("S3", "a naked body does not end in `asm`".to_owned());
        }
        self.at = Location::Body;
    }

    /// Whether `local` exists; S7 otherwise.
    fn local(&mut self, local: Local, what: &str) -> bool {
        let exists = (local.0 as usize) < self.body.locals.len();
        if !exists {
            self.violation(
                "S7",
                format!(
                    "{what} names `{local}`, but the body has {} locals",
                    self.body.locals.len()
                ),
            );
        }
        exists
    }

    /// E1: when a `switch` tests the tag of an enum with payloads, each case
    /// is the stored value of exactly one variant of the enum (ADR 0104).
    /// The analysis of linearity tells the variant of an edge by it.
    fn variant_cases(&mut self, discr: &Operand, cases: &[(u64, BasicBlock)]) {
        let Operand::Copy(place) = discr else {
            return;
        };
        let [head @ .., PlaceElem::Tag] = place.projection else {
            return;
        };
        let Some(decl) = self.body.locals.get(place.local.0 as usize) else {
            return;
        };
        let mut ty = decl.ty;
        for elem in head {
            match project(ty, elem) {
                Ok(next) => ty = next,
                Err(_) => return,
            }
        }
        let Ty::Enum(e) = ty else {
            return;
        };
        let Some(variants) = e.def().variants.get() else {
            return;
        };
        for &(value, _) in cases {
            let named = (variants.iter())
                .filter(|variant| variant.value.get() == Some(&value))
                .count();
            if named != 1 {
                self.violation(
                    "E1",
                    format!(
                        "the case {value:#x} is the value of {named} variants of `{ty}`, not one"
                    ),
                );
            }
        }
    }

    /// S7, T1 and T2 for a place: its type and whether accessing it is
    /// volatile, if its base exists and every projection applies to the type
    /// before it. Each index local that exists, the two bounds of a
    /// `Subslice` included, is checked for T2, whatever became of the walk; the
    /// walk stops at the first projection that does not apply.
    fn place(&mut self, place: Place) -> Option<Typed> {
        let mut typed = self.local(place.local, "a place").then(|| Typed {
            ty: self.body.locals[place.local.0 as usize].ty,
            volatile: false,
        });
        for elem in place.projection {
            let indexes = match elem {
                PlaceElem::Offset(index)
                | PlaceElem::Index(index)
                | PlaceElem::Element(_, index) => [Some(*index), None],
                PlaceElem::Subslice(from, to) => [Some(*from), Some(*to)],
                PlaceElem::Deref
                | PlaceElem::ConstIndex(_)
                | PlaceElem::ConstSubslice(..)
                | PlaceElem::Field(_)
                | PlaceElem::ConstElement(..)
                | PlaceElem::Tag
                | PlaceElem::Payload { .. } => [None, None],
            };
            for index in indexes.into_iter().flatten() {
                if self.local(index, "a projection")
                    && self.local_ty(index) != Some(Ty::Int(IntTy::Usize))
                {
                    self.violation(
                        "T2",
                        format!(
                            "the index `{index}` has type `{}`, not `usize`",
                            self.body.locals[index.0 as usize].ty
                        ),
                    );
                }
            }
            if let Some(from) = typed {
                typed = match project(from.ty, elem) {
                    Ok(ty) => Some(Typed {
                        ty,
                        volatile: volatile_after(from, elem),
                    }),
                    Err(text) => {
                        self.violation("T1", text);
                        None
                    }
                };
            }
        }
        typed
    }

    fn local_ty(&self, local: Local) -> Option<Ty> {
        self.body.locals.get(local.0 as usize).map(|decl| decl.ty)
    }

    /// T3 and T4 for a place read or written whole. Its type, if it is a
    /// type of values, which T5 and T9 rely on.
    fn whole(&mut self, typed: Option<Typed>, verb: &str) -> Option<Ty> {
        let typed = typed?;
        if matches!(typed.ty, Ty::Never | Ty::Slice(_) | Ty::Peripheral(_)) {
            self.violation(
                "T3",
                format!(
                    "a place of type `{}`, which has no value, is {verb}",
                    typed.ty
                ),
            );
            return None;
        }
        if typed.volatile && typed.ty.is_aggregate() {
            self.violation(
                "T4",
                format!(
                    "a volatile place of type `{}`, an aggregate, is {verb}",
                    typed.ty
                ),
            );
        }
        Some(typed.ty)
    }

    /// A place that is read: S7, T1 to T4. Its type, if a value.
    fn read(&mut self, place: Place) -> Option<Ty> {
        let typed = self.place(place);
        self.whole(typed, "read")
    }

    /// A place that is written whole: the checks of a read, and S8.
    fn written(&mut self, place: Place, how: &str) -> Option<Ty> {
        let typed = self.place(place);
        self.constant_write(place, how);
        self.shared_write(place, how);
        self.whole(typed, "written")
    }

    /// R2: a place that is written, borrowed `&mut` or given to `&raw mut`
    /// (`how` says which) does not go through the `Deref` of a shared
    /// reference. A projection T1 rejected leaves nothing to judge.
    fn shared_write(&mut self, place: Place, how: &str) {
        let Some(mut ty) = self.local_ty(place.local) else {
            return;
        };
        for elem in place.projection {
            if let (PlaceElem::Deref, Ty::Ref(r)) = (elem, ty)
                && !r.mutable
            {
                self.violation(
                    "R2",
                    format!(
                        "{how} the place `{place}`, which goes through the shared reference `{ty}`"
                    ),
                );
                return;
            }
            match project(ty, elem) {
                Ok(next) => ty = next,
                Err(_) => return,
            }
        }
    }

    /// R3: the place of a `Ref` is a local followed by `Field`, `Index`,
    /// `ConstIndex`, `Subslice`, `ConstSubslice`, `Deref` and `Offset`
    /// projections only.
    fn ref_place(&mut self, place: Place) {
        for elem in place.projection {
            match elem {
                PlaceElem::Field(_)
                | PlaceElem::Index(_)
                | PlaceElem::ConstIndex(_)
                | PlaceElem::Subslice(..)
                | PlaceElem::ConstSubslice(..)
                | PlaceElem::Deref
                | PlaceElem::Offset(_) => {}
                PlaceElem::Element(..)
                | PlaceElem::ConstElement(..)
                | PlaceElem::Tag
                | PlaceElem::Payload { .. } => {
                    self.violation(
                        "R3",
                        format!("a `Ref` of a place through `{elem:?}`, not a field, an element or a dereference"),
                    );
                    break;
                }
            }
        }
    }

    /// S8: a constant local that is changed, as a place written, or taken
    /// by `&raw mut`, through no pointer. A base that does not exist was
    /// reported by S7.
    fn constant_write(&mut self, place: Place, how: &str) {
        let Some(decl) = self.body.locals.get(place.local.0 as usize) else {
            return;
        };
        let through_pointer = place
            .projection
            .iter()
            .any(|elem| matches!(elem, PlaceElem::Deref | PlaceElem::Offset(_)));
        if !through_pointer && decl.constant.is_some() {
            self.violation(
                "S8",
                format!(
                    "{how} the local `{}`, which holds read-only data",
                    place.local
                ),
            );
        }
    }

    /// T6 for the bits of an integer constant.
    fn int_const(&mut self, bits: u64, int: IntTy) {
        if bits & !int.mask() != 0 {
            self.violation(
                "T6",
                format!(
                    "the constant {bits:#x} has bits above the {} bits of `{int}`",
                    int.bits()
                ),
            );
        }
    }

    /// An operand: S7, T1 to T4, T6. Its type, if it is valid.
    fn operand(&mut self, operand: &Operand) -> Option<Ty> {
        match operand {
            Operand::Copy(place) => {
                let ty = self.read(*place)?;
                if !ty.is_copy() {
                    self.violation(
                        "M1",
                        format!("`copy` reads a place of the non-copy type `{ty}`"),
                    );
                }
                Some(ty)
            }
            Operand::Move(place, _) => {
                let typed = self.place(*place);
                if typed.is_some() {
                    self.move_place(*place);
                }
                let ty = self.whole(typed, "read")?;
                if ty.is_copy() {
                    self.violation(
                        "M2",
                        format!("`move` reads a place of the copy type `{ty}`"),
                    );
                }
                Some(ty)
            }
            Operand::Const(Const::Int(bits, int)) => {
                self.int_const(*bits, *int);
                Some(Ty::Int(*int))
            }
            Operand::Const(Const::Bool(_)) => Some(Ty::Bool),
            Operand::Const(Const::Unit) => Some(Ty::Unit),
            Operand::Const(Const::Static(s)) => {
                self.static_named(*s, "an operand");
                Some(s.def().address_ty())
            }
            Operand::Const(Const::Peripheral(p)) => {
                self.peripheral_named(*p);
                Some(p.def().address_ty())
            }
        }
    }

    /// M3 for the place of a `move`, whose projections T1 accepted: a local
    /// not bound to read-only data, followed by `Field` and `Payload`
    /// projections only.
    fn move_place(&mut self, place: Place) {
        let constant = self
            .body
            .locals
            .get(place.local.0 as usize)
            .is_some_and(|decl| decl.constant.is_some());
        if constant {
            self.violation(
                "M3",
                format!(
                    "`move` of the local `{}`, which holds read-only data",
                    place.local
                ),
            );
        }
        for elem in place.projection {
            match elem {
                PlaceElem::Field(_) | PlaceElem::Payload { .. } => {}
                PlaceElem::Deref
                | PlaceElem::Offset(_)
                | PlaceElem::Index(_)
                | PlaceElem::ConstIndex(_)
                | PlaceElem::Subslice(..)
                | PlaceElem::ConstSubslice(..)
                | PlaceElem::Element(..)
                | PlaceElem::ConstElement(..)
                | PlaceElem::Tag => {
                    self.violation(
                        "M3",
                        format!("`move` of a place through `{elem:?}`, not a field or a payload"),
                    );
                    break;
                }
            }
        }
    }

    /// M4 for the type of every local: a struct or an enum with payloads
    /// marked copy has copy fields and payload values only, looked for
    /// through arrays, fields and payload values, each definition once.
    fn copy_kinds(&mut self) {
        let body = self.body;
        let mut seen = HashSet::new();
        for decl in &body.locals {
            self.copy_kind(decl.ty, &mut seen);
        }
    }

    fn copy_kind(&mut self, ty: Ty, seen: &mut HashSet<*const ()>) {
        // The leaked definitions, by address: a struct and an enum are told
        // apart by their type, not by the address alone.
        match ty {
            Ty::Array(elem, _) => self.copy_kind(*elem, seen),
            Ty::Struct(s) => {
                let def = s.def();
                // A struct whose fields were never set is S-rule territory
                // of its users; it has nothing to check here.
                let Some(fields) = def.fields.get() else {
                    return;
                };
                if !seen.insert(ptr::from_ref(def).cast()) {
                    return;
                }
                if def.copy && def.linear {
                    self.violation(
                        "L2",
                        format!("`{}` is marked both copy and linear", def.name),
                    );
                }
                for field in fields {
                    if def.copy && !field.ty.is_copy() {
                        self.violation(
                            "M4",
                            format!(
                                "`{}` is marked copy, but its field `{}` has the non-copy type `{}`",
                                def.name, field.name, field.ty
                            ),
                        );
                    }
                    self.copy_kind(field.ty, seen);
                }
            }
            Ty::Enum(e) => {
                let def = e.def();
                let Some(variants) = def.variants.get() else {
                    return;
                };
                if !seen.insert(ptr::from_ref(def).cast()) {
                    return;
                }
                for variant in variants {
                    for value in &variant.payload {
                        if def.copy && !value.ty.is_copy() {
                            self.violation(
                                "M4",
                                format!(
                                    "`{}` is marked copy, but the payload value {} of `{}` has the non-copy type `{}`",
                                    def.name, value.name, variant.name, value.ty
                                ),
                            );
                        }
                        self.copy_kind(value.ty, seen);
                    }
                }
            }
            Ty::Unit
            | Ty::Never
            | Ty::Bool
            | Ty::Int(_)
            | Ty::Ptr(_)
            | Ty::Ref(_)
            | Ty::Slice(_)
            | Ty::Peripheral(_) => {}
        }
    }

    /// L3 for the type of every local: it reaches no array or slice of linear
    /// elements, through arrays, slices, fields, payload values and the
    /// pointees of pointers, each definition once.
    fn linear_kinds(&mut self) {
        let body = self.body;
        let mut seen = HashSet::new();
        for (local, decl) in body.locals() {
            let mut found = Vec::new();
            linear_arrays(decl.ty, &mut seen, &mut found);
            for (array, elem) in found {
                self.violation(
                    "L3",
                    format!(
                        "the local `{local}` of type `{}` reaches `{array}`, whose elements have the linear type `{elem}`",
                        decl.ty
                    ),
                );
            }
        }
    }

    /// R1 for the type of every local: a reference is the type of a local
    /// only, never of `_0`, the return value, and no type reaches one,
    /// through arrays, slices, fields, payload values, pointees and
    /// referents, each definition once. By it a place of a reference type is
    /// a whole local.
    fn reference_positions(&mut self) {
        let body = self.body;
        let mut seen = HashSet::new();
        for (local, decl) in body.locals() {
            if local == Local::RETURN && matches!(decl.ty, Ty::Ref(_)) {
                self.violation(
                    "R1",
                    format!("the body returns the reference `{}`", decl.ty),
                );
            }
            let mut found = Vec::new();
            references_below(decl.ty, &mut seen, &mut found);
            for reference in found {
                self.violation(
                    "R1",
                    format!(
                        "the local `{local}` of type `{}` reaches the reference `{reference}`",
                        decl.ty
                    ),
                );
            }
        }
    }

    /// R4: a local assigned the address of a static or a peripheral
    /// (`Use(const static S)` or `Use(const peripheral P)`) is a temporary: it
    /// has no name, is neither a parameter nor the return place, no other
    /// statement, call destination or `asm` output writes it, and no `Ref` or
    /// `AddressOf` takes its own place. The analyses read `(*_t)` as the
    /// static or peripheral `_t` holds the address of (ADR 0112).
    fn address_temporaries(&mut self) {
        let body = self.body;
        let mut addressed: Vec<Local> = Vec::new();
        // The writes of whole locals, and the borrows of whole locals, in the
        // order of the body.
        let mut written: Vec<(Local, Location)> = Vec::new();
        let mut taken: Vec<(Local, Location)> = Vec::new();
        for (block, data) in body.blocks() {
            for (index, statement) in data.statements.iter().enumerate() {
                let at = Location::Statement(block, index);
                match statement {
                    Statement::Assign(dest, rvalue) => {
                        if dest.projection.is_empty() {
                            written.push((dest.local, at));
                            if matches!(
                                rvalue,
                                Rvalue::Use(Operand::Const(
                                    Const::Static(_) | Const::Peripheral(_)
                                ))
                            ) && !addressed.contains(&dest.local)
                            {
                                addressed.push(dest.local);
                            }
                        }
                        if let Rvalue::Ref { place, .. } | Rvalue::AddressOf { place, .. } = rvalue
                            && place.projection.is_empty()
                        {
                            taken.push((place.local, at));
                        }
                    }
                    Statement::Call {
                        dest: Some(dest), ..
                    } if dest.projection.is_empty() => written.push((dest.local, at)),
                    Statement::Call { .. }
                    | Statement::InlineAsm(_)
                    | Statement::CriticalEnter(_)
                    | Statement::CriticalExit(_)
                    | Statement::New(..)
                    | Statement::End(..) => {}
                }
                if let Some(asm) = statement.asm() {
                    asm_outputs(asm, at, &mut written);
                }
            }
            if let Some(asm) = data.terminator.asm() {
                asm_outputs(asm, Location::Terminator(block), &mut written);
            }
        }
        for &local in &addressed {
            let writes: Vec<Location> = (written.iter())
                .filter(|&&(written, _)| written == local)
                .map(|&(_, at)| at)
                .collect();
            let named = (body.locals.get(local.0 as usize)).is_some_and(|decl| decl.name.is_some())
                || local.0 as usize <= body.param_count;
            if named {
                self.at = writes[0];
                self.violation(
                    "R4",
                    format!(
                        "the local `{local}` is assigned the address of a static or a peripheral, but is named, a parameter or the return place"
                    ),
                );
            }
            for &at in &writes[1..] {
                self.at = at;
                self.violation(
                    "R4",
                    format!(
                        "the local `{local}`, which holds the address of a static or a peripheral, is written again"
                    ),
                );
            }
        }
        for &(local, at) in &taken {
            if addressed.contains(&local) {
                self.at = at;
                self.violation(
                    "R4",
                    format!(
                        "the place of the local `{local}`, which holds the address of a static or a peripheral, is borrowed or has its address taken"
                    ),
                );
            }
        }
        self.at = Location::Body;
    }

    /// P2: `s` is the static of `Mir::statics` at its id, as the same leaked
    /// definition, so that a reference left from before a renumbering is
    /// caught.
    fn static_named(&mut self, s: StaticRef, what: &str) {
        let Some(program) = self.program else {
            return;
        };
        let def = s.def();
        match program.mir.statics.get(def.id.0 as usize) {
            None => self.violation(
                "P2",
                format!(
                    "{what} names the static `{}` (id {}), but the program has {} statics",
                    def.name,
                    def.id.0,
                    program.mir.statics.len()
                ),
            ),
            Some(held) if !ptr::eq(held.def.def(), def) => self.violation(
                "P2",
                format!(
                    "{what} holds another definition of the static `{}` (id {}) than the one in `Mir::statics`, `{}`",
                    def.name,
                    def.id.0,
                    held.def.def().name
                ),
            ),
            Some(_) => {}
        }
    }

    /// P2: `p` is a peripheral of `Mir::peripherals`.
    fn peripheral_named(&mut self, p: PeripheralRef) {
        let Some(program) = self.program else {
            return;
        };
        if !program.peripherals.contains(&ptr::from_ref(p.def())) {
            self.violation(
                "P2",
                format!(
                    "an operand names the peripheral `{}`, which is not in `Mir::peripherals`",
                    p.def().name
                ),
            );
        }
    }

    /// P2: `data` is read-only data of the program.
    fn data_named(&mut self, data: DataId, what: &str) {
        let Some(program) = self.program else {
            return;
        };
        if data.0 as usize >= program.mir.data.len() {
            self.violation(
                "P2",
                format!(
                    "{what} names data {}, but the program has {} pieces",
                    data.0,
                    program.mir.data.len()
                ),
            );
        }
    }

    /// P2 for the read-only data the locals are bound to.
    fn data_of_locals(&mut self) {
        let body = self.body;
        for (local, decl) in body.locals() {
            if let Some(data) = decl.constant {
                self.data_named(data, &format!("the local `{local}`"));
            }
        }
    }

    /// P2 and P4 for a body that a call or a `sym` names; the body, if the
    /// program has it.
    fn callee(&mut self, id: BodyId, what: &str) -> Option<&'a Body> {
        let program = self.program?;
        let Some(callee) = program.mir.bodies.get(id.0 as usize) else {
            self.violation(
                "P2",
                format!(
                    "{what} names body {}, but the program has {} bodies",
                    id.0,
                    program.mir.bodies.len()
                ),
            );
            return None;
        };
        if callee.comptime && !self.body.comptime {
            self.violation(
                "P4",
                format!(
                    "{what} names `{}`, which runs only at compile time, in a body of the program",
                    callee.name
                ),
            );
        }
        Some(callee)
    }

    /// P2 to P4 for a call, whose destination and arguments are of the types
    /// given if they are valid.
    fn call(&mut self, id: BodyId, dest: Option<Ty>, args: &[Option<Ty>]) {
        let Some(callee) = self.callee(id, "a call") else {
            return;
        };
        // A callee short of locals is its own S1.
        let Some(params) = callee.locals.get(1..=callee.param_count) else {
            return;
        };
        if args.len() != params.len() {
            self.violation(
                "P3",
                format!(
                    "a call to `{}` passes {} arguments, but it has {} parameters",
                    callee.name,
                    args.len(),
                    params.len()
                ),
            );
        } else {
            for (index, (arg, param)) in args.iter().zip(params).enumerate() {
                if let Some(arg) = *arg
                    && !fits(arg, param.ty)
                {
                    self.violation(
                        "P3",
                        format!(
                            "argument {index} of a call to `{}` has type `{arg}`, which does not fit the parameter's `{}`",
                            callee.name, param.ty
                        ),
                    );
                }
            }
        }
        if let Some(dest) = dest
            && !fits(callee.ret, dest)
        {
            self.violation(
                "P3",
                format!(
                    "a call to `{}` returns `{}`, which does not fit the destination's `{dest}`",
                    callee.name, callee.ret
                ),
            );
        }
    }

    /// P2 and P8 for where a `panic` says it is, if it does.
    fn panic_site(&mut self, site: Option<PanicSite>) {
        if let Some(site) = site {
            match site.info {
                PanicInfoAt::Local(local) => {
                    self.local(local, "a panic site");
                }
                PanicInfoAt::Data(data) => self.data_named(data, "a panic site"),
            }
            self.static_named(site.file, "a panic site's file");
        }
        let Some(program) = self.program else {
            return;
        };
        // The panic function's parameter, if it takes one; a body that does
        // not exist, or has none to name, is for P2 and P5.
        let Some(panic) = program.mir.bodies.get(program.mir.panic.0 as usize) else {
            return;
        };
        let takes = panic.param_count > 0;
        match (self.body.comptime, site, takes) {
            (true, None, _) | (false, None, false) => {}
            (true, Some(_), _) => self.violation(
                "P8",
                "a `panic` of a `comptime` body carries a site, but code run at compile time never calls the panic function"
                    .to_owned(),
            ),
            (false, None, true) => self.violation(
                "P8",
                "a `panic` carries no site, but the panic function takes a parameter".to_owned(),
            ),
            (false, Some(_), false) => self.violation(
                "P8",
                "a `panic` carries a site, but the panic function takes no parameter".to_owned(),
            ),
            (false, Some(site), true) => {
                if let PanicInfoAt::Local(local) = site.info
                    && let Some(have) = self.local_ty(local)
                    && let Some(Ty::Ptr(param)) = panic.locals.get(1).map(|decl| decl.ty)
                    && have != *param.pointee
                {
                    self.violation(
                        "P8",
                        format!(
                            "the site is in `{local}` of type `{have}`, but the panic function's parameter points to `{}`",
                            param.pointee
                        ),
                    );
                }
            }
        }
    }

    fn statement(&mut self, statement: &Statement) {
        match statement {
            Statement::Assign(place, rvalue) => {
                let dest = self.written(*place, "an assignment writes");
                self.linear_write(*place, dest, "an assignment");
                self.rvalue(rvalue, dest);
            }
            Statement::Call { dest, callee, args } => {
                let dest = dest.and_then(|place| {
                    let ty = self.written(place, "a call's destination is");
                    self.linear_write(place, ty, "a call's destination");
                    ty
                });
                let args: Vec<Option<Ty>> = args.iter().map(|arg| self.operand(arg)).collect();
                self.call(*callee, dest, &args);
            }
            Statement::InlineAsm(asm) => self.asm(asm, false),
            Statement::CriticalEnter(local) | Statement::CriticalExit(local) => {
                self.critical_local(*local);
            }
            Statement::New(place, _) => self.linear_place(*place, "`new`"),
            Statement::End(place, _) => self.linear_place(*place, "`end`"),
        }
    }

    /// L1: the place of `new` and of `end` is a local not bound to read-only data,
    /// followed by `Field` and `Payload` projections only, of a struct
    /// marked linear. `what` names the statement.
    fn linear_place(&mut self, place: Place, what: &str) {
        let typed = self.place(place);
        let constant = (self.body.locals.get(place.local.0 as usize))
            .is_some_and(|decl| decl.constant.is_some());
        if constant {
            self.violation(
                "L1",
                format!(
                    "{what} of the local `{}`, which holds read-only data",
                    place.local
                ),
            );
        }
        for elem in place.projection {
            match elem {
                PlaceElem::Field(_) | PlaceElem::Payload { .. } => {}
                PlaceElem::Deref
                | PlaceElem::Offset(_)
                | PlaceElem::Index(_)
                | PlaceElem::ConstIndex(_)
                | PlaceElem::Subslice(..)
                | PlaceElem::ConstSubslice(..)
                | PlaceElem::Element(..)
                | PlaceElem::ConstElement(..)
                | PlaceElem::Tag => {
                    self.violation(
                        "L1",
                        format!("{what} of a place through `{elem:?}`, not a field or a payload"),
                    );
                    break;
                }
            }
        }
        // A projection T1 rejected leaves no type to judge.
        if let Some(typed) = typed
            && !matches!(typed.ty, Ty::Struct(s) if s.def().linear)
        {
            self.violation(
                "L1",
                format!(
                    "{what} of a place of type `{}`, not a struct marked linear",
                    typed.ty
                ),
            );
        }
    }

    /// L4: what `how` writes to `place`, of type `ty` if it is a valid type
    /// of values, goes through a pointer and is linear, where it could never
    /// be moved out again. An array of linear elements is L3's.
    fn linear_write(&mut self, place: Place, ty: Option<Ty>, how: &str) {
        if let Some(ty) = ty
            && place.is_indirect()
            && ty.is_linear_outside_arrays()
        {
            self.violation(
                "L4",
                format!("{how} writes a value of the linear type `{ty}` through a pointer"),
            );
        }
    }

    /// S7 and T2 for the local of a `critical` statement.
    fn critical_local(&mut self, local: Local) {
        if self.local(local, "a `critical` statement")
            && self.local_ty(local) != Some(Ty::Int(IntTy::U32))
        {
            self.violation(
                "T2",
                format!(
                    "the local `{local}` of a `critical` statement has type `{}`, not `u32`",
                    self.body.locals[local.0 as usize].ty
                ),
            );
        }
    }

    /// T5: a type that does not fit the place's, or an operand the rvalue
    /// does not apply to.
    fn fit(&mut self, what: &str, ty: Ty, dest: Option<Ty>) {
        if let Some(dest) = dest
            && !fits(ty, dest)
        {
            self.violation(
                "T5",
                format!("{what} has type `{ty}`, which does not fit the place's `{dest}`"),
            );
        }
    }

    /// An assignment's rvalue, and T5 against the type of the place it goes
    /// to, if that is a valid type of values.
    fn rvalue(&mut self, rvalue: &Rvalue, dest: Option<Ty>) {
        match rvalue {
            Rvalue::Use(operand) => {
                let Some(ty) = self.operand(operand) else {
                    return;
                };
                // A raw slice into a raw slice of the same elements, whatever
                // the mutability and volatility (ADR 0031). A reference to a
                // slice fits only its own type (ADR 0109, T5).
                let slices = raw_slice_elem(ty).is_some()
                    && dest.is_some_and(|dest| raw_slice_elem(dest) == raw_slice_elem(ty));
                if !slices {
                    self.fit("`use`", ty, dest);
                }
            }
            Rvalue::Unary(op, operand) => {
                let Some(ty) = self.operand(operand) else {
                    return;
                };
                let applies = match op {
                    UnOp::Neg => matches!(ty, Ty::Int(int) if int.is_signed()),
                    UnOp::Not => matches!(ty, Ty::Int(_) | Ty::Bool),
                };
                if applies {
                    self.fit("a unary operation", ty, dest);
                } else {
                    self.violation(
                        "T5",
                        format!("`{op:?}` does not apply to an operand of type `{ty}`"),
                    );
                }
            }
            Rvalue::Binary(op, lhs, rhs) => {
                let lhs = self.operand(lhs);
                let rhs = self.operand(rhs);
                let (Some(lhs), Some(rhs)) = (lhs, rhs) else {
                    return;
                };
                self.binary(*op, lhs, rhs, dest);
            }
            Rvalue::Cast(operand, target) => {
                let Some(from) = self.operand(operand) else {
                    return;
                };
                // The one cast whose sides are slices: a reference to a slice
                // to the raw slice of its mutability and elements (ADR 0109).
                if let Ty::Ref(r) = from
                    && let Ty::Slice(elem) = r.referent
                    && let Ty::Ptr(to) = target
                    && to.pointee == &Ty::Slice(elem)
                    && to.mutable == r.mutable
                    && !to.volatile
                {
                    self.fit("a cast", *target, dest);
                } else if from.is_aggregate() || target.is_aggregate() {
                    self.violation(
                        "T5",
                        format!("a cast from `{from}` to `{target}`, one of them an aggregate"),
                    );
                } else if let Ty::Ref(_) = target {
                    self.violation(
                        "T5",
                        format!("a cast from `{from}` to the reference `{target}`"),
                    );
                } else if let Ty::Ref(_) = from
                    && !matches!(target, Ty::Ptr(ptr) if thin(ptr))
                {
                    self.violation(
                        "T5",
                        format!("a cast from the reference `{from}` to `{target}`, not a thin raw pointer"),
                    );
                } else {
                    self.fit("a cast", *target, dest);
                }
            }
            Rvalue::AddressOf {
                mutable: true,
                place,
            } => {
                let typed = self.place(*place);
                self.constant_write(*place, "`&raw mut` takes");
                self.shared_write(*place, "`&raw mut` takes");
                self.address_of(true, typed, dest);
            }
            Rvalue::AddressOf {
                mutable: false,
                place,
            } => {
                let typed = self.place(*place);
                self.address_of(false, typed, dest);
            }
            Rvalue::Ref { mutable, place } => {
                let typed = self.place(*place);
                if *mutable {
                    self.constant_write(*place, "`&mut` takes");
                    self.shared_write(*place, "`&mut` takes");
                }
                self.ref_place(*place);
                self.borrow(*mutable, typed, dest);
            }
            Rvalue::Slice { ptr, len } => {
                let ptr = self.operand(ptr);
                let len = self.operand(len);
                if let Some(len) = len
                    && len != Ty::Int(IntTy::Usize)
                {
                    self.violation("T5", format!("a slice's length has type `{len}`"));
                }
                let Some(ptr) = ptr else {
                    return;
                };
                let Ty::Ptr(from) = ptr else {
                    self.violation("T5", format!("a slice is made from `{ptr}`, no pointer"));
                    return;
                };
                if !thin(&from) {
                    self.violation("T5", format!("a slice is made from `{ptr}`, not thin"));
                    return;
                }
                let Some(dest) = dest else {
                    return;
                };
                let by_elem = |elem: Ty| raw_slice_elem(dest) == Some(elem);
                let elements = by_elem(*from.pointee)
                    || matches!(from.pointee, Ty::Array(elem, _) if by_elem(**elem));
                if !elements {
                    self.violation(
                        "T5",
                        format!("a slice made from `{ptr}` does not fit the place's `{dest}`"),
                    );
                }
            }
            Rvalue::SliceAddress(place) => {
                let Some(typed) = self.place(*place) else {
                    return;
                };
                if let Ty::Slice(_) = typed.ty {
                    self.unsized_operand("the address", typed.ty);
                    return;
                }
                match typed.ty.slice_address_ty() {
                    Some(address) => self.fit("a slice's address", address, dest),
                    None => self.violation(
                        "T5",
                        format!("the address of `{}`, which is no slice", typed.ty),
                    ),
                }
            }
            Rvalue::SliceLen(place) => {
                let Some(typed) = self.place(*place) else {
                    return;
                };
                if let Ty::Slice(_) = typed.ty {
                    self.unsized_operand("the length", typed.ty);
                } else if typed.ty.slice_elem().is_some() {
                    self.fit("a slice's length", Ty::Int(IntTy::Usize), dest);
                } else {
                    self.violation(
                        "T5",
                        format!("the length of `{}`, which is no slice", typed.ty),
                    );
                }
            }
        }
    }

    /// U1: `SliceAddress` and `SliceLen` read a slice, not the place of type
    /// `[T]` that one points to or refers to (ADR 0109). The operands and
    /// destinations that name such a place are T3's.
    fn unsized_operand(&mut self, what: &str, ty: Ty) {
        self.violation(
            "U1",
            format!("{what} of a place of type `{ty}`, which only `Index`, a sub-slice, `Ref` and `AddressOf` use"),
        );
    }

    /// T5 for a binary operation on operands of valid types.
    fn binary(&mut self, op: BinOp, lhs: Ty, rhs: Ty, dest: Option<Ty>) {
        let symbol = op.symbol();
        if op.is_shift() {
            if matches!((lhs, rhs), (Ty::Int(_), Ty::Int(_))) {
                self.fit("a shift", lhs, dest);
            } else {
                self.violation(
                    "T5",
                    format!("`{symbol}` applies to integers, not `{lhs}` and `{rhs}`"),
                );
            }
        } else if op.is_comparison() {
            let one_type = fits(lhs, rhs) || fits(rhs, lhs);
            let comparable = match lhs {
                Ty::Int(_) => true,
                Ty::Bool => matches!(op, BinOp::Eq | BinOp::Ne),
                Ty::Ptr(ptr) => thin(&ptr),
                Ty::Unit | Ty::Never | Ty::Array(..) | Ty::Slice(_) | Ty::Struct(_) => false,
                Ty::Enum(_) | Ty::Peripheral(_) => false,
                // Neither the values nor the addresses are meant (ADR 0105).
                Ty::Ref(_) => false,
            };
            if one_type && comparable {
                self.fit("a comparison", Ty::Bool, dest);
            } else {
                self.violation(
                    "T5",
                    format!("`{symbol}` does not compare `{lhs}` with `{rhs}`"),
                );
            }
        } else {
            let applies = lhs == rhs
                && match lhs {
                    Ty::Int(_) => true,
                    Ty::Bool => op.is_bitwise(),
                    _ => false,
                };
            if applies {
                self.fit("an arithmetic operation", lhs, dest);
            } else {
                self.violation(
                    "T5",
                    format!("`{symbol}` does not apply to `{lhs}` and `{rhs}`"),
                );
            }
        }
    }

    /// T5 for `&raw const` and `&raw mut` of a place of type `typed.ty`: a
    /// pointer to it that is not volatile, `*const` if written so.
    fn address_of(&mut self, mutable: bool, typed: Option<Typed>, dest: Option<Ty>) {
        let (Some(typed), Some(dest)) = (typed, dest) else {
            return;
        };
        let fits = matches!(
            dest,
            Ty::Ptr(ptr) if ptr.pointee == &typed.ty && !ptr.volatile && (mutable || !ptr.mutable)
        );
        if !fits {
            let how = if mutable { "mut" } else { "const" };
            self.violation(
                "T5",
                format!(
                    "`&raw {how}` of a place of type `{}` does not fit the place's `{dest}`",
                    typed.ty
                ),
            );
        }
    }

    /// T5 for `&place` and `&mut place` of a place of type `typed.ty`: the
    /// reference to it, `&mut` if written so, of a place that is not
    /// volatile. A `&mut` does not fit a `&` place: the conversion is a `Ref`
    /// (ADR 0107).
    fn borrow(&mut self, mutable: bool, typed: Option<Typed>, dest: Option<Ty>) {
        let Some(typed) = typed else {
            return;
        };
        let how = if mutable { "&mut" } else { "&" };
        if typed.volatile {
            self.violation(
                "T5",
                format!("`{how}` of a volatile place of type `{}`", typed.ty),
            );
        }
        let Some(dest) = dest else {
            return;
        };
        let fits = matches!(
            dest,
            Ty::Ref(r) if r.referent == &typed.ty && r.mutable == mutable
        );
        if !fits {
            self.violation(
                "T5",
                format!(
                    "`{how}` of a place of type `{}` does not fit the place's `{dest}`",
                    typed.ty
                ),
            );
        }
    }

    fn terminator(&mut self, terminator: &Terminator) {
        let blocks = self.body.blocks.len();
        for next in terminator.successors() {
            if next == BasicBlock::START {
                self.violation("S5", "a terminator jumps to `bb0`".to_owned());
            } else if next.0 as usize >= blocks {
                self.violation(
                    "S5",
                    format!("a terminator jumps to {next}, but the body has {blocks} blocks"),
                );
            }
        }
        match terminator {
            Terminator::Goto(_) | Terminator::Unreachable => {}
            Terminator::If { cond, .. } => {
                if let Some(ty) = self.operand(cond)
                    && ty != Ty::Bool
                {
                    self.violation("T7", format!("`if` tests `{ty}`, not `bool`"));
                }
            }
            Terminator::Switch { discr, cases, .. } => {
                if let Some(ty) = self.operand(discr) {
                    self.switch(ty, cases);
                    self.variant_cases(discr, cases);
                }
            }
            Terminator::Return => {
                if self.body.ret == Ty::Never {
                    self.violation("S6", "a body that returns `!` has a `return`".to_owned());
                }
            }
            Terminator::Panic(cause, site) => {
                match cause {
                    PanicCause::AssertionFailed(Some((message, _))) => {
                        if let Some(ty) = self.operand(message) {
                            let is_message = ty.slice_elem() == Some(Ty::Int(IntTy::U8))
                                && matches!(ty, Ty::Ptr(ptr) if !ptr.mutable && !ptr.volatile);
                            if !is_message {
                                self.violation(
                                    "T8",
                                    format!("the message of an `assert` has type `{ty}`, not `*const [u8]`"),
                                );
                            }
                        }
                    }
                    PanicCause::AssertionFailed(None)
                    | PanicCause::DivisionByZero
                    | PanicCause::IndexOutOfBounds
                    | PanicCause::NoVariant
                    | PanicCause::FieldOutOfRange => {}
                }
                self.panic_site(*site);
            }
            Terminator::InlineAsm(asm) => self.asm(asm, true),
        }
    }

    /// C1 to C3: walks the blocks reached from `bb0`, giving each the stack
    /// of `critical_enter` locals open at its start and comparing it at every
    /// edge into it. A block nothing reaches is not looked at (ADR 0093), and
    /// an edge to a block that does not exist or to `bb0` was reported by S5.
    /// A block whose stack another edge contradicts is walked once, with the
    /// stack of the first edge found, so that one fault is one violation.
    fn critical(&mut self) {
        let body = self.body;
        if body.blocks.is_empty() {
            return;
        }
        let mut entry: Vec<Option<Vec<Local>>> = vec![None; body.blocks.len()];
        entry[0] = Some(Vec::new());
        let mut work = vec![BasicBlock::START];
        while let Some(block) = work.pop() {
            let Some(mut open) = entry[block.0 as usize].clone() else {
                continue;
            };
            let data = &body.blocks[block.0 as usize];
            for (index, statement) in data.statements.iter().enumerate() {
                self.at = Location::Statement(block, index);
                match statement {
                    Statement::CriticalEnter(local) => open.push(*local),
                    Statement::CriticalExit(local) => self.critical_exit(&mut open, *local),
                    Statement::Assign(..)
                    | Statement::Call { .. }
                    | Statement::InlineAsm(_)
                    | Statement::New(..)
                    | Statement::End(..) => {}
                }
            }
            self.at = Location::Terminator(block);
            if matches!(data.terminator, Terminator::Return) && !open.is_empty() {
                self.violation(
                    "C3",
                    format!("a `return` with {} `critical` blocks open", open.len()),
                );
            }
            for next in data.terminator.successors() {
                if next == BasicBlock::START {
                    continue;
                }
                let Some(slot) = entry.get_mut(next.0 as usize) else {
                    continue;
                };
                match slot {
                    None => {
                        *slot = Some(open.clone());
                        work.push(next);
                    }
                    Some(first) if *first != open => {
                        let text = format!(
                            "{next} is entered with the open blocks {} and with {}",
                            stack(first),
                            stack(&open)
                        );
                        self.violation("C1", text);
                    }
                    Some(_) => {}
                }
            }
        }
        self.at = Location::Body;
    }

    /// C2 for a `critical_exit` of `local` with the blocks `open`. The walk
    /// goes on from a stack that makes one fault one violation: an exit of a
    /// local open further down closes it and the blocks above it (the likely
    /// fault is the exits of the inner ones that are missing), and an exit of
    /// a local that is not open closes nothing. When `local` does not exist S7
    /// spoke already, and that it is not open is not reported again; at depth
    /// 0 it is, since that is a fault of its own.
    fn critical_exit(&mut self, open: &mut Vec<Local>, local: Local) {
        let Some(&top) = open.last() else {
            self.violation(
                "C2",
                format!("`critical_exit {local}` with no `critical` block open"),
            );
            return;
        };
        let exists = (local.0 as usize) < self.body.locals.len();
        match open.iter().rposition(|&was| was == local) {
            Some(position) if position + 1 == open.len() => {
                open.pop();
            }
            Some(position) => {
                open.truncate(position);
                self.violation(
                    "C2",
                    format!("`critical_exit {local}` while the innermost open block is `{top}`"),
                );
            }
            None if exists => self.violation(
                "C2",
                format!(
                    "`critical_exit {local}` of a local that is not open, the innermost open block is `{top}`"
                ),
            ),
            None => {}
        }
    }

    /// T7 for the cases of a `switch` on a value of type `ty`.
    fn switch(&mut self, ty: Ty, cases: &[(u64, BasicBlock)]) {
        let mask = match ty {
            Ty::Int(int) => int.mask(),
            Ty::Bool => 1,
            Ty::Unit
            | Ty::Never
            | Ty::Ptr(_)
            | Ty::Ref(_)
            | Ty::Array(..)
            | Ty::Slice(_)
            | Ty::Struct(_)
            | Ty::Enum(_)
            | Ty::Peripheral(_) => {
                self.violation(
                    "T7",
                    format!("`switch` tests `{ty}`, not an integer or `bool`"),
                );
                return;
            }
        };
        let mut seen = HashSet::new();
        for &(value, _) in cases {
            if value & !mask != 0 {
                self.violation(
                    "T7",
                    format!(
                        "the case {value:#x} does not fit the {} bits tested",
                        mask.count_ones()
                    ),
                );
            } else if !seen.insert(value) {
                self.violation("T7", format!("the case {value:#x} appears twice"));
            }
        }
    }

    /// S7 to S9, T6 and T9 for an `asm`, a statement or a terminator.
    fn asm(&mut self, asm: &InlineAsm, terminator: bool) {
        let noreturn = asm.options.noreturn;
        if terminator && !noreturn {
            self.violation("S9", "an `asm` terminator lacks `noreturn`".to_owned());
        }
        if !terminator && noreturn {
            self.violation("S9", "an `asm` statement has `noreturn`".to_owned());
        }
        for piece in &asm.pieces {
            match piece {
                AsmPiece::Operand(index) if *index >= asm.operands.len() => self.violation(
                    "S9",
                    format!(
                        "a placeholder names operand {index}, but the `asm` has {}",
                        asm.operands.len()
                    ),
                ),
                AsmPiece::Operand(_) | AsmPiece::Text(_) => {}
            }
        }
        for operand in &asm.operands {
            match &operand.kind {
                AsmOperandKind::In(value) => {
                    let ty = self.operand(value);
                    self.asm_value(ty, "an `asm` input");
                }
                AsmOperandKind::Out { place, .. } => {
                    if let Some(place) = place {
                        let ty = self.written(*place, "an `asm` output is");
                        self.linear_write(*place, ty, "an `asm` output");
                        self.asm_value(ty, "an `asm` output");
                    }
                }
                AsmOperandKind::InOut { input, place } => {
                    let ty = self.operand(input);
                    self.asm_value(ty, "an `asm` input");
                    if let Some(place) = place {
                        let ty = self.written(*place, "an `asm` output is");
                        self.linear_write(*place, ty, "an `asm` output");
                        self.asm_value(ty, "an `asm` output");
                    }
                }
                AsmOperandKind::Sym(AsmSym::Body(id)) => {
                    self.callee(*id, "an `asm` `sym`");
                }
                AsmOperandKind::Sym(AsmSym::Static(s)) => self.static_named(*s, "an `asm` `sym`"),
                AsmOperandKind::Const(bits, int) => self.int_const(*bits, *int),
            }
        }
    }

    /// T9: an integer of at most 32 bits or a thin pointer.
    fn asm_value(&mut self, ty: Option<Ty>, what: &str) {
        let Some(ty) = ty else {
            return;
        };
        let allowed = match ty {
            Ty::Int(int) => int.bits() <= 32,
            Ty::Ptr(ptr) => thin(&ptr),
            Ty::Unit
            | Ty::Never
            | Ty::Bool
            | Ty::Ref(_)
            | Ty::Array(..)
            | Ty::Slice(_)
            | Ty::Struct(_)
            | Ty::Enum(_)
            | Ty::Peripheral(_) => false,
        };
        if !allowed {
            self.violation(
                "T9",
                format!(
                    "{what} has type `{ty}`, not an integer of at most 32 bits or a thin pointer"
                ),
            );
        }
    }
}

/// Adds to `written` the whole locals the outputs of `asm` write, at `at`.
fn asm_outputs(asm: &InlineAsm, at: Location, written: &mut Vec<(Local, Location)>) {
    for operand in &asm.operands {
        match &operand.kind {
            AsmOperandKind::Out {
                place: Some(place), ..
            }
            | AsmOperandKind::InOut {
                place: Some(place), ..
            } if place.projection.is_empty() => written.push((place.local, at)),
            AsmOperandKind::In(_)
            | AsmOperandKind::Out { .. }
            | AsmOperandKind::InOut { .. }
            | AsmOperandKind::Sym(_)
            | AsmOperandKind::Const(..) => {}
        }
    }
}

/// What a body is for in the program, which fixes the shape it must have
/// (P4, P5).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Entry,
    Panic,
    Reset,
    CriticalEnter,
    CriticalExit,
    Handler,
}

/// The rules of the program that do not look inside a body: the roles bodies
/// have (P2, P4, P5), the vector table (P2), statics (P6, P7) and read-only
/// data (P7). The rules that do are the `Checker`'s, which ran on each body.
struct Rules<'a> {
    mir: &'a Mir,
    /// The violations that concern no body.
    whole: Vec<Violation>,
    /// The violations of each body, by index, as the `Checker` found them.
    bodies: Vec<Vec<Violation>>,
}

impl Rules<'_> {
    /// A violation of what `subject` describes, which is no body.
    fn of(&mut self, subject: &str, rule: &'static str, text: String) {
        self.whole.push(Violation {
            body: subject.to_owned(),
            at: Location::Program,
            rule,
            text,
        });
    }

    /// A violation of the body at `index`, as a whole.
    fn of_body(&mut self, index: usize, rule: &'static str, text: String) {
        let violation = Violation {
            body: self.mir.bodies[index].name.clone(),
            at: Location::Body,
            rule,
            text,
        };
        self.bodies[index].push(violation);
    }

    /// P2, P4 and P5 for `entry`, `panic`, `reset`, the two functions of a
    /// `critical` block and the handlers.
    fn roles(&mut self) {
        let mir = self.mir;
        let mut roles = vec![
            (Role::Entry, "`entry`".to_owned(), mir.entry),
            (Role::Panic, "`panic`".to_owned(), mir.panic),
        ];
        roles.extend(mir.reset.map(|id| (Role::Reset, "`reset`".to_owned(), id)));
        roles.extend(
            (mir.critical_enter).map(|id| (Role::CriticalEnter, "`critical_enter`".to_owned(), id)),
        );
        roles.extend(
            (mir.critical_exit).map(|id| (Role::CriticalExit, "`critical_exit`".to_owned(), id)),
        );
        for handler in &mir.handlers {
            let what = format!("the handler of `{}`", handler.vector);
            roles.push((Role::Handler, what, handler.body));
        }
        for (role, what, id) in roles {
            self.role(role, &what, id);
        }
    }

    fn role(&mut self, role: Role, what: &str, id: BodyId) {
        let Some(body) = self.mir.bodies.get(id.0 as usize) else {
            let text = format!(
                "{what} is body {}, but the program has {} bodies",
                id.0,
                self.mir.bodies.len()
            );
            self.of("the program", "P2", text);
            return;
        };
        let index = id.0 as usize;
        let params = body.param_count;
        let first = body.locals.get(1).map(|decl| decl.ty);
        let runs = !matches!(role, Role::CriticalEnter | Role::CriticalExit);
        if runs && body.comptime {
            self.of_body(
                index,
                "P4",
                format!("{what} runs only at compile time, but it is part of the program"),
            );
        }
        let mut wrong = Vec::new();
        let never = matches!(role, Role::Entry | Role::Panic);
        if never && body.ret != Ty::Never {
            wrong.push(format!("{what} returns `{}`, not `!`", body.ret));
        }
        match role {
            Role::Entry | Role::Handler | Role::CriticalEnter if params != 0 => {
                wrong.push(format!("{what} takes {params} parameters, not none"));
            }
            Role::Panic if params > 1 => {
                wrong.push(format!("{what} takes {params} parameters, not none or one"));
            }
            Role::Panic if params == 1 && !matches!(first, Some(Ty::Ptr(ptr)) if thin(&ptr)) => {
                wrong.push(format!(
                    "{what} takes a parameter of type `{}`, not a thin pointer",
                    first.map_or("?".to_owned(), |ty| ty.to_string())
                ));
            }
            Role::CriticalExit if params != 1 => {
                wrong.push(format!("{what} takes {params} parameters, not one"));
            }
            Role::CriticalExit if first != Some(Ty::Int(IntTy::U32)) => {
                wrong.push(format!("{what} takes a parameter that is not a `u32`"));
            }
            Role::Entry
            | Role::Panic
            | Role::Reset
            | Role::CriticalEnter
            | Role::CriticalExit
            | Role::Handler => {}
        }
        if role == Role::CriticalEnter && body.ret != Ty::Int(IntTy::U32) {
            wrong.push(format!("{what} returns `{}`, not `u32`", body.ret));
        }
        for text in wrong {
            self.of_body(index, "P5", text);
        }
    }

    /// P2 for the words of the vector table and its default handlers.
    fn vector_table(&mut self) {
        let mir = self.mir;
        let Some(table) = &mir.vector_table else {
            return;
        };
        for (index, word) in table.words.iter().enumerate() {
            match word {
                VectorWord::Handler(id) if id.0 as usize >= mir.bodies.len() => {
                    let text = format!(
                        "word {index} is the handler body {}, but the program has {} bodies",
                        id.0,
                        mir.bodies.len()
                    );
                    self.of("the vector table", "P2", text);
                }
                VectorWord::Default(default) if *default >= table.defaults.len() => {
                    let text = format!(
                        "word {index} is the default handler {default}, but the table has {}",
                        table.defaults.len()
                    );
                    self.of("the vector table", "P2", text);
                }
                VectorWord::Handler(_)
                | VectorWord::Default(_)
                | VectorWord::StackEnd
                | VectorWord::Reset
                | VectorWord::Reserved => {}
            }
        }
        for default in &table.defaults {
            if let Some(info) = default.info
                && info.0 as usize >= mir.data.len()
            {
                let text = format!(
                    "its information is data {}, but the program has {} pieces",
                    info.0,
                    mir.data.len()
                );
                self.of(&format!("default handler `{}`", default.symbol), "P2", text);
            }
        }
    }

    /// The size of the static at `id`, if it exists and has a type of values.
    fn static_size(&self, id: usize) -> Option<u64> {
        let ty = self.mir.statics.get(id)?.def.def().ty;
        values(ty).then(|| ty.layout().size)
    }

    /// L3 for the type of a static: it is not linear, and reaches no array
    /// or slice of linear elements (see `linear_kinds`). An array of linear
    /// elements is reported as such, and not also as a linear type.
    fn linear_static(&mut self, subject: &str, ty: Ty, seen: &mut HashSet<*const ()>) {
        if ty.is_linear_outside_arrays() {
            let text = format!("its type `{ty}` is linear");
            self.of(subject, "L3", text);
        }
        let mut found = Vec::new();
        linear_arrays(ty, seen, &mut found);
        for (array, elem) in found {
            let text = format!(
                "its type `{ty}` reaches `{array}`, whose elements have the linear type `{elem}`"
            );
            self.of(subject, "L3", text);
        }
    }

    /// R1 for the type of a static: it is not a reference, and reaches none
    /// (see `reference_positions`).
    fn reference_static(&mut self, subject: &str, ty: Ty, seen: &mut HashSet<*const ()>) {
        // One fault, whatever the reference reaches.
        if let Ty::Ref(_) = ty {
            let text = format!("its type `{ty}` is a reference");
            self.of(subject, "R1", text);
            return;
        }
        let mut found = Vec::new();
        references_below(ty, seen, &mut found);
        for reference in found {
            let text = format!("its type `{ty}` reaches the reference `{reference}`");
            self.of(subject, "R1", text);
        }
    }

    /// P6 and P7 for the statics, and P2 for the targets of their relocations.
    fn statics(&mut self) {
        let mir = self.mir;
        let mut seen = HashSet::new();
        let mut seen_references = HashSet::new();
        for (index, held) in mir.statics.iter().enumerate() {
            let def = held.def.def();
            let subject = format!("static `{}`", def.name);
            self.linear_static(&subject, def.ty, &mut seen);
            self.reference_static(&subject, def.ty, &mut seen_references);
            if def.id.0 as usize != index {
                let text = format!("its id is {}, but it is at index {index}", def.id.0);
                self.of(&subject, "P6", text);
            }
            let size = self.static_size(index);
            if size.is_none() {
                let text = format!("its type `{}` is not a type of values", def.ty);
                self.of(&subject, "P6", text);
            }
            if def.external {
                if !held.bytes.is_empty() {
                    let text = format!("it is external but has {} bytes", held.bytes.len());
                    self.of(&subject, "P6", text);
                }
            } else if let Some(size) = size
                && held.bytes.len() as u64 != size
            {
                let text = format!(
                    "it has {} bytes, but its type `{}` has size {size}",
                    held.bytes.len(),
                    def.ty
                );
                self.of(&subject, "P6", text);
            }
            self.relocations(&subject, held.bytes.len(), &held.relocations);
        }
    }

    /// P7 for the read-only data, and P2 for the targets of its relocations.
    fn data(&mut self) {
        let mir = self.mir;
        for (
            index,
            ConstData {
                bytes, relocations, ..
            },
        ) in mir.data.iter().enumerate()
        {
            self.relocations(&format!("data {index}"), bytes.len(), relocations);
        }
    }

    /// P2 and P7 for the relocations of `subject`, which has `len` bytes.
    fn relocations(&mut self, subject: &str, len: usize, relocations: &[Relocation]) {
        for relocation in relocations {
            let target = relocation.target.0 as usize;
            let end = u64::from(relocation.offset) + target::POINTER_BYTES;
            if end > len as u64 {
                let text = format!(
                    "the relocation at {} ends at byte {end}, past its {len} bytes",
                    relocation.offset
                );
                self.of(subject, "P7", text);
            }
            if target >= self.mir.statics.len() {
                let text = format!(
                    "the relocation at {} points to static {target}, but the program has {}",
                    relocation.offset,
                    self.mir.statics.len()
                );
                self.of(subject, "P2", text);
            } else if let Some(size) = self.static_size(target)
                && u64::from(relocation.addend) > size
            {
                let text = format!(
                    "the relocation at {} has addend {}, past the {size} bytes of its target",
                    relocation.offset, relocation.addend
                );
                self.of(subject, "P7", text);
            }
        }
    }
}

/// L3: appends to `found`, each with the type of its elements, the arrays and
/// slices of linear elements that `ty` reaches, through arrays, slices,
/// fields, payload values and the pointees of pointers. The structs and enums
/// in `seen` were looked into already. An array whose elements are arrays is
/// reported at the inner one, so that a fault is one violation.
fn linear_arrays(ty: Ty, seen: &mut HashSet<*const ()>, found: &mut Vec<(Ty, Ty)>) {
    match ty {
        Ty::Array(elem, _) | Ty::Slice(elem) => {
            linear_arrays(*elem, seen, found);
            if elem.is_linear_outside_arrays() {
                found.push((ty, *elem));
            }
        }
        Ty::Ptr(ptr) => linear_arrays(*ptr.pointee, seen, found),
        Ty::Ref(r) => linear_arrays(*r.referent, seen, found),
        Ty::Struct(s) => {
            let def = s.def();
            let Some(fields) = def.fields.get() else {
                return;
            };
            if seen.insert(ptr::from_ref(def).cast()) {
                for field in fields {
                    linear_arrays(field.ty, seen, found);
                }
            }
        }
        Ty::Enum(e) => {
            let def = e.def();
            let Some(variants) = def.variants.get() else {
                return;
            };
            if seen.insert(ptr::from_ref(def).cast()) {
                for value in variants.iter().flat_map(|variant| &variant.payload) {
                    linear_arrays(value.ty, seen, found);
                }
            }
        }
        Ty::Unit | Ty::Never | Ty::Bool | Ty::Int(_) | Ty::Peripheral(_) => {}
    }
}

/// R1: appends to `found` the references that `ty` reaches, through arrays,
/// slices, fields, payload values, pointees and referents: a reference below
/// the top of a type is never the type of anything. A reference is not looked
/// into past the first one met. The structs and enums in `seen` were looked
/// into already.
fn references_below(ty: Ty, seen: &mut HashSet<*const ()>, found: &mut Vec<Ty>) {
    match ty {
        Ty::Array(elem, _) | Ty::Slice(elem) => reference_part(*elem, seen, found),
        Ty::Ptr(ptr) => reference_part(*ptr.pointee, seen, found),
        Ty::Ref(r) => reference_part(*r.referent, seen, found),
        Ty::Struct(s) => {
            let def = s.def();
            let Some(fields) = def.fields.get() else {
                return;
            };
            if seen.insert(ptr::from_ref(def).cast()) {
                for field in fields {
                    reference_part(field.ty, seen, found);
                }
            }
        }
        Ty::Enum(e) => {
            let def = e.def();
            let Some(variants) = def.variants.get() else {
                return;
            };
            if seen.insert(ptr::from_ref(def).cast()) {
                for value in variants.iter().flat_map(|variant| &variant.payload) {
                    reference_part(value.ty, seen, found);
                }
            }
        }
        Ty::Unit | Ty::Never | Ty::Bool | Ty::Int(_) | Ty::Peripheral(_) => {}
    }
}

/// A part of a type, for `references_below`: found if it is a reference,
/// looked into otherwise.
fn reference_part(part: Ty, seen: &mut HashSet<*const ()>, found: &mut Vec<Ty>) {
    if let Ty::Ref(_) = part {
        found.push(part);
    } else {
        references_below(part, seen, found);
    }
}

/// Whether a static of this type is a type of values (as in T3) of a size:
/// not `!`, `[T]` or a peripheral type, nor an array of the last two.
fn values(ty: Ty) -> bool {
    match ty {
        Ty::Never | Ty::Slice(_) | Ty::Peripheral(_) => false,
        Ty::Array(item, _) => values(*item),
        Ty::Unit
        | Ty::Bool
        | Ty::Int(_)
        | Ty::Ptr(_)
        | Ty::Ref(_)
        | Ty::Struct(_)
        | Ty::Enum(_) => true,
    }
}

/// A place's type, and whether accessing it is volatile.
#[derive(Clone, Copy)]
struct Typed {
    ty: Ty,
    volatile: bool,
}

/// Whether `from` fits a place of type `to` (ADR 0095, "Types"): the same
/// type, or `*mut T` where `*const T` is wanted, ADR 0024's conversion.
fn fits(from: Ty, to: Ty) -> bool {
    match (from, to) {
        (Ty::Ptr(from), Ty::Ptr(to)) => {
            from.pointee == to.pointee
                && from.volatile == to.volatile
                && (from.mutable || !to.mutable)
        }
        _ => from == to,
    }
}

/// Whether accessing the place after `elem` is volatile, as code generation
/// decides it (`address` in `src/codegen/mod.rs`, `Builder::is_volatile`):
/// the last pointer dereferenced sets it, a register of a peripheral type
/// makes it so.
fn volatile_after(from: Typed, elem: &PlaceElem) -> bool {
    match (elem, from.ty) {
        (PlaceElem::Deref | PlaceElem::Offset(_), Ty::Ptr(ptr)) => ptr.volatile,
        // A reference is not volatile (ADR 0105).
        (PlaceElem::Deref, Ty::Ref(_)) => false,
        (
            PlaceElem::Field(_) | PlaceElem::Element(..) | PlaceElem::ConstElement(..),
            Ty::Peripheral(_),
        ) => true,
        _ => from.volatile,
    }
}

/// T1: the type of a place after one more projection, or what is wrong. It
/// reads the definitions without the accessors that panic when something is
/// missing. Every `PlaceElem` is named, so that a new one fails to compile
/// here.
fn project(ty: Ty, elem: &PlaceElem) -> Result<Ty, String> {
    let wrong = || -> Result<Ty, String> { Err(format!("`{elem:?}` does not apply to `{ty}`")) };
    match elem {
        PlaceElem::Deref => match ty {
            Ty::Ptr(ptr) => Ok(*ptr.pointee),
            Ty::Ref(r) => Ok(*r.referent),
            _ => wrong(),
        },
        PlaceElem::Offset(_) => match ty {
            Ty::Ptr(ptr) if matches!(ptr.pointee, Ty::Slice(_) | Ty::Peripheral(_)) => Err(
                format!("`Offset` of `{ty}`, a pointer to a type with no size"),
            ),
            Ty::Ptr(ptr) => Ok(*ptr.pointee),
            _ => wrong(),
        },
        PlaceElem::Index(_) => match ty {
            Ty::Array(item, _) | Ty::Slice(item) => Ok(*item),
            _ => wrong(),
        },
        PlaceElem::ConstIndex(index) => match ty {
            Ty::Array(item, len) if *index < len => Ok(*item),
            Ty::Array(_, len) => Err(format!(
                "`ConstIndex({index})` of `{ty}`, of {len} elements"
            )),
            _ => wrong(),
        },
        PlaceElem::Subslice(..) => match ty {
            Ty::Array(item, _) | Ty::Slice(item) => Ok(Ty::Slice(item)),
            _ => wrong(),
        },
        PlaceElem::ConstSubslice(from, to) => match ty {
            Ty::Array(item, len) if from <= to && *to <= len => Ok(Ty::Slice(item)),
            Ty::Array(_, len) => Err(format!(
                "`ConstSubslice({from}, {to})` of `{ty}`, of {len} elements: it needs `from <= to <= {len}`"
            )),
            _ => wrong(),
        },
        PlaceElem::Field(index) => match ty {
            Ty::Struct(s) => {
                let fields = s.def().fields.get();
                let field = fields.and_then(|fields| fields.get(*index as usize));
                field
                    .map(|field| field.ty)
                    .ok_or_else(|| format!("`Field({index})` of `{ty}`, which has no such field"))
            }
            Ty::Peripheral(p) => match entry(p, *index)? {
                (ty, None) => Ok(ty),
                (_, Some(_)) => Err(format!(
                    "`Field({index})` of `{p:?}`, an entry that is an array"
                )),
            },
            _ => wrong(),
        },
        PlaceElem::Element(entry_index, _) | PlaceElem::ConstElement(entry_index, _) => {
            let Ty::Peripheral(p) = ty else {
                return wrong();
            };
            match entry(p, *entry_index)? {
                (_, None) => Err(format!("`{elem:?}` of `{p:?}`, an entry that is no array")),
                (ty, Some((len, _))) => match elem {
                    PlaceElem::ConstElement(_, index) if *index >= len => {
                        Err(format!("`{elem:?}` of `{p:?}`, an entry of {len} elements"))
                    }
                    _ => Ok(ty),
                },
            }
        }
        PlaceElem::Tag => match ty {
            Ty::Enum(e) => Ok(Ty::Int(e.def().int)),
            _ => wrong(),
        },
        PlaceElem::Payload { variant, index } => match ty {
            Ty::Enum(e) => {
                let variants = e.def().variants.get();
                let value = variants
                    .and_then(|variants| variants.get(*variant as usize))
                    .and_then(|variant| variant.payload.get(*index as usize));
                value.map(|value| value.ty).ok_or_else(|| {
                    format!("`Payload {{ variant: {variant}, index: {index} }}` of `{ty}`, which has no such value")
                })
            }
            _ => wrong(),
        },
    }
}

/// An entry of a peripheral type: its element type, and its length and step
/// if it is an array (ADR 0064).
fn entry(p: PeripheralTypeRef, index: u32) -> Result<(Ty, Option<(u64, u64)>), String> {
    let def = p.def();
    let register = def
        .registers
        .get(index as usize)
        .ok_or_else(|| format!("`{}` has no entry {index}", def.name))?;
    let layout = def
        .layout()
        .ok_or_else(|| format!("`{}` has no layout", def.name))?;
    let array = layout.arrays.get(index as usize).copied().flatten();
    Ok((register.ty, array))
}

/// A stack of open blocks, as `[_2, _5]` (outermost first).
fn stack(open: &[Local]) -> String {
    let locals: Vec<String> = open.iter().map(ToString::to_string).collect();
    format!("[{}]", locals.join(", "))
}

/// The element type of a raw slice, `*const [T]` or `*mut [T]`; a reference
/// to a slice has none, since it fits only its own type (ADR 0109, T5).
fn raw_slice_elem(ty: Ty) -> Option<Ty> {
    match ty {
        Ty::Ptr(_) => ty.slice_elem(),
        _ => None,
    }
}

/// A pointer to a sized type, or to a slice's elements: one address.
fn thin(ptr: &Ptr) -> bool {
    !matches!(ptr.pointee, Ty::Slice(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::{
        AsmOperand, AsmOptions, AsmReg, BasicBlockData, DataId, DefaultHandler, EnumId, EnumRef,
        Field, Handler, Layout, LocalDecl, PanicSite, PeripheralLayout, Ref, Register, Static,
        StaticId, StructRef, Variant, VectorTable,
    };
    use crate::span::{SourceMap, Span};

    fn span() -> Span {
        let file = SourceMap::default().add("main.ers", "");
        Span::new(file, 0, 0)
    }

    fn decl(ty: Ty) -> LocalDecl {
        LocalDecl {
            ty,
            name: None,
            constant: None,
        }
    }

    fn block(statements: Vec<Statement>, terminator: Terminator) -> BasicBlockData {
        BasicBlockData {
            spans: vec![span(); statements.len()],
            statements,
            terminator,
            terminator_span: span(),
        }
    }

    fn local(n: u32) -> Place {
        Place::local(Local(n))
    }

    fn copy(n: u32) -> Operand {
        Operand::Copy(local(n))
    }

    fn u32_ty() -> Ty {
        Ty::Int(IntTy::U32)
    }

    fn reference(mutable: bool, referent: Ty) -> Ty {
        Ty::Ref(Ref {
            mutable,
            referent: Box::leak(Box::new(referent)),
        })
    }

    fn usize_ty() -> Ty {
        Ty::Int(IntTy::Usize)
    }

    /// A pointer type.
    fn ptr(mutable: bool, volatile: bool, pointee: Ty) -> Ty {
        Ty::Ptr(crate::mir::Ptr {
            mutable,
            volatile,
            pointee: Box::leak(Box::new(pointee)),
        })
    }

    /// A place: a local followed by projections.
    fn projected(local: u32, projection: &[PlaceElem]) -> Place {
        Place {
            local: Local(local),
            projection: Box::leak(projection.to_vec().into_boxed_slice()),
        }
    }

    /// A peripheral type with a register `_0: u32` and a register array
    /// `_1: [u32; 4]`, laid out.
    fn peripheral() -> PeripheralTypeRef {
        let register = |name: &str| Register {
            name: name.to_owned(),
            ty: u32_ty(),
            group: None,
        };
        let p = PeripheralTypeRef::new("P".to_owned(), vec![register("a"), register("b")]);
        p.def().set_layout(PeripheralLayout {
            offsets: vec![0, 4],
            arrays: vec![None, Some((4, 4))],
            extent: 20,
            align: 4,
        });
        p
    }

    /// A local number that `valid` has no local for.
    const GHOST: u32 = 50;

    /// `fn f(_1: u32) -> u32`, with `_2: u32` as a constant local, `_3: u32`
    /// as a variable, `_4: *mut u32`, `_5: [u32; 4]`, `_6: *mut u32` as a
    /// constant local, `_7: *const u32`, `_8: usize`, `_9: *mut P` of a
    /// peripheral type and `_10: *mut [u32]`: `bb0` assigns `_0 = copy _1`
    /// and goes to `bb1`, which returns. No local has the number `GHOST`.
    fn valid() -> Body {
        let constant = |ty| LocalDecl {
            ty,
            name: None,
            constant: Some(DataId(0)),
        };
        Body {
            name: "f".to_owned(),
            span: span(),
            comptime: false,
            ret: u32_ty(),
            param_count: 1,
            locals: vec![
                decl(u32_ty()),
                decl(u32_ty()),
                constant(u32_ty()),
                decl(u32_ty()),
                decl(ptr(true, false, u32_ty())),
                decl(Ty::Array(Box::leak(Box::new(u32_ty())), 4)),
                constant(ptr(true, false, u32_ty())),
                decl(ptr(false, false, u32_ty())),
                decl(usize_ty()),
                decl(ptr(true, false, Ty::Peripheral(peripheral()))),
                decl(ptr(true, false, Ty::Slice(Box::leak(Box::new(u32_ty()))))),
            ],
            blocks: vec![
                block(
                    vec![Statement::Assign(local(0), Rvalue::Use(copy(1)))],
                    Terminator::Goto(BasicBlock(1)),
                ),
                block(Vec::new(), Terminator::Return),
            ],
            runtime: false,
            external: false,
            naked: false,
            align: None,
        }
    }

    fn asm(options: AsmOptions, pieces: Vec<AsmPiece>, operands: Vec<AsmOperand>) -> InlineAsm {
        InlineAsm {
            pieces,
            operands,
            clobbers: Vec::new(),
            options,
            span: span(),
        }
    }

    fn noreturn() -> AsmOptions {
        AsmOptions {
            noreturn: true,
            ..AsmOptions::default()
        }
    }

    fn naked() -> Body {
        let mut body = valid();
        body.ret = Ty::Unit;
        body.param_count = 0;
        body.locals = vec![decl(Ty::Unit)];
        body.naked = true;
        body.blocks = vec![block(
            Vec::new(),
            Terminator::InlineAsm(Box::leak(Box::new(asm(noreturn(), Vec::new(), Vec::new())))),
        )];
        body
    }

    fn external() -> Body {
        let mut body = valid();
        body.external = true;
        body.locals.truncate(2);
        body.blocks.clear();
        body
    }

    /// The rules of the violations of `body`, in order.
    fn rules(body: &Body) -> Vec<&'static str> {
        check_body(body).iter().map(|v| v.rule).collect()
    }

    /// The body breaks `rule` once and nothing else.
    #[track_caller]
    fn breaks_only(body: &Body, rule: &str) {
        assert_eq!(rules(body), [rule], "{:#?}", check_body(body));
    }

    fn statement_of(body: &mut Body, statement: Statement) {
        body.blocks[0].statements.push(statement);
        body.blocks[0].spans.push(span());
    }

    #[test]
    fn a_valid_body_reports_nothing() {
        assert!(check_body(&valid()).is_empty());
        assert!(check_body(&naked()).is_empty());
        assert!(check_body(&external()).is_empty());
    }

    #[test]
    fn s1_locals_hold_the_return_place_and_the_parameters() {
        let mut body = valid();
        body.ret = Ty::Bool;
        breaks_only(&body, "S1");

        let mut body = valid();
        body.param_count = 20;
        breaks_only(&body, "S1");

        let mut body = valid();
        body.locals.clear();
        body.blocks = vec![block(Vec::new(), Terminator::Return)];
        body.param_count = 0;
        breaks_only(&body, "S1");
    }

    #[test]
    fn s2_an_external_body_has_no_blocks_and_no_other_locals() {
        let mut body = external();
        body.blocks = vec![block(Vec::new(), Terminator::Return)];
        breaks_only(&body, "S2");

        let mut body = external();
        body.locals.push(decl(u32_ty()));
        breaks_only(&body, "S2");

        let mut body = valid();
        body.blocks.clear();
        breaks_only(&body, "S2");
    }

    #[test]
    fn s3_a_naked_body_is_one_asm_terminator() {
        let mut body = naked();
        body.blocks.push(block(Vec::new(), Terminator::Return));
        breaks_only(&body, "S3");

        let mut body = naked();
        statement_of(
            &mut body,
            Statement::Assign(local(0), Rvalue::Use(Operand::Const(Const::Unit))),
        );
        breaks_only(&body, "S3");

        let mut body = naked();
        body.blocks[0].terminator = Terminator::Return;
        breaks_only(&body, "S3");

        // Without any block, S2 speaks and S3 does not repeat it.
        let mut body = naked();
        body.blocks.clear();
        breaks_only(&body, "S2");

        // That the assembly lacks `noreturn` is S9's alone.
        let mut body = naked();
        body.blocks[0].terminator = Terminator::InlineAsm(Box::leak(Box::new(asm(
            AsmOptions::default(),
            vec![],
            vec![],
        ))));
        breaks_only(&body, "S9");
    }

    #[test]
    fn s4_every_block_has_a_span_per_statement() {
        let mut body = valid();
        body.blocks[0].spans.clear();
        breaks_only(&body, "S4");
        body.blocks[0].spans = vec![span(); 3];
        breaks_only(&body, "S4");
    }

    #[test]
    fn s5_a_terminator_names_blocks_that_exist_and_not_the_start() {
        for target in [BasicBlock(2), BasicBlock(0)] {
            let mut body = valid();
            body.blocks[0].terminator = Terminator::Goto(target);
            breaks_only(&body, "S5");
        }
        let mut body = valid();
        body.blocks[0].terminator = Terminator::If {
            cond: Operand::Const(Const::Bool(true)),
            then: BasicBlock(1),
            else_: BasicBlock(7),
        };
        breaks_only(&body, "S5");
        let mut body = valid();
        body.blocks[0].terminator = Terminator::Switch {
            discr: copy(1),
            cases: Box::leak(Box::new([(0, BasicBlock(1)), (1, BasicBlock(9))])),
            otherwise: BasicBlock(1),
        };
        breaks_only(&body, "S5");
        let mut body = valid();
        body.blocks[0].terminator = Terminator::Switch {
            discr: copy(1),
            cases: &[],
            otherwise: BasicBlock(0),
        };
        breaks_only(&body, "S5");
    }

    #[test]
    fn s6_a_body_returning_never_has_no_return() {
        let mut body = valid();
        body.ret = Ty::Never;
        body.locals[0].ty = Ty::Never;
        body.blocks[0].statements.clear();
        body.blocks[0].spans.clear();
        breaks_only(&body, "S6");
        body.blocks[1].terminator = Terminator::Unreachable;
        assert!(check_body(&body).is_empty());
    }

    #[test]
    fn s7_every_local_named_exists() {
        let ghost = GHOST;
        let cases = [
            // The base of a place, written and read.
            Statement::Assign(local(ghost), Rvalue::Use(copy(1))),
            Statement::Assign(local(3), Rvalue::Use(copy(ghost))),
            Statement::Assign(local(3), Rvalue::SliceLen(local(ghost))),
            // The index of each projection that has one.
            Statement::Assign(
                local(3),
                Rvalue::Use(Operand::Copy(projected(
                    4,
                    &[PlaceElem::Offset(Local(ghost))],
                ))),
            ),
            Statement::Assign(
                local(3),
                Rvalue::Use(Operand::Copy(projected(
                    5,
                    &[PlaceElem::Index(Local(ghost))],
                ))),
            ),
            Statement::Assign(
                local(3),
                Rvalue::Use(Operand::Copy(projected(
                    9,
                    &[PlaceElem::Deref, PlaceElem::Element(1, Local(ghost))],
                ))),
            ),
            // Either bound of a sub-slice, followed by the element that makes
            // the `[T]` in between a place that can be read.
            Statement::Assign(
                local(3),
                Rvalue::Use(Operand::Copy(projected(
                    5,
                    &[
                        PlaceElem::Subslice(Local(ghost), Local(8)),
                        PlaceElem::Index(Local(8)),
                    ],
                ))),
            ),
            Statement::Assign(
                local(3),
                Rvalue::Use(Operand::Copy(projected(
                    5,
                    &[
                        PlaceElem::Subslice(Local(8), Local(ghost)),
                        PlaceElem::Index(Local(8)),
                    ],
                ))),
            ),
            Statement::Call {
                dest: Some(local(ghost)),
                callee: BodyId(0),
                args: Vec::new(),
            },
            // Every operand of a statement: a call's arguments, the
            // operands of each rvalue, the inputs of an `asm`.
            Statement::Call {
                dest: None,
                callee: BodyId(0),
                args: vec![copy(1), copy(ghost)],
            },
            Statement::Assign(local(3), Rvalue::Unary(crate::ops::UnOp::Neg, copy(ghost))),
            Statement::Assign(
                local(3),
                Rvalue::Binary(crate::ops::BinOp::Add, copy(ghost), copy(1)),
            ),
            Statement::Assign(
                local(3),
                Rvalue::Binary(crate::ops::BinOp::Add, copy(1), copy(ghost)),
            ),
            Statement::Assign(local(3), Rvalue::Cast(copy(ghost), u32_ty())),
            Statement::Assign(
                local(3),
                Rvalue::Slice {
                    ptr: copy(ghost),
                    len: copy(8),
                },
            ),
            Statement::Assign(
                local(10),
                Rvalue::Slice {
                    ptr: copy(4),
                    len: copy(ghost),
                },
            ),
            Statement::Assign(local(3), Rvalue::SliceAddress(local(ghost))),
            Statement::Assign(
                local(3),
                Rvalue::AddressOf {
                    mutable: false,
                    place: local(ghost),
                },
            ),
            Statement::InlineAsm(Box::new(asm(
                AsmOptions::default(),
                Vec::new(),
                vec![AsmOperand {
                    reg: AsmReg::Any,
                    kind: AsmOperandKind::In(copy(ghost)),
                }],
            ))),
            Statement::InlineAsm(Box::new(asm(
                AsmOptions::default(),
                Vec::new(),
                vec![AsmOperand {
                    reg: AsmReg::Any,
                    kind: AsmOperandKind::InOut {
                        input: copy(ghost),
                        place: None,
                    },
                }],
            ))),
        ];
        for statement in cases {
            let mut body = valid();
            statement_of(&mut body, statement);
            breaks_only(&body, "S7");
        }
        // The local of a `critical` statement: an enter alone would leave a
        // block open at the `return` (C3), so the two go together, one
        // violation for each.
        let mut body = valid();
        statement_of(&mut body, Statement::CriticalEnter(Local(ghost)));
        statement_of(&mut body, Statement::CriticalExit(Local(ghost)));
        assert_eq!(rules(&body), ["S7", "S7"]);
        // The operands of a terminator.
        let terminators = [
            Terminator::If {
                cond: copy(ghost),
                then: BasicBlock(1),
                else_: BasicBlock(1),
            },
            Terminator::Switch {
                discr: copy(ghost),
                cases: &[],
                otherwise: BasicBlock(1),
            },
            Terminator::Panic(
                PanicCause::AssertionFailed(Some((copy(ghost), span()))),
                None,
            ),
            Terminator::InlineAsm(Box::leak(Box::new(asm(
                noreturn(),
                Vec::new(),
                vec![AsmOperand {
                    reg: AsmReg::Any,
                    kind: AsmOperandKind::In(copy(ghost)),
                }],
            )))),
        ];
        for terminator in terminators {
            let mut body = valid();
            body.blocks[0].terminator = terminator;
            breaks_only(&body, "S7");
        }
        let site = |info| PanicSite {
            info,
            file: StaticRef::new(StaticId(0), "f".to_owned(), Ty::Unit, false, true),
            line: 1,
            column: 1,
        };
        let mut body = valid();
        body.blocks[1].terminator = Terminator::Panic(
            PanicCause::DivisionByZero,
            Some(site(PanicInfoAt::Local(Local(ghost)))),
        );
        breaks_only(&body, "S7");
        // A data id is not a local.
        body.blocks[1].terminator = Terminator::Panic(
            PanicCause::DivisionByZero,
            Some(site(PanicInfoAt::Data(DataId(ghost)))),
        );
        assert!(check_body(&body).is_empty());
    }

    #[test]
    fn s7_does_not_cascade_into_s8() {
        // The ghost local has no declaration to be constant.
        let mut body = valid();
        statement_of(
            &mut body,
            Statement::Assign(
                local(GHOST),
                Rvalue::AddressOf {
                    mutable: true,
                    place: local(GHOST),
                },
            ),
        );
        let found = check_body(&body);
        assert_eq!(found.len(), 2, "{found:#?}");
        assert!(found.iter().all(|v| v.rule == "S7"));
    }

    #[test]
    fn s8_a_constant_local_is_never_written() {
        let out = |place| AsmOperand {
            reg: AsmReg::Any,
            kind: AsmOperandKind::Out {
                late: false,
                place: Some(place),
            },
        };
        let inout = |place| AsmOperand {
            reg: AsmReg::Any,
            kind: AsmOperandKind::InOut {
                input: Operand::Const(Const::Int(0, IntTy::U32)),
                place: Some(place),
            },
        };
        let statements = [
            Statement::Assign(local(2), Rvalue::Use(copy(1))),
            Statement::Call {
                dest: Some(local(2)),
                callee: BodyId(0),
                args: Vec::new(),
            },
            Statement::InlineAsm(Box::new(asm(
                AsmOptions::default(),
                Vec::new(),
                vec![out(local(2))],
            ))),
            Statement::InlineAsm(Box::new(asm(
                AsmOptions::default(),
                Vec::new(),
                vec![inout(local(2))],
            ))),
            Statement::Assign(
                local(4),
                Rvalue::AddressOf {
                    mutable: true,
                    place: local(2),
                },
            ),
        ];
        for statement in statements {
            let mut body = valid();
            statement_of(&mut body, statement);
            breaks_only(&body, "S8");
        }

        // Read, taken as `&raw const`, or written through a pointer it holds:
        // none changes the local.
        let through = |elem| projected(6, &[elem]);
        let mut body = valid();
        statement_of(&mut body, Statement::Assign(local(3), Rvalue::Use(copy(2))));
        statement_of(
            &mut body,
            Statement::Assign(
                local(7),
                Rvalue::AddressOf {
                    mutable: false,
                    place: local(2),
                },
            ),
        );
        statement_of(
            &mut body,
            Statement::Assign(through(PlaceElem::Deref), Rvalue::Use(copy(1))),
        );
        statement_of(
            &mut body,
            Statement::Assign(through(PlaceElem::Offset(Local(8))), Rvalue::Use(copy(1))),
        );
        statement_of(
            &mut body,
            Statement::Assign(
                local(4),
                Rvalue::AddressOf {
                    mutable: true,
                    place: through(PlaceElem::Deref),
                },
            ),
        );
        assert!(check_body(&body).is_empty(), "{:#?}", check_body(&body));
    }

    #[test]
    fn s9_asm_noreturn_is_a_terminator_and_placeholders_name_operands() {
        let mut body = valid();
        statement_of(
            &mut body,
            Statement::InlineAsm(Box::new(asm(noreturn(), Vec::new(), Vec::new()))),
        );
        breaks_only(&body, "S9");

        let mut body = valid();
        body.blocks[1].terminator = Terminator::InlineAsm(Box::leak(Box::new(asm(
            AsmOptions::default(),
            Vec::new(),
            Vec::new(),
        ))));
        breaks_only(&body, "S9");

        let mut body = valid();
        statement_of(
            &mut body,
            Statement::InlineAsm(Box::new(asm(
                AsmOptions::default(),
                vec![AsmPiece::Text("mv ".to_owned()), AsmPiece::Operand(0)],
                Vec::new(),
            ))),
        );
        breaks_only(&body, "S9");
    }

    #[test]
    fn the_violations_are_in_the_fixed_order() {
        // S5 in the terminator, S7 in the statement, S4 in the block: listed
        // by place in the body, the block first, the terminator last.
        let mut body = valid();
        statement_of(&mut body, Statement::CriticalEnter(Local(GHOST)));
        body.blocks[0].spans.pop();
        body.blocks[0].terminator = Terminator::Goto(BasicBlock(4));
        body.ret = Ty::Bool;
        let found = check_body(&body);
        let rules: Vec<_> = found.iter().map(|v| (v.at, v.rule)).collect();
        assert_eq!(
            rules,
            [
                (Location::Body, "S1"),
                (Location::Block(BasicBlock(0)), "S4"),
                (Location::Statement(BasicBlock(0), 1), "S7"),
                (Location::Terminator(BasicBlock(0)), "S5"),
            ]
        );
    }

    #[test]
    #[should_panic(
        expected = "invalid MIR, a bug of the compiler (ADR 0095): 3 violations\n  \
        `f`, bb0: S4: 2 statements but 1 spans\n  \
        `f`, bb0, statement 1: S7: a `critical` statement names `_50`, but the body has 11 locals\n  \
        `f`, bb0, terminator: S5: a terminator jumps to bb4, but the body has 2 blocks"
    )]
    fn the_panic_lists_every_violation() {
        let mut body = valid();
        statement_of(&mut body, Statement::CriticalEnter(Local(GHOST)));
        body.blocks[0].spans.pop();
        body.blocks[0].terminator = Terminator::Goto(BasicBlock(4));
        assert_valid_body(&body);
    }

    #[test]
    fn a_valid_body_does_not_panic_and_is_counted() {
        let before = bodies_checked();
        assert_valid_body(&valid());
        assert_eq!(bodies_checked(), before + 1);
    }

    // ---- Types (ADR 0095, T1 to T9) ----

    /// `struct Pair { a: u32, b: u32 }`.
    fn pair() -> StructRef {
        let s = StructRef::new(
            "Pair".to_owned(),
            Layout { size: 8, align: 4 },
            false,
            true,
            false,
        );
        let field = |name: &str, offset| Field {
            name: name.to_owned(),
            ty: u32_ty(),
            offset,
        };
        s.def().set_fields(vec![field("a", 0), field("b", 4)]);
        s
    }

    /// `enum E { None, Some(u32) }`, with a `u8` tag.
    fn option() -> EnumRef {
        let e = EnumRef::new(
            EnumId(0),
            "E".to_owned(),
            IntTy::U8,
            Layout { size: 8, align: 4 },
            true,
        );
        e.def().set_variants(vec![
            Variant {
                name: "None".to_owned(),
                value: std::sync::OnceLock::from(0),
                payload: Vec::new(),
            },
            Variant {
                name: "Some".to_owned(),
                value: std::sync::OnceLock::from(1),
                payload: vec![Field {
                    name: "0".to_owned(),
                    ty: u32_ty(),
                    offset: 4,
                }],
            },
        ]);
        e
    }

    /// `struct Holder { q: *mut Pair }`.
    fn holder() -> StructRef {
        let s = StructRef::new(
            "Holder".to_owned(),
            Layout { size: 4, align: 4 },
            false,
            true,
            false,
        );
        s.def().set_fields(vec![Field {
            name: "q".to_owned(),
            ty: ptr(true, false, Ty::Struct(pair())),
            offset: 0,
        }]);
        s
    }

    // The locals `typed` adds to `valid`.
    const BOOL: u32 = 11;
    const I32: u32 = 12;
    const U8: u32 = 13;
    const SLICE_C: u32 = 14;
    const SLICE_M: u32 = 15;
    const ARRAY_PTR: u32 = 16;
    const U64: u32 = 17;

    /// `valid` with `_11: bool`, `_12: i32`, `_13: u8`, `_14: *const [u8]`,
    /// `_15: *mut [u8]`, `_16: *const [u8; 4]` and `_17: u64`.
    fn typed() -> Body {
        let mut body = valid();
        let bytes = Ty::Int(IntTy::U8);
        let slice = |mutable| ptr(mutable, false, Ty::Slice(Box::leak(Box::new(bytes))));
        body.locals.extend([
            decl(Ty::Bool),
            decl(Ty::Int(IntTy::I32)),
            decl(bytes),
            decl(slice(false)),
            decl(slice(true)),
            decl(ptr(false, false, Ty::Array(Box::leak(Box::new(bytes)), 4))),
            decl(Ty::Int(IntTy::U64)),
        ]);
        body
    }

    /// The rules broken by a call that passes `projection` of a new local of
    /// type `ty`, read whole.
    fn reading(ty: Ty, projection: &[PlaceElem]) -> Vec<&'static str> {
        let mut body = valid();
        body.locals.push(decl(ty));
        let n = body.locals.len() as u32 - 1;
        statement_of(
            &mut body,
            Statement::Call {
                dest: None,
                callee: BodyId(0),
                args: vec![Operand::Copy(projected(n, projection))],
            },
        );
        rules(&body)
    }

    /// The rules broken by assigning `rvalue` to a new local of type `dest`,
    /// in a body that has the locals of `typed`.
    fn assigning(dest: Ty, rvalue: Rvalue) -> Vec<&'static str> {
        let mut body = typed();
        body.locals.push(decl(dest));
        let n = body.locals.len() as u32 - 1;
        statement_of(&mut body, Statement::Assign(local(n), rvalue));
        rules(&body)
    }

    #[track_caller]
    fn accepts(rules: Vec<&'static str>) {
        assert_eq!(rules, Vec::<&str>::new());
    }

    #[track_caller]
    fn rejects(rules: Vec<&'static str>, rule: &str) {
        assert_eq!(rules, [rule]);
    }

    #[test]
    fn t1_every_projection_applies_to_the_type_before_it() {
        let usize_local = Local(8);
        let array = Ty::Array(Box::leak(Box::new(u32_ty())), 4);
        let pair = Ty::Struct(pair());
        let option = Ty::Enum(option());
        let periph = Ty::Peripheral(peripheral());
        let read = |ty: Ty, projection: &[PlaceElem], rule: Option<&str>| {
            let found = reading(ty, projection);
            assert_eq!(found, rule.as_slice(), "{ty}: {projection:?}");
        };

        // `Deref`: to a pointer, thin or slice.
        read(ptr(true, false, u32_ty()), &[PlaceElem::Deref], None);
        read(u32_ty(), &[PlaceElem::Deref], Some("T1"));
        // `Deref` of a reference gives its referent; `Offset` does not apply to
        // one (it is pointer arithmetic).
        read(reference(false, u32_ty()), &[PlaceElem::Deref], None);
        read(reference(true, u32_ty()), &[PlaceElem::Deref], None);
        read(
            reference(true, u32_ty()),
            &[PlaceElem::Offset(usize_local)],
            Some("T1"),
        );
        // `Offset`: to a thin pointer to a sized type.
        let offset = [PlaceElem::Offset(usize_local)];
        read(ptr(true, false, u32_ty()), &offset, None);
        read(u32_ty(), &offset, Some("T1"));
        let slice = Ty::Slice(Box::leak(Box::new(u32_ty())));
        read(ptr(true, false, slice), &offset, Some("T1"));
        read(ptr(true, false, periph), &offset, Some("T1"));
        // `Index` and `ConstIndex`: to an array, the second below its length.
        read(array, &[PlaceElem::Index(usize_local)], None);
        read(u32_ty(), &[PlaceElem::Index(usize_local)], Some("T1"));
        read(array, &[PlaceElem::ConstIndex(3)], None);
        read(array, &[PlaceElem::ConstIndex(4)], Some("T1"));
        read(u32_ty(), &[PlaceElem::ConstIndex(0)], Some("T1"));
        // `Field`: to a struct with that field, or to a peripheral type whose
        // entry is no array.
        read(pair, &[PlaceElem::Field(1)], None);
        read(pair, &[PlaceElem::Field(2)], Some("T1"));
        read(u32_ty(), &[PlaceElem::Field(0)], Some("T1"));
        read(periph, &[PlaceElem::Field(0)], None);
        read(periph, &[PlaceElem::Field(1)], Some("T1"));
        read(periph, &[PlaceElem::Field(2)], Some("T1"));
        // `Element` and `ConstElement`: to a peripheral type whose entry is
        // an array, the second below its length.
        read(periph, &[PlaceElem::Element(1, usize_local)], None);
        read(periph, &[PlaceElem::Element(0, usize_local)], Some("T1"));
        read(periph, &[PlaceElem::Element(2, usize_local)], Some("T1"));
        read(u32_ty(), &[PlaceElem::Element(0, usize_local)], Some("T1"));
        read(periph, &[PlaceElem::ConstElement(1, 3)], None);
        read(periph, &[PlaceElem::ConstElement(1, 4)], Some("T1"));
        read(periph, &[PlaceElem::ConstElement(0, 0)], Some("T1"));
        read(u32_ty(), &[PlaceElem::ConstElement(0, 0)], Some("T1"));
        // A peripheral type without a layout cannot tell its arrays.
        let unlaid = Ty::Peripheral(PeripheralTypeRef::new(
            "Q".to_owned(),
            vec![Register {
                name: "a".to_owned(),
                ty: u32_ty(),
                group: None,
            }],
        ));
        read(unlaid, &[PlaceElem::Field(0)], Some("T1"));
        // `Tag` and `Payload`: to an enum with payloads, with that variant
        // and payload value.
        read(option, &[PlaceElem::Tag], None);
        read(u32_ty(), &[PlaceElem::Tag], Some("T1"));
        let payload = |variant, index| [PlaceElem::Payload { variant, index }];
        read(option, &payload(1, 0), None);
        read(option, &payload(2, 0), Some("T1"));
        read(option, &payload(1, 1), Some("T1"));
        read(option, &payload(0, 0), Some("T1"));
        read(u32_ty(), &payload(0, 0), Some("T1"));
        // A struct whose fields were never set.
        let unset = StructRef::new(
            "U".to_owned(),
            Layout { size: 0, align: 1 },
            false,
            true,
            false,
        );
        read(Ty::Struct(unset), &[PlaceElem::Field(0)], Some("T1"));

        // The walk stops at the first projection that does not apply.
        read(
            u32_ty(),
            &[PlaceElem::Deref, PlaceElem::Deref, PlaceElem::Field(9)],
            Some("T1"),
        );
        // And a projection can apply after others.
        read(
            ptr(true, false, option),
            &[
                PlaceElem::Deref,
                PlaceElem::Payload {
                    variant: 1,
                    index: 0,
                },
            ],
            None,
        );
    }

    #[test]
    fn t1_does_not_cascade_into_the_rules_of_the_type() {
        // The place has no type, so neither T3, T4 nor T5 can say anything.
        let mut body = valid();
        statement_of(
            &mut body,
            Statement::Assign(
                local(3),
                Rvalue::Use(Operand::Copy(projected(1, &[PlaceElem::Deref]))),
            ),
        );
        breaks_only(&body, "T1");
        let mut body = valid();
        statement_of(
            &mut body,
            Statement::Assign(projected(3, &[PlaceElem::Tag]), Rvalue::Use(copy(1))),
        );
        breaks_only(&body, "T1");
    }

    #[test]
    fn t2_an_index_is_a_usize_and_a_critical_local_a_u32() {
        let wrong = Local(1);
        let places = [
            projected(4, &[PlaceElem::Offset(wrong)]),
            projected(5, &[PlaceElem::Index(wrong)]),
            projected(9, &[PlaceElem::Deref, PlaceElem::Element(1, wrong)]),
        ];
        for place in places {
            let mut body = valid();
            statement_of(
                &mut body,
                Statement::Call {
                    dest: None,
                    callee: BodyId(0),
                    args: vec![Operand::Copy(place)],
                },
            );
            breaks_only(&body, "T2");
        }
        // An enter and its exit (an enter alone is left open, C3), the local
        // wrong in one of them or in both.
        let critical = |enter: u32, exit: u32| {
            let mut body = valid();
            statement_of(&mut body, Statement::CriticalEnter(Local(enter)));
            statement_of(&mut body, Statement::CriticalExit(Local(exit)));
            body
        };
        assert_eq!(rules(&critical(8, 8)), ["T2", "T2"]);
        assert!(check_body(&critical(1, 1)).is_empty());
        assert!(check_body(&critical(3, 3)).is_empty());
    }

    #[test]
    fn t3_a_place_read_or_written_whole_has_a_value() {
        let slice = Ty::Slice(Box::leak(Box::new(u32_ty())));
        // `[T]` and a peripheral type, read through a pointer.
        rejects(reading(ptr(true, false, slice), &[PlaceElem::Deref]), "T3");
        let periph = Ty::Peripheral(peripheral());
        rejects(reading(ptr(true, false, periph), &[PlaceElem::Deref]), "T3");
        // `!`, read and written whole.
        rejects(reading(Ty::Never, &[]), "T3");
        let mut body = valid();
        body.locals.push(decl(Ty::Never));
        statement_of(
            &mut body,
            Statement::Assign(local(11), Rvalue::Use(Operand::Const(Const::Unit))),
        );
        breaks_only(&body, "T3");
        // Written through a pointer, as the destination of a call, and as an
        // `asm` output.
        let mut body = valid();
        statement_of(
            &mut body,
            Statement::Assign(projected(10, &[PlaceElem::Deref]), Rvalue::Use(copy(1))),
        );
        breaks_only(&body, "T3");
        let mut body = valid();
        body.locals.push(decl(Ty::Never));
        statement_of(
            &mut body,
            Statement::Call {
                dest: Some(local(11)),
                callee: BodyId(0),
                args: Vec::new(),
            },
        );
        breaks_only(&body, "T3");
        let mut body = valid();
        statement_of(
            &mut body,
            Statement::InlineAsm(Box::new(asm(
                AsmOptions::default(),
                Vec::new(),
                vec![AsmOperand {
                    reg: AsmReg::Any,
                    kind: AsmOperandKind::Out {
                        late: false,
                        place: Some(projected(10, &[PlaceElem::Deref])),
                    },
                }],
            ))),
        );
        breaks_only(&body, "T3");
        // `()` is a value.
        accepts(reading(Ty::Unit, &[]));
    }

    #[test]
    fn t4_a_volatile_place_read_or_written_whole_is_no_aggregate() {
        let pair = Ty::Struct(pair());
        let array = Ty::Array(Box::leak(Box::new(u32_ty())), 4);
        let slice = ptr(true, false, Ty::Slice(Box::leak(Box::new(u32_ty()))));
        let deref = [PlaceElem::Deref];
        for pointee in [pair, array, Ty::Enum(option()), slice] {
            rejects(reading(ptr(true, true, pointee), &deref), "T4");
            // Not volatile, it is a value like any other.
            accepts(reading(ptr(true, false, pointee), &deref));
        }
        // Written whole: `*p = x`, with `x` of the pointee's type.
        let mut body = valid();
        body.locals.push(decl(pair));
        body.locals.push(decl(ptr(true, true, pair)));
        statement_of(
            &mut body,
            Statement::Assign(projected(12, &deref), Rvalue::Use(copy(11))),
        );
        breaks_only(&body, "T4");
        // A scalar, `()` and a pointer are values to access.
        accepts(reading(ptr(true, true, u32_ty()), &deref));
        accepts(reading(ptr(true, true, Ty::Unit), &deref));
        accepts(reading(ptr(true, true, ptr(true, false, u32_ty())), &deref));
        // A register is accessed volatile, and is no aggregate.
        accepts(reading(
            ptr(true, false, Ty::Peripheral(peripheral())),
            &[PlaceElem::Deref, PlaceElem::Field(0)],
        ));
        // The last pointer decides: `*(*p).q` with `p` volatile and `q` not.
        let holder = Ty::Struct(holder());
        accepts(reading(
            ptr(true, true, holder),
            &[PlaceElem::Deref, PlaceElem::Field(0), PlaceElem::Deref],
        ));
        // `(*p).q`, the pointer itself, is read through a volatile one.
        accepts(reading(
            ptr(true, true, holder),
            &[PlaceElem::Deref, PlaceElem::Field(0)],
        ));
        // And a volatile pointer after a non-volatile one makes it volatile.
        let volatile = StructRef::new(
            "VH".to_owned(),
            Layout { size: 4, align: 4 },
            false,
            true,
            false,
        );
        volatile.def().set_fields(vec![Field {
            name: "q".to_owned(),
            ty: ptr(true, true, pair),
            offset: 0,
        }]);
        rejects(
            reading(
                ptr(true, false, Ty::Struct(volatile)),
                &[PlaceElem::Deref, PlaceElem::Field(0), PlaceElem::Deref],
            ),
            "T4",
        );
    }

    #[test]
    fn t5_use() {
        let u = u32_ty();
        accepts(assigning(u, Rvalue::Use(copy(1))));
        rejects(assigning(Ty::Bool, Rvalue::Use(copy(1))), "T5");
        // ADR 0024: `*mut T` where `*const T` is wanted, and not the reverse.
        accepts(assigning(ptr(false, false, u), Rvalue::Use(copy(4))));
        rejects(assigning(ptr(true, false, u), Rvalue::Use(copy(7))), "T5");
        // Another pointee, or another volatility.
        rejects(
            assigning(ptr(true, false, Ty::Bool), Rvalue::Use(copy(4))),
            "T5",
        );
        rejects(assigning(ptr(true, true, u), Rvalue::Use(copy(4))), "T5");
        // A slice goes to a slice of the same elements, whatever the
        // mutability and volatility (ADR 0031), and to no other.
        let bytes = Ty::Int(IntTy::U8);
        let slice =
            |mutable, volatile, elem| ptr(mutable, volatile, Ty::Slice(Box::leak(Box::new(elem))));
        accepts(assigning(
            slice(false, true, bytes),
            Rvalue::Use(copy(SLICE_M)),
        ));
        accepts(assigning(
            slice(true, false, bytes),
            Rvalue::Use(copy(SLICE_C)),
        ));
        rejects(
            assigning(slice(false, false, u), Rvalue::Use(copy(SLICE_C))),
            "T5",
        );
        rejects(assigning(u, Rvalue::Use(copy(SLICE_C))), "T5");
        // Constants: an integer, `()`, and the address of a static.
        let int = Operand::Const(Const::Int(7, IntTy::U32));
        accepts(assigning(u, Rvalue::Use(int)));
        rejects(assigning(Ty::Bool, Rvalue::Use(int)), "T5");
        accepts(assigning(
            Ty::Unit,
            Rvalue::Use(Operand::Const(Const::Unit)),
        ));
        let s = StaticRef::new(StaticId(0), "S".to_owned(), u, true, false);
        let address = Operand::Const(Const::Static(s));
        accepts(assigning(ptr(true, false, u), Rvalue::Use(address)));
        accepts(assigning(ptr(false, false, u), Rvalue::Use(address)));
        rejects(
            assigning(ptr(true, false, Ty::Bool), Rvalue::Use(address)),
            "T5",
        );
        // A `static` that is not `mut` has a `*const` address.
        let read_only = StaticRef::new(StaticId(1), "R".to_owned(), u, false, false);
        let address = Operand::Const(Const::Static(read_only));
        accepts(assigning(ptr(false, false, u), Rvalue::Use(address)));
        rejects(assigning(ptr(true, false, u), Rvalue::Use(address)), "T5");
    }

    #[test]
    fn t5_unary() {
        accepts(assigning(
            Ty::Int(IntTy::I32),
            Rvalue::Unary(UnOp::Neg, copy(I32)),
        ));
        rejects(assigning(u32_ty(), Rvalue::Unary(UnOp::Neg, copy(1))), "T5");
        rejects(
            assigning(Ty::Bool, Rvalue::Unary(UnOp::Neg, copy(BOOL))),
            "T5",
        );
        accepts(assigning(Ty::Bool, Rvalue::Unary(UnOp::Not, copy(BOOL))));
        accepts(assigning(u32_ty(), Rvalue::Unary(UnOp::Not, copy(1))));
        rejects(
            assigning(
                ptr(true, false, u32_ty()),
                Rvalue::Unary(UnOp::Not, copy(4)),
            ),
            "T5",
        );
        // The result has the operand's type.
        rejects(assigning(Ty::Bool, Rvalue::Unary(UnOp::Not, copy(1))), "T5");
    }

    #[test]
    fn t5_binary() {
        let u = u32_ty();
        let bin = |op, lhs, rhs| Rvalue::Binary(op, copy(lhs), copy(rhs));
        // Arithmetic: one integer type, which is the result's.
        accepts(assigning(u, bin(BinOp::Add, 1, 3)));
        rejects(assigning(u, bin(BinOp::Add, 1, 8)), "T5");
        rejects(assigning(u, bin(BinOp::Add, BOOL, BOOL)), "T5");
        rejects(assigning(Ty::Bool, bin(BinOp::Mul, 1, 3)), "T5");
        rejects(assigning(u, bin(BinOp::Sub, 4, 4)), "T5");
        // Bitwise: `bool` too.
        accepts(assigning(Ty::Bool, bin(BinOp::BitXor, BOOL, BOOL)));
        // Shifts: integers of any widths, with the left one's type.
        accepts(assigning(u, bin(BinOp::Shl, 1, U8)));
        accepts(assigning(u, bin(BinOp::Shr, 1, U64)));
        rejects(assigning(u, bin(BinOp::Shl, 1, BOOL)), "T5");
        rejects(assigning(u, bin(BinOp::Shr, BOOL, 1)), "T5");
        rejects(assigning(Ty::Int(IntTy::U8), bin(BinOp::Shl, 1, U8)), "T5");
        // Comparisons: `bool`, from operands of one type up to fitting.
        accepts(assigning(Ty::Bool, bin(BinOp::Lt, 1, 3)));
        rejects(assigning(u, bin(BinOp::Lt, 1, 3)), "T5");
        rejects(assigning(Ty::Bool, bin(BinOp::Lt, 1, 8)), "T5");
        accepts(assigning(Ty::Bool, bin(BinOp::Eq, BOOL, BOOL)));
        accepts(assigning(Ty::Bool, bin(BinOp::Ne, BOOL, BOOL)));
        rejects(assigning(Ty::Bool, bin(BinOp::Lt, BOOL, BOOL)), "T5");
        accepts(assigning(Ty::Bool, bin(BinOp::Le, 4, 4)));
        accepts(assigning(Ty::Bool, bin(BinOp::Eq, 4, 7)));
        accepts(assigning(Ty::Bool, bin(BinOp::Eq, 7, 4)));
        rejects(assigning(Ty::Bool, bin(BinOp::Eq, 4, 1)), "T5");
        // Slices and aggregates are not compared.
        rejects(assigning(Ty::Bool, bin(BinOp::Eq, SLICE_C, SLICE_C)), "T5");
        rejects(assigning(Ty::Bool, bin(BinOp::Eq, 5, 5)), "T5");
    }

    #[test]
    fn t5_cast() {
        let cast = |from, to| Rvalue::Cast(copy(from), to);
        accepts(assigning(Ty::Int(IntTy::U8), cast(1, Ty::Int(IntTy::U8))));
        accepts(assigning(usize_ty(), cast(4, usize_ty())));
        let to_bool = ptr(true, false, Ty::Bool);
        accepts(assigning(to_bool, cast(4, to_bool)));
        // The result has the target's type.
        rejects(assigning(u32_ty(), cast(1, Ty::Int(IntTy::U8))), "T5");
        // Neither side is an array, struct, slice or enum with payloads.
        rejects(assigning(u32_ty(), cast(5, u32_ty())), "T5");
        rejects(assigning(u32_ty(), cast(SLICE_C, u32_ty())), "T5");
        let array = Ty::Array(Box::leak(Box::new(u32_ty())), 4);
        rejects(assigning(array, cast(1, array)), "T5");
        let pair = Ty::Struct(pair());
        rejects(assigning(pair, cast(1, pair)), "T5");
        let slice = ptr(false, false, Ty::Slice(Box::leak(Box::new(u32_ty()))));
        rejects(assigning(slice, cast(SLICE_C, slice)), "T5");
    }

    #[test]
    fn t5_address_of() {
        let u = u32_ty();
        let of = |mutable, place| Rvalue::AddressOf { mutable, place };
        accepts(assigning(ptr(false, false, u), of(false, local(3))));
        accepts(assigning(ptr(true, false, u), of(true, local(3))));
        // `*mut` converts to `*const`, not the reverse.
        accepts(assigning(ptr(false, false, u), of(true, local(3))));
        rejects(assigning(ptr(true, false, u), of(false, local(3))), "T5");
        // The pointee is the place's type, and the pointer is not volatile.
        rejects(
            assigning(ptr(false, false, Ty::Bool), of(false, local(3))),
            "T5",
        );
        rejects(assigning(ptr(false, true, u), of(false, local(3))), "T5");
        rejects(assigning(u, of(false, local(3))), "T5");
        // A place of type `[T]` gives a slice.
        let slice = Ty::Slice(Box::leak(Box::new(u)));
        let elements = projected(10, &[PlaceElem::Deref]);
        accepts(assigning(ptr(true, false, slice), of(true, elements)));
        accepts(assigning(ptr(false, false, slice), of(false, elements)));
        // The address of a register: the place is volatile, the pointer not.
        let register = projected(9, &[PlaceElem::Deref, PlaceElem::Field(0)]);
        accepts(assigning(ptr(true, false, u), of(true, register)));
    }

    /// The rules broken by `statements` in `bb0` of `valid` with `locals`
    /// added after `_10`, numbered from 11.
    fn with_locals(locals: &[Ty], statements: Vec<Statement>) -> Vec<&'static str> {
        let mut body = valid();
        body.locals.extend(locals.iter().copied().map(decl));
        for statement in statements {
            statement_of(&mut body, statement);
        }
        rules(&body)
    }

    #[test]
    fn t5_ref() {
        let u = u32_ty();
        let shared = reference(false, u);
        let mutable = reference(true, u);
        let make = |mutable, place| Rvalue::Ref { mutable, place };
        // `_11: &u32`, `_12: &mut u32`, `_13: &bool`, `_14: *mut volatile u32`.
        let locals = [
            shared,
            mutable,
            reference(false, Ty::Bool),
            ptr(true, true, u),
        ];
        let assign = |dest, rvalue| Statement::Assign(local(dest), rvalue);
        accepts(with_locals(
            &locals,
            vec![
                assign(11, make(false, local(3))),
                assign(12, make(true, local(3))),
            ],
        ));
        // The type is a reference to the place's type, `&mut` if written so:
        // a `&mut` does not fit a `&` place, nor the reverse.
        let mismatches = [
            assign(11, make(true, local(3))),
            assign(12, make(false, local(3))),
            assign(13, make(false, local(3))),
            assign(3, make(false, local(3))),
            assign(4, make(false, local(3))),
        ];
        for statement in mismatches {
            let found = with_locals(&locals, vec![statement]);
            assert!(found.contains(&"T5"), "{found:?}");
        }
        // A `&mut T` used as a `&T`: the conversion is a `Ref`, never a `Use`.
        rejects(
            with_locals(&locals, vec![assign(11, Rvalue::Use(moved(local(12))))]),
            "T5",
        );
        // A reference fits the same reference type, copied if shared.
        accepts(with_locals(
            &[shared, shared],
            vec![assign(12, Rvalue::Use(copy(11)))],
        ));
        // A volatile place has no reference.
        let volatile = projected(14, &[PlaceElem::Deref]);
        rejects(
            with_locals(&locals, vec![assign(11, make(false, volatile))]),
            "T5",
        );
        // The place of a reference through a reference is not volatile.
        let through = projected(12, &[PlaceElem::Deref]);
        accepts(with_locals(&locals, vec![assign(11, make(false, through))]));
        // A cast from a reference goes to a raw pointer, never to a reference.
        let pointer = ptr(false, false, u);
        let cast = |from, to| Rvalue::Cast(copy(from), to);
        accepts(with_locals(
            &[shared, pointer],
            vec![assign(12, cast(11, pointer))],
        ));
        rejects(
            with_locals(&[pointer, shared], vec![assign(12, cast(11, shared))]),
            "T5",
        );
        rejects(
            with_locals(
                &[shared, usize_ty()],
                vec![assign(12, cast(11, usize_ty()))],
            ),
            "T5",
        );
    }

    // ---- Places of type `[T]` and references to slices (ADR 0109) ----

    /// `[u32]`.
    fn unsized_u32() -> Ty {
        Ty::Slice(Box::leak(Box::new(u32_ty())))
    }

    #[test]
    fn t1_index_applies_to_the_place_of_a_slice() {
        let usize_local = Local(8);
        let index = [PlaceElem::Deref, PlaceElem::Index(usize_local)];
        // `Index` of the `[T]` a raw slice points to, and a reference to a
        // slice refers to.
        accepts(reading(ptr(false, false, unsized_u32()), &index));
        accepts(reading(ptr(true, true, unsized_u32()), &index));
        accepts(reading(reference(false, unsized_u32()), &index));
        accepts(reading(reference(true, unsized_u32()), &index));
        // `ConstIndex` is for arrays.
        let constant = [PlaceElem::Deref, PlaceElem::ConstIndex(0)];
        rejects(reading(ptr(false, false, unsized_u32()), &constant), "T1");
        rejects(reading(reference(false, unsized_u32()), &constant), "T1");
        // `Index` of what is no array and no `[T]`.
        rejects(reading(reference(false, u32_ty()), &index), "T1");
        // The index is a `usize`, and the element's type is the slice's.
        let wrong = [PlaceElem::Deref, PlaceElem::Index(Local(1))];
        rejects(reading(reference(false, unsized_u32()), &wrong), "T2");
        // A place of type `[T]` is read whole by no one (T3), and has no
        // fields.
        rejects(
            reading(reference(false, unsized_u32()), &[PlaceElem::Deref]),
            "T3",
        );
        rejects(
            reading(
                reference(false, unsized_u32()),
                &[PlaceElem::Deref, PlaceElem::Field(0)],
            ),
            "T1",
        );
    }

    #[test]
    fn t1_const_subslice_applies_to_an_array_within_its_length() {
        let usize_local = Local(8);
        let array = Ty::Array(Box::leak(Box::new(u32_ty())), 4);
        // Followed by `Index`, so that the `[T]` in between is not read whole.
        let element = |from, to| {
            [
                PlaceElem::ConstSubslice(from, to),
                PlaceElem::Index(usize_local),
            ]
        };
        accepts(reading(array, &element(0, 4)));
        accepts(reading(array, &element(1, 3)));
        // Empty, at the length too.
        accepts(reading(array, &element(2, 2)));
        accepts(reading(array, &element(4, 4)));
        // `from <= to <= N`.
        rejects(reading(array, &element(3, 2)), "T1");
        rejects(reading(array, &element(0, 5)), "T1");
        rejects(reading(array, &element(5, 5)), "T1");
        // Not of a `[T]`, which `Subslice` is for, nor of what is no array.
        let through = [
            PlaceElem::Deref,
            PlaceElem::ConstSubslice(0, 1),
            PlaceElem::Index(usize_local),
        ];
        rejects(reading(reference(false, unsized_u32()), &through), "T1");
        rejects(reading(u32_ty(), &element(0, 0)), "T1");
    }

    #[test]
    fn t1_subslice_applies_to_an_array_or_a_slice_with_two_usize_bounds() {
        let usize_local = Local(8);
        let array = Ty::Array(Box::leak(Box::new(u32_ty())), 4);
        // Followed by `Index`, so that the `[T]` in between is not read whole.
        let element = [
            PlaceElem::Subslice(usize_local, usize_local),
            PlaceElem::Index(usize_local),
        ];
        accepts(reading(array, &element));
        // Of the `[T]` a raw slice points to and a reference to a slice
        // refers to, and of a sub-slice.
        let through = [
            PlaceElem::Deref,
            PlaceElem::Subslice(usize_local, usize_local),
            PlaceElem::Index(usize_local),
        ];
        accepts(reading(ptr(false, false, unsized_u32()), &through));
        accepts(reading(reference(true, unsized_u32()), &through));
        let nested = [
            PlaceElem::Deref,
            PlaceElem::Subslice(usize_local, usize_local),
            PlaceElem::Subslice(usize_local, usize_local),
            PlaceElem::Index(usize_local),
        ];
        accepts(reading(reference(false, unsized_u32()), &nested));
        // Of what is neither: the pointee of a thin pointer, an integer.
        rejects(reading(ptr(false, false, u32_ty()), &through), "T1");
        rejects(reading(reference(false, u32_ty()), &through), "T1");
        rejects(reading(u32_ty(), &element), "T1");
        // The bounds are `usize` (T2), each of them.
        let wrong = Local(1);
        for bounds in [(wrong, usize_local), (usize_local, wrong)] {
            let place = [
                PlaceElem::Subslice(bounds.0, bounds.1),
                PlaceElem::Index(usize_local),
            ];
            rejects(reading(array, &place), "T2");
        }
        // The `[T]` it makes is no value, and has no fields (U1, T1, T3).
        rejects(
            reading(array, &[PlaceElem::Subslice(usize_local, usize_local)]),
            "T3",
        );
        rejects(
            reading(
                array,
                &[
                    PlaceElem::Subslice(usize_local, usize_local),
                    PlaceElem::Field(0),
                ],
            ),
            "T1",
        );
    }

    #[test]
    fn r3_the_place_of_a_ref_may_be_a_subslice_of_an_array_or_a_slice() {
        let array = Ty::Array(Box::leak(Box::new(u32_ty())), 4);
        let slice = |mutable| reference(mutable, unsized_u32());
        let borrow =
            |dest, mutable, place| Statement::Assign(local(dest), Rvalue::Ref { mutable, place });
        // `_11: [u32; 4]`, `_12: &[u32]`, `_13: &mut [u32]`, `_14: &[u32; 4]`.
        let locals = [array, slice(false), slice(true), reference(false, array)];
        // Of an array, whole or in part, and of the array a reference refers to
        // (the conversion of ADR 0108).
        accepts(with_locals(
            &locals,
            vec![
                borrow(12, false, projected(11, &[PlaceElem::ConstSubslice(0, 4)])),
                borrow(13, true, projected(11, &[PlaceElem::ConstSubslice(1, 3)])),
                borrow(
                    12,
                    false,
                    projected(14, &[PlaceElem::Deref, PlaceElem::ConstSubslice(0, 4)]),
                ),
                // And a `Subslice`, of an array and of a `[T]` a reference
                // refers to.
                borrow(
                    13,
                    true,
                    projected(11, &[PlaceElem::Subslice(Local(8), Local(8))]),
                ),
                borrow(
                    12,
                    false,
                    projected(
                        12,
                        &[PlaceElem::Deref, PlaceElem::Subslice(Local(8), Local(8))],
                    ),
                ),
            ],
        ));
        // A `&mut` through the shared reference is R2's, whatever the place.
        rejects(
            with_locals(
                &locals,
                vec![borrow(
                    13,
                    true,
                    projected(14, &[PlaceElem::Deref, PlaceElem::ConstSubslice(0, 4)]),
                )],
            ),
            "R2",
        );
        // The type of the reference is the slice's, of the array's elements.
        rejects(
            with_locals(
                &[array, reference(false, Ty::Slice(&Ty::Bool))],
                vec![borrow(
                    12,
                    false,
                    projected(11, &[PlaceElem::ConstSubslice(0, 4)]),
                )],
            ),
            "T5",
        );
    }

    #[test]
    fn t5_ref_and_address_of_a_slice_place() {
        let u = u32_ty();
        let place = projected(10, &[PlaceElem::Deref]);
        let borrow = |mutable| Rvalue::Ref { mutable, place };
        let address = |mutable| Rvalue::AddressOf { mutable, place };
        let slice = |mutable, elem| reference(mutable, Ty::Slice(Box::leak(Box::new(elem))));
        let raw = |mutable, elem| ptr(mutable, false, Ty::Slice(Box::leak(Box::new(elem))));
        // A reference to a slice, or a raw slice, of the place's elements.
        accepts(assigning(slice(false, u), borrow(false)));
        accepts(assigning(slice(true, u), borrow(true)));
        accepts(assigning(raw(false, u), address(false)));
        accepts(assigning(raw(true, u), address(true)));
        // Not of other elements, a thin reference, or another mutability.
        rejects(assigning(slice(false, Ty::Bool), borrow(false)), "T5");
        rejects(assigning(reference(false, u), borrow(false)), "T5");
        rejects(assigning(slice(true, u), borrow(false)), "T5");
        rejects(assigning(slice(false, u), borrow(true)), "T5");
        rejects(assigning(raw(true, u), address(false)), "T5");
    }

    #[test]
    fn t5_a_reference_to_a_slice_fits_only_its_own_type() {
        let u = u32_ty();
        let shared = reference(false, unsized_u32());
        let unique = reference(true, unsized_u32());
        let raw_const = ptr(false, false, unsized_u32());
        let raw_mut = ptr(true, false, unsized_u32());
        let assign = |dest, rvalue| Statement::Assign(local(dest), rvalue);
        // `_11: &[u32]`, `_12: &mut [u32]`, `_13: *const [u32]`, `_14: *mut
        // [u32]`, `_15: &[u32]`.
        let locals = [shared, unique, raw_const, raw_mut, shared];
        accepts(with_locals(
            &locals,
            vec![assign(15, Rvalue::Use(copy(11)))],
        ));
        // Neither way between a reference and a raw slice is a `Use`: the
        // way out is a `Cast`, and nothing goes in.
        for (dest, from) in [(13, 11), (13, 12), (11, 13), (14, 12), (12, 14), (11, 12)] {
            let operand = if from == 12 {
                moved(local(from))
            } else {
                copy(from)
            };
            let found = with_locals(&locals, vec![assign(dest, Rvalue::Use(operand))]);
            assert_eq!(found, ["T5"], "_{dest} = use _{from}");
        }
        // `SliceLen` and `SliceAddress` read a reference to a slice as they
        // read a raw one; the address is a thin raw pointer of the same
        // mutability.
        let more = [
            shared,
            unique,
            usize_ty(),
            ptr(false, false, u),
            ptr(true, false, u),
        ];
        accepts(with_locals(
            &more,
            vec![
                assign(13, Rvalue::SliceLen(local(11))),
                assign(13, Rvalue::SliceLen(local(12))),
                assign(14, Rvalue::SliceAddress(local(11))),
                assign(15, Rvalue::SliceAddress(local(12))),
            ],
        ));
        let found = with_locals(
            &more,
            vec![
                assign(15, Rvalue::SliceAddress(local(11))),
                assign(13, Rvalue::SliceLen(local(1))),
            ],
        );
        assert_eq!(found, ["T5", "T5"]);
    }

    #[test]
    fn t5_a_reference_to_a_slice_casts_to_the_raw_slice_of_its_mutability() {
        let shared = reference(false, unsized_u32());
        let unique = reference(true, unsized_u32());
        let raw_const = ptr(false, false, unsized_u32());
        let raw_mut = ptr(true, false, unsized_u32());
        let bytes = ptr(
            false,
            false,
            Ty::Slice(Box::leak(Box::new(Ty::Int(IntTy::U8)))),
        );
        let volatile = ptr(false, true, unsized_u32());
        let assign = |dest, from| {
            // A `&mut` is moved, whatever else is read.
            let operand = if from == 12 {
                moved(local(from))
            } else {
                copy(from)
            };
            Statement::Assign(
                local(dest),
                Rvalue::Cast(
                    operand,
                    with_locals_ty(dest, &[shared, unique, raw_const, raw_mut, bytes, volatile]),
                ),
            )
        };
        // `_11: &[u32]`, `_12: &mut [u32]`, `_13: *const [u32]`, `_14: *mut
        // [u32]`, `_15: *const [u8]`, `_16: *const volatile [u32]`.
        let locals = [shared, unique, raw_const, raw_mut, bytes, volatile];
        accepts(with_locals(&locals, vec![assign(13, 11)]));
        accepts(with_locals(&locals, vec![assign(14, 12)]));
        // Another mutability, other elements, a volatile slice: rejected.
        for (dest, from) in [(14, 11), (13, 12), (15, 11), (16, 11)] {
            let found = with_locals(&locals, vec![assign(dest, from)]);
            assert_eq!(found, ["T5"], "_{dest} = _{from} as ..");
        }
        // Nothing casts to a reference, and a raw slice casts to no slice.
        rejects(
            with_locals(
                &locals,
                vec![Statement::Assign(local(11), Rvalue::Cast(copy(13), shared))],
            ),
            "T5",
        );
        rejects(
            with_locals(
                &locals,
                vec![Statement::Assign(
                    local(13),
                    Rvalue::Cast(copy(14), raw_const),
                )],
            ),
            "T5",
        );
        // A struct is no operand of a cast either, to a raw slice.
        let aggregate = Ty::Struct(pair());
        rejects(
            with_locals(
                &[aggregate, raw_const],
                vec![Statement::Assign(
                    local(12),
                    Rvalue::Cast(copy(11), raw_const),
                )],
            ),
            "T5",
        );
        // A reference to a slice casts to no thin pointer: MIR building goes
        // through the raw slice.
        let thin = ptr(false, false, u32_ty());
        rejects(
            with_locals(
                &[shared, thin],
                vec![Statement::Assign(local(12), Rvalue::Cast(copy(11), thin))],
            ),
            "T5",
        );
    }

    /// The type of the new local `n` among `types`, which `with_locals` numbers
    /// from 11.
    fn with_locals_ty(n: u32, types: &[Ty]) -> Ty {
        types[n as usize - 11]
    }

    #[test]
    fn u1_a_place_of_type_unsized_is_read_by_no_slice_operation() {
        let place = projected(10, &[PlaceElem::Deref]);
        let usize_dest = usize_ty();
        let address_dest = ptr(false, false, u32_ty());
        // The length and the address are a slice's, not those of the `[T]`
        // that a slice points to.
        rejects(assigning(usize_dest, Rvalue::SliceLen(place)), "U1");
        rejects(assigning(address_dest, Rvalue::SliceAddress(place)), "U1");
        // The slice itself is read by both.
        accepts(assigning(usize_dest, Rvalue::SliceLen(local(10))));
        // As an operand, a destination, a `switch` or an `asm` operand, the
        // place is read or written whole, which T3 says and reports.
        let mut body = valid();
        statement_of(
            &mut body,
            Statement::Assign(local(3), Rvalue::Use(Operand::Copy(place))),
        );
        breaks_only(&body, "T3");
        let mut body = valid();
        body.blocks[0].terminator = Terminator::Switch {
            discr: Operand::Copy(place),
            cases: &[(0, BasicBlock(1))],
            otherwise: BasicBlock(1),
        };
        breaks_only(&body, "T3");
    }

    #[test]
    fn r2_a_write_does_not_go_through_a_shared_reference() {
        let u = u32_ty();
        let shared = reference(false, u);
        let mutable = reference(true, u);
        // `_11: &u32`, `_12: &mut u32`, `_13: &u32`, `_14: &mut u32`.
        let locals = [shared, mutable, shared, mutable];
        let through = |n| projected(n, &[PlaceElem::Deref]);
        let assign = |place, rvalue| Statement::Assign(place, rvalue);
        // Reads and borrows `&` are fine through either.
        accepts(with_locals(
            &locals,
            vec![
                assign(local(3), Rvalue::Use(copy(1))),
                assign(
                    local(13),
                    Rvalue::Ref {
                        mutable: false,
                        place: through(11),
                    },
                ),
            ],
        ));
        // A write through `&mut` is fine; through `&` it is R2.
        accepts(with_locals(
            &locals,
            vec![assign(through(12), Rvalue::Use(copy(1)))],
        ));
        rejects(
            with_locals(&locals, vec![assign(through(11), Rvalue::Use(copy(1)))]),
            "R2",
        );
        // So are `&mut` and `&raw mut` of a place through `&`.
        rejects(
            with_locals(
                &locals,
                vec![assign(
                    local(14),
                    Rvalue::Ref {
                        mutable: true,
                        place: through(11),
                    },
                )],
            ),
            "R2",
        );
        rejects(
            with_locals(
                &[shared, ptr(true, false, u)],
                vec![assign(
                    local(12),
                    Rvalue::AddressOf {
                        mutable: true,
                        place: through(11),
                    },
                )],
            ),
            "R2",
        );
        // The destination of a call.
        rejects(
            with_locals(
                &locals,
                vec![Statement::Call {
                    dest: Some(through(11)),
                    callee: BodyId(0),
                    args: vec![copy(1)],
                }],
            ),
            "R2",
        );
        // A field of a struct held behind a shared reference.
        let pair = Ty::Struct(pair());
        rejects(
            with_locals(
                &[reference(false, pair)],
                vec![assign(
                    projected(11, &[PlaceElem::Deref, PlaceElem::Field(0)]),
                    Rvalue::Use(copy(1)),
                )],
            ),
            "R2",
        );
    }

    #[test]
    fn r3_the_place_of_a_ref_has_no_payload_or_tag() {
        let option = Ty::Enum(option());
        let u = u32_ty();
        // `_11: Option`, `_12: &u32`, `_13: &Option`.
        let locals = [option, reference(false, u), reference(false, option)];
        let assign = |dest, place| {
            Statement::Assign(
                local(dest),
                Rvalue::Ref {
                    mutable: false,
                    place,
                },
            )
        };
        accepts(with_locals(&locals, vec![assign(13, local(11))]));
        // The payload of an enum is reached by a `match`, never borrowed.
        let payload = PlaceElem::Payload {
            variant: 1,
            index: 0,
        };
        rejects(
            with_locals(&locals, vec![assign(12, projected(11, &[payload]))]),
            "R3",
        );
        let tag = Ty::Int(IntTy::U8);
        rejects(
            with_locals(
                &[option, reference(false, tag)],
                vec![assign(12, projected(11, &[PlaceElem::Tag]))],
            ),
            "R3",
        );
        // Fields, elements and dereferences are fine.
        let pair = Ty::Struct(pair());
        let array = Ty::Array(Box::leak(Box::new(u)), 4);
        accepts(with_locals(
            &[pair, reference(false, u)],
            vec![assign(12, projected(11, &[PlaceElem::Field(1)]))],
        ));
        accepts(with_locals(
            &[array, reference(false, u)],
            vec![assign(12, projected(11, &[PlaceElem::ConstIndex(3)]))],
        ));
    }

    #[test]
    fn r4_the_address_of_a_static_or_a_peripheral_is_held_by_a_temporary_written_once() {
        let u = u32_ty();
        let s = StaticRef::new(StaticId(0), "S".to_owned(), u, true, false);
        let p = PeripheralRef::new("UART".to_owned(), 0x4000_0000, peripheral());
        let address_of_s = || Rvalue::Use(Operand::Const(Const::Static(s)));
        let address_of_p = || Rvalue::Use(Operand::Const(Const::Peripheral(p)));
        let assign = |dest, rvalue| Statement::Assign(local(dest), rvalue);
        // `_11: *mut u32` and `_12: *mut P`; the temporaries are read through.
        let locals = [ptr(true, false, u), p.def().address_ty()];
        accepts(with_locals(
            &locals,
            vec![
                assign(11, address_of_s()),
                assign(
                    0,
                    Rvalue::Use(Operand::Copy(projected(11, &[PlaceElem::Deref]))),
                ),
                assign(12, address_of_p()),
                assign(
                    0,
                    Rvalue::Use(Operand::Copy(projected(
                        12,
                        &[PlaceElem::Deref, PlaceElem::Field(0)],
                    ))),
                ),
            ],
        ));
        // Not a named local: a `const static` assigned to a variable.
        let mut named = valid();
        named.locals.push(decl(locals[0]));
        named.locals[11].name = Some("x".to_owned());
        statement_of(&mut named, assign(11, address_of_s()));
        breaks_only(&named, "R4");
        // Nor a parameter.
        let mut param = valid();
        param.locals.push(decl(locals[0]));
        param.param_count = 11;
        statement_of(&mut param, assign(11, address_of_s()));
        breaks_only(&param, "R4");
        // Written a second time: by another address, a copy, a call's
        // destination or an `asm` output.
        let again =
            |second: Statement| with_locals(&locals, vec![assign(11, address_of_s()), second]);
        rejects(again(assign(11, address_of_s())), "R4");
        rejects(again(assign(11, Rvalue::Use(copy(4)))), "R4");
        rejects(
            again(Statement::Call {
                dest: Some(local(11)),
                callee: BodyId(0),
                args: Vec::new(),
            }),
            "R4",
        );
        let out = AsmOperand {
            reg: AsmReg::Any,
            kind: AsmOperandKind::Out {
                late: false,
                place: Some(local(11)),
            },
        };
        let writes = asm(AsmOptions::default(), Vec::new(), vec![out]);
        assert_eq!(again(Statement::InlineAsm(Box::new(writes))), ["R4"]);
        // A write through the pointer is no write of the temporary.
        accepts(again(assign(
            0,
            Rvalue::Use(Operand::Copy(projected(11, &[PlaceElem::Deref]))),
        )));
        // The place of the temporary itself is borrowed or its address taken.
        let borrowed = [locals[0], reference(false, locals[0])];
        rejects(
            with_locals(
                &borrowed,
                vec![
                    assign(11, address_of_s()),
                    assign(
                        12,
                        Rvalue::Ref {
                            mutable: false,
                            place: local(11),
                        },
                    ),
                ],
            ),
            "R4",
        );
        let taken = [locals[0], ptr(false, false, locals[0])];
        rejects(
            with_locals(
                &taken,
                vec![
                    assign(11, address_of_s()),
                    assign(
                        12,
                        Rvalue::AddressOf {
                            mutable: false,
                            place: local(11),
                        },
                    ),
                ],
            ),
            "R4",
        );
        // A peripheral's temporary is the same: a second write and `&raw const
        // _t` are rejected; `&raw mut P` copies it, which is no write of it.
        let twice = with_locals(
            &locals[1..],
            vec![assign(11, address_of_p()), assign(11, address_of_p())],
        );
        rejects(twice, "R4");
        let taken = [locals[1], ptr(false, false, locals[1])];
        rejects(
            with_locals(
                &taken,
                vec![
                    assign(11, address_of_p()),
                    assign(
                        12,
                        Rvalue::AddressOf {
                            mutable: false,
                            place: local(11),
                        },
                    ),
                ],
            ),
            "R4",
        );
        accepts(with_locals(
            &[locals[1], locals[1]],
            vec![
                assign(11, address_of_p()),
                assign(12, Rvalue::Use(copy(11))),
            ],
        ));
    }

    #[test]
    fn t5_slice_and_its_parts() {
        let u = u32_ty();
        let bytes = Ty::Int(IntTy::U8);
        let slice =
            |mutable, volatile, elem| ptr(mutable, volatile, Ty::Slice(Box::leak(Box::new(elem))));
        let make = |ptr, len| Rvalue::Slice {
            ptr: copy(ptr),
            len: copy(len),
        };
        // From a thin pointer to `T`, and from one to `[T; N]` (ADR 0031),
        // into a slice of any mutability and volatility.
        accepts(assigning(slice(true, false, u), make(4, 8)));
        accepts(assigning(slice(false, true, u), make(4, 8)));
        accepts(assigning(slice(true, false, bytes), make(ARRAY_PTR, 8)));
        // The length is a `usize`, the pointer is thin, the elements agree.
        rejects(assigning(slice(true, false, u), make(4, 1)), "T5");
        rejects(assigning(slice(true, false, u), make(10, 8)), "T5");
        rejects(assigning(slice(true, false, u), make(1, 8)), "T5");
        rejects(assigning(slice(true, false, bytes), make(4, 8)), "T5");
        rejects(assigning(u, make(4, 8)), "T5");
        // The address of a slice keeps its mutability and volatility.
        let address = |place| Rvalue::SliceAddress(local(place));
        accepts(assigning(ptr(true, false, u), address(10)));
        accepts(assigning(ptr(false, false, u), address(10)));
        rejects(assigning(ptr(false, true, u), address(10)), "T5");
        rejects(assigning(ptr(true, false, bytes), address(10)), "T5");
        rejects(assigning(u, address(1)), "T5");
        rejects(assigning(ptr(true, false, u), address(4)), "T5");
        // The length is a `usize`, of a slice.
        accepts(assigning(usize_ty(), Rvalue::SliceLen(local(10))));
        rejects(assigning(u, Rvalue::SliceLen(local(10))), "T5");
        rejects(assigning(usize_ty(), Rvalue::SliceLen(local(4))), "T5");
        // What `Builder::cast` builds: a slice into a slice of another
        // mutability and volatility, and a pointer to an array into a slice.
        accepts(assigning(
            slice(false, true, bytes),
            Rvalue::Use(copy(SLICE_M)),
        ));
        accepts(assigning(slice(true, false, bytes), make(ARRAY_PTR, 8)));
    }

    #[test]
    fn the_rules_of_types_do_not_cascade_from_what_failed_before() {
        // The operand names a local that does not exist: S7 only.
        let mut body = typed();
        statement_of(
            &mut body,
            Statement::Assign(local(BOOL), Rvalue::Use(copy(GHOST))),
        );
        breaks_only(&body, "S7");
        // The place is `[T]`: T3 only, whatever the rvalue.
        let mut body = typed();
        statement_of(
            &mut body,
            Statement::Assign(projected(10, &[PlaceElem::Deref]), Rvalue::Use(copy(1))),
        );
        breaks_only(&body, "T3");
        // The operand is `[T]`: T3 only.
        let mut body = typed();
        let slice = Operand::Copy(projected(10, &[PlaceElem::Deref]));
        statement_of(&mut body, Statement::Assign(local(3), Rvalue::Use(slice)));
        breaks_only(&body, "T3");
        // An `asm` output of type `[T]`: T3 only, not also T9.
        let mut body = typed();
        statement_of(
            &mut body,
            Statement::InlineAsm(Box::new(asm(
                AsmOptions::default(),
                Vec::new(),
                vec![AsmOperand {
                    reg: AsmReg::Any,
                    kind: AsmOperandKind::Out {
                        late: false,
                        place: Some(projected(10, &[PlaceElem::Deref])),
                    },
                }],
            ))),
        );
        breaks_only(&body, "T3");
    }

    #[test]
    fn t6_an_integer_constant_has_no_bit_above_its_width() {
        let int = |bits, ty| {
            assigning(
                Ty::Int(ty),
                Rvalue::Use(Operand::Const(Const::Int(bits, ty))),
            )
        };
        accepts(int(0xFF, IntTy::U8));
        rejects(int(0x100, IntTy::U8), "T6");
        accepts(int(0xFF, IntTy::I8));
        rejects(int(u64::MAX, IntTy::I8), "T6");
        accepts(int(0xFFFF_FFFF, IntTy::U32));
        rejects(int(0x1_0000_0000, IntTy::U32), "T6");
        // `usize` has the width of the target's pointers.
        accepts(int(IntTy::Usize.mask(), IntTy::Usize));
        rejects(int(IntTy::Usize.mask() + 1, IntTy::Usize), "T6");
        accepts(int(u64::MAX, IntTy::U64));
        accepts(int(u64::MAX, IntTy::I64));
        // Wherever an operand is, and in an `asm` `const`.
        let mut body = typed();
        statement_of(
            &mut body,
            Statement::Call {
                dest: None,
                callee: BodyId(0),
                args: vec![Operand::Const(Const::Int(0x100, IntTy::U8))],
            },
        );
        breaks_only(&body, "T6");
        for (bits, expected) in [(0xFF, Vec::new()), (0x1FF, vec!["T6"])] {
            let mut body = typed();
            statement_of(
                &mut body,
                Statement::InlineAsm(Box::new(asm(
                    AsmOptions::default(),
                    Vec::new(),
                    vec![AsmOperand {
                        reg: AsmReg::Any,
                        kind: AsmOperandKind::Const(bits, IntTy::U8),
                    }],
                ))),
            );
            assert_eq!(rules(&body), expected);
        }
    }

    #[test]
    fn t7_if_tests_a_bool_and_switch_an_integer_or_bool_with_distinct_cases() {
        let mut body = typed();
        body.blocks[0].terminator = Terminator::If {
            cond: copy(BOOL),
            then: BasicBlock(1),
            else_: BasicBlock(1),
        };
        assert!(check_body(&body).is_empty());
        body.blocks[0].terminator = Terminator::If {
            cond: copy(1),
            then: BasicBlock(1),
            else_: BasicBlock(1),
        };
        breaks_only(&body, "T7");

        const ONE: BasicBlock = BasicBlock(1);
        let switch = |discr: u32, cases: &'static [(u64, BasicBlock)]| {
            let mut body = typed();
            body.blocks[0].terminator = Terminator::Switch {
                discr: copy(discr),
                cases,
                otherwise: ONE,
            };
            rules(&body)
        };
        accepts(switch(BOOL, &[(0, ONE), (1, ONE)]));
        rejects(switch(BOOL, &[(0, ONE), (2, ONE)]), "T7");
        accepts(switch(U8, &[(0, ONE), (255, ONE)]));
        rejects(switch(U8, &[(0, ONE), (256, ONE)]), "T7");
        accepts(switch(1, &[(0xFFFF_FFFF, ONE)]));
        rejects(switch(1, &[(0x1_0000_0000, ONE)]), "T7");
        accepts(switch(U64, &[(u64::MAX, ONE)]));
        // Distinct.
        rejects(switch(1, &[(3, ONE), (3, ONE)]), "T7");
        rejects(switch(BOOL, &[(1, ONE), (0, ONE), (1, ONE)]), "T7");
        // Not an integer or `bool`.
        rejects(switch(4, &[]), "T7");
        rejects(switch(5, &[]), "T7");
        // One violation for each case at fault.
        assert_eq!(switch(U8, &[(256, ONE), (512, ONE)]), ["T7", "T7"]);
    }

    #[test]
    fn e1_each_case_on_the_tag_of_an_enum_is_the_value_of_one_variant() {
        const ONE: BasicBlock = BasicBlock(1);
        let on_tag = |e: EnumRef, cases: &'static [(u64, BasicBlock)]| {
            let mut body = valid();
            body.locals.push(decl(Ty::Enum(e)));
            let n = body.locals.len() as u32 - 1;
            body.blocks[0].terminator = Terminator::Switch {
                discr: Operand::Copy(projected(n, &[PlaceElem::Tag])),
                cases,
                otherwise: ONE,
            };
            rules(&body)
        };
        // `None` is 0 and `Some` is 1.
        accepts(on_tag(option(), &[(0, ONE), (1, ONE)]));
        accepts(on_tag(option(), &[(1, ONE)]));
        accepts(on_tag(option(), &[]));
        // A case that no variant has.
        rejects(on_tag(option(), &[(0, ONE), (2, ONE)]), "E1");
        // Two variants with one value, and a variant with none stored.
        let valued = |none: Option<u64>, some: Option<u64>| {
            let e = EnumRef::new(
                EnumId(2),
                "V".to_owned(),
                IntTy::U8,
                Layout { size: 1, align: 1 },
                true,
            );
            let value = |v: Option<u64>| v.map_or_else(Default::default, std::sync::OnceLock::from);
            e.def().set_variants(vec![
                Variant {
                    name: "None".to_owned(),
                    value: value(none),
                    payload: Vec::new(),
                },
                Variant {
                    name: "Some".to_owned(),
                    value: value(some),
                    payload: Vec::new(),
                },
            ]);
            e
        };
        rejects(on_tag(valued(Some(1), Some(1)), &[(1, ONE)]), "E1");
        rejects(on_tag(valued(Some(0), None), &[(1, ONE)]), "E1");
        accepts(on_tag(valued(Some(0), None), &[(0, ONE)]));
        // One violation for each case at fault; a tag copied elsewhere and a
        // `switch` on an integer are not looked at.
        assert_eq!(on_tag(option(), &[(2, ONE), (3, ONE)]), ["E1", "E1"]);
        accepts(on_tag_copy());
    }

    /// A `switch` on a `u8` local, with a case no enum has.
    fn on_tag_copy() -> Vec<&'static str> {
        let mut body = typed();
        body.blocks[0].terminator = Terminator::Switch {
            discr: copy(U8),
            cases: &[(9, BasicBlock(1))],
            otherwise: BasicBlock(1),
        };
        rules(&body)
    }

    #[test]
    fn t8_the_message_of_an_assert_is_a_const_slice_of_bytes() {
        let assertion = |message| {
            let mut body = typed();
            body.blocks[1].terminator = Terminator::Panic(
                PanicCause::AssertionFailed(Some((copy(message), span()))),
                None,
            );
            rules(&body)
        };
        accepts(assertion(SLICE_C));
        rejects(assertion(SLICE_M), "T8");
        rejects(assertion(ARRAY_PTR), "T8");
        rejects(assertion(1), "T8");
        rejects(assertion(10), "T8");
    }

    #[test]
    fn t9_an_asm_value_is_an_integer_of_at_most_32_bits_or_a_thin_pointer() {
        let with = |kind: AsmOperandKind| {
            let mut body = typed();
            statement_of(
                &mut body,
                Statement::InlineAsm(Box::new(asm(
                    AsmOptions::default(),
                    Vec::new(),
                    vec![AsmOperand {
                        reg: AsmReg::Any,
                        kind,
                    }],
                ))),
            );
            rules(&body)
        };
        let input = |local| AsmOperandKind::In(copy(local));
        accepts(with(input(1)));
        accepts(with(input(U8)));
        accepts(with(input(8)));
        accepts(with(input(4)));
        rejects(with(input(U64)), "T9");
        rejects(with(input(BOOL)), "T9");
        rejects(with(input(SLICE_C)), "T9");
        rejects(with(input(5)), "T9");
        // An output's place, and an `inout`'s two parts.
        let output = |place| AsmOperandKind::Out {
            late: false,
            place: Some(local(place)),
        };
        accepts(with(output(3)));
        rejects(with(output(BOOL)), "T9");
        rejects(with(output(U64)), "T9");
        let inout = |value, place| AsmOperandKind::InOut {
            input: copy(value),
            place: Some(local(place)),
        };
        accepts(with(inout(1, 3)));
        rejects(with(inout(U64, 3)), "T9");
        rejects(with(inout(1, U64)), "T9");
        assert_eq!(with(inout(U64, U64)), ["T9", "T9"]);
        // `_` as an output has no place.
        accepts(with(AsmOperandKind::Out {
            late: false,
            place: None,
        }));
        // The same in a terminator.
        let mut body = typed();
        body.blocks[1].terminator = Terminator::InlineAsm(Box::leak(Box::new(asm(
            noreturn(),
            Vec::new(),
            vec![AsmOperand {
                reg: AsmReg::Any,
                kind: input(U64),
            }],
        ))));
        breaks_only(&body, "T9");
    }

    // ---- Moves (ADR 0098, M1 to M4) ----

    /// `struct Handle { n: u32 }`, not copy.
    fn handle() -> StructRef {
        let s = StructRef::new(
            "Handle".to_owned(),
            Layout { size: 4, align: 4 },
            false,
            false,
            false,
        );
        s.def().set_fields(vec![Field {
            name: "n".to_owned(),
            ty: u32_ty(),
            offset: 0,
        }]);
        s
    }

    /// `struct Wrapper { h: Handle, n: u32, items: [Handle; 2] }`, not copy.
    fn wrapper() -> StructRef {
        let s = StructRef::new(
            "Wrapper".to_owned(),
            Layout { size: 16, align: 4 },
            false,
            false,
            false,
        );
        let field = |name: &str, ty, offset| Field {
            name: name.to_owned(),
            ty,
            offset,
        };
        s.def().set_fields(vec![
            field("h", Ty::Struct(handle()), 0),
            field("n", u32_ty(), 4),
            field("items", handles(2), 8),
        ]);
        s
    }

    /// `[Handle; len]`.
    fn handles(len: u64) -> Ty {
        Ty::Array(Box::leak(Box::new(Ty::Struct(handle()))), len)
    }

    /// `enum Slot { Free, Full(Handle) }`, not copy, with a `u8` tag.
    fn slot() -> EnumRef {
        let e = EnumRef::new(
            EnumId(1),
            "Slot".to_owned(),
            IntTy::U8,
            Layout { size: 8, align: 4 },
            false,
        );
        e.def().set_variants(vec![
            Variant {
                name: "Free".to_owned(),
                value: std::sync::OnceLock::from(0),
                payload: Vec::new(),
            },
            Variant {
                name: "Full".to_owned(),
                value: std::sync::OnceLock::from(1),
                payload: vec![Field {
                    name: "0".to_owned(),
                    ty: Ty::Struct(handle()),
                    offset: 4,
                }],
            },
        ]);
        e
    }

    /// `struct Marked { h: Handle }` declared `#[copy]`, which it must not
    /// be: its field is not copy.
    fn marked() -> StructRef {
        let s = StructRef::new(
            "Marked".to_owned(),
            Layout { size: 4, align: 4 },
            false,
            true,
            false,
        );
        s.def().set_fields(vec![Field {
            name: "h".to_owned(),
            ty: Ty::Struct(handle()),
            offset: 0,
        }]);
        s
    }

    fn moved(place: Place) -> Operand {
        Operand::Move(place, span())
    }

    /// The rules broken by a call that passes, as `operand` reads it,
    /// `projection` of a new local of type `ty`.
    fn passing(
        ty: Ty,
        projection: &[PlaceElem],
        operand: fn(Place) -> Operand,
    ) -> Vec<&'static str> {
        let mut body = valid();
        body.locals.push(decl(ty));
        let n = body.locals.len() as u32 - 1;
        statement_of(
            &mut body,
            Statement::Call {
                dest: None,
                callee: BodyId(0),
                args: vec![operand(projected(n, projection))],
            },
        );
        rules(&body)
    }

    /// The rules broken by a body with a new local of type `ty`.
    fn declaring(ty: Ty) -> Vec<&'static str> {
        let mut body = valid();
        body.locals.push(decl(ty));
        rules(&body)
    }

    #[test]
    fn m1_a_copy_reads_a_place_of_a_copy_type() {
        let copying = |ty, projection: &[PlaceElem]| passing(ty, projection, Operand::Copy);
        let array = |ty| Ty::Array(Box::leak(Box::new(ty)), 2);
        accepts(copying(u32_ty(), &[]));
        accepts(copying(Ty::Struct(pair()), &[]));
        accepts(copying(Ty::Enum(option()), &[]));
        accepts(copying(array(u32_ty()), &[]));
        accepts(copying(ptr(true, false, Ty::Struct(handle())), &[]));
        // A copy part of a non-copy struct.
        accepts(copying(Ty::Struct(wrapper()), &[PlaceElem::Field(1)]));
        rejects(copying(Ty::Struct(handle()), &[]), "M1");
        rejects(copying(Ty::Enum(slot()), &[]), "M1");
        rejects(copying(handles(2), &[]), "M1");
        rejects(copying(array(handles(2)), &[]), "M1");
        rejects(copying(Ty::Struct(wrapper()), &[PlaceElem::Field(0)]), "M1");
        // Valid MIR beside: the value of a non-copy parameter moved.
        let held = Ty::Struct(handle());
        let mut body = valid();
        body.ret = held;
        body.locals[0] = decl(held);
        body.locals[1] = decl(held);
        body.blocks[0].statements = vec![Statement::Assign(local(0), Rvalue::Use(moved(local(1))))];
        assert!(check_body(&body).is_empty(), "{:#?}", check_body(&body));
    }

    #[test]
    fn m2_a_move_reads_a_place_of_a_non_copy_type() {
        let moving = |ty, projection: &[PlaceElem]| passing(ty, projection, moved);
        let payload = PlaceElem::Payload {
            variant: 1,
            index: 0,
        };
        accepts(moving(Ty::Struct(handle()), &[]));
        accepts(moving(Ty::Enum(slot()), &[]));
        accepts(moving(handles(2), &[]));
        accepts(moving(Ty::Struct(wrapper()), &[PlaceElem::Field(0)]));
        accepts(moving(Ty::Enum(slot()), &[payload]));
        rejects(moving(u32_ty(), &[]), "M2");
        rejects(moving(Ty::Struct(pair()), &[]), "M2");
        rejects(moving(Ty::Enum(option()), &[]), "M2");
        rejects(moving(ptr(true, false, Ty::Struct(handle())), &[]), "M2");
        rejects(moving(Ty::Struct(wrapper()), &[PlaceElem::Field(1)]), "M2");
    }

    #[test]
    fn m3_the_place_of_a_move_is_a_local_followed_by_fields_and_payloads() {
        let moving = |ty, projection: &[PlaceElem]| passing(ty, projection, moved);
        let to_handle = ptr(true, false, Ty::Struct(handle()));
        // Through a pointer, in an array, after a field.
        rejects(moving(to_handle, &[PlaceElem::Deref]), "M3");
        rejects(
            moving(
                ptr(true, false, handles(2)),
                &[PlaceElem::Deref, PlaceElem::ConstIndex(1)],
            ),
            "M3",
        );
        rejects(moving(handles(2), &[PlaceElem::ConstIndex(1)]), "M3");
        rejects(moving(handles(2), &[PlaceElem::Index(Local(8))]), "M3");
        rejects(
            moving(
                Ty::Struct(wrapper()),
                &[PlaceElem::Field(2), PlaceElem::ConstIndex(0)],
            ),
            "M3",
        );
        rejects(moving(to_handle, &[PlaceElem::Offset(Local(8))]), "M3");
        // A local bound to read-only data.
        let mut body = valid();
        body.locals.push(LocalDecl {
            ty: Ty::Struct(handle()),
            name: None,
            constant: Some(DataId(0)),
        });
        let n = body.locals.len() as u32 - 1;
        statement_of(
            &mut body,
            Statement::Call {
                dest: None,
                callee: BodyId(0),
                args: vec![moved(local(n))],
            },
        );
        breaks_only(&body, "M3");
        // A projection T1 rejects is not checked against M3, nor M2.
        rejects(moving(Ty::Struct(handle()), &[PlaceElem::Deref]), "T1");
        rejects(moving(Ty::Struct(handle()), &[PlaceElem::Tag]), "T1");
        rejects(moving(Ty::Struct(handle()), &[PlaceElem::Field(5)]), "T1");
    }

    #[test]
    fn m4_a_type_marked_copy_has_copy_parts_only() {
        accepts(declaring(Ty::Struct(pair())));
        accepts(declaring(Ty::Enum(option())));
        accepts(declaring(Ty::Struct(wrapper())));
        accepts(declaring(Ty::Enum(slot())));
        // A pointer to it is not looked into: nothing contains it by value.
        accepts(declaring(ptr(true, false, Ty::Struct(marked()))));
        rejects(declaring(Ty::Struct(marked())), "M4");
        // Through an array, and as a field of a type that is not marked.
        rejects(
            declaring(Ty::Array(Box::leak(Box::new(Ty::Struct(marked()))), 3)),
            "M4",
        );
        let outer = StructRef::new(
            "Outer".to_owned(),
            Layout { size: 4, align: 4 },
            false,
            false,
            false,
        );
        outer.def().set_fields(vec![Field {
            name: "m".to_owned(),
            ty: Ty::Struct(marked()),
            offset: 0,
        }]);
        rejects(declaring(Ty::Struct(outer)), "M4");
        // An enum with payloads, at a payload value.
        let flagged = EnumRef::new(
            EnumId(2),
            "Flagged".to_owned(),
            IntTy::U8,
            Layout { size: 8, align: 4 },
            true,
        );
        flagged.def().set_variants(vec![Variant {
            name: "Held".to_owned(),
            value: std::sync::OnceLock::from(0),
            payload: vec![
                Field {
                    name: "0".to_owned(),
                    ty: u32_ty(),
                    offset: 4,
                },
                Field {
                    name: "1".to_owned(),
                    ty: Ty::Struct(handle()),
                    offset: 8,
                },
            ],
        }]);
        rejects(declaring(Ty::Enum(flagged)), "M4");
        // Each definition once, however many locals have its type.
        let shared = marked();
        let mut body = valid();
        body.locals.extend([
            decl(Ty::Struct(shared)),
            decl(Ty::Struct(shared)),
            decl(Ty::Array(Box::leak(Box::new(Ty::Struct(shared))), 2)),
        ]);
        breaks_only(&body, "M4");
    }

    // ---- Linear values (ADR 0103, L1 to L4) ----

    /// A struct of the given kind, with a `u32` field `n` and the fields
    /// `more` after it.
    fn kind(name: &str, copy: bool, linear: bool, more: Vec<(&str, Ty)>) -> StructRef {
        let s = StructRef::new(
            name.to_owned(),
            Layout { size: 4, align: 4 },
            false,
            copy,
            linear,
        );
        let mut fields = vec![Field {
            name: "n".to_owned(),
            ty: u32_ty(),
            offset: 0,
        }];
        fields.extend(more.into_iter().map(|(name, ty)| Field {
            name: name.to_owned(),
            ty,
            offset: 4,
        }));
        s.def().set_fields(fields);
        s
    }

    /// `#[linear] struct Transfer { n: u32 }`.
    fn transfer() -> StructRef {
        thread_local! {
            static MADE: StructRef = kind("Transfer", false, true, Vec::new());
        }
        MADE.with(|s| *s)
    }

    /// `[Transfer; len]`.
    fn transfers(len: u64) -> Ty {
        Ty::Array(Box::leak(Box::new(Ty::Struct(transfer()))), len)
    }

    /// `struct Holds { n: u32, t: Transfer }`, not marked, linear by its field.
    fn holds() -> StructRef {
        thread_local! {
            static MADE: StructRef =
                kind("Holds", false, false, vec![("t", Ty::Struct(transfer()))]);
        }
        MADE.with(|s| *s)
    }

    /// `#[linear] struct Link { n: u32, t: Transfer }`: a linear struct
    /// holding another.
    fn link() -> StructRef {
        thread_local! {
            static MADE: StructRef =
                kind("Link", false, true, vec![("t", Ty::Struct(transfer()))]);
        }
        MADE.with(|s| *s)
    }

    /// `enum Lent { Free, Full(Transfer) }`, a `u8` tag, linear by its payload.
    fn lent() -> EnumRef {
        thread_local! {
            static MADE: EnumRef = made_lent();
        }
        MADE.with(|e| *e)
    }

    fn made_lent() -> EnumRef {
        let e = EnumRef::new(
            EnumId(3),
            "Lent".to_owned(),
            IntTy::U8,
            Layout { size: 8, align: 4 },
            false,
        );
        e.def().set_variants(vec![
            Variant {
                name: "Free".to_owned(),
                value: std::sync::OnceLock::from(0),
                payload: Vec::new(),
            },
            Variant {
                name: "Full".to_owned(),
                value: std::sync::OnceLock::from(1),
                payload: vec![Field {
                    name: "0".to_owned(),
                    ty: Ty::Struct(transfer()),
                    offset: 4,
                }],
            },
        ]);
        e
    }

    /// The rules broken by `new` of `projection` of a new local of type `ty`.
    fn newing(ty: Ty, projection: &[PlaceElem]) -> Vec<&'static str> {
        let mut body = valid();
        body.locals.push(decl(ty));
        let n = body.locals.len() as u32 - 1;
        statement_of(&mut body, Statement::New(projected(n, projection), span()));
        rules(&body)
    }

    #[test]
    fn l1_new_names_a_place_of_a_struct_marked_linear() {
        let payload = PlaceElem::Payload {
            variant: 1,
            index: 0,
        };
        accepts(newing(Ty::Struct(transfer()), &[]));
        accepts(newing(Ty::Struct(link()), &[]));
        // A part of a place, through fields and payload values.
        accepts(newing(Ty::Struct(link()), &[PlaceElem::Field(1)]));
        accepts(newing(Ty::Struct(holds()), &[PlaceElem::Field(1)]));
        accepts(newing(Ty::Enum(lent()), &[payload]));
        // A struct that is not marked, whatever it holds, and other types.
        rejects(newing(Ty::Struct(holds()), &[]), "L1");
        rejects(newing(Ty::Struct(handle()), &[]), "L1");
        rejects(newing(u32_ty(), &[]), "L1");
        rejects(newing(Ty::Enum(lent()), &[]), "L1");
        rejects(newing(Ty::Struct(link()), &[PlaceElem::Field(0)]), "L1");
        // Through a pointer, once, though the pointee is a linear struct.
        let to = ptr(true, false, Ty::Struct(transfer()));
        rejects(newing(to, &[PlaceElem::Deref]), "L1");
        rejects(newing(to, &[PlaceElem::Offset(Local(8))]), "L1");
        // A local bound to read-only data.
        let mut body = valid();
        body.locals.push(LocalDecl {
            ty: Ty::Struct(transfer()),
            name: None,
            constant: Some(DataId(0)),
        });
        let n = body.locals.len() as u32 - 1;
        statement_of(&mut body, Statement::New(local(n), span()));
        breaks_only(&body, "L1");
        // A local that does not exist, or a projection T1 rejects, is not
        // judged again.
        let mut body = valid();
        statement_of(&mut body, Statement::New(local(99), span()));
        breaks_only(&body, "S7");
        rejects(newing(Ty::Struct(transfer()), &[PlaceElem::Field(5)]), "T1");
    }

    /// The rules broken by `end` of `projection` of a new local of type `ty`.
    fn ending(ty: Ty, projection: &[PlaceElem]) -> Vec<&'static str> {
        let mut body = valid();
        body.locals.push(decl(ty));
        let n = body.locals.len() as u32 - 1;
        statement_of(&mut body, Statement::End(projected(n, projection), span()));
        rules(&body)
    }

    #[test]
    fn l1_end_names_a_place_of_a_struct_marked_linear() {
        let payload = PlaceElem::Payload {
            variant: 1,
            index: 0,
        };
        accepts(ending(Ty::Struct(transfer()), &[]));
        accepts(ending(Ty::Struct(link()), &[]));
        // A part of a place, through fields and payload values.
        accepts(ending(Ty::Struct(link()), &[PlaceElem::Field(1)]));
        accepts(ending(Ty::Struct(holds()), &[PlaceElem::Field(1)]));
        accepts(ending(Ty::Enum(lent()), &[payload]));
        // A struct that is not marked, whatever it holds, and other types.
        rejects(ending(Ty::Struct(holds()), &[]), "L1");
        rejects(ending(Ty::Struct(handle()), &[]), "L1");
        rejects(ending(u32_ty(), &[]), "L1");
        rejects(ending(Ty::Enum(lent()), &[]), "L1");
        // Through a pointer, once, though the pointee is a linear struct.
        let to = ptr(true, false, Ty::Struct(transfer()));
        rejects(ending(to, &[PlaceElem::Deref]), "L1");
        rejects(ending(to, &[PlaceElem::Offset(Local(8))]), "L1");
        // A local bound to read-only data.
        let mut body = valid();
        body.locals.push(LocalDecl {
            ty: Ty::Struct(transfer()),
            name: None,
            constant: Some(DataId(0)),
        });
        let n = body.locals.len() as u32 - 1;
        statement_of(&mut body, Statement::End(local(n), span()));
        breaks_only(&body, "L1");
        // A local that does not exist is not judged again.
        let mut body = valid();
        statement_of(&mut body, Statement::End(local(99), span()));
        breaks_only(&body, "S7");
    }

    #[test]
    fn l2_no_struct_is_marked_both_copy_and_linear() {
        accepts(declaring(Ty::Struct(transfer())));
        let both = kind("Both", true, true, Vec::new());
        rejects(declaring(Ty::Struct(both)), "L2");
        // Through a field, and a pointer that is not looked into: each
        // definition once.
        let outer = kind("Outer", false, false, vec![("b", Ty::Struct(both))]);
        let mut body = valid();
        body.locals.extend([
            decl(Ty::Struct(both)),
            decl(Ty::Struct(outer)),
            decl(ptr(true, false, Ty::Struct(both))),
        ]);
        breaks_only(&body, "L2");
    }

    #[test]
    fn l3_no_local_reaches_an_array_or_a_slice_of_linear_elements() {
        let slice = |elem| Ty::Slice(Box::leak(Box::new(elem)));
        let array = |elem, len| Ty::Array(Box::leak(Box::new(elem)), len);
        accepts(declaring(Ty::Struct(transfer())));
        accepts(declaring(Ty::Enum(lent())));
        accepts(declaring(ptr(true, false, Ty::Struct(transfer()))));
        accepts(declaring(array(u32_ty(), 2)));
        accepts(declaring(ptr(false, false, slice(u32_ty()))));
        rejects(declaring(transfers(2)), "L3");
        // An element that is linear by what it holds.
        rejects(declaring(array(Ty::Struct(holds()), 2)), "L3");
        rejects(declaring(array(Ty::Enum(lent()), 2)), "L3");
        // A pointer to a slice, and to an array.
        rejects(
            declaring(ptr(false, false, slice(Ty::Struct(transfer())))),
            "L3",
        );
        rejects(declaring(ptr(true, false, transfers(3))), "L3");
        // Nested arrays are one fault, at the inner array.
        rejects(declaring(array(transfers(2), 2)), "L3");
        // Through a field and a payload value.
        let table = kind("Table", false, false, vec![("slots", transfers(2))]);
        rejects(declaring(Ty::Struct(table)), "L3");
        let row = EnumRef::new(
            EnumId(4),
            "Row".to_owned(),
            IntTy::U8,
            Layout { size: 12, align: 4 },
            false,
        );
        row.def().set_variants(vec![Variant {
            name: "Full".to_owned(),
            value: std::sync::OnceLock::from(0),
            payload: vec![Field {
                name: "0".to_owned(),
                ty: transfers(2),
                offset: 4,
            }],
        }]);
        rejects(declaring(Ty::Enum(row)), "L3");
        // Through the pointee of a field.
        let aims = kind(
            "Aims",
            false,
            false,
            vec![("to", ptr(true, false, transfers(2)))],
        );
        rejects(declaring(Ty::Struct(aims)), "L3");
        // Each definition once, however many locals reach it.
        let mut body = valid();
        body.locals.extend([
            decl(Ty::Struct(table)),
            decl(Ty::Struct(table)),
            decl(ptr(true, false, Ty::Struct(table))),
        ]);
        breaks_only(&body, "L3");
    }

    #[test]
    fn l3_no_static_is_linear_or_reaches_an_array_of_linear_elements() {
        let with = |ty| {
            let mut mir = program();
            let id = mir.statics.len() as u32;
            mir.statics.push(Static {
                def: StaticRef::new(StaticId(id), "L".to_owned(), ty, false, true),
                bytes: Vec::new(),
                align: 4,
                relocations: Vec::new(),
            });
            program_rules(&mir)
        };
        assert_eq!(with(u32_ty()), Vec::<&str>::new());
        // A pointer to a linear struct is held, and nothing else.
        assert_eq!(
            with(ptr(false, false, Ty::Struct(transfer()))),
            Vec::<&str>::new()
        );
        assert_eq!(with(Ty::Struct(transfer())), ["L3"]);
        assert_eq!(with(Ty::Struct(holds())), ["L3"]);
        assert_eq!(with(Ty::Enum(lent())), ["L3"]);
        // An array of linear elements is the array's fault, once, and not
        // also a static of a linear type.
        assert_eq!(with(transfers(2)), ["L3"]);
        let table = kind("Table", false, false, vec![("slots", transfers(2))]);
        assert_eq!(with(Ty::Struct(table)), ["L3"]);
    }

    /// `struct Holder { n: u32, r: &u32 }` and `enum Held { Full(&u32) }`: the
    /// definitions that hold a reference.
    fn holding_references() -> (StructRef, EnumRef) {
        let holder = kind(
            "Holder",
            false,
            false,
            vec![("r", reference(false, u32_ty()))],
        );
        let held = EnumRef::new(
            EnumId(5),
            "Held".to_owned(),
            IntTy::U8,
            Layout { size: 8, align: 4 },
            false,
        );
        held.def().set_variants(vec![Variant {
            name: "Full".to_owned(),
            value: std::sync::OnceLock::from(0),
            payload: vec![Field {
                name: "0".to_owned(),
                ty: reference(true, u32_ty()),
                offset: 4,
            }],
        }]);
        (holder, held)
    }

    #[test]
    fn r1_a_reference_is_only_the_type_of_a_local() {
        let array = |elem, len| Ty::Array(Box::leak(Box::new(elem)), len);
        let slice = |elem| Ty::Slice(Box::leak(Box::new(elem)));
        let shared = reference(false, u32_ty());
        accepts(declaring(shared));
        accepts(declaring(reference(true, Ty::Struct(pair()))));
        accepts(declaring(reference(false, array(u32_ty(), 2))));
        // A pointer to a reference's referent is a pointer to a `u32`.
        accepts(declaring(ptr(false, false, u32_ty())));
        // A referent, an element, a pointee.
        rejects(declaring(reference(false, shared)), "R1");
        rejects(declaring(reference(true, reference(true, u32_ty()))), "R1");
        rejects(declaring(array(shared, 2)), "R1");
        rejects(declaring(ptr(false, false, slice(shared))), "R1");
        rejects(declaring(ptr(true, false, shared)), "R1");
        // A field, and a payload value.
        let (holder, held) = holding_references();
        rejects(declaring(Ty::Struct(holder)), "R1");
        rejects(declaring(Ty::Enum(held)), "R1");
        // Through a pointer to a definition, and an array of one.
        rejects(declaring(ptr(true, false, Ty::Struct(holder))), "R1");
        rejects(declaring(array(Ty::Enum(held), 2)), "R1");
        // Each definition once, however many locals reach it.
        let mut body = valid();
        body.locals.extend([
            decl(Ty::Struct(holder)),
            decl(Ty::Struct(holder)),
            decl(ptr(true, false, Ty::Struct(holder))),
        ]);
        breaks_only(&body, "R1");
    }

    #[test]
    fn r1_no_body_returns_a_reference() {
        let shared = reference(false, u32_ty());
        // `fn f(_1: &u32) -> &u32 { _0 = copy _1; return }`
        let mut body = valid();
        body.ret = shared;
        body.locals[0] = decl(shared);
        body.locals[1] = decl(shared);
        breaks_only(&body, "R1");
        // A return type that holds one is the type's fault as well, once.
        let (holder, _) = holding_references();
        let mut body = valid();
        body.ret = Ty::Struct(holder);
        body.locals[0] = decl(Ty::Struct(holder));
        body.locals[1] = decl(Ty::Struct(holder));
        body.blocks[0].statements[0] = Statement::Assign(local(0), Rvalue::Use(moved(local(1))));
        breaks_only(&body, "R1");
    }

    #[test]
    fn r1_no_static_is_a_reference_or_reaches_one() {
        let with = |ty| {
            let mut mir = program();
            let id = mir.statics.len() as u32;
            mir.statics.push(Static {
                def: StaticRef::new(StaticId(id), "R".to_owned(), ty, false, true),
                bytes: Vec::new(),
                align: 4,
                relocations: Vec::new(),
            });
            program_rules(&mir)
        };
        let shared = reference(false, u32_ty());
        let (holder, held) = holding_references();
        assert_eq!(with(u32_ty()), Vec::<&str>::new());
        assert_eq!(with(ptr(false, false, u32_ty())), Vec::<&str>::new());
        // The type itself, and what it reaches.
        assert_eq!(with(shared), ["R1"]);
        assert_eq!(with(reference(true, shared)), ["R1"]);
        assert_eq!(with(ptr(false, false, shared)), ["R1"]);
        assert_eq!(with(Ty::Struct(holder)), ["R1"]);
        assert_eq!(with(Ty::Enum(held)), ["R1"]);
        assert_eq!(with(Ty::Array(Box::leak(Box::new(shared)), 2)), ["R1"]);
    }

    /// The rules broken by a write to `projection` of a new local of type
    /// `pointer`, of a new local of type `value`, as an assignment or as the
    /// destination of a call.
    fn writing(pointer: Ty, projection: &[PlaceElem], value: Ty, call: bool) -> Vec<&'static str> {
        let mut body = valid();
        body.locals.extend([decl(pointer), decl(value)]);
        let n = body.locals.len() as u32 - 2;
        let place = projected(n, projection);
        let statement = if call {
            Statement::Call {
                dest: Some(place),
                callee: BodyId(0),
                args: Vec::new(),
            }
        } else {
            let from = local(n + 1);
            let operand = if value.is_copy() {
                Operand::Copy(from)
            } else {
                moved(from)
            };
            Statement::Assign(place, Rvalue::Use(operand))
        };
        statement_of(&mut body, statement);
        rules(&body)
    }

    #[test]
    fn l4_no_linear_value_is_written_through_a_pointer() {
        let to = |ty| ptr(true, false, ty);
        let deref = [PlaceElem::Deref];
        for call in [false, true] {
            // A copy value, and a copy field of a linear struct.
            accepts(writing(to(u32_ty()), &deref, u32_ty(), call));
            accepts(writing(
                to(Ty::Struct(link())),
                &[PlaceElem::Deref, PlaceElem::Field(0)],
                u32_ty(),
                call,
            ));
            // A pointer to a linear value, written whole, or the value itself.
            accepts(writing(
                to(to(Ty::Struct(transfer()))),
                &deref,
                to(Ty::Struct(transfer())),
                call,
            ));
            rejects(
                writing(
                    to(Ty::Struct(transfer())),
                    &deref,
                    Ty::Struct(transfer()),
                    call,
                ),
                "L4",
            );
            rejects(
                writing(to(Ty::Struct(holds())), &deref, Ty::Struct(holds()), call),
                "L4",
            );
            rejects(
                writing(
                    to(Ty::Struct(holds())),
                    &[PlaceElem::Deref, PlaceElem::Field(1)],
                    Ty::Struct(transfer()),
                    call,
                ),
                "L4",
            );
            rejects(
                writing(to(Ty::Enum(lent())), &deref, Ty::Enum(lent()), call),
                "L4",
            );
            rejects(
                writing(
                    to(Ty::Struct(transfer())),
                    &[PlaceElem::Offset(Local(8))],
                    Ty::Struct(transfer()),
                    call,
                ),
                "L4",
            );
        }
        // Into a local, and into a part of a local: no pointer.
        let mut body = valid();
        body.locals
            .extend([decl(Ty::Struct(holds())), decl(Ty::Struct(transfer()))]);
        let n = body.locals.len() as u32 - 2;
        statement_of(
            &mut body,
            Statement::Assign(
                projected(n, &[PlaceElem::Field(1)]),
                Rvalue::Use(moved(local(n + 1))),
            ),
        );
        accepts(rules(&body));
    }

    // ---- `critical` blocks (ADR 0095, C1 to C3) ----

    /// A body of the blocks given as statements and a terminator, with the
    /// locals of `typed`: `_1` and `_3` are `u32`, the locals of `critical`
    /// blocks, and `_11` is the `bool` an `if` tests.
    fn flow(blocks: Vec<(Vec<Statement>, Terminator)>) -> Body {
        let mut body = typed();
        body.blocks = blocks
            .into_iter()
            .map(|(statements, terminator)| block(statements, terminator))
            .collect();
        body
    }

    fn enter(n: u32) -> Statement {
        Statement::CriticalEnter(Local(n))
    }

    fn exit(n: u32) -> Statement {
        Statement::CriticalExit(Local(n))
    }

    fn goto(n: u32) -> Terminator {
        Terminator::Goto(BasicBlock(n))
    }

    fn branch(then: u32, else_: u32) -> Terminator {
        Terminator::If {
            cond: Operand::Copy(local(BOOL)),
            then: BasicBlock(then),
            else_: BasicBlock(else_),
        }
    }

    fn fails() -> Terminator {
        Terminator::Panic(PanicCause::DivisionByZero, None)
    }

    #[test]
    fn critical_blocks_that_are_left_on_every_path_are_valid() {
        // Nested, and a join where both paths have the same blocks open.
        accepts(rules(&flow(vec![
            (vec![enter(1), enter(3)], branch(1, 2)),
            (vec![], goto(3)),
            (vec![], goto(3)),
            (vec![exit(3), exit(1)], Terminator::Return),
        ])));
        // A `break` leaving two blocks, in a loop that enters them again.
        accepts(rules(&flow(vec![
            (vec![], goto(1)),
            (vec![enter(1), enter(3)], branch(2, 3)),
            (vec![exit(3), exit(1)], goto(1)),
            (vec![exit(3), exit(1)], goto(4)),
            (vec![], Terminator::Return),
        ])));
        // A `return` inside a block, which leaves it first.
        accepts(rules(&flow(vec![
            (vec![enter(1)], branch(1, 2)),
            (vec![exit(1)], Terminator::Return),
            (vec![exit(1)], Terminator::Return),
        ])));
        // A panic inside a block: nothing returns to close it.
        accepts(rules(&flow(vec![(vec![enter(1)], fails())])));
        // The same block entered again, by a loop, after it was left.
        accepts(rules(&flow(vec![
            (vec![], goto(1)),
            (vec![enter(1), exit(1)], branch(1, 2)),
            (vec![], Terminator::Return),
        ])));
        // A block nothing reaches, with an exit that closes nothing and a
        // `return` at depth 1.
        accepts(rules(&flow(vec![
            (vec![], Terminator::Return),
            (vec![exit(1)], Terminator::Return),
            (vec![enter(3)], Terminator::Return),
        ])));
    }

    #[test]
    fn c1_a_block_has_one_stack_of_open_blocks_whatever_the_path() {
        // Entered at depth 0 and at depth 1.
        let body = flow(vec![
            (vec![], branch(1, 2)),
            (vec![enter(1)], goto(3)),
            (vec![], goto(3)),
            (vec![], fails()),
        ]);
        breaks_only(&body, "C1");
        let found = check_body(&body);
        assert_eq!(found[0].at, Location::Terminator(BasicBlock(1)));
        // Entered at depth 1 through two different locals.
        breaks_only(
            &flow(vec![
                (vec![], branch(1, 2)),
                (vec![enter(1)], goto(3)),
                (vec![enter(3)], goto(3)),
                (vec![], fails()),
            ]),
            "C1",
        );
        // A loop that opens a block on every turn.
        breaks_only(
            &flow(vec![(vec![], goto(1)), (vec![enter(1)], goto(1))]),
            "C1",
        );
        // The fault is reported once, not again in what follows the join.
        breaks_only(
            &flow(vec![
                (vec![], branch(1, 2)),
                (vec![enter(1)], goto(3)),
                (vec![], goto(3)),
                (vec![], goto(4)),
                (vec![], fails()),
            ]),
            "C1",
        );
    }

    #[test]
    fn c2_an_exit_closes_the_innermost_open_block() {
        // At depth 0.
        let body = flow(vec![(vec![exit(1)], Terminator::Return)]);
        breaks_only(&body, "C2");
        assert_eq!(
            check_body(&body)[0].at,
            Location::Statement(BasicBlock(0), 0)
        );
        // The outer local named while the inner is open: one violation, the
        // exit closes both.
        breaks_only(
            &flow(vec![(
                vec![enter(1), enter(3), exit(1)],
                Terminator::Return,
            )]),
            "C2",
        );
        // A local that is not open, while another is.
        breaks_only(
            &flow(vec![(vec![enter(1), exit(3), exit(1)], Terminator::Return)]),
            "C2",
        );
    }

    #[test]
    fn c2_does_not_repeat_a_missing_local() {
        // S7 spoke of the local; the exit is not also a mismatch.
        let body = flow(vec![(vec![enter(1), exit(GHOST)], Terminator::Return)]);
        assert_eq!(rules(&body), ["S7", "C3"]);
        // At depth 0 it still is a fault of its own.
        let body = flow(vec![(vec![exit(GHOST)], Terminator::Return)]);
        assert_eq!(rules(&body), ["C2", "S7"]);
    }

    #[test]
    fn c3_a_return_is_at_depth_0() {
        let body = flow(vec![(vec![enter(1)], Terminator::Return)]);
        breaks_only(&body, "C3");
        assert_eq!(check_body(&body)[0].at, Location::Terminator(BasicBlock(0)));
        breaks_only(
            &flow(vec![(
                vec![enter(1), enter(3), exit(3)],
                Terminator::Return,
            )]),
            "C3",
        );
    }

    // ---- The program (ADR 0095, P1 to P8) ----

    /// `fn name(params) -> ret` with `extra` locals after the parameters, which
    /// returns at once (diverges, for `!`).
    fn fun(name: &str, params: &[Ty], ret: Ty, extra: &[Ty]) -> Body {
        let mut body = valid();
        body.name = name.to_owned();
        body.ret = ret;
        body.param_count = params.len();
        body.locals = std::iter::once(ret)
            .chain(params.iter().copied())
            .chain(extra.iter().copied())
            .map(decl)
            .collect();
        let end = if ret == Ty::Never {
            Terminator::Unreachable
        } else {
            Terminator::Return
        };
        body.blocks = vec![block(Vec::new(), end)];
        body
    }

    fn call(callee: u32, dest: Option<Place>, args: Vec<Operand>) -> Statement {
        Statement::Call {
            dest,
            callee: BodyId(callee),
            args,
        }
    }

    // The bodies of `program`.
    const ENTRY: usize = 0;
    const PANIC: usize = 1;
    const CALLEE: usize = 2;
    const COMPILE_TIME: usize = 3;
    const TICK: usize = 4;
    const CALLER: usize = 5;
    const READS: usize = 6;
    const START: usize = 7;
    const ENTER: usize = 8;
    const EXIT: usize = 9;

    /// A program that follows every rule:
    /// 0 `entry() -> !`, 1 `panic() -> !`, 2 `callee(u32) -> u32`,
    /// 3 `compile_time() -> u32` (`comptime`), 4 `tick()` (a handler),
    /// 5 `caller()` with the locals `_1: u32`, `_2: *mut u32` and
    /// `_3: *const u32`, which calls 2 and 6, 6 `reads(*const u32)`,
    /// 7 `start() -> !` (`reset`), 8 `enter() -> u32` and 9 `exit(u32)`;
    /// the statics 0 `S: u32`, 1 `extern X: u32`, 2 `P: *const u32` pointing
    /// to `S` and 3 the byte string `b"hi"`; one piece of data of 8 bytes that
    /// points to `S` at its end; one peripheral; a vector table.
    fn program() -> Mir {
        let u = u32_ty();
        let mut compile_time = fun("compile_time", &[], u, &[]);
        compile_time.comptime = true;
        let mut caller = fun(
            "caller",
            &[],
            Ty::Unit,
            &[u, ptr(true, false, u), ptr(false, false, u)],
        );
        for statement in [
            call(CALLEE as u32, Some(local(1)), vec![copy(1)]),
            call(READS as u32, None, vec![copy(2)]),
        ] {
            statement_of(&mut caller, statement);
        }
        let stat = |id, name: &str, ty, external| {
            StaticRef::new(StaticId(id), name.to_owned(), ty, false, external)
        };
        let held = |def, bytes: usize, relocations| Static {
            def,
            bytes: vec![0; bytes],
            align: 4,
            relocations,
        };
        let s = stat(0, "S", u, false);
        let hi = StaticRef::byte_string(StaticId(3), b"hi".to_vec());
        Mir {
            structs: Vec::new(),
            bit_structs: Vec::new(),
            enums: Vec::new(),
            peripheral_types: Vec::new(),
            peripherals: vec![PeripheralRef::new(
                "UART".to_owned(),
                0x4000_0000,
                peripheral(),
            )],
            data: vec![ConstData {
                bytes: vec![0; 8],
                align: 4,
                relocations: vec![Relocation {
                    offset: 4,
                    target: StaticId(0),
                    addend: 4,
                }],
            }],
            statics: vec![
                held(s, 4, Vec::new()),
                held(stat(1, "X", u, true), 0, Vec::new()),
                held(
                    stat(2, "P", ptr(false, false, u), false),
                    4,
                    vec![Relocation {
                        offset: 0,
                        target: StaticId(0),
                        addend: 4,
                    }],
                ),
                Static {
                    def: hi,
                    bytes: b"hi".to_vec(),
                    align: 1,
                    relocations: Vec::new(),
                },
            ],
            bodies: vec![
                fun("entry", &[], Ty::Never, &[]),
                fun("panic", &[], Ty::Never, &[]),
                fun("callee", &[u], u, &[]),
                compile_time,
                fun("tick", &[], Ty::Unit, &[]),
                caller,
                fun("reads", &[ptr(false, false, u)], Ty::Unit, &[]),
                fun("start", &[], Ty::Never, &[]),
                fun("enter", &[], u, &[]),
                fun("exit", &[u], Ty::Unit, &[]),
            ],
            entry: BodyId(ENTRY as u32),
            panic: BodyId(PANIC as u32),
            reset: Some(BodyId(START as u32)),
            critical_enter: Some(BodyId(ENTER as u32)),
            critical_exit: Some(BodyId(EXIT as u32)),
            handlers: vec![Handler {
                vector: "SysTick".to_owned(),
                word: 2,
                kind: crate::target::VectorKind::Maskable,
                body: BodyId(TICK as u32),
            }],
            vector_table: Some(VectorTable {
                words: vec![
                    VectorWord::StackEnd,
                    VectorWord::Reset,
                    VectorWord::Handler(BodyId(TICK as u32)),
                    VectorWord::Default(0),
                ],
                defaults: vec![DefaultHandler {
                    symbol: "__erebos_unhandled".to_owned(),
                    info: Some(DataId(0)),
                }],
            }),
            memory: None,
        }
    }

    /// `program` whose panic function takes a `*const Info`, and `caller` has
    /// a local `_4: Info`; the type `Info`.
    fn with_panic_info() -> (Mir, Ty) {
        let mut mir = program();
        let info = Ty::Struct(pair());
        mir.bodies[PANIC] = fun("panic", &[ptr(false, false, info)], Ty::Never, &[]);
        mir.bodies[CALLER].locals.push(decl(info));
        (mir, info)
    }

    /// The rules of the violations of the program, in order.
    fn program_rules(mir: &Mir) -> Vec<&'static str> {
        check_program(mir).iter().map(|v| v.rule).collect()
    }

    #[track_caller]
    fn program_breaks_only(mir: &Mir, rule: &str) {
        assert_eq!(program_rules(mir), [rule], "{:#?}", check_program(mir));
    }

    #[test]
    fn a_valid_program_reports_nothing() {
        assert!(check_program(&program()).is_empty());
        // After `leave_out` these may be `None`.
        let mut mir = program();
        mir.reset = None;
        mir.critical_enter = None;
        mir.critical_exit = None;
        mir.vector_table = None;
        mir.handlers.clear();
        assert!(check_program(&mir).is_empty());
        let (mir, _) = with_panic_info();
        assert!(check_program(&mir).is_empty());
    }

    #[test]
    fn p1_every_body_follows_the_rules_of_one_body() {
        let mut mir = program();
        mir.bodies[CALLEE].blocks[0].terminator = Terminator::Goto(BasicBlock(7));
        mir.bodies[CALLER].blocks[0].terminator = Terminator::Goto(BasicBlock(8));
        let found = check_program(&mir);
        assert_eq!(found.len(), 2, "{found:#?}");
        assert_eq!((found[0].body.as_str(), found[0].rule), ("callee", "S5"));
        assert_eq!((found[1].body.as_str(), found[1].rule), ("caller", "S5"));

        // Each body is looked at once, and the program once.
        let bodies = bodies_checked();
        let programs = programs_checked();
        check_program(&program());
        assert_eq!(bodies_checked() - bodies, 10);
        assert_eq!(programs_checked() - programs, 1);
    }

    #[test]
    fn the_program_s_violations_come_before_those_of_the_bodies_by_index() {
        let mut mir = program();
        mir.bodies[CALLER].blocks[0].terminator = Terminator::Goto(BasicBlock(8));
        mir.bodies[CALLEE].blocks[0].terminator = Terminator::Goto(BasicBlock(7));
        mir.statics[1].bytes = vec![0];
        mir.data[0].relocations[0].addend = 9;
        let found = check_program(&mir);
        let order: Vec<(&str, &str)> = found.iter().map(|v| (v.body.as_str(), v.rule)).collect();
        assert_eq!(
            order,
            [
                ("static `X`", "P6"),
                ("data 0", "P7"),
                ("callee", "S5"),
                ("caller", "S5")
            ]
        );
        assert_eq!(
            found[0].to_string(),
            "static `X`: P6: it is external but has 1 bytes"
        );
        assert_eq!(
            found[2].to_string(),
            "`callee`, bb0, terminator: S5: a terminator jumps to bb7, but the body has 1 blocks"
        );
    }

    #[test]
    fn p2_a_body_named_exists() {
        // A callee, and a `sym`.
        let mut mir = program();
        statement_of(&mut mir.bodies[CALLER], call(99, None, Vec::new()));
        program_breaks_only(&mir, "P2");
        let mut mir = program();
        let sym = |sym| {
            Statement::InlineAsm(Box::new(asm(
                AsmOptions::default(),
                Vec::new(),
                vec![AsmOperand {
                    reg: AsmReg::Any,
                    kind: AsmOperandKind::Sym(sym),
                }],
            )))
        };
        statement_of(&mut mir.bodies[CALLER], sym(AsmSym::Body(BodyId(99))));
        program_breaks_only(&mir, "P2");
        // A `sym` of a static that is the program's, and of a body.
        let mut mir = program();
        statement_of(&mut mir.bodies[CALLER], sym(AsmSym::Body(BodyId(2))));
        let held = mir.statics[0].def;
        statement_of(&mut mir.bodies[CALLER], sym(AsmSym::Static(held)));
        assert!(check_program(&mir).is_empty());
        statement_of(
            &mut mir.bodies[CALLER],
            sym(AsmSym::Static(StaticRef::new(
                StaticId(0),
                "S".to_owned(),
                u32_ty(),
                false,
                false,
            ))),
        );
        program_breaks_only(&mir, "P2");

        // The roles, each past the end.
        let past = BodyId(99);
        let roles: [fn(&mut Mir, BodyId); 6] = [
            |mir, id| mir.entry = id,
            |mir, id| mir.panic = id,
            |mir, id| mir.reset = Some(id),
            |mir, id| mir.critical_enter = Some(id),
            |mir, id| mir.critical_exit = Some(id),
            |mir, id| mir.handlers[0].body = id,
        ];
        for role in roles {
            let mut mir = program();
            role(&mut mir, past);
            program_breaks_only(&mir, "P2");
        }

        // The vector table.
        let mut mir = program();
        mir.vector_table.as_mut().unwrap().words[2] = VectorWord::Handler(past);
        program_breaks_only(&mir, "P2");
        let mut mir = program();
        mir.vector_table.as_mut().unwrap().words[3] = VectorWord::Default(5);
        program_breaks_only(&mir, "P2");
        let mut mir = program();
        mir.vector_table.as_mut().unwrap().defaults[0].info = Some(DataId(9));
        program_breaks_only(&mir, "P2");
    }

    #[test]
    fn p2_a_static_named_is_that_static() {
        let into = |mir: &mut Mir, operand| {
            let target = ptr(false, false, u32_ty());
            let caller = &mut mir.bodies[CALLER];
            caller.locals.push(decl(target));
            let last = caller.locals.len() as u32 - 1;
            statement_of(
                caller,
                Statement::Assign(local(last), Rvalue::Use(Operand::Const(operand))),
            );
        };
        let mut mir = program();
        let held = mir.statics[0].def;
        into(&mut mir, Const::Static(held));
        assert!(check_program(&mir).is_empty());

        // Same id, another definition: what is left from before a renumbering.
        let stale = StaticRef::new(StaticId(0), "S".to_owned(), u32_ty(), false, false);
        into(&mut mir, Const::Static(stale));
        program_breaks_only(&mir, "P2");

        let mut mir = program();
        let beyond = StaticRef::new(StaticId(99), "Z".to_owned(), u32_ty(), false, false);
        into(&mut mir, Const::Static(beyond));
        program_breaks_only(&mir, "P2");

        // A panic site's file.
        let (mut mir, _) = with_panic_info();
        let site = |file| PanicSite {
            info: PanicInfoAt::Data(DataId(0)),
            file,
            line: 1,
            column: 1,
        };
        let held = mir.statics[3].def;
        mir.bodies[CALLER].blocks[0].terminator =
            Terminator::Panic(PanicCause::DivisionByZero, Some(site(held)));
        assert!(check_program(&mir).is_empty());
        mir.bodies[CALLER].blocks[0].terminator =
            Terminator::Panic(PanicCause::DivisionByZero, Some(site(beyond)));
        program_breaks_only(&mir, "P2");
    }

    #[test]
    fn p2_a_peripheral_named_is_in_the_programs() {
        let mut mir = program();
        let uart = mir.peripherals[0];
        let target = uart.def().address_ty();
        mir.bodies[CALLER].locals.push(decl(target));
        let last = mir.bodies[CALLER].locals.len() as u32 - 1;
        let assign = |peripheral| {
            Statement::Assign(
                local(last),
                Rvalue::Use(Operand::Const(Const::Peripheral(peripheral))),
            )
        };
        statement_of(&mut mir.bodies[CALLER], assign(uart));
        assert!(check_program(&mir).is_empty());
        // Another peripheral of the same name and address, not in the list.
        let other = PeripheralRef::new("UART".to_owned(), 0x4000_0000, peripheral());
        mir.bodies[CALLER].blocks[0].statements.pop();
        mir.bodies[CALLER].blocks[0].spans.pop();
        statement_of(&mut mir.bodies[CALLER], assign(other));
        // Its address type is another pointer to another peripheral type, so
        // T5 would also speak: give the local the type of the other.
        mir.bodies[CALLER].locals[last as usize].ty = other.def().address_ty();
        program_breaks_only(&mir, "P2");
    }

    #[test]
    fn p2_read_only_data_named_exists() {
        // A local's.
        let mut mir = program();
        mir.bodies[CALLER].locals[2].constant = Some(DataId(0));
        assert!(check_program(&mir).is_empty());
        mir.bodies[CALLER].locals[2].constant = Some(DataId(9));
        program_breaks_only(&mir, "P2");
        // A panic site's.
        let (mut mir, _) = with_panic_info();
        let file = mir.statics[3].def;
        let site = |info| PanicSite {
            info,
            file,
            line: 1,
            column: 1,
        };
        mir.bodies[CALLER].blocks[0].terminator = Terminator::Panic(
            PanicCause::DivisionByZero,
            Some(site(PanicInfoAt::Data(DataId(9)))),
        );
        program_breaks_only(&mir, "P2");
        // The targets of relocations, of a static and of data.
        let mut mir = program();
        mir.statics[2].relocations[0].target = StaticId(9);
        program_breaks_only(&mir, "P2");
        let mut mir = program();
        mir.data[0].relocations[0].target = StaticId(9);
        program_breaks_only(&mir, "P2");
    }

    #[test]
    fn p3_a_call_passes_what_the_callee_takes() {
        // One argument too many, and one too few.
        let mut mir = program();
        mir.bodies[CALLER].blocks[0].statements[0] =
            call(CALLEE as u32, Some(local(1)), vec![copy(1), copy(1)]);
        program_breaks_only(&mir, "P3");
        let mut mir = program();
        mir.bodies[CALLER].blocks[0].statements[0] =
            call(CALLEE as u32, Some(local(1)), Vec::new());
        program_breaks_only(&mir, "P3");
        // An argument of the wrong type, and a `*mut u32` where `*const u32` is
        // taken (the program has the second already).
        let mut mir = program();
        mir.bodies[CALLER].blocks[0].statements[0] =
            call(CALLEE as u32, Some(local(1)), vec![copy(2)]);
        program_breaks_only(&mir, "P3");
        let mut mir = program();
        mir.bodies[CALLER].blocks[0].statements[1] = call(READS as u32, None, vec![copy(3)]);
        assert!(check_program(&mir).is_empty());
        // `*const u32` where `*mut u32` is taken is not.
        mir.bodies[READS].locals[1].ty = ptr(true, false, u32_ty());
        program_breaks_only(&mir, "P3");
        // A destination the return type does not fit; none is fine.
        let mut mir = program();
        mir.bodies[CALLER].blocks[0].statements[0] =
            call(CALLEE as u32, Some(local(2)), vec![copy(1)]);
        program_breaks_only(&mir, "P3");
        let mut mir = program();
        mir.bodies[CALLER].blocks[0].statements[0] = call(CALLEE as u32, None, vec![copy(1)]);
        assert!(check_program(&mir).is_empty());
    }

    #[test]
    fn p3_does_not_cascade_from_an_argument_or_a_callee_that_is_wrong() {
        // The argument is a place that does not fit its type: T1 speaks, and
        // its type is not compared with the parameter's.
        let mut mir = program();
        mir.bodies[CALLER].blocks[0].statements[0] = call(
            CALLEE as u32,
            Some(local(1)),
            vec![Operand::Copy(projected(1, &[PlaceElem::Deref]))],
        );
        program_breaks_only(&mir, "T1");
        // The callee does not exist: P2 only, with no arity to compare.
        let mut mir = program();
        mir.bodies[CALLER].blocks[0].statements[0] =
            call(99, Some(local(1)), vec![copy(1), copy(1)]);
        program_breaks_only(&mir, "P2");
    }

    #[test]
    fn p4_a_body_of_the_program_does_not_use_one_for_compile_time() {
        let mut mir = program();
        statement_of(
            &mut mir.bodies[CALLER],
            call(COMPILE_TIME as u32, None, Vec::new()),
        );
        program_breaks_only(&mir, "P4");
        let mut mir = program();
        statement_of(
            &mut mir.bodies[CALLER],
            Statement::InlineAsm(Box::new(asm(
                AsmOptions::default(),
                Vec::new(),
                vec![AsmOperand {
                    reg: AsmReg::Any,
                    kind: AsmOperandKind::Sym(AsmSym::Body(BodyId(COMPILE_TIME as u32))),
                }],
            ))),
        );
        program_breaks_only(&mir, "P4");
        // A body for compile time may call one, and itself.
        let mut mir = program();
        mir.bodies[CALLER].comptime = true;
        statement_of(
            &mut mir.bodies[CALLER],
            call(COMPILE_TIME as u32, None, Vec::new()),
        );
        assert!(check_program(&mir).is_empty());

        // The roles that run in the program.
        let roles: [fn(&mut Mir); 4] = [
            |mir| mir.bodies[ENTRY].comptime = true,
            |mir| mir.bodies[PANIC].comptime = true,
            |mir| mir.bodies[START].comptime = true,
            |mir| mir.bodies[TICK].comptime = true,
        ];
        for role in roles {
            let mut mir = program();
            role(&mut mir);
            program_breaks_only(&mir, "P4");
        }
    }

    #[test]
    fn p5_the_roles_have_their_shapes() {
        let u = u32_ty();
        let (_, info) = with_panic_info();
        type Change = fn(&mut Mir, Ty, Ty);
        let cases: [(&str, Change); 11] = [
            ("entry returns u32", |mir, u, _| {
                mir.bodies[ENTRY] = fun("entry", &[], u, &[]);
            }),
            ("entry takes a parameter", |mir, u, _| {
                mir.bodies[ENTRY] = fun("entry", &[u], Ty::Never, &[]);
            }),
            ("panic returns ()", |mir, _, _| {
                mir.bodies[PANIC] = fun("panic", &[], Ty::Unit, &[]);
            }),
            ("panic takes two", |mir, _, info| {
                let p = ptr(false, false, info);
                mir.bodies[PANIC] = fun("panic", &[p, p], Ty::Never, &[]);
            }),
            ("panic takes a u32", |mir, u, _| {
                mir.bodies[PANIC] = fun("panic", &[u], Ty::Never, &[]);
            }),
            ("panic takes a slice", |mir, _, _| {
                let slice = ptr(
                    false,
                    false,
                    Ty::Slice(Box::leak(Box::new(Ty::Int(IntTy::U8)))),
                );
                mir.bodies[PANIC] = fun("panic", &[slice], Ty::Never, &[]);
            }),
            ("a handler takes a parameter", |mir, u, _| {
                mir.bodies[TICK] = fun("tick", &[u], Ty::Unit, &[]);
            }),
            ("critical_enter returns ()", |mir, _, _| {
                mir.bodies[ENTER] = fun("enter", &[], Ty::Unit, &[]);
            }),
            ("critical_enter takes a parameter", |mir, u, _| {
                mir.bodies[ENTER] = fun("enter", &[u], u, &[]);
            }),
            ("critical_exit takes none", |mir, _, _| {
                mir.bodies[EXIT] = fun("exit", &[], Ty::Unit, &[]);
            }),
            ("critical_exit takes a u64", |mir, _, _| {
                let wide = Ty::Int(IntTy::U64);
                mir.bodies[EXIT] = fun("exit", &[wide], Ty::Unit, &[]);
            }),
        ];
        for (what, change) in cases {
            let mut mir = program();
            change(&mut mir, u, info);
            assert_eq!(
                program_rules(&mir),
                ["P5"],
                "{what}: {:#?}",
                check_program(&mir)
            );
        }
        // The shapes that are allowed: a panic function with no parameter or
        // one thin pointer, and a `reset` of any shape.
        let (mir, _) = with_panic_info();
        assert!(check_program(&mir).is_empty());
        let mut mir = program();
        mir.bodies[START] = fun("start", &[u], u, &[]);
        assert!(check_program(&mir).is_empty());
    }

    #[test]
    fn p6_a_static_has_its_index_a_type_of_values_and_its_bytes() {
        // Its id is not its index.
        let mut mir = program();
        mir.statics[1].def = StaticRef::new(StaticId(5), "X".to_owned(), u32_ty(), false, true);
        program_breaks_only(&mir, "P6");
        // An external static with bytes.
        let mut mir = program();
        mir.statics[1].bytes = vec![0; 4];
        program_breaks_only(&mir, "P6");
        // One byte too few, and one too many.
        let mut mir = program();
        mir.statics[0].bytes.pop();
        program_breaks_only(&mir, "P6");
        let mut mir = program();
        mir.statics[0].bytes.push(0);
        program_breaks_only(&mir, "P6");
        // A type with no value: reported once, whatever its bytes, and the
        // relocation that points to it is not also at fault.
        for ty in [
            Ty::Slice(Box::leak(Box::new(u32_ty()))),
            Ty::Never,
            Ty::Peripheral(peripheral()),
        ] {
            let mut mir = program();
            mir.statics[0].def = StaticRef::new(StaticId(0), "S".to_owned(), ty, false, false);
            program_breaks_only(&mir, "P6");
        }
    }

    #[test]
    fn p7_a_relocation_lies_inside_its_bytes_and_its_target() {
        // Past the end of a static's bytes, and of data's.
        let mut mir = program();
        mir.statics[2].relocations[0].offset = 1;
        program_breaks_only(&mir, "P7");
        let mut mir = program();
        mir.data[0].relocations[0].offset = 5;
        program_breaks_only(&mir, "P7");
        // The last place there is room for it is fine.
        let mut mir = program();
        mir.data[0].relocations[0].offset = 4;
        assert!(check_program(&mir).is_empty());
        // An addend past the target's size, from a static and from data;
        // the size itself is fine (the program has it).
        let mut mir = program();
        mir.statics[2].relocations[0].addend = 5;
        program_breaks_only(&mir, "P7");
        let mut mir = program();
        mir.data[0].relocations[0].addend = 5;
        program_breaks_only(&mir, "P7");
        // An external target has its type's size, though it has no bytes.
        let mut mir = program();
        mir.data[0].relocations[0].target = StaticId(1);
        assert!(check_program(&mir).is_empty());
        mir.data[0].relocations[0].addend = 5;
        program_breaks_only(&mir, "P7");
    }

    #[test]
    fn p8_a_panic_carries_a_site_when_the_panic_function_takes_one() {
        let panics = |mir: &mut Mir, comptime: bool, site| {
            mir.bodies[CALLER].comptime = comptime;
            mir.bodies[CALLER].blocks[0].terminator =
                Terminator::Panic(PanicCause::DivisionByZero, site);
        };
        let at = |mir: &Mir, info| PanicSite {
            info,
            file: mir.statics[3].def,
            line: 1,
            column: 1,
        };
        // A function that takes none: no site.
        let mut mir = program();
        panics(&mut mir, false, None);
        assert!(check_program(&mir).is_empty());
        let site = at(&mir, PanicInfoAt::Data(DataId(0)));
        panics(&mut mir, false, Some(site));
        program_breaks_only(&mir, "P8");

        // A function that takes one: a runtime body carries a site, in data
        // or in a local of the type the parameter points to.
        let (mut mir, _) = with_panic_info();
        panics(&mut mir, false, None);
        program_breaks_only(&mir, "P8");
        let data = at(&mir, PanicInfoAt::Data(DataId(0)));
        panics(&mut mir, false, Some(data));
        assert!(check_program(&mir).is_empty());
        let local = at(&mir, PanicInfoAt::Local(Local(4)));
        panics(&mut mir, false, Some(local));
        assert!(check_program(&mir).is_empty());
        // In a local of another type.
        let other = at(&mir, PanicInfoAt::Local(Local(1)));
        panics(&mut mir, false, Some(other));
        program_breaks_only(&mir, "P8");

        // Code for compile time never calls it: no site, whatever the function.
        panics(&mut mir, true, None);
        assert!(check_program(&mir).is_empty());
        panics(&mut mir, true, Some(data));
        program_breaks_only(&mir, "P8");
        let mut plain = program();
        panics(&mut plain, true, None);
        assert!(check_program(&plain).is_empty());
    }
}
