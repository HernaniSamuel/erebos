//! Checking `asm` expressions (ADR 0079): the operands, the registers of the
//! target, the placeholders of the template and the options. What MIR
//! building needs of the result, the template in pieces and each operand's
//! register, is kept in the tables.

use std::collections::HashMap;

use crate::diag::Diagnostic;
use crate::hir::{
    AccessMode, Asm, AsmOperand, AsmOperandKind, AsmOptionKind, AsmReg, AsmSym, Block, Expr,
    ExprKind, Fn, IntTy, Stmt, Ty,
};
use crate::span::Span;
use crate::target::Register;

use super::Checker;

/// A piece of the template of an `asm` expression, checked: text, or an
/// operand, by its index in the operands as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AsmPiece {
    Text(String),
    Operand(usize),
}

/// What type checking found out about an `asm` expression, for MIR building.
#[derive(Debug, Default)]
pub struct AsmChecked {
    /// The template: the strings joined with line breaks, with each
    /// placeholder replaced by the operand it names.
    pub pieces: Vec<AsmPiece>,
    /// For each operand, the canonical name of its explicit register, if it
    /// has one.
    pub registers: Vec<Option<&'static str>>,
}

/// How an operand uses its register: what the checks of registers need.
struct RegisterUse {
    register: &'static Register,
    /// The operand, whose span labels the register.
    span: Span,
    input: bool,
    output: bool,
    /// The output is written only after every input has been read.
    late: bool,
}

impl Checker<'_, '_> {
    /// An `asm` expression: `()`, or `!` with `noreturn`.
    pub(super) fn asm(&mut self, f: &Fn, expr: &Expr, asm: &Asm) -> Ty {
        for operand in &asm.operands {
            self.asm_operand(f, operand);
        }
        let registers = self.asm_registers(asm);
        let pieces = self.asm_template(asm);
        self.asm_options(asm);
        self.q.tables.asms.insert(
            expr.id,
            AsmChecked {
                pieces,
                registers: registers
                    .iter()
                    .map(|register| register.map(|register| register.name))
                    .collect(),
            },
        );
        if asm
            .options
            .iter()
            .any(|option| option.kind == AsmOptionKind::Noreturn)
        {
            Ty::Never
        } else {
            Ty::Unit
        }
    }

    /// The body of a `#[naked]` function is one `asm` expression, as a
    /// statement or as the tail, with `noreturn` and only `sym` and `const`
    /// operands (ADR 0079): the compiler writes no code around it, so
    /// nothing may need any (a local, a value in a register) and the code
    /// itself returns.
    pub(super) fn naked_body(&mut self, f: &Fn, body: &Block) {
        // What the body holds, in order: the statements and the tail, with
        // the `asm` expression of each that is one.
        let mut items: Vec<(Span, Option<(&Expr, &Asm)>)> = Vec::new();
        fn asm_of(expr: &Expr) -> Option<(&Expr, &Asm)> {
            match &expr.kind {
                ExprKind::Asm(asm) => Some((expr, asm)),
                _ => None,
            }
        }
        for stmt in &body.stmts {
            items.push(match stmt {
                Stmt::Let { local, init } => (f.local(*local).span.to(init.span), None),
                Stmt::Expr { expr, .. } => (expr.span, asm_of(expr)),
            });
        }
        if let Some(tail) = &body.tail {
            items.push((tail.span, asm_of(tail)));
        }
        let help = "the body of a naked function is one `asm(..., options(noreturn))` expression with only `sym` and `const` operands, which does everything, returning included; the compiler writes no code around it (ADR 0079)";
        let found = (items.iter().enumerate()).find_map(|(i, item)| item.1.map(|asm| (i, asm)));
        let Some((first, (expr, asm))) = found else {
            let mut diag = Diagnostic::error(
                "ty::naked-body",
                format!(
                    "the body of the naked function `{}` is not one `asm` expression",
                    f.name
                ),
            );
            diag = if items.is_empty() {
                diag.label(body.span, "an empty body")
            } else {
                items.iter().fold(diag, |diag, (span, _)| {
                    diag.label(*span, "not an `asm` expression")
                })
            };
            self.q.diags.push(diag.help(help));
            return;
        };
        if items.len() > 1 {
            let mut diag = Diagnostic::error(
                "ty::naked-body",
                format!(
                    "the body of the naked function `{}` is not one `asm` expression",
                    f.name
                ),
            );
            for (i, (span, _)) in items.iter().enumerate() {
                if i != first {
                    diag = diag.label(*span, "not allowed here");
                }
            }
            self.q
                .diags
                .push(diag.label(items[first].0, "the one `asm`").help(help));
        }
        if !asm
            .options
            .iter()
            .any(|option| option.kind == AsmOptionKind::Noreturn)
        {
            self.q.diags.push(
                Diagnostic::error(
                    "ty::naked-body",
                    "the `asm` of a naked function must have `noreturn`",
                )
                .label(expr.span, "the compiler would add a return after this")
                .help("a naked function has no epilogue, so the template itself returns (`ret`, `bx lr`, a jump) and the expression is `asm(..., options(noreturn))` (ADR 0079)"),
            );
        }
        for operand in &asm.operands {
            if !matches!(
                operand.kind,
                AsmOperandKind::Sym(_) | AsmOperandKind::Const(_)
            ) {
                self.q.diags.push(
                    Diagnostic::error(
                        "ty::naked-body",
                        "the `asm` of a naked function takes only `sym` and `const` operands",
                    )
                    .label(operand.span, "not allowed here")
                    .help("a naked function has no variables: its parameters arrive where the C ABI puts them, and the template reads them there; only symbols and compile-time values can be named (ADR 0079)"),
                );
            }
        }
    }

