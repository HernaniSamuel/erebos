//! Validation of MIR (ADR 0095): the rules every consumer of MIR relies on,
//! checked where MIR is made. A violation is a bug of MIR building, never of
//! the program, so a run that finds one panics with the whole list.
//!
//! The checks read MIR only, compute their own types and never call what
//! panics on bad input (`Body::place_ty`, `Body::params`). A rule whose input
//! another rule found invalid is skipped for that input, so that one fault is
//! one violation. The `match`es on `Statement`, `Terminator`, `Rvalue`,
//! `Operand`, `PlaceElem` and `AsmOperandKind` name every variant and have
//! no `_` arm, so that a new variant fails to compile here.

use std::fmt;

use super::{
    AsmOperandKind, AsmPiece, BasicBlock, Body, InlineAsm, Local, Operand, PanicCause, PanicInfoAt,
    Place, PlaceElem, Rvalue, Statement, Terminator, Ty,
};

/// Where in a body a rule is broken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Location {
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
            Location::Body => (0, 0),
            Location::Block(block) => (block.0 + 1, 0),
            Location::Statement(block, index) => (block.0 + 1, index + 1),
            Location::Terminator(block) => (block.0 + 1, usize::MAX),
        }
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
}

/// How many bodies `check_body` has looked at on this thread.
#[cfg(test)]
pub fn bodies_checked() -> usize {
    BODIES.with(std::cell::Cell::get)
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
        text += &format!("\n  {violation}");
    }
    text
}

/// Checks the rules of one body at the end of building it, and panics with
/// the list of violations if it breaks any.
pub fn assert_valid_body(body: &Body) {
    assert_valid(&check_body(body));
}

/// Every violation of the rules of one body, in the order of ADR 0095.
pub fn check_body(body: &Body) -> Vec<Violation> {
    #[cfg(test)]
    BODIES.with(|count| count.set(count.get() + 1));
    let mut checker = Checker {
        body,
        at: Location::Body,
        found: Vec::new(),
    };
    checker.shape();
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
    let mut found = checker.found;
    found.sort_by_key(|violation| (violation.at.key(), violation.rule));
    found
}

struct Checker<'a> {
    body: &'a Body,
    at: Location,
    found: Vec<Violation>,
}

