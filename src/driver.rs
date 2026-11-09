//! Runs the pipeline. `build` produces an executable; `dump` stops after a
//! given stage and returns its textual form, backing `--emit` and the
//! snapshot tests.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

pub use crate::codegen::OptLevel;
use crate::diag::{self, Diagnostic, Diagnostics};
use crate::link::LinkError;
use crate::query::Queries;
use crate::span::SourceMap;
use crate::syntax::{ast, lexer, parser};
/// The target of a build that does not choose one (ADR 0073), for the command
/// line, which reaches the compiler through the driver alone (ADR 0063).
pub use crate::target::DEFAULT as DEFAULT_TARGET;
pub use crate::target::Target;
use crate::target::{self, TARGETS};
use crate::{analysis, codegen, hir, link, mir, svd, ty};

/// The target `--target` names, or the error of ADR 0073 that lists the
/// known names. The command line reaches `target` through here (ADR 0063).
pub fn target_by_name(name: &str) -> Result<&'static Target, String> {
    target::by_name(name).ok_or_else(|| {
        let names: Vec<&str> = TARGETS.iter().map(|target| target.name).collect();
        format!(
            "unknown target `{name}`; the targets are {}",
            names.join(", ")
        )
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Tokens,
    Ast,
    Hir,
    Typeck,
    Mir,
    LinkerScript,
    LlvmIr,
}

impl Stage {
    pub const ALL: &[Stage] = &[
        Stage::Tokens,
        Stage::Ast,
        Stage::Hir,
        Stage::Typeck,
        Stage::Mir,
        Stage::LinkerScript,
        Stage::LlvmIr,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Stage::Tokens => "tokens",
            Stage::Ast => "ast",
            Stage::Hir => "hir",
            Stage::Typeck => "typeck",
            Stage::Mir => "mir",
            Stage::LinkerScript => "linker-script",
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

/// The files of `core`, embedded in the compiler and compiled with every
/// program (ADR 0052), each by the name diagnostics give it; the root file
/// first.
const CORE_FILES: &[(&str, &str)] = &[
    ("rt/core.ers", include_str!("../rt/core.ers")),
    ("rt/builtins.ers", include_str!("../rt/builtins.ers")),
];

/// The module of `core` whose file depends on the target (ADR 0080).
const CORE_ARCH_MODULE: &str = "core::arch";

/// The file of `core::arch` for `arch`, by the name diagnostics give it, as
/// ADR 0080's table says.
fn core_arch_file(arch: target::Arch) -> (&'static str, &'static str) {
    match arch {
        target::Arch::Riscv32 => (
            "rt/arch/riscv32.ers",
            include_str!("../rt/arch/riscv32.ers"),
        ),
        target::Arch::Armv7m => ("rt/arch/armv7m.ers", include_str!("../rt/arch/armv7m.ers")),
    }
}

/// Runs the pipeline up to `stage` on the program whose root file is at
/// `path` and holds `text`: the text is compiled, and the path gives the
/// root file its name and locates the module files. `core` is compiled
/// with it, but dumps show the program alone (ADR 0052). The stages are
/// those of `target` (ADR 0073). On failure, returns the rendered
/// diagnostics.
pub fn dump(
    path: &Path,
    text: &str,
    stage: Stage,
    target: &'static Target,
    opt: OptLevel,
    color: bool,
) -> Result<String, String> {
    let mut sources = SourceMap::default();
    let mut diags = Diagnostics::default();

    let program = Program::at(path, text);
    let output = match front_end(&program, Some(stage), target, &mut sources, &mut diags) {
        FrontEnd::Dump(dump) => Some(dump),
        FrontEnd::Mir(mir) if stage == Stage::LinkerScript => match &mir.memory {
            Some(map) => Some(link::linker_script(map, target)),
            None => {
                diags.push(link::no_memory_map());
                None
            }
        },
        FrontEnd::Mir(mir) => match codegen::emit_llvm_ir(&mir, target, opt) {
            Ok(ir) => Some(ir),
            Err(failures) => {
                failures.into_iter().for_each(|diag| diags.push(diag));
                None
            }
        },
        FrontEnd::Failed => None,
    };
    match output {
        Some(output) if !diags.has_errors() => Ok(output),
        _ => Err(diag::render(&diags.into_vec(), &sources, color)),
    }
}

/// Compiles the program whose root file is `input` into an ELF executable
/// at `output` for `target`, linked with the object files `link`
/// (ADR 0026). On failure, returns the rendered diagnostics or error
/// message.
pub fn build(
    input: &Path,
    output: &Path,
    link: &[PathBuf],
    target: &'static Target,
    opt: OptLevel,
    color: bool,
) -> Result<(), String> {
    let text = fs::read_to_string(input)
        .map_err(|e| format!("error: cannot read `{}`: {e}", input.display()))?;
    let mut sources = SourceMap::default();
    let mut diags = Diagnostics::default();

    let program = Program::at(input, &text);
    let mir = match front_end(&program, None, target, &mut sources, &mut diags) {
        FrontEnd::Mir(mir) if !diags.has_errors() => mir,
        _ => return Err(diag::render(&diags.into_vec(), &sources, color)),
    };
    // Checked before code generation, so that nothing is written.
    let Some(map) = &mir.memory else {
        return Err(diag::render(&[link::no_memory_map()], &sources, color));
    };

    // Intermediate files sit next to the output and are removed afterwards.
    let object = output.with_extension("o");
    let script = output.with_extension("ld");
    // What code generation reports is diagnostics, with spans of the program
    // (ADR 0079).
    let result = codegen::emit_object(&mir, target, opt, &object)
        .map_err(|failures| {
            let mut diags = Diagnostics::default();
            failures.into_iter().for_each(|diag| diags.push(diag));
            diag::render(&diags.into_vec(), &sources, color)
        })
        .and_then(|()| {
            fs::write(&script, link::linker_script(map, target))
                .map_err(|e| format!("error: cannot write `{}`: {e}", script.display()))
        })
        .and_then(|()| {
            link::link(&object, link, &script, output).map_err(|e| match e {
                LinkError::Run(e) => format!("error: {e}"),
                LinkError::Failed(stderr) => link_failure(&stderr, map, &sources, color),
            })
        });
    let _ = fs::remove_file(&object);
    let _ = fs::remove_file(&script);
    result
}

/// What `erebos svd` did: where it wrote the module, and the warnings it
/// reported, rendered (empty when there were none).
pub struct SvdOutput {
    pub output: PathBuf,
    pub warnings: String,
}

/// Generates the Erebos module for the SVD file `input` (ADR 0070) and
/// writes it at `output`, or, when it is left out, in the input's directory
/// under a name made from the input's (see [`svd::default_output_name`]). An
/// existing file is replaced. Nothing is written when an error is reported,
/// and an output that is the input is refused before anything is read. On
/// failure, returns the rendered diagnostics or error message.
pub fn svd(input: &Path, output: Option<&Path>, color: bool) -> Result<SvdOutput, String> {
    let file_name = input
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| format!("error: `{}` is no file name", input.display()))?;
    let output = match output {
        Some(output) => output.to_path_buf(),
        None => {
            let name = svd::default_output_name(&file_name).ok_or_else(|| {
                format!(
                    "error: cannot make a module name from `{file_name}`: name the output \
                     with `-o`"
                )
            })?;
            input.with_file_name(name)
        }
    };
    if same_file(input, &output) {
        return Err(format!(
            "error: the output `{}` is the input file; name another with `-o`",
            output.display()
        ));
    }
    let bytes =
        fs::read(input).map_err(|e| format!("error: cannot read `{}`: {e}", input.display()))?;
    let mut diags = Diagnostics::default();
    let Some(text) = svd::utf8(&bytes, &mut diags) else {
        return Err(diag::render(
            &diags.into_vec(),
            &SourceMap::default(),
            color,
        ));
    };
    let (generated, diags, sources) = svd::generate(&file_name, text);
    let rendered = diag::render(&diags.into_vec(), &sources, color);
    let Some(generated) = generated else {
        return Err(rendered);
    };
    fs::write(&output, generated)
        .map_err(|e| format!("error: cannot write `{}`: {e}", output.display()))?;
    Ok(SvdOutput {
        output,
        warnings: rendered,
    })
}

/// Whether two paths name one file: the same path, or one that the file
/// system resolves to the other's file (a link, another spelling, another
/// case on a system that ignores it).
fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// What `build` reports when `ld.lld` fails with the error output
/// `stderr`: a `link::region-overflow` per region of `map` it reports
/// overflowing, then the linker's other lines as they are, but for what
/// follows from an overflow.
fn link_failure(stderr: &str, map: &mir::MemoryMap, sources: &SourceMap, color: bool) -> String {
    let (overflows, rest) = link::region_overflows(stderr, map);
    let mut out = diag::render(&overflows, sources, color);
    let rest = rest.join("\n");
    if !rest.trim().is_empty() || overflows.is_empty() {
        out += &format!("error: linking failed:\n{}", rest.trim_end());
    }
    out
}

/// A program to compile: its root file's name and text, and where the
/// files of its modules are read from.
struct Program<'a> {
    /// The name sources and diagnostics give the root file. Every file of
    /// a program is named by its path from the root file's directory
    /// (ADR 0049), so the root file is named by its file name.
    name: String,
    text: &'a str,
    loader: Loader,
    /// The file of the module `core::arch`, in `core` only (ADR 0080).
    arch: Option<&'static str>,
}

impl<'a> Program<'a> {
    /// The program whose root file is at `path` and holds `text`.
    fn at(path: &Path, text: &'a str) -> Self {
        let dir = match path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
            _ => PathBuf::from("."),
        };
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| {
                let typed = name.to_string_lossy();
                let entries = Loader::Dir(dir.clone()).entries("").unwrap_or_default();
                listed_name(&typed, &entries)
            },
        );
        Program {
            name,
            text,
            loader: Loader::Dir(dir),
            arch: None,
        }
    }

    /// The file of the module at `path`, `file` by ADR 0049's rule: the
    /// module `core::arch` has the one of the target instead (ADR 0080).
    fn module_file(&self, path: &str, file: String) -> String {
        match self.arch {
            Some(arch) if path == CORE_ARCH_MODULE => arch.to_owned(),
            _ => file,
        }
    }
}

