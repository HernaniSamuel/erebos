//! Typed HIR -> MIR.

use crate::hir::{self, Hir};
use crate::mir::{BasicBlock, BasicBlockData, Body, BodyId, Mir, Terminator, Ty};
use crate::ty::TypeckResults;

/// The input must have passed type checking without errors.
pub fn build(hir: &Hir, types: &TypeckResults) -> Mir {
    let bodies = hir.fns().map(|(_, f)| build_body(f, types)).collect();
    Mir {
        bodies,
        // MIR bodies are built in HIR function order.
        entry: BodyId(hir.entry.0),
    }
}

fn build_body(f: &hir::Fn, types: &TypeckResults) -> Body {
    let mut builder = Builder {
        types,
        blocks: Vec::new(),
        current: None,
    };
    let start = builder.new_block();
    builder.current = Some(start);
    builder.block(&f.body);
    if builder.current.is_some() {
        builder.terminate(Terminator::Return);
    }

    Body {
        name: f.name.clone(),
        ret: lower_ty(f.ret),
        blocks: builder
            .blocks
            .into_iter()
            .map(|t| BasicBlockData {
                terminator: t.expect("every block is terminated"),
            })
            .collect(),
    }
}

fn lower_ty(ty: hir::Ty) -> Ty {
    match ty {
        hir::Ty::Unit => Ty::Unit,
        hir::Ty::Never => Ty::Never,
    }
}

struct Builder<'a> {
    types: &'a TypeckResults,
    /// Terminators, filled in as blocks are finished.
    blocks: Vec<Option<Terminator>>,
    /// The block code is currently appended to; `None` after control flow
    /// has diverged, so following code is unreachable.
    current: Option<BasicBlock>,
}

impl Builder<'_> {
    fn new_block(&mut self) -> BasicBlock {
        self.blocks.push(None);
        BasicBlock(self.blocks.len() as u32 - 1)
    }

    fn terminate(&mut self, terminator: Terminator) {
        let bb = self.current.take().expect("no block to terminate");
        self.blocks[bb.0 as usize] = Some(terminator);
    }

    fn block(&mut self, block: &hir::Block) {
        for expr in &block.stmts {
            if self.current.is_none() {
                // Unreachable code: nothing to emit.
                break;
            }
            self.expr(expr);
        }
    }

    fn expr(&mut self, expr: &hir::Expr) {
        match &expr.kind {
            hir::ExprKind::Loop(body) => {
                let head = self.new_block();
                self.terminate(Terminator::Goto(head));
                self.current = Some(head);
                self.block(body);
                if self.current.is_some() {
                    self.terminate(Terminator::Goto(head));
                }
            }
        }

        // Control cannot continue past an expression of type `!`.
        if self.types.expr_ty(expr.id) == hir::Ty::Never && self.current.is_some() {
            self.terminate(Terminator::Unreachable);
        }
    }
}