impl Checker<'_> {
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

    /// S7 for the base and the index locals of a place. Whether the base
    /// exists is the result.
    fn place(&mut self, place: Place) -> bool {
        let base = self.local(place.local, "a place");
        for elem in place.projection {
            match elem {
                PlaceElem::Offset(index)
                | PlaceElem::Index(index)
                | PlaceElem::Element(_, index) => {
                    self.local(*index, "a projection");
                }
                PlaceElem::Deref
                | PlaceElem::ConstIndex(_)
                | PlaceElem::Field(_)
                | PlaceElem::ConstElement(..)
                | PlaceElem::Tag
                | PlaceElem::Payload { .. } => {}
            }
        }
        base
    }

    /// S7 for a place that is read.
    fn read(&mut self, place: Place) {
        self.place(place);
    }

    /// S7 for a place, and S8 for the constant local it may change: a
    /// place written, or taken by `&raw mut`, through no pointer.
    fn write(&mut self, place: Place, how: &str) {
        if !self.place(place) {
            return;
        }
        let through_pointer = place
            .projection
            .iter()
            .any(|elem| matches!(elem, PlaceElem::Deref | PlaceElem::Offset(_)));
        if !through_pointer && self.body.locals[place.local.0 as usize].constant.is_some() {
            self.violation(
                "S8",
                format!(
                    "{how} the local `{}`, which holds read-only data",
                    place.local
                ),
            );
        }
    }

    fn operand(&mut self, operand: &Operand) {
        match operand {
            Operand::Copy(place) => self.read(*place),
            Operand::Const(_) => {}
        }
    }

    fn statement(&mut self, statement: &Statement) {
        match statement {
            Statement::Assign(dest, rvalue) => {
                self.write(*dest, "an assignment writes");
                self.rvalue(rvalue);
            }
            Statement::Call { dest, args, .. } => {
                if let Some(dest) = dest {
                    self.write(*dest, "a call's destination is");
                }
                for arg in args {
                    self.operand(arg);
                }
            }
            Statement::InlineAsm(asm) => self.asm(asm, false),
            Statement::CriticalEnter(local) | Statement::CriticalExit(local) => {
                self.local(*local, "a `critical` statement");
            }
        }
    }

    fn rvalue(&mut self, rvalue: &Rvalue) {
        match rvalue {
            Rvalue::Use(operand) | Rvalue::Unary(_, operand) | Rvalue::Cast(operand, _) => {
                self.operand(operand)
            }
            Rvalue::Binary(_, lhs, rhs) => {
                self.operand(lhs);
                self.operand(rhs);
            }
            Rvalue::AddressOf {
                mutable: true,
                place,
            } => self.write(*place, "`&raw mut` takes"),
            Rvalue::AddressOf {
                mutable: false,
                place,
            } => self.read(*place),
            Rvalue::Slice { ptr, len } => {
                self.operand(ptr);
                self.operand(len);
            }
            Rvalue::SliceAddress(place) | Rvalue::SliceLen(place) => self.read(*place),
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
            Terminator::If { cond, .. } => self.operand(cond),
            Terminator::Switch { discr, .. } => self.operand(discr),
            Terminator::Return => {
                if self.body.ret == Ty::Never {
                    self.violation("S6", "a body that returns `!` has a `return`".to_owned());
                }
            }
            Terminator::Panic(cause, site) => {
                match cause {
                    PanicCause::AssertionFailed(Some((message, _))) => self.operand(message),
                    PanicCause::AssertionFailed(None)
                    | PanicCause::DivisionByZero
                    | PanicCause::IndexOutOfBounds
                    | PanicCause::NoVariant
                    | PanicCause::FieldOutOfRange => {}
                }
                if let Some(site) = site {
                    match site.info {
                        PanicInfoAt::Local(local) => {
                            self.local(local, "a panic site");
                        }
                        PanicInfoAt::Data(_) => {}
                    }
                }
            }
            Terminator::InlineAsm(asm) => self.asm(asm, true),
        }
    }

    /// S7 to S9 for an `asm`, a statement or a terminator.
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
                AsmOperandKind::In(value) => self.operand(value),
                AsmOperandKind::Out { place, .. } => {
                    if let Some(place) = place {
                        self.write(*place, "an `asm` output is");
                    }
                }
                AsmOperandKind::InOut { input, place } => {
                    self.operand(input);
                    if let Some(place) = place {
                        self.write(*place, "an `asm` output is");
                    }
                }
                AsmOperandKind::Sym(_) | AsmOperandKind::Const(..) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::{
        AsmOperand, AsmOptions, AsmReg, BasicBlockData, Const, DataId, IntTy, LocalDecl, PanicSite,
        StaticRef,
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

    /// `fn f(_1: u32) -> u32`, with `_2: u32` as a constant local, `_3: u32`
    /// as a variable and `_4: *mut u32`: `bb0` assigns `_0 = copy _1` and
    /// goes to `bb1`, which returns.
    fn valid() -> Body {
        Body {
            name: "f".to_owned(),
            span: span(),
            comptime: false,
            ret: u32_ty(),
            param_count: 1,
            locals: vec![
                decl(u32_ty()),
                decl(u32_ty()),
                LocalDecl {
                    ty: u32_ty(),
                    name: None,
                    constant: Some(DataId(0)),
                },
                decl(u32_ty()),
                decl(Ty::Ptr(crate::mir::Ptr {
                    mutable: true,
                    volatile: false,
                    pointee: Box::leak(Box::new(u32_ty())),
                })),
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
        body.param_count = 5;
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
        statement_of(&mut body, Statement::CriticalEnter(Local(0)));
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
        breaks_only(&body, "S6");
        body.blocks[1].terminator = Terminator::Unreachable;
        assert!(check_body(&body).is_empty());
    }

    #[test]
    fn s7_every_local_named_exists() {
        let ghost = 9;
        let at_index = |elem: PlaceElem| -> Place {
            Place {
                local: Local(4),
                projection: Box::leak(Box::new([elem])),
            }
        };
        let cases = [
            // The base of a place, written and read.
            Statement::Assign(local(ghost), Rvalue::Use(copy(1))),
            Statement::Assign(local(3), Rvalue::Use(copy(ghost))),
            Statement::Assign(local(3), Rvalue::SliceLen(local(ghost))),
            // The index of each projection that has one.
            Statement::Assign(
                local(3),
                Rvalue::Use(Operand::Copy(at_index(PlaceElem::Offset(Local(ghost))))),
            ),
            Statement::Assign(
                local(3),
                Rvalue::Use(Operand::Copy(at_index(PlaceElem::Index(Local(ghost))))),
            ),
            Statement::Assign(
                local(3),
                Rvalue::Use(Operand::Copy(at_index(PlaceElem::Element(0, Local(ghost))))),
            ),
            Statement::CriticalEnter(Local(ghost)),
            Statement::CriticalExit(Local(ghost)),
            Statement::Call {
                dest: Some(local(ghost)),
                callee: crate::mir::BodyId(0),
                args: Vec::new(),
            },
            // Every operand of a statement: a call's arguments, the
            // operands of each rvalue, the inputs of an `asm`.
            Statement::Call {
                dest: None,
                callee: crate::mir::BodyId(0),
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
                    len: copy(1),
                },
            ),
            Statement::Assign(
                local(3),
                Rvalue::Slice {
                    ptr: copy(1),
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
            file: StaticRef::new(
                crate::mir::StaticId(0),
                "f".to_owned(),
                Ty::Unit,
                false,
                true,
            ),
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
                local(9),
                Rvalue::AddressOf {
                    mutable: true,
                    place: local(9),
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
                callee: crate::mir::BodyId(0),
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
        let through = |elem| Place {
            local: Local(2),
            projection: Box::leak(Box::new([elem])),
        };
        let mut body = valid();
        statement_of(&mut body, Statement::Assign(local(3), Rvalue::Use(copy(2))));
        statement_of(
            &mut body,
            Statement::Assign(
                local(4),
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
            Statement::Assign(through(PlaceElem::Offset(Local(1))), Rvalue::Use(copy(1))),
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
        statement_of(&mut body, Statement::CriticalEnter(Local(9)));
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
        `f`, bb0, statement 1: S7: a `critical` statement names `_9`, but the body has 5 locals\n  \
        `f`, bb0, terminator: S5: a terminator jumps to bb4, but the body has 2 blocks"
    )]
    fn the_panic_lists_every_violation() {
        let mut body = valid();
        statement_of(&mut body, Statement::CriticalEnter(Local(9)));
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
}