impl Program<'static> {
    /// `core`, from the compiler binary (ADR 0052), with the file of
    /// `core::arch` that `target` has (ADR 0080). Its files are named from the
    /// repository's root, `rt/core.ers`, so its root module's directory is
    /// `rt`.
    fn core(target: &Target) -> Self {
        let (name, text) = CORE_FILES[0];
        let arch = core_arch_file(target.arch);
        let files = CORE_FILES.iter().copied().chain([arch]).collect();
        Program {
            name: name.to_owned(),
            text,
            loader: Loader::Core(files),
            arch: Some(arch.0),
        }
    }
}

/// The root file's name as its directory lists it, `entries`, given the
/// name `typed` on the command line. On a file system that ignores case,
/// `MAIN.ERS` opens `main.ers`; naming the root file as listed keeps
/// `mod main;` in it from reading it again, and diagnostics name it as
/// its modules' files are named (ADR 0049). The name as typed if no entry
/// matches it, even ignoring case, or several do.
fn listed_name(typed: &str, entries: &[String]) -> String {
    if entries.iter().any(|entry| entry == typed) {
        return typed.to_owned();
    }
    let mut matches = entries
        .iter()
        .filter(|entry| entry.to_lowercase() == typed.to_lowercase());
    match (matches.next(), matches.next()) {
        (Some(entry), None) => entry.clone(),
        _ => typed.to_owned(),
    }
}