    /// The expressions of an operand, and the types of its values.
    fn asm_operand(&mut self, f: &Fn, operand: &AsmOperand) {
        match &operand.kind {
            AsmOperandKind::In { expr, .. } => {
                let ty = self.check(f, expr, None);
                self.asm_operand_ty(expr.span, ty);
            }
            AsmOperandKind::Out { place, .. } => {
                if let Some(place) = place {
                    self.asm_output(f, place);
                }
            }
            AsmOperandKind::InOut { expr, out, .. } => match out {
                // The place is read before the code and written after it.
                None => self.asm_inout_place(f, expr),
                Some(out) => {
                    let ty = self.check(f, expr, None);
                    self.asm_operand_ty(expr.span, ty);
                    if let Some(place) = out {
                        let out_ty = self.asm_output(f, place);
                        if !matches!(ty, Ty::Error | Ty::Never)
                            && !matches!(out_ty, Ty::Error | Ty::Never)
                            && ty != out_ty
                        {
                            self.q.diags.push(
                                Diagnostic::error(
                                    "ty::asm-operand-type",
                                    "the two sides of `inout ... =>` have different types",
                                )
                                .label(expr.span, format!("passes a `{ty}`"))
                                .label(place.span, format!("receives a `{out_ty}`"))
                                .help("one register holds the value in and out, so both sides have one type; convert with `as` (ADR 0079)"),
                            );
                        }
                    }
                }
            },
            AsmOperandKind::Const(expr) => {
                let ty = self.check(f, expr, None);
                if !matches!(ty, Ty::Int(_) | Ty::Error | Ty::Never) {
                    self.q.diags.push(
                        Diagnostic::error(
                            "ty::asm-operand-type",
                            format!("a `const` operand is an integer, found `{ty}`"),
                        )
                        .label(expr.span, format!("has type `{ty}`"))
                        .help("`const` puts the value of an integer into the template as a number; convert it with `as` (ADR 0079)"),
                    );
                }
            }
            AsmOperandKind::Sym(AsmSym::Other(what, span)) => {
                self.q.diags.push(
                    Diagnostic::error("ty::asm-sym", format!("`sym` cannot name {what}"))
                        .label(*span, format!("{what}, not a function or a static"))
                        .help("`sym` names a function, an `extern fn`, a static or an `extern static`, and the template gets its symbol (ADR 0079)"),
                );
            }
            AsmOperandKind::Sym(_) | AsmOperandKind::ClobberAbi(_) => {}
        }
    }

