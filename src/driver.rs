//! Runs the pipeline. `dump` stops after a given stage and returns its
//! textual form; it backs `--emit` and the snapshot tests.

use std::fmt;
use std::str::FromStr;

use crate::diag::{self, Diagnostic, Diagnostics};
use crate::span::{FileId, SourceMap};
use crate::syntax::{lexer, parser};
use crate::{codegen, hir, mir, target, ty};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Tokens,
    Ast,
    Hir,
    Typeck,
    Mir,
    LlvmIr,
}

impl Stage {
    pub const ALL: &[Stage] = &[
        Stage::Tokens,
        Stage::Ast,
        Stage::Hir,
        Stage::Typeck,
        Stage::Mir,
        Stage::LlvmIr,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Stage::Tokens => "tokens",
            Stage::Ast => "ast",
            Stage::Hir => "hir",
            Stage::Typeck => "typeck",
            Stage::Mir => "mir",
            Stage::LlvmIr => "llvm-ir",
        }
    }
}

impl FromStr for Stage {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Stage::ALL
            .iter()
            .copied()
            .find(|stage| stage.name() == s)
            .ok_or_else(|| {
                let names: Vec<_> = Stage::ALL.iter().map(|s| s.name()).collect();
                format!(
                    "unknown stage `{s}` (expected one of: {})",
                    names.join(", ")
                )
            })
    }
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Runs the pipeline on one file up to `stage`. On failure, returns the
/// rendered diagnostics.
pub fn dump(name: &str, text: &str, stage: Stage, color: bool) -> Result<String, String> {
    let mut sources = SourceMap::default();
    let file = sources.add(name, text);
    let mut diags = Diagnostics::default();

    let output = run_to(&sources, file, stage, &mut diags);
    match output {
        Some(output) if !diags.has_errors() => Ok(output),
        _ => Err(diag::render(&diags.into_vec(), &sources, color)),
    }
}

/// Returns the dump of `stage`, or `None` if an earlier stage failed.
fn run_to(
    sources: &SourceMap,
    file: FileId,
    stage: Stage,
    diags: &mut Diagnostics,
) -> Option<String> {
    let text = &sources.file(file).text;

    // Syntax phases keep going after errors to report as much as possible.
    let tokens = lexer::lex(file, text, diags);
    if stage == Stage::Tokens {
        let dump = tokens
            .iter()
            .map(|t| format!("{t:?} {:?}\n", sources.snippet(t.span)))
            .collect();
        return Some(dump);
    }

    let ast = parser::parse(text, &tokens, diags);
    if stage == Stage::Ast {
        return Some(format!("{ast:#?}\n"));
    }

    // Semantic phases need a well-formed input.
    if diags.has_errors() {
        return None;
    }

    let hir = hir::lower(&ast, diags)?;
    if stage == Stage::Hir {
        return Some(format!("{hir:#?}\n"));
    }

    let types = ty::check(&hir, diags);
    if stage == Stage::Typeck {
        return Some(ty::dump(&hir, &types));
    }

    // Later phases rely on well-typed input.
    if diags.has_errors() {
        return None;
    }

    let mir = mir::build(&hir, &types);
    if stage == Stage::Mir {
        return Some(mir.to_string());
    }

    if stage == Stage::LlvmIr {
        return match codegen::emit_llvm_ir(&mir, &target::RISCV32_VIRT) {
            Ok(ir) => Some(ir),
            Err(e) => {
                diags.push(Diagnostic::error("codegen::llvm", e));
                None
            }
        };
    }

    unreachable!("every stage returns above")
}