/// Where the files of a program's modules are read from. Paths are
/// relative to the root file's directory, with `/` between names
/// (ADR 0049); `""` is that directory.
enum Loader {
    /// The file system, below this directory.
    Dir(PathBuf),
    /// The files of `core`, `CORE_FILES` and the file of `core::arch`
    /// (ADR 0052, 0080).
    Core(Vec<(&'static str, &'static str)>),
    /// Files held in memory, each by its path, with its text or the error
    /// reading it gives.
    #[cfg(test)]
    Memory(Vec<(&'static str, Result<&'static str, &'static str>)>),
}

impl Loader {
    /// The text of the file at `path`.
    fn read(&self, path: &str) -> Result<String, String> {
        match self {
            Loader::Dir(dir) => fs::read_to_string(dir.join(path)).map_err(|e| e.to_string()),
            Loader::Core(files) => files
                .iter()
                .find(|(name, _)| *name == path)
                .map(|(_, text)| (*text).to_owned())
                .ok_or_else(|| "not found".to_owned()),
            #[cfg(test)]
            Loader::Memory(files) => files.iter().find(|(name, _)| *name == path).map_or_else(
                || Err("not found".to_owned()),
                |(_, text)| text.map(str::to_owned).map_err(str::to_owned),
            ),
        }
    }

    /// The names of the files and directories in the directory at `dir`,
    /// or why it cannot be listed.
    fn entries(&self, dir: &str) -> Result<Vec<String>, String> {
        match self {
            Loader::Dir(root) => fs::read_dir(root.join(dir))
                .and_then(|entries| {
                    entries
                        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
                        .collect()
                })
                .map_err(|e| e.to_string()),
            Loader::Core(files) => Ok(listing(files.iter().map(|(path, _)| *path), dir)),
            // A directory's own path given with an error stands for a
            // directory that cannot be listed.
            #[cfg(test)]
            Loader::Memory(files) => {
                if let Some((_, Err(e))) = files.iter().find(|(path, _)| *path == dir) {
                    return Err((*e).to_owned());
                }
                Ok(listing(files.iter().map(|(path, _)| *path), dir))
            }
        }
    }
}

/// The names of the files and directories in the directory `dir`, among
/// files held in memory at `paths`.
fn listing<'a>(paths: impl Iterator<Item = &'a str>, dir: &str) -> Vec<String> {
    let prefix = if dir.is_empty() {
        String::new()
    } else {
        format!("{dir}/")
    };
    paths
        .filter_map(|path| path.strip_prefix(prefix.as_str()))
        .filter_map(|rest| rest.split('/').next())
        .map(str::to_owned)
        .collect()
}

/// Reads the file of every `mod name;` among `items`, and in the modules
/// they hold, and puts its items in the module's body, then does the same
/// in that file (ADR 0049). `dir` is the directory of the module holding
/// `items`, and `module` its path, both `""` for the root module. Files are
/// added to `sources` in the order their `mod` declarations are met, depth
/// first. Syntax errors go to `diags`; a file that cannot be read is
/// `resolve::module-file`, in `file_errors`, and its module stays without
/// a body.
fn load_modules(
    items: &mut [ast::Item],
    dir: &str,
    module: &str,
    program: &Program,
    sources: &mut SourceMap,
    diags: &mut Diagnostics,
    file_errors: &mut Diagnostics,
) {
    for item in items {
        let ast::Item::Mod(decl) = item else {
            continue;
        };
        let name = &decl.name.name;
        let path = join(module, "::", name);
        let child_dir = join(dir, "/", name);
        let mut items = match decl.items.take() {
            Some(items) => items,
            None => {
                let file_path = program.module_file(&path, join(dir, "/", &format!("{name}.ers")));
                let text = match read_module_file(&file_path, &path, program) {
                    Ok(text) => text,
                    Err(error) => {
                        file_errors.push(error.label(decl.span, "declared here"));
                        continue;
                    }
                };
                let file = sources.add(file_path, text);
                let text = &sources.file(file).text;
                let tokens = lexer::lex(file, text, diags);
                parser::parse(text, &tokens, diags).items
            }
        };
        load_modules(
            &mut items,
            &child_dir,
            &path,
            program,
            sources,
            diags,
            file_errors,
        );
        decl.items = Some(items);
    }
}

/// `a` and `b` joined by `sep`, or `b` alone if `a` is empty.
fn join(a: &str, sep: &str, b: &str) -> String {
    if a.is_empty() {
        b.to_owned()
    } else {
        format!("{a}{sep}{b}")
    }
}

/// The text of `file`, the file of module `module`, or the
/// `resolve::module-file` error, still to be labelled. Every name on the
/// path must be in its directory's listing as written: one found there
/// only when case is ignored is an error on every file system, so that a
/// program reads the same files, or fails the same way, everywhere
/// (ADR 0049).
fn read_module_file(file: &str, module: &str, program: &Program) -> Result<String, Diagnostic> {
    let cannot_find = || {
        Diagnostic::error(
            "resolve::module-file",
            format!("cannot find `{file}`, the file of module `{module}`"),
        )
    };
    if file == program.name {
        return Err(Diagnostic::error(
            "resolve::module-file",
            format!("the file of module `{module}` is the root file, `{file}`"),
        )
        .help("the root module cannot be its own child: give the module another name (ADR 0049)"));
    }

    let cannot_read = |reason: String| {
        Diagnostic::error(
            "resolve::module-file",
            format!("cannot read `{file}`, the file of module `{module}`: {reason}"),
        )
    };

    // Each directory listed is the root file's or one found in the listing
    // before, so failing to list it is not a missing file.
    let mut dir = String::new();
    for name in file.split('/') {
        let entries = program.loader.entries(&dir).map_err(|e| {
            let what = if dir.is_empty() {
                "the root file's directory".to_owned()
            } else {
                format!("`{dir}`")
            };
            cannot_read(format!("cannot list {what}: {e}"))
        })?;
        let at = join(&dir, "/", name);
        if !entries.iter().any(|entry| entry == name) {
            let other = entries
                .iter()
                .find(|entry| entry.to_lowercase() == name.to_lowercase());
            return Err(match other {
                Some(other) => cannot_find().help(format!(
                    "`{}` differs from `{at}` only in case: the names of files and directories must match the names of modules exactly, also where the file system ignores case (ADR 0049)",
                    join(&dir, "/", other)
                )),
                None => cannot_find().help(
                    "`mod name;` reads `name.ers` from the directory of the module declaring it, which is the root file's directory for the root module and the directory named after the module for any other; there is no `mod.ers` (ADR 0049)",
                ),
            });
        }
        dir = at;
    }

    program.loader.read(file).map_err(cannot_read)
}

enum FrontEnd {
    /// Stopped at the requested stage.
    Dump(String),
    /// Ran to completion (or the requested stage comes after MIR). Boxed:
    /// the MIR is much larger than the other variants.
    Mir(Box<mir::Mir>),
    /// A stage failed; the errors are in the diagnostics.
    Failed,
}

/// Runs everything up to MIR, stopping early at `stop` if it is a front-end
/// stage. The `tokens` and `ast` stages show the root file alone: the
/// files of its modules are read after it is parsed, and `core`'s after
/// them (ADR 0049, 0052). Every later dump leaves `core` out, and so does
/// the MIR returned with a `stop`, for the `llvm-ir` dump. Type checking
/// checks the memory map against `target` (ADR 0073).
fn front_end(
    program: &Program,
    stop: Option<Stage>,
    target: &'static Target,
    sources: &mut SourceMap,
    diags: &mut Diagnostics,
) -> FrontEnd {
    let hir = match to_hir(program, stop, target, sources, diags) {
        Ok(hir) => hir,
        Err(end) => return end,
    };
    if stop == Some(Stage::Hir) {
        return FrontEnd::Dump(hir.dump());
    }

    // Every item is checked, on demand of the others and then in order
    // (ADR 0028).
    let mut queries = Queries::new(&hir, target);
    queries.check_all();
    if stop == Some(Stage::Typeck) {
        let dump = ty::dump(&mut queries);
        diags.append(queries.diags);
        return FrontEnd::Dump(dump);
    }

    // Every function that type checks is lowered and checked, whatever
    // errors the others have (ADR 0032); code generation relies on a
    // program without errors.
    queries.lower_all();
    if diags.has_errors() || queries.diags.has_errors() {
        diags.append(queries.diags);
        return FrontEnd::Failed;
    }

    let core = queries.core_parts();
    let mut mir = queries.into_mir();
    // The first MIR analysis (ADR 0093), over the program with `core`.
    analysis::check(&mir, diags);
    if diags.has_errors() {
        return FrontEnd::Failed;
    }
    if stop.is_some() {
        mir.leave_out(&core);
    }
    if stop == Some(Stage::Mir) {
        return FrontEnd::Dump(mir.to_string());
    }
    FrontEnd::Mir(Box::new(mir))
}

/// The front end up to the HIR: `Err` with what `front_end` returns when it
/// stops earlier, at the `tokens` or `ast` stage or on an error. At the
/// `hir` stage the HIR is returned even with errors in the diagnostics.
fn to_hir(
    program: &Program,
    stop: Option<Stage>,
    target: &'static Target,
    sources: &mut SourceMap,
    diags: &mut Diagnostics,
) -> Result<hir::Hir, FrontEnd> {
    let file = sources.add(&program.name, program.text);

    // Syntax phases keep going after errors to report as much as possible.
    let tokens = lexer::lex(file, program.text, diags);
    if stop == Some(Stage::Tokens) {
        let dump = tokens
            .iter()
            .map(|t| format!("{t:?} {:?}\n", sources.snippet(t.span)))
            .collect();
        return Err(FrontEnd::Dump(dump));
    }

    let mut ast = parser::parse(program.text, &tokens, diags);
    if stop == Some(Stage::Ast) {
        return Err(FrontEnd::Dump(format!("{ast:#?}\n")));
    }

    let mut file_errors = Diagnostics::default();
    load_modules(
        &mut ast.items,
        "",
        "",
        program,
        sources,
        diags,
        &mut file_errors,
    );

    let core = Program::core(target);
    let core_file = sources.add(&core.name, core.text);
    let tokens = lexer::lex(core_file, core.text, diags);
    let mut core_ast = parser::parse(core.text, &tokens, diags);
    load_modules(
        &mut core_ast.items,
        "rt",
        "core",
        &core,
        sources,
        diags,
        &mut file_errors,
    );

    // Semantic phases need a well-formed input. A module whose file could
    // not be read is resolved as empty, and its error reported with the
    // resolver's.
    let syntax_errors = diags.has_errors();
    diags.append(file_errors);
    if syntax_errors {
        return Err(FrontEnd::Failed);
    }

    // Later layers locate the compiler's checks without the text (ADR 0053).
    let files = sources
        .files()
        .map(|file| hir::FileDef::new(&file.name, &file.text))
        .collect();
    match hir::lower(&ast, &core_ast, files, core_file.index(), diags) {
        // Type checking needs every name resolved.
        Some(hir) if !diags.has_errors() || stop == Some(Stage::Hir) => Ok(hir),
        _ => Err(FrontEnd::Failed),
    }
}

/// The HIR of the program whose root file `main.ers` holds `text`, with
/// `core` for the default target, for the tests of the layers after it;
/// `None` if a stage before the HIR stopped.
#[cfg(test)]
pub(crate) fn hir_of(text: &'static str) -> Option<hir::Hir> {
    let program = Program {
        name: "main.ers".to_owned(),
        text,
        loader: Loader::Memory(Vec::new()),
        arch: None,
    };
    let mut sources = SourceMap::default();
    let mut diags = Diagnostics::default();
    to_hir(&program, None, target::DEFAULT, &mut sources, &mut diags).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::DEFAULT;

    /// The program of root file `main.ers`, holding `text`, whose module
    /// files are `files`.
    fn program(
        text: &'static str,
        files: Vec<(&'static str, Result<&'static str, &'static str>)>,
    ) -> Program<'static> {
        Program {
            name: "main.ers".to_owned(),
            text,
            loader: Loader::Memory(files),
            arch: None,
        }
    }

    /// The names of the files `program` reads, in the order of their ids,
    /// and the codes it reports up to the HIR.
    fn load(program: &Program) -> (Vec<String>, Vec<&'static str>) {
        let mut sources = SourceMap::default();
        let mut diags = Diagnostics::default();
        front_end(program, Some(Stage::Hir), DEFAULT, &mut sources, &mut diags);
        let names = sources.files().map(|file| file.name.clone()).collect();
        let codes = diags.into_vec().iter().map(|d| d.code).collect();
        (names, codes)
    }

    /// Files are numbered depth first, in the order their `mod`s are met,
    /// whatever the order of the loader's files (ADR 0049); `core`'s follow
    /// the program's (ADR 0052).
    #[test]
    fn module_files_are_numbered_depth_first() {
        let program = program(
            "mod a; mod c { mod d; } mod e;",
            vec![
                ("e.ers", Ok("")),
                ("c/d.ers", Ok("")),
                ("a/b.ers", Ok("")),
                ("a.ers", Ok("mod b;")),
            ],
        );
        let (names, codes) = load(&program);
        assert_eq!(
            names,
            [
                "main.ers",
                "a.ers",
                "a/b.ers",
                "c/d.ers",
                "e.ers",
                "rt/core.ers",
                "rt/builtins.ers",
                "rt/arch/riscv32.ers"
            ]
        );
        assert_eq!(codes, ["resolve::missing-entry", "resolve::missing-panic"]);
    }

    /// The file of `core::arch` is the target's (ADR 0080): a build reads one
    /// of them, named as the table of the ADR names it, and a program's own
    /// module called `arch` is not that module.
    #[test]
    fn core_arch_file_depends_on_the_target() {
        for (target, file) in [
            (&target::RISCV32IMAC, "rt/arch/riscv32.ers"),
            (&target::THUMBV7M, "rt/arch/armv7m.ers"),
        ] {
            let program = program("mod arch;", vec![("arch.ers", Ok(""))]);
            let mut sources = SourceMap::default();
            let mut diags = Diagnostics::default();
            front_end(&program, Some(Stage::Hir), target, &mut sources, &mut diags);
            let names: Vec<&str> = sources.files().map(|file| file.name.as_str()).collect();
            assert_eq!(
                names,
                [
                    "main.ers",
                    "arch.ers",
                    "rt/core.ers",
                    "rt/builtins.ers",
                    file
                ],
                "{}",
                target.name
            );
        }
    }

    /// A file that is listed but cannot be read is `resolve::module-file`,
    /// with the reason, and the other files are still read.
    #[test]
    fn unreadable_module_file() {
        let program = program(
            "mod a; mod b;",
            vec![
                ("a.ers", Err("stream did not contain valid UTF-8")),
                ("b.ers", Ok("")),
            ],
        );
        let mut sources = SourceMap::default();
        let mut diags = Diagnostics::default();
        front_end(
            &program,
            Some(Stage::Hir),
            DEFAULT,
            &mut sources,
            &mut diags,
        );
        let diags = diags.into_vec();
        assert_eq!(diags[0].code, "resolve::module-file");
        assert_eq!(
            diags[0].message,
            "cannot read `a.ers`, the file of module `a`: stream did not contain valid UTF-8"
        );
        let names: Vec<&str> = sources.files().map(|file| file.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "main.ers",
                "b.ers",
                "rt/core.ers",
                "rt/builtins.ers",
                "rt/arch/riscv32.ers"
            ]
        );
    }