    /// An output place: assigned to, as `=` does, and not a bit field, which
    /// is no place in memory (ADR 0059). Returns the type of the place.
    fn asm_output(&mut self, f: &Fn, place: &Expr) -> Ty {
        let ty = self.place(f, place, AccessMode::Write);
        if self.q.tables.bit_field(place.id).is_some() {
            self.q.diags.push(
                Diagnostic::error(
                    "ty::asm-operand-type",
                    "a bit field cannot receive the output of `asm`",
                )
                .label(place.span, "a bit field, with no place in memory")
                .help("store the output in a variable of the integer type, and assign the field from it (ADR 0059, 0079)"),
            );
            return Ty::Error;
        }
        self.asm_operand_ty(place.span, ty);
        ty
    }

    /// The expression of an `inout` without `=>`, a place that is read and
    /// then written.
    fn asm_inout_place(&mut self, f: &Fn, expr: &Expr) {
        let is_place = matches!(
            expr.kind,
            ExprKind::Local(_)
                | ExprKind::Static(_)
                | ExprKind::Peripheral(_)
                | ExprKind::ByteString(_)
                | ExprKind::Deref(_)
                | ExprKind::Index(..)
                | ExprKind::Field(..)
        );
        if !is_place {
            self.check(f, expr, None);
            self.q.diags.push(
                Diagnostic::error(
                    "ty::not-assignable",
                    "an `inout` without `=>` needs a place to store the result in",
                )
                .label(expr.span, "not a variable, `*pointer` or `a[index]`")
                .help("write the place the result goes to after `=>`, or `=> _` to drop it (ADR 0079)"),
            );
            return;
        }
        let ty = self.place(f, expr, AccessMode::ReadWrite);
        if self.q.tables.bit_field(expr.id).is_some() {
            self.q.diags.push(
                Diagnostic::error(
                    "ty::asm-operand-type",
                    "a bit field cannot receive the output of `asm`",
                )
                .label(expr.span, "a bit field, with no place in memory")
                .help("pass the field's value and write the result to a variable with `=> place` (ADR 0059, 0079)"),
            );
            return;
        }
        self.asm_operand_ty(expr.span, ty);
    }

    /// An operand of a register is an integer of at most 32 bits, or a thin
    /// pointer.
    fn asm_operand_ty(&mut self, span: Span, ty: Ty) {
        let help = match ty {
            Ty::Error | Ty::Never => return,
            Ty::Int(int) if int.bits() <= 32 => return,
            Ty::Ptr(_) if ty.slice_elem().is_none() => return,
            Ty::Int(IntTy::U64 | IntTy::I64) => {
                "a 64-bit integer takes two registers: pass its halves as two `u32`, converted with `as` (ADR 0079)"
            }
            Ty::Ptr(_) => {
                "a pointer to a slice is an address and a length; pass the address, converted with `as`, and the length as a `usize` (ADR 0031, 0079)"
            }
            _ => {
                "an operand is an integer of at most 32 bits or a thin pointer; convert other values with `as` (ADR 0008, 0079)"
            }
        };
        self.q.diags.push(
            Diagnostic::error(
                "ty::asm-operand-type",
                format!("`{ty}` cannot be the operand of a register"),
            )
            .label(span, format!("has type `{ty}`"))
            .help(help),
        );
    }

