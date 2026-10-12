//! Runs the pipeline. `dump` stops after a given stage and returns its
//! textual form; it backs `--emit` and the snapshot tests.

use std::fmt;
use std::str::FromStr;

use crate::diag::{self, Diagnostics};
use crate::hir;
use crate::span::{FileId, SourceMap};
use crate::syntax::{lexer, parser};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Tokens,
    Ast,
    Hir,
}

impl Stage {
    pub const ALL: &[Stage] = &[Stage::Tokens, Stage::Ast, Stage::Hir];

    pub fn name(self) -> &'static str {
        match self {
            Stage::Tokens => "tokens",
            Stage::Ast => "ast",
            Stage::Hir => "hir",
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

    unreachable!("every stage returns above")
}