    /// A directory on the path that is there but cannot be listed is
    /// `resolve::module-file` with the reason, not a missing file.
    #[test]
    fn unlistable_module_directory() {
        let program = program(
            "mod a { mod b; }",
            vec![("a", Err("access denied")), ("a/b.ers", Ok(""))],
        );
        let mut sources = SourceMap::default();
        let mut diags = Diagnostics::default();
        front_end(
            &program,
            Some(Stage::Hir),
            DEFAULT,
            &mut sources,
            &mut diags,
        );
        let diags = diags.into_vec();
        assert_eq!(diags[0].code, "resolve::module-file");
        assert_eq!(
            diags[0].message,
            "cannot read `a/b.ers`, the file of module `a::b`: cannot list `a`: access denied"
        );
        assert_eq!(diags[0].help, None);
    }

    /// The root file is named as its directory lists it, so that on a file
    /// system that ignores case `MAIN.ERS` is `main.ers`, and `mod main;`
    /// in it is still the root file.
    #[test]
    fn root_file_named_as_listed() {
        let entries = ["main.ers".to_owned(), "uart.ers".to_owned()];
        assert_eq!(listed_name("MAIN.ERS", &entries), "main.ers");
        assert_eq!(listed_name("main.ers", &entries), "main.ers");
        assert_eq!(listed_name("other.ers", &entries), "other.ers");
        let both = ["MAIN.ERS".to_owned(), "main.ers".to_owned()];
        assert_eq!(listed_name("MAIN.ERS", &both), "MAIN.ERS");
        assert_eq!(listed_name("Main.ers", &both), "Main.ers");
    }

