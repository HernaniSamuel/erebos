//! Inline assembly in MIR (ADR 0079): the template and its operands, for
//! code generation to derive the constraints from. The compiler does not
//! read the template.

use super::{BodyId, IntTy, Operand, Place, StaticRef};
use crate::span::Span;

/// An `asm` expression: a statement, or, with `noreturn`, a terminator with
/// no successor. By the time it runs, every operand value has been
/// evaluated, and every output place and its checks too, left to right
/// (ADR 0018, 0079); after it, the outputs are stored in the order of
/// `operands`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineAsm {
    /// The template, the strings joined with line breaks, with each
    /// placeholder replaced by the operand it names.
    pub pieces: Vec<AsmPiece>,
    /// In the order written, but for `clobber_abi`.
    pub operands: Vec<AsmOperand>,
    /// The registers `clobber_abi` covers, by their canonical names; none
    /// without it.
    pub clobbers: Vec<&'static str>,
    pub options: AsmOptions,
    /// The `asm` expression, where the assembler's errors are located.
    pub span: Span,
}

/// A piece of the template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AsmPiece {
    /// Copied as it is: ASCII, with its braces as they are, not doubled.
    Text(String),
    /// What the operand, by its index in `operands`, is replaced by: its
    /// register, its symbol or its value.
    Operand(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AsmOperand {
    pub reg: AsmReg,
    pub kind: AsmOperandKind,
}

/// Where an operand lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AsmReg {
    /// `reg`: a register the compiler chooses.
    Any,
    /// The register with this canonical name (`x10`, `r0`).
    Fixed(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AsmOperandKind {
    /// `in`: the value of an integer or a thin pointer.
    In(Operand),
    /// `out` and `lateout`; no place for `_`. `late`: the register may be one
    /// an input used.
    Out { late: bool, place: Option<Place> },
    /// `inout`: the value passed in, and the place the register is stored
    /// in; none for `=> _`.
    InOut {
        input: Operand,
        place: Option<Place>,
    },
    /// `sym`: the symbol of a function or a static.
    Sym(AsmSym),
    /// `const`: the bits of an integer computed at compile time.
    Const(u64, IntTy),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AsmSym {
    Body(BodyId),
    Static(StaticRef),
}

/// The options of an `asm` expression (ADR 0079).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AsmOptions {
    pub pure: bool,
    pub nomem: bool,
    pub readonly: bool,
    pub nostack: bool,
    pub preserves_flags: bool,
    pub noreturn: bool,
}

impl AsmOptions {
    /// The options set, as written, in the order of the grammar.
    pub fn names(self) -> Vec<&'static str> {
        [
            (self.pure, "pure"),
            (self.nomem, "nomem"),
            (self.readonly, "readonly"),
            (self.nostack, "nostack"),
            (self.preserves_flags, "preserves_flags"),
            (self.noreturn, "noreturn"),
        ]
        .into_iter()
        .filter(|&(set, _)| set)
        .map(|(_, name)| name)
        .collect()
    }
}

impl InlineAsm {
    /// Calls `f` on every operand value the code reads.
    pub(super) fn for_each_operand(&mut self, f: &mut impl FnMut(&mut Operand)) {
        for operand in &mut self.operands {
            match &mut operand.kind {
                AsmOperandKind::In(value) | AsmOperandKind::InOut { input: value, .. } => f(value),
                AsmOperandKind::Out { .. } | AsmOperandKind::Sym(_) | AsmOperandKind::Const(..) => {
                }
            }
        }
    }

    /// The operand values the code reads: the inputs of `in` and `inout`.
    /// The read-only twin of `for_each_operand`.
    pub fn inputs(&self) -> impl Iterator<Item = &Operand> {
        self.operands
            .iter()
            .filter_map(|operand| match &operand.kind {
                AsmOperandKind::In(value) | AsmOperandKind::InOut { input: value, .. } => {
                    Some(value)
                }
                AsmOperandKind::Out { .. } | AsmOperandKind::Sym(_) | AsmOperandKind::Const(..) => {
                    None
                }
            })
    }

    /// The symbols `sym` operands name.
    pub fn syms(&self) -> impl Iterator<Item = AsmSym> + '_ {
        self.operands
            .iter()
            .filter_map(|operand| match operand.kind {
                AsmOperandKind::Sym(sym) => Some(sym),
                _ => None,
            })
    }

    /// Calls `f` on every symbol a `sym` operand names.
    pub(super) fn for_each_sym(&mut self, f: &mut impl FnMut(&mut AsmSym)) {
        for operand in &mut self.operands {
            if let AsmOperandKind::Sym(sym) = &mut operand.kind {
                f(sym);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asm(operands: Vec<AsmOperand>) -> InlineAsm {
        let file = crate::span::SourceMap::default().add("main.ers", "");
        InlineAsm {
            pieces: vec![AsmPiece::Text("nop".to_owned())],
            operands,
            clobbers: Vec::new(),
            options: AsmOptions::default(),
            span: Span::new(file, 0, 0),
        }
    }

    fn sym(body: u32) -> AsmOperand {
        AsmOperand {
            reg: AsmReg::Any,
            kind: AsmOperandKind::Sym(AsmSym::Body(BodyId(body))),
        }
    }

    #[test]
    fn options_are_named_in_the_order_of_the_grammar() {
        let options = AsmOptions {
            noreturn: true,
            pure: true,
            nostack: true,
            ..AsmOptions::default()
        };
        assert_eq!(options.names(), ["pure", "nostack", "noreturn"]);
        assert!(AsmOptions::default().names().is_empty());
    }

    #[test]
    fn the_symbols_are_those_of_the_sym_operands_only() {
        let value = AsmOperand {
            reg: AsmReg::Fixed("x10"),
            kind: AsmOperandKind::Const(1, IntTy::U32),
        };
        let mut inline = asm(vec![sym(3), value, sym(5)]);
        let bodies: Vec<AsmSym> = inline.syms().collect();
        assert_eq!(bodies, [AsmSym::Body(BodyId(3)), AsmSym::Body(BodyId(5))]);
        inline.for_each_sym(&mut |sym| {
            if let AsmSym::Body(id) = sym {
                id.0 += 10;
            }
        });
        let bodies: Vec<AsmSym> = inline.syms().collect();
        assert_eq!(bodies, [AsmSym::Body(BodyId(13)), AsmSym::Body(BodyId(15))]);
    }

    #[test]
    fn operand_values_are_the_inputs() {
        let value = Operand::Const(super::super::Const::Int(7, IntTy::U32));
        let place = Place::local(super::super::Local(2));
        let mut inline = asm(vec![
            AsmOperand {
                reg: AsmReg::Any,
                kind: AsmOperandKind::In(value),
            },
            AsmOperand {
                reg: AsmReg::Any,
                kind: AsmOperandKind::Out {
                    late: false,
                    place: Some(place),
                },
            },
            AsmOperand {
                reg: AsmReg::Any,
                kind: AsmOperandKind::InOut {
                    input: value,
                    place: None,
                },
            },
        ]);
        let mut seen = 0;
        inline.for_each_operand(&mut |_| seen += 1);
        assert_eq!(seen, 2);
        assert_eq!(inline.inputs().count(), 2);
    }
}
