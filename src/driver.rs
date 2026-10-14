//! Runs the pipeline. `build` produces an executable; `dump` stops after a
//! given stage and returns its textual form, backing `--emit` and the
//! snapshot tests.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

pub use crate::codegen::OptLevel;
use crate::diag::{self, Diagnostic, Diagnostics};
use crate::query::Queries;
use crate::span::{FileId, SourceMap};
use crate::syntax::{lexer, parser};
use crate::{codegen, hir, link, mir, target, ty};

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

/// The runtime support functions, compiled into every program (ADR 0020).
pub const RUNTIME_SOURCE: &str = include_str!("../rt/builtins.ers");
const RUNTIME_NAME: &str = "rt/builtins.ers";

/// Runs the pipeline on one file up to `stage`. Dumps show the program
/// alone, without the runtime support functions. On failure, returns the
/// rendered diagnostics.
pub fn dump(
    name: &str,
    text: &str,
    stage: Stage,
    opt: OptLevel,
    color: bool,
) -> Result<String, String> {
    let mut sources = SourceMap::default();
    let file = sources.add(name, text);
    let mut diags = Diagnostics::default();

    let output = match front_end(&sources, file, None, Some(stage), &mut diags) {
        FrontEnd::Dump(dump) => Some(dump),
        FrontEnd::Mir(mir) => codegen::emit_llvm_ir(&mir, &target::RISCV32_VIRT, opt)
            .map_err(|e| diags.push(Diagnostic::error("codegen::llvm", e)))
            .ok(),
        FrontEnd::Failed => None,
    };
    match output {
        Some(output) if !diags.has_errors() => Ok(output),
        _ => Err(diag::render(&diags.into_vec(), &sources, color)),
    }
}

/// Compiles `input` into an ELF executable at `output`, linked with the
/// object files `link` (ADR 0026). On failure, returns the rendered
/// diagnostics or error message.
pub fn build(
    input: &Path,
    output: &Path,
    link: &[PathBuf],
    opt: OptLevel,
    color: bool,
) -> Result<(), String> {
    let text = fs::read_to_string(input)
        .map_err(|e| format!("error: cannot read `{}`: {e}", input.display()))?;
    let mut sources = SourceMap::default();
    let file = sources.add(input.display().to_string(), text);
    let mut diags = Diagnostics::default();

    let runtime = sources.add(RUNTIME_NAME, RUNTIME_SOURCE);
    let mir = match front_end(&sources, file, Some(runtime), None, &mut diags) {
        FrontEnd::Mir(mir) if !diags.has_errors() => mir,
        _ => return Err(diag::render(&diags.into_vec(), &sources, color)),
    };

    // Intermediate files sit next to the output and are removed afterwards.
    let target = &target::RISCV32_VIRT;
    let object = output.with_extension("o");
    let script = output.with_extension("ld");
    let result = codegen::emit_object(&mir, target, opt, &object)
        .and_then(|()| {
            fs::write(&script, link::linker_script(target))
                .map_err(|e| format!("cannot write `{}`: {e}", script.display()))
        })
        .and_then(|()| link::link(&object, link, &script, output));
    let _ = fs::remove_file(&object);
    let _ = fs::remove_file(&script);
    result.map_err(|e| format!("error: {e}"))
}

enum FrontEnd {
    /// Stopped at the requested stage.
    Dump(String),
    /// Ran to completion (or the requested stage comes after MIR).
    Mir(mir::Mir),
    /// A stage failed; the errors are in the diagnostics.
    Failed,
}

/// Runs everything up to MIR, stopping early at `stop` if it is a front-end
/// stage.
fn front_end(
    sources: &SourceMap,
    file: FileId,
    runtime: Option<FileId>,
    stop: Option<Stage>,
    diags: &mut Diagnostics,
) -> FrontEnd {
    let text = &sources.file(file).text;

    // Syntax phases keep going after errors to report as much as possible.
    let tokens = lexer::lex(file, text, diags);
    if stop == Some(Stage::Tokens) {
        let dump = tokens
            .iter()
            .map(|t| format!("{t:?} {:?}\n", sources.snippet(t.span)))
            .collect();
        return FrontEnd::Dump(dump);
    }

    let ast = parser::parse(text, &tokens, diags);
    if stop == Some(Stage::Ast) {
        return FrontEnd::Dump(format!("{ast:#?}\n"));
    }

    let runtime_ast = runtime.map(|rt| {
        let text = &sources.file(rt).text;
        let tokens = lexer::lex(rt, text, diags);
        parser::parse(text, &tokens, diags)
    });

    // Semantic phases need a well-formed input.
    if diags.has_errors() {
        return FrontEnd::Failed;
    }

    let hir = match hir::lower(&ast, runtime_ast.as_ref(), diags) {
        // Type checking needs every name resolved.
        Some(hir) if !diags.has_errors() || stop == Some(Stage::Hir) => hir,
        _ => return FrontEnd::Failed,
    };
    if stop == Some(Stage::Hir) {
        return FrontEnd::Dump(format!("{hir:#?}\n"));
    }

    // Every item is checked, on demand of the others and then in order
    // (ADR 0028).
    let mut queries = Queries::new(&hir);
    queries.check_all();
    if stop == Some(Stage::Typeck) {
        let dump = ty::dump(&mut queries);
        diags.append(queries.diags);
        return FrontEnd::Dump(dump);
    }

    // Later phases rely on well-typed input.
    if diags.has_errors() || queries.diags.has_errors() {
        diags.append(queries.diags);
        return FrontEnd::Failed;
    }

    let mir = queries.into_mir();
    if stop == Some(Stage::Mir) {
        return FrontEnd::Dump(mir.to_string());
    }
    FrontEnd::Mir(mir)
}