    // ADR 0093, "What is a call": the calls code generation and LLVM insert
    // are not in MIR, and the check does not see them. That holds while the
    // functions they reach name no `static mut` and have no check. The walk
    // below is the test's own, not `analysis`'s: it follows `sym` operands,
    // which the check does not, and it reads every block, reachable or not.

    /// The whole MIR of a program of `text` on `target`, `core` included,
    /// before `leave_out`.
    fn whole_mir(target: &'static Target, text: &'static str) -> mir::Mir {
        let program = program(text, Vec::new());
        let mut sources = SourceMap::default();
        let mut diags = Diagnostics::default();
        match front_end(&program, None, target, &mut sources, &mut diags) {
            FrontEnd::Mir(mir) => *mir,
            FrontEnd::Dump(_) | FrontEnd::Failed => {
                panic!(
                    "{}: the program has errors: {:?}",
                    target.name,
                    diags.into_vec()
                )
            }
        }
    }

    /// What a walk found: the bodies that hold a check (`Terminator::Panic`)
    /// and the `static mut` they or others name, as `body: static`.
    #[derive(Debug, Default, PartialEq)]
    struct Found {
        checks: Vec<String>,
        statics: Vec<String>,
    }

    /// The `static mut`s a body names, by operand or by `sym`, and whether it
    /// holds a check; the bodies it calls or gives a `sym` to.
    fn read_body(body: &mir::Body, found: &mut Found, next: &mut Vec<mir::BodyId>) {
        let mut note = |stat: mir::StaticRef| {
            if stat.def().mutable {
                found
                    .statics
                    .push(format!("{}: {}", body.name, stat.def().name));
            }
        };
        let mut checks = false;
        for (_, block) in body.blocks() {
            let mut asms = Vec::new();
            for statement in &block.statements {
                statement.visit_operands(&mut |operand| {
                    if let mir::Operand::Const(mir::Const::Static(stat)) = operand {
                        note(*stat);
                    }
                });
                if let mir::Statement::Call { callee, .. } = statement {
                    next.push(*callee);
                }
                asms.extend(statement.asm());
            }
            block.terminator.visit_operands(&mut |operand| {
                if let mir::Operand::Const(mir::Const::Static(stat)) = operand {
                    note(*stat);
                }
            });
            asms.extend(block.terminator.asm());
            checks |= matches!(block.terminator, mir::Terminator::Panic(..));
            for asm in asms {
                for sym in asm.syms() {
                    match sym {
                        mir::AsmSym::Body(callee) => next.push(callee),
                        mir::AsmSym::Static(stat) => note(stat),
                    }
                }
            }
        }
        if checks {
            found.checks.push(body.name.clone());
        }
    }