    /// The register of each operand, and the checks of ADR 0079 about the
    /// registers: they exist, the compiler does not keep them, no two
    /// operands name one that cannot be shared, and `clobber_abi` leaves
    /// none to the compiler.
    fn asm_registers(&mut self, asm: &Asm) -> Vec<Option<&'static Register>> {
        let target = self.q.target();
        let mut uses: Vec<RegisterUse> = Vec::new();
        let mut registers = Vec::new();
        for operand in &asm.operands {
            let (reg, input, output, late) = match &operand.kind {
                AsmOperandKind::In { reg, .. } => (reg, true, false, false),
                AsmOperandKind::Out { reg, late, .. } => (reg, false, true, *late),
                AsmOperandKind::InOut { reg, .. } => (reg, true, true, false),
                _ => {
                    registers.push(None);
                    continue;
                }
            };
            let AsmReg::Explicit(name) = reg else {
                registers.push(None);
                continue;
            };
            let written = String::from_utf8_lossy(&name.bytes).into_owned();
            let Some(register) = target.register(&name.bytes) else {
                self.q.diags.push(
                    Diagnostic::error(
                        "ty::asm-register",
                        format!(
                            "`{}` is no register of {}",
                            name.bytes.escape_ascii(),
                            target.name
                        ),
                    )
                    .label(name.span, "unknown register")
                    .help(format!(
                        "the registers of {} are {} (ADR 0079)",
                        target.name, target.register_names
                    )),
                );
                registers.push(None);
                continue;
            };
            if register.reserved {
                let canonical = if register.name == written {
                    String::new()
                } else {
                    format!(" (`{}`)", register.name)
                };
                self.q.diags.push(
                    Diagnostic::error(
                        "ty::asm-register",
                        format!("the compiler keeps `{written}`{canonical} for itself"),
                    )
                    .label(name.span, "reserved register")
                    .help(format!(
                        "the compiler relies on these registers for itself, as the stack, frame and base pointers or the program counter; the registers reserved on {} are {} (ADR 0079)",
                        target.name,
                        reserved_names(target.registers)
                    )),
                );
                registers.push(None);
                continue;
            }
            registers.push(Some(register));
            uses.push(RegisterUse {
                register,
                span: operand.span,
                input,
                output,
                late,
            });
        }
        // The first earlier operand that conflicts with each, reported at
        // the later: one that shares the register without conflict (an `in`
        // and a `lateout`) does not hide a conflict with another.
        for (i, later) in uses.iter().enumerate() {
            let found = uses[..i]
                .iter()
                .filter(|earlier| std::ptr::eq(earlier.register, later.register))
                .find_map(|earlier| conflict(earlier, later).map(|parts| (earlier, parts)));
            let Some((earlier, (message, help))) = found else {
                continue;
            };
            self.q.diags.push(
                Diagnostic::error("ty::asm-register", message)
                    .label(later.span, "named again")
                    .label(earlier.span, "first named here")
                    .help(format!("{help} (ADR 0079)")),
            );
        }
        // With `clobber_abi`, an output in a register the compiler chooses
        // could land in one that a call overwrites.
        let clobber = asm
            .operands
            .iter()
            .find(|operand| matches!(operand.kind, AsmOperandKind::ClobberAbi(_)));
        if let Some(clobber) = clobber {
            for operand in &asm.operands {
                let class = match &operand.kind {
                    AsmOperandKind::Out {
                        reg: AsmReg::Class(span),
                        ..
                    }
                    | AsmOperandKind::InOut {
                        reg: AsmReg::Class(span),
                        ..
                    } => *span,
                    _ => continue,
                };
                self.q.diags.push(
                    Diagnostic::error(
                        "ty::asm-register",
                        "an output beside `clobber_abi` names its register",
                    )
                    .label(class, "the compiler would choose it")
                    .label(clobber.span, "this overwrites every register a C call changes")
                    .help("write the register as a string, `out(\"a0\")`: the compiler could choose one `clobber_abi` overwrites (ADR 0079)"),
                );
            }
        }
        registers
    }

    /// The template, in pieces, with the checks of its placeholders, its
    /// bytes, its operands' names and which operands it uses.
    fn asm_template(&mut self, asm: &Asm) -> Vec<AsmPiece> {
        // The operands a template places: those without a register of their
        // own, numbered in the order written, and named.
        let mut positional: Vec<usize> = Vec::new();
        let mut named: HashMap<&str, usize> = HashMap::new();
        let mut placeable = vec![false; asm.operands.len()];
        for (index, operand) in asm.operands.iter().enumerate() {
            let explicit = matches!(
                &operand.kind,
                AsmOperandKind::In {
                    reg: AsmReg::Explicit(_),
                    ..
                } | AsmOperandKind::Out {
                    reg: AsmReg::Explicit(_),
                    ..
                } | AsmOperandKind::InOut {
                    reg: AsmReg::Explicit(_),
                    ..
                } | AsmOperandKind::ClobberAbi(_)
            );
            if explicit {
                continue;
            }
            match &operand.name {
                None => {
                    positional.push(index);
                    placeable[index] = true;
                }
                Some((name, span)) => {
                    if let Some(&first) = named.get(name.as_str()) {
                        let first_span = asm.operands[first]
                            .name
                            .as_ref()
                            .map_or(asm.operands[first].span, |(_, span)| *span);
                        self.q.diags.push(
                            Diagnostic::error(
                                "ty::asm-template",
                                format!("two operands are named `{name}`"),
                            )
                            .label(*span, "named again")
                            .label(first_span, "first named here")
                            .help("a placeholder `{name}` places one operand, so each name belongs to one (ADR 0079)"),
                        );
                    } else {
                        named.insert(name, index);
                        placeable[index] = true;
                    }
                }
            }
        }

        let names_checked = self.q.diags.len();
        let mut used = vec![false; asm.operands.len()];
        let mut pieces = Vec::new();
        let mut text = String::new();
        let mut next = 0;
        for (n, template) in asm.templates.iter().enumerate() {
            if n > 0 {
                text.push('\n');
            }
            let bytes = &template.bytes;
            let mut at = 0;
            while at < bytes.len() {
                let byte = bytes[at];
                match byte {
                    b'{' if bytes.get(at + 1) == Some(&b'{') => {
                        text.push('{');
                        at += 2;
                    }
                    b'}' if bytes.get(at + 1) == Some(&b'}') => {
                        text.push('}');
                        at += 2;
                    }
                    b'}' => {
                        self.unmatched_brace(template.span_of(at, at + 1), '}', "{", "opens");
                        at += 1;
                    }
                    b'{' => {
                        // The placeholder ends at the next `}`, before any `{`.
                        let end = bytes[at + 1..]
                            .iter()
                            .position(|&b| matches!(b, b'}' | b'{'))
                            .filter(|&len| bytes[at + 1 + len] == b'}');
                        let Some(len) = end else {
                            self.unmatched_brace(template.span_of(at, at + 1), '{', "}", "closes");
                            at += 1;
                            continue;
                        };
                        let content = &bytes[at + 1..at + 1 + len];
                        let span = template.span_of(at, at + len + 2);
                        let found =
                            self.placeholder(asm, &positional, &named, content, span, &mut next);
                        if let Some(index) = found {
                            used[index] = true;
                            if !text.is_empty() {
                                pieces.push(AsmPiece::Text(std::mem::take(&mut text)));
                            }
                            pieces.push(AsmPiece::Operand(index));
                        }
                        at += len + 2;
                    }
                    0x01..=0x7F => {
                        text.push(char::from(byte));
                        at += 1;
                    }
                    _ => {
                        self.q.diags.push(
                            Diagnostic::error(
                                "ty::asm-template",
                                format!("the template holds the byte `\\x{byte:02x}`"),
                            )
                            .label(template.span_of(at, at + 1), "not ASCII text")
                            .help("a template is ASCII, from `0x01` to `0x7F` (ADR 0079)"),
                        );
                        at += 1;
                    }
                }
            }
        }
        if !text.is_empty() {
            pieces.push(AsmPiece::Text(text));
        }

        // Every operand a template places is placed; unless the template is
        // wrong, since a placeholder that names no operand may be what the
        // operand was meant for.
        let wrong = self.q.diags.len() > names_checked;
        for (index, operand) in asm.operands.iter().enumerate() {
            if placeable[index] && !used[index] && !wrong {
                self.q.diags.push(
                    Diagnostic::error("ty::asm-template", "an operand is not used in the template")
                        .label(operand.span, "no placeholder places it")
                        .help("write `{}`, `{N}` or `{name}` where its register, symbol or value goes; an operand with a register of its own is exempt (ADR 0079)"),
                );
            }
        }
        pieces
    }

    /// `ty::asm-template` for a brace that matches none.
    fn unmatched_brace(&mut self, span: Span, brace: char, other: &str, how: &str) {
        self.q.diags.push(
            Diagnostic::error(
                "ty::asm-template",
                format!("unmatched `{brace}` in the template"),
            )
            .label(span, format!("no `{other}` {how} it"))
            .help("write `{{` and `}}` for a brace of the instructions themselves (ADR 0079)"),
        );
    }

    /// The operand a placeholder with `content` between its braces names,
    /// at `span`, reporting it if it names none.
    fn placeholder(
        &mut self,
        asm: &Asm,
        positional: &[usize],
        named: &HashMap<&str, usize>,
        content: &[u8],
        span: Span,
        next: &mut usize,
    ) -> Option<usize> {
        let text = String::from_utf8_lossy(content).into_owned();
        let of_unnamed = format!(
            "the operands without a name or a register are numbered from 0, and there {} {}",
            if positional.len() == 1 { "is" } else { "are" },
            positional.len()
        );
        if content.is_empty() {
            let number = *next;
            *next += 1;
            return match positional.get(number) {
                Some(&index) => Some(index),
                None => {
                    self.q.diags.push(
                        Diagnostic::error(
                            "ty::asm-template",
                            format!("`{{}}` takes operand number {number}, which does not exist"),
                        )
                        .label(span, format!("takes the operand number {number}"))
                        .help(format!("each `{{}}` takes the next operand without a name or a register; {of_unnamed} (ADR 0079)")),
                    );
                    None
                }
            };
        }
        if content.iter().all(u8::is_ascii_digit) {
            let found = text.parse::<usize>().ok().and_then(|n| positional.get(n));
            if found.is_none() {
                self.q.diags.push(
                    Diagnostic::error(
                        "ty::asm-template",
                        format!("the placeholder `{{{text}}}` names no operand"),
                    )
                    .label(span, "no such operand")
                    .help(format!("{of_unnamed} (ADR 0079)")),
                );
            }
            return found.copied();
        }
        let is_name = content[0].is_ascii_alphabetic() || content[0] == b'_';
        if !is_name
            || !content
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || *b == b'_')
        {
            self.q.diags.push(
                Diagnostic::error(
                    "ty::asm-template",
                    format!("`{{{}}}` is no placeholder", content.escape_ascii()),
                )
                .label(span, "not `{}`, `{N}` or `{name}`")
                .help("a placeholder is `{}`, `{N}` or `{name}`; operand modifiers such as `{0:x}` do not exist (ADR 0079)"),
            );
            return None;
        }
        if let Some(&index) = named.get(text.as_str()) {
            return Some(index);
        }
        let mut diag = Diagnostic::error(
            "ty::asm-template",
            format!("the placeholder `{{{text}}}` names no operand"),
        )
        .label(span, format!("no operand is named `{text}`"));
        let operands = asm
            .operands
            .iter()
            .filter_map(|operand| operand.name.as_ref().map(|(name, _)| format!("`{name}`")))
            .collect::<Vec<_>>();
        diag = if self.q.target().register(content).is_some() {
            diag.help(format!(
                "`{text}` is a register: an operand with a register of its own is not placed, so write the register itself in the template, as `{text}` (ADR 0079)"
            ))
        } else if operands.is_empty() {
            diag.help("no operand has a name: write `name = in(reg) ...` to name one (ADR 0079)")
        } else {
            diag.help(format!(
                "the named operands are {} (ADR 0079)",
                operands.join(", ")
            ))
        };
        self.q.diags.push(diag);
        None
    }

    /// The options, and what they promise: they do not contradict each other
    /// or the operands, and `clobber_abi` names the C ABI, once.
    fn asm_options(&mut self, asm: &Asm) {
        let mut seen: Vec<(AsmOptionKind, Span)> = Vec::new();
        for option in &asm.options {
            let name = AsmOptionKind::ALL
                .iter()
                .find(|(_, kind)| *kind == option.kind)
                .map_or("", |(name, _)| *name);
            if let Some((_, first)) = seen.iter().find(|(kind, _)| *kind == option.kind) {
                self.q.diags.push(
                    Diagnostic::error(
                        "ty::asm-options",
                        format!("the option `{name}` is given twice"),
                    )
                    .label(option.span, "given again")
                    .label(*first, "first given here"),
                );
            } else {
                seen.push((option.kind, option.span));
            }
        }
        let has = |kind| seen.iter().find(|(k, _)| *k == kind).map(|(_, span)| *span);
        let outputs = asm.operands.iter().find(|operand| {
            matches!(
                operand.kind,
                AsmOperandKind::Out { .. } | AsmOperandKind::InOut { .. }
            )
        });
        if let (Some(nomem), Some(readonly)) =
            (has(AsmOptionKind::Nomem), has(AsmOptionKind::Readonly))
        {
            self.q.diags.push(
                Diagnostic::error("ty::asm-options", "`nomem` and `readonly` contradict each other")
                    .label(readonly, "reads memory")
                    .label(nomem, "touches no memory")
                    .help("`nomem` promises no access to memory, `readonly` promises reads only: give the one that is true (ADR 0079)"),
            );
        }
        if let Some(pure) = has(AsmOptionKind::Pure) {
            if outputs.is_none() {
                self.q.diags.push(
                    Diagnostic::error("ty::asm-options", "`pure` needs an output")
                        .label(pure, "has no effect but its outputs, and there are none")
                        .help("a `pure` expression may be removed when its outputs are unused: with no output there is nothing to keep it for (ADR 0079)"),
                );
            }
            if has(AsmOptionKind::Nomem).is_none() && has(AsmOptionKind::Readonly).is_none() {
                self.q.diags.push(
                    Diagnostic::error("ty::asm-options", "`pure` needs `nomem` or `readonly`")
                        .label(pure, "may be removed, or run once for equal inputs")
                        .help("what depends on memory the compiler cannot see: add `nomem`, or `readonly` if it reads memory (ADR 0079)"),
                );
            }
        }
        if let (Some(noreturn), Some(output)) = (has(AsmOptionKind::Noreturn), outputs) {
            self.q.diags.push(
                Diagnostic::error(
                    "ty::asm-options",
                    "`noreturn` and an output contradict each other",
                )
                .label(output.span, "an output is stored when the code ends")
                .label(noreturn, "the code never ends")
                .help("remove the output, or the option (ADR 0079)"),
            );
        }
        let mut abis: Vec<Span> = Vec::new();
        for operand in &asm.operands {
            let AsmOperandKind::ClobberAbi(abi) = &operand.kind else {
                continue;
            };
            if let Some(first) = abis.first() {
                self.q.diags.push(
                    Diagnostic::error("ty::asm-options", "`clobber_abi` is given twice")
                        .label(operand.span, "given again")
                        .label(*first, "first given here"),
                );
            }
            abis.push(operand.span);
            if abi.bytes != b"C" {
                self.q.diags.push(
                    Diagnostic::error(
                        "ty::asm-options",
                        format!("there is no ABI named `{}`", abi.bytes.escape_ascii()),
                    )
                    .label(abi.span, "unknown ABI")
                    .help("only the C ABI exists: `clobber_abi(\"C\")` (ADR 0006, 0079)"),
                );
            }
        }
    }
}

/// How two operands on one register conflict: the message and the help, or
/// `None` if they may share it.
fn conflict(earlier: &RegisterUse, later: &RegisterUse) -> Option<(String, &'static str)> {
    let register = later.register.name;
    if earlier.input && later.input {
        Some((
            format!("the register `{register}` is named by two inputs"),
            "an input has a register to itself",
        ))
    } else if earlier.output && later.output {
        Some((
            format!("the register `{register}` is named by two outputs"),
            "an output has a register to itself",
        ))
    } else if (earlier.output && !earlier.late) || (later.output && !later.late) {
        Some((
            format!("the register `{register}` is named by an input and an `out`"),
            "an `out` promises a register that no input uses: write `inout` to pass a value in and out through one register, or `lateout` where the output may reuse an input's register",
        ))
    } else {
        None
    }
}

/// The reserved registers among `registers`, as a list of names.
fn reserved_names(registers: &[Register]) -> String {
    let names: Vec<String> = registers
        .iter()
        .filter(|register| register.reserved)
        .map(|register| {
            let mut name = format!("`{}`", register.name);
            if let Some(alias) = register.aliases.first() {
                name += &format!(" (`{alias}`)");
            }
            name
        })
        .collect();
    names.join(", ")
}
