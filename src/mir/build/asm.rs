//! `asm` expressions (ADR 0079): the operands and output places are
//! evaluated left to right, then the statement runs the code and stores its
//! outputs.

use crate::hir;
use crate::mir::{
    AsmOperand, AsmOperandKind, AsmOptions, AsmPiece, AsmReg, AsmSym, BodyId, Const, InlineAsm,
    IntTy, Operand, Place, Rvalue, Statement, Terminator,
};
use crate::ty;

use super::{Builder, is_simple};

impl Builder<'_, '_> {
    /// An `asm` expression, whose value is `()`: a statement, or a
    /// terminator with the option `noreturn`.
    pub(super) fn asm(&mut self, expr: &hir::Expr, asm: &hir::Asm) -> Operand {
        // What runs before the code, in order: the values of inputs, the
        // places of outputs and the values of constants. Each is evaluated
        // as soon as its turn comes, so a later one cannot change it
        // (ADR 0018).
        let evaluated: Vec<&hir::Expr> = asm.operands.iter().flat_map(evaluated).collect();
        let mut done = 0;
        let mut operands = Vec::new();
        // The index in `operands` of each operand as written.
        let mut index_of: Vec<Option<usize>> = Vec::new();
        let checked = self.q.tables.asm(expr.id);
        let registers = checked.registers.clone();
        let pieces = checked.pieces.clone();
        for (written, operand) in asm.operands.iter().enumerate() {
            let reg = |reg: &hir::AsmReg| match (reg, registers[written]) {
                (hir::AsmReg::Explicit(_), Some(name)) => AsmReg::Fixed(name),
                _ => AsmReg::Any,
            };
            let (reg, kind) = match &operand.kind {
                hir::AsmOperandKind::In { reg: r, expr } => {
                    let value = self.asm_value(expr, &evaluated[done + 1..]);
                    done += 1;
                    (reg(r), AsmOperandKind::In(value))
                }
                hir::AsmOperandKind::Out {
                    reg: r,
                    late,
                    place,
                } => {
                    let place = place.as_ref().map(|place| {
                        let at = self.asm_place(place, &evaluated[done + 1..]);
                        done += 1;
                        at
                    });
                    let kind = AsmOperandKind::Out { late: *late, place };
                    (reg(r), kind)
                }
                hir::AsmOperandKind::InOut {
                    reg: r,
                    expr,
                    out: None,
                } => {
                    // Read before the code runs, written after it, with the
                    // place evaluated once.
                    let place = self.asm_place(expr, &evaluated[done + 1..]);
                    done += 1;
                    let ty = self.place_ty(place);
                    let input = self.temp(ty, Rvalue::Use(Operand::Copy(place)));
                    let kind = AsmOperandKind::InOut {
                        input,
                        place: Some(place),
                    };
                    (reg(r), kind)
                }
                hir::AsmOperandKind::InOut {
                    reg: r,
                    expr,
                    out: Some(out),
                } => {
                    let input = self.asm_value(expr, &evaluated[done + 1..]);
                    done += 1;
                    let place = out.as_ref().map(|place| {
                        let at = self.asm_place(place, &evaluated[done + 1..]);
                        done += 1;
                        at
                    });
                    (reg(r), AsmOperandKind::InOut { input, place })
                }
                hir::AsmOperandKind::Sym(sym) => {
                    (AsmReg::Any, AsmOperandKind::Sym(self.asm_sym(sym)))
                }
                hir::AsmOperandKind::Const(value) => {
                    let value = self.expr(value);
                    done += 1;
                    let (bits, int) = match value {
                        Operand::Const(Const::Int(bits, int)) => (bits, int),
                        // The value failed to compute, which was reported.
                        _ => {
                            self.failed = true;
                            (0, IntTy::U32)
                        }
                    };
                    (AsmReg::Any, AsmOperandKind::Const(bits, int))
                }
                hir::AsmOperandKind::ClobberAbi(_) => {
                    index_of.push(None);
                    continue;
                }
            };
            // One of them did not finish, and nothing follows it.
            if self.current.is_none() {
                return Operand::Const(Const::Unit);
            }
            index_of.push(Some(operands.len()));
            operands.push(AsmOperand { reg, kind });
        }

        let pieces = pieces
            .iter()
            .map(|piece| match piece {
                ty::AsmPiece::Text(text) => AsmPiece::Text(text.clone()),
                // Only operands with no register of their own are placed,
                // and each of them has an index.
                ty::AsmPiece::Operand(index) => {
                    AsmPiece::Operand(index_of[*index].unwrap_or_default())
                }
            })
            .collect();
        let clobber_abi = (asm.operands.iter())
            .any(|operand| matches!(operand.kind, hir::AsmOperandKind::ClobberAbi(_)));
        let mut options = AsmOptions::default();
        for option in &asm.options {
            match option.kind {
                hir::AsmOptionKind::Pure => options.pure = true,
                hir::AsmOptionKind::Nomem => options.nomem = true,
                hir::AsmOptionKind::Readonly => options.readonly = true,
                hir::AsmOptionKind::Nostack => options.nostack = true,
                hir::AsmOptionKind::PreservesFlags => options.preserves_flags = true,
                hir::AsmOptionKind::Noreturn => options.noreturn = true,
            }
        }
        let inline = InlineAsm {
            pieces,
            operands,
            clobbers: if clobber_abi {
                self.q.target().c_clobbers.to_vec()
            } else {
                Vec::new()
            },
            options,
            span: expr.span,
        };
        if options.noreturn {
            self.terminate(Terminator::InlineAsm(Box::leak(Box::new(inline))));
        } else {
            self.push(Statement::InlineAsm(Box::new(inline)));
        }
        Operand::Const(Const::Unit)
    }

    /// The value of an input, read now if what is evaluated after it, `later`,
    /// could assign it.
    fn asm_value(&mut self, expr: &hir::Expr, later: &[&hir::Expr]) -> Operand {
        let value = self.expr(expr);
        if matches!(value, Operand::Copy(_)) && !later.iter().all(|e| is_simple(e)) {
            let ty = self.operand_ty(value);
            return self.temp(ty, Rvalue::Use(value));
        }
        value
    }

    /// The place of an output, with its checks evaluated now.
    fn asm_place(&mut self, place: &hir::Expr, later: &[&hir::Expr]) -> Place {
        let later_writes = !later.iter().all(|e| is_simple(e));
        self.place_then(place, later_writes, false)
    }

    fn asm_sym(&mut self, sym: &hir::AsmSym) -> AsmSym {
        match sym {
            // MIR bodies are built in HIR function order.
            hir::AsmSym::Fn(id) => AsmSym::Body(BodyId(id.0)),
            hir::AsmSym::Static(id) => AsmSym::Static(self.q.mir_static(*id)),
            // Reported by type checking; the body is not used.
            hir::AsmSym::Other(..) => {
                self.failed = true;
                AsmSym::Body(BodyId(0))
            }
        }
    }
}

/// The expressions of an operand that run before the code: the values it
/// passes in and the places it stores in, in order.
fn evaluated(operand: &hir::AsmOperand) -> Vec<&hir::Expr> {
    match &operand.kind {
        hir::AsmOperandKind::In { expr, .. } | hir::AsmOperandKind::Const(expr) => vec![expr],
        hir::AsmOperandKind::Out { place, .. } => place.iter().collect(),
        hir::AsmOperandKind::InOut { expr, out, .. } => {
            let mut all = vec![expr];
            all.extend(out.iter().flatten());
            all
        }
        hir::AsmOperandKind::Sym(_) | hir::AsmOperandKind::ClobberAbi(_) => Vec::new(),
    }
}