    /// What the bodies reach from `roots`, following calls and `sym` operands.
    fn walk(mir: &mir::Mir, roots: &[mir::BodyId]) -> Found {
        let mut found = Found::default();
        let mut seen = vec![false; mir.bodies.len()];
        let mut work = roots.to_vec();
        while let Some(id) = work.pop() {
            if std::mem::replace(&mut seen[id.0 as usize], true) {
                continue;
            }
            read_body(mir.body(id), &mut found, &mut work);
        }
        found.checks.sort();
        found.statics.sort();
        found
    }

    /// The functions "What is a call" lists: every `#[export]` function of
    /// `core` (`runtime`, with a symbol that is no path) but `_start`, and the
    /// two of the `critical` block.
    fn inserted_roots(mir: &mir::Mir) -> Vec<mir::BodyId> {
        let mut roots: Vec<mir::BodyId> = mir
            .bodies()
            .filter(|(id, body)| body.runtime && !body.name.contains('.') && Some(*id) != mir.reset)
            .map(|(id, _)| id)
            .collect();
        roots.extend(mir.critical_enter);
        roots.extend(mir.critical_exit);
        roots
    }

    const BARE: &str = "entry fn main() -> ! { loop {} } panic fn on_panic() -> ! { loop {} }";

    const BOTH_TARGETS: [&Target; 2] = [&target::RISCV32IMAC, &target::THUMBV7M];

