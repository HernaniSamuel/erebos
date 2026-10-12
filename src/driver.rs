//! Runs the pipeline. `dump` stops after a given stage and returns its
//! textual form; it backs `--emit` and the snapshot tests.

use std::fmt;
use std::str::FromStr;

use crate::diag::{self, Diagnostics};
use crate::span::SourceMap;
use crate::syntax::lexer;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Tokens,
}

impl Stage {
    pub const ALL: &[Stage] = &[Stage::Tokens];

    pub fn name(self) -> &'static str {
        match self {
            Stage::Tokens => "tokens",
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

    let tokens = lexer::lex(file, text, &mut diags);
    let output = match stage {
        Stage::Tokens => tokens
            .iter()
            .map(|t| format!("{t:?} {:?}\n", sources.snippet(t.span)))
            .collect(),
    };

    if diags.has_errors() {
        return Err(diag::render(&diags.into_vec(), &sources, color));
    }
    Ok(output)
}