    /// The calls the compiler inserts reach no check and no `static mut`, on
    /// both targets. A later change to `core` that gives one a check fails
    /// here, and must then add the edge to the check (ADR 0093).
    #[test]
    fn inserted_calls_reach_no_static_and_no_check() {
        for target in BOTH_TARGETS {
            let mir = whole_mir(target, BARE);
            let roots = inserted_roots(&mir);
            assert_eq!(walk(&mir, &roots), Found::default(), "{}", target.name);
        }
    }

    /// The list of roots is not empty by accident: a rename of `memcpy`, or
    /// a change of how `core` marks its functions, would otherwise pass the
    /// test above with nothing to walk.
    #[test]
    fn the_inserted_roots_are_there() {
        for target in BOTH_TARGETS {
            let mir = whole_mir(target, BARE);
            let names: Vec<&str> = inserted_roots(&mir)
                .into_iter()
                .map(|id| mir.body(id).name.as_str())
                .collect();
            let mut wanted = vec!["memcpy", "__critical_enter", "__critical_exit"];
            if std::ptr::eq(target, &target::THUMBV7M) {
                wanted.push("__aeabi_uldivmod");
            }
            for name in wanted {
                assert!(
                    names.iter().any(|n| n.rsplit('.').next() == Some(name)),
                    "{}: no root `{name}` among {names:?}",
                    target.name
                );
            }
            assert!(
                names.iter().all(|name| *name != "_start"),
                "{}: `_start` is no root",
                target.name
            );
        }
    }

    /// The walk can fail: from roots the program declares, it finds a check
    /// in a function that has one, and a `static mut` in another, through
    /// a call and through a `sym`.
    #[test]
    fn the_walk_finds_a_check_and_a_static() {
        let text = "
            static mut COUNT: u32 = 0;
            fn checked(a: u32, d: u32) -> u32 { a / d }
            fn writes() { COUNT = 1; }
            fn through_call() { writes(); }
            #[naked]
            fn through_sym() -> ! {
                asm(\"j {}\", sym writes, options(noreturn));
            }
            fn quiet() {}
            entry fn main() -> ! { loop {} }
            panic fn on_panic() -> ! { loop {} }";
        for target in BOTH_TARGETS {
            let mir = whole_mir(target, text);
            let root = |name: &str| -> Vec<mir::BodyId> {
                mir.bodies()
                    .filter(|(_, body)| body.name == name)
                    .map(|(id, _)| id)
                    .collect()
            };
            let at = |name: &str| format!("{}: {name}", target.name);
            assert_eq!(
                walk(&mir, &root("checked")),
                Found {
                    checks: vec!["checked".to_owned()],
                    statics: Vec::new()
                },
                "{}",
                at("checked")
            );
            let writes = Found {
                checks: Vec::new(),
                statics: vec!["writes: COUNT".to_owned()],
            };
            for name in ["writes", "through_call", "through_sym"] {
                assert_eq!(walk(&mir, &root(name)), writes, "{}", at(name));
            }
            assert_eq!(
                walk(&mir, &root("quiet")),
                Found::default(),
                "{}",
                at("quiet")
            );
        }
    }

    /// `Mir::reset` leads into the program through `sym entry`, so a walk
    /// from it reaches the program's statics; its own body names none, as
    /// "Contexts, as MIR shows them" relies on.
    #[test]
    fn reset_names_no_static_mut() {
        for target in BOTH_TARGETS {
            let mir = whole_mir(
                target,
                "static mut COUNT: u32 = 0; entry fn main() -> ! { COUNT = 1; loop {} } panic fn on_panic() -> ! { loop {} }",
            );
            let reset = mir.reset.expect("core holds `_start`");
            let mut found = Found::default();
            let mut next = Vec::new();
            read_body(mir.body(reset), &mut found, &mut next);
            assert_eq!(found, Found::default(), "{}", target.name);
            assert!(!next.is_empty(), "{}: `_start` leads nowhere", target.name);
            // The walk would find the program's static through it.
            assert_eq!(
                walk(&mir, &[reset]).statics,
                ["main: COUNT"],
                "{}",
                target.name
            );
        }
    }
}
