//! Rules of the compiler's architecture, checked mechanically (ADR 0063):
//! the layers use only the modules the pipeline allows them, external
//! crates stay in the layer that wraps them, integer arithmetic never
//! reaches LLVM with `nsw`/`nuw` (ADR 0007), and the panics in the
//! compiler do not grow unnoticed.
//!
//! The checks read the source as text, with comments and the contents of
//! string and character literals blanked out. They do not parse Rust, but
//! read the paths that name another module (`crate::`, `super::`, and a
//! module imported by name) as `use` trees, over as many lines as a tree
//! takes. What they cannot read, such as a test module that is not the
//! last item of its file, fails the test instead of passing unchecked.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use erebos::driver::{self, OptLevel, Stage};

/// Modules every layer may use. They carry no layer's output and use no
/// layer, only each other: spans and diagnostics are shared by everything
/// (ADR 0003), and `int`, `ops`, `builtin` and `target` are the vocabulary
/// the layers have in common.
const SHARED: &[&str] = &["span", "diag", "int", "ops", "builtin", "target"];

/// Each module of `src/`, with the modules it may use besides [`SHARED`]
/// (ADR 0003, 0028, 0033, 0063). A module of a layer may always use its
/// own submodules. A file that belongs to no module here fails the test.
const LAYERS: &[(&str, &[&str])] = &[
    ("span", &[]),
    ("diag", &[]),
    ("int", &[]),
    ("ops", &[]),
    ("builtin", &[]),
    ("target", &[]),
    ("syntax", &[]),
    ("hir", &["syntax"]),
    ("layout", &["hir"]),
    ("memory", &[]),
    ("ty", &["hir", "layout", "query"]),
    ("mir", &["layout", "memory"]),
    (
        "mir::build",
        &["mir", "hir", "ty", "layout", "memory", "query", "eval"],
    ),
    ("eval", &["hir", "mir", "query"]),
    (
        "query",
        &["hir", "ty", "layout", "memory", "mir", "mir::build", "eval"],
    ),
    ("codegen", &["mir"]),
    ("link", &["mir"]),
    (
        "driver",
        &["syntax", "hir", "ty", "query", "mir", "codegen", "link"],
    ),
];

/// Edges of [`LAYERS`] that may name only ids (items whose name ends in
/// `Id`): the evaluator runs MIR and names HIR items only to ask `query`
/// for them (ADR 0033, rule 1).
const ONLY_IDS: &[(&str, &str)] = &[("eval", "hir")];

/// External crates, and the only module that may use each.
const EXTERNAL: &[(&str, &str)] = &[("inkwell", "codegen"), ("miette", "diag")];

/// Names no module may use: the raw LLVM API, which `inkwell` re-exports,
/// would bypass the checks on codegen (`LLVMBuildNSWAdd`).
const FORBIDDEN: &[&str] = &["llvm_sys"];

/// The integer operations of LLVM IR that can carry `nsw` or `nuw`.
const WRAPPING_OPS: &[&str] = &["add", "sub", "mul", "shl", "trunc"];

/// The panics each file of `src/` holds outside its test module (see
/// [`count_panics`]). User errors are diagnostics, never panics
/// (AGENTS.md), so a panic is only for an internal invariant. A count that
/// grows means a new one: make sure no user input reaches it, raise the
/// count, and say why in the step's `Decisions`. A count that drops is
/// lowered here, so it cannot grow back.
const PANICS: &[(&str, usize)] = &[
    ("src/codegen/mod.rs", 67),
    ("src/diag.rs", 2),
    ("src/eval.rs", 28),
    ("src/hir/lower.rs", 5),
    ("src/hir/lower/expr.rs", 3),
    ("src/hir/lower/hardware.rs", 1),
    ("src/hir/lower/pattern.rs", 3),
    ("src/hir/lower/resolve.rs", 7),
    ("src/hir/mod.rs", 3),
    ("src/layout.rs", 2),
    ("src/mir/build.rs", 40),
    ("src/mir/build/bit_struct.rs", 3),
    ("src/mir/mod.rs", 16),
    ("src/query.rs", 27),
    ("src/query/bit_struct.rs", 2),
    ("src/query/hardware.rs", 3),
    ("src/span.rs", 3),
    ("src/syntax/lexer.rs", 1),
    ("src/syntax/parser.rs", 1),
    ("src/ty.rs", 2),
    ("src/ty/dump.rs", 13),
    ("src/ty/expr.rs", 4),
    ("src/ty/pattern.rs", 6),
    ("src/ty/usefulness.rs", 4),
];

#[test]
fn layers_use_only_the_modules_the_pipeline_allows() {
    let sources = sources();
    let reexports = reexports(&sources);
    let mut errors = Vec::new();
    for source in &sources {
        if source.module.is_empty() {
            continue;
        }
        let Some((from, _)) = layer_of(&source.module) else {
            errors.push(format!(
                "{}: belongs to no module of `LAYERS`: give it its place in the pipeline (ADR 0063)",
                source.name
            ));
            continue;
        };
        for found in uses(&source.code, &source.module) {
            let checked = found
                .path
                .and_then(|path| check_use(from, &path, &reexports));
            if let Err(error) = checked {
                errors.push(format!("{}:{}: {error}", source.name, found.line));
            }
        }
    }
    assert!(errors.is_empty(), "\n{}\n", errors.join("\n"));
}

/// `lib.rs` only declares the modules, and the command line reaches the
/// compiler through the driver alone.
#[test]
fn the_crate_root_and_the_command_line_hold_no_layer() {
    let mut errors = Vec::new();
    for source in sources() {
        match source.name.as_str() {
            "src/lib.rs" => {
                for (i, line) in source.code.lines().enumerate() {
                    let line = line.trim();
                    if !(line.is_empty() || line.starts_with("pub mod ") && line.ends_with(';')) {
                        errors.push(format!("src/lib.rs:{}: holds more than `pub mod`", i + 1));
                    }
                }
            }
            "src/main.rs" => {
                for found in paths_after(&source.code, "erebos", &[]) {
                    let path = found.path.unwrap_or_default();
                    if path.get(1).map(String::as_str) != Some("driver") {
                        errors.push(format!(
                            "src/main.rs:{}: uses the compiler other than through `erebos::driver`",
                            found.line
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    assert!(errors.is_empty(), "\n{}\n", errors.join("\n"));
}

#[test]
fn external_crates_stay_in_their_module() {
    let mut errors = Vec::new();
    for source in sources() {
        for (i, line) in source.code.lines().enumerate() {
            if let Some(name) = idents(line).find(|name| FORBIDDEN.contains(name)) {
                errors.push(format!(
                    "{}:{}: `{name}` is not used in the compiler (ADR 0063)",
                    source.name,
                    i + 1
                ));
            }
        }
        let layer = layer_of(&source.module).map_or("", |(layer, _)| layer);
        for &(krate, owner) in EXTERNAL {
            if layer != owner {
                for found in paths_after(&source.code, krate, &[]) {
                    errors.push(format!(
                        "{}:{}: `{krate}` is used outside `{owner}` (ADR 0063)",
                        source.name, found.line
                    ));
                }
            }
        }
    }
    assert!(errors.is_empty(), "\n{}\n", errors.join("\n"));
}

/// Codegen calls no builder method and sets no flag that makes integer
/// arithmetic poison on overflow. This covers every path of codegen, also
/// those no test program reaches and the code of `core`.
#[test]
fn codegen_names_no_wrap_flag() {
    let mut errors = Vec::new();
    for source in sources() {
        if layer_of(&source.module).map(|(layer, _)| layer) != Some("codegen") {
            continue;
        }
        for (i, line) in source.code.lines().enumerate() {
            if let Some(name) = idents(line).find(|name| names_wrap_flag(name)) {
                errors.push(format!("{}:{}: `{name}` (ADR 0007)", source.name, i + 1));
            }
        }
    }
    assert!(errors.is_empty(), "\n{}\n", errors.join("\n"));
}

/// Unoptimized IR is what codegen emits, through `mem2reg` alone, which
/// adds no flags; with `-O`, LLVM may add the flags it has proven safe.
/// The dumps leave `core` out, which [`codegen_names_no_wrap_flag`] covers.
#[test]
fn integer_arithmetic_has_no_wrap_flags() {
    let mut errors = Vec::new();
    let mut seen = BTreeSet::new();
    for path in test_programs() {
        let text = fs::read_to_string(&path).unwrap().replace("\r\n", "\n");
        let Ok(ir) = driver::dump(&path, &text, Stage::LlvmIr, OptLevel::None, false) else {
            continue;
        };
        for line in ir.lines() {
            let Some((op, flags)) = integer_op(line) else {
                continue;
            };
            seen.insert(op);
            if flags.iter().any(|&f| f == "nsw" || f == "nuw") {
                errors.push(format!("{}: `{}` (ADR 0007)", display(&path), line.trim()));
            }
        }
    }
    let missing: Vec<_> = WRAPPING_OPS
        .iter()
        .filter(|op| !seen.contains(*op))
        .collect();
    assert!(
        missing.is_empty(),
        "no test program emits {missing:?}: the check would pass unchecked"
    );
    assert!(errors.is_empty(), "\n{}\n", errors.join("\n"));
}

#[test]
fn panics_do_not_grow() {
    let expected: BTreeMap<String, usize> = PANICS
        .iter()
        .map(|&(file, count)| (file.to_owned(), count))
        .collect();
    let actual: BTreeMap<String, usize> = sources()
        .iter()
        .map(|source| {
            (
                source.name.clone(),
                count_panics(&source.name, &source.code),
            )
        })
        .filter(|&(_, count)| count > 0)
        .collect();
    let files: BTreeSet<&String> = expected.keys().chain(actual.keys()).collect();
    let (mut grown, mut dropped) = (String::new(), String::new());
    for file in files {
        let before = expected.get(file).copied().unwrap_or(0);
        let now = actual.get(file).copied().unwrap_or(0);
        if now > before {
            grown += &format!("  {file}: {before} -> {now}\n");
        } else if now < before {
            dropped += &format!("    (\"{file}\", {now}),\n");
        }
    }
    let mut report = String::new();
    if !grown.is_empty() {
        report += &format!(
            "Panics were added. Each must be an internal invariant no user input \
             reaches (AGENTS.md): check each one, record why in the step's \
             `Decisions`, and only then raise the count in `PANICS` by hand:\n\n{grown}\n"
        );
    }
    if !dropped.is_empty() {
        report += &format!("Panics were removed. Lower their counts in `PANICS`:\n\n{dropped}");
    }
    assert!(report.is_empty(), "\n{report}");
}

#[test]
fn the_checks_see_what_they_check() {
    let module = |path: &str| path.split("::").map(str::to_owned).collect::<Vec<_>>();
    let found = |code: &str, at: &str| -> Vec<Result<Vec<String>, String>> {
        uses(&strip(code), &module(at))
            .into_iter()
            .map(|found| found.path)
            .collect()
    };
    let ok = |paths: &[&str]| -> Vec<Result<Vec<String>, String>> {
        paths.iter().map(|p| Ok(module(p))).collect()
    };

    assert_eq!(found("use crate::hir::Ty;", "ty"), ok(&["hir::Ty"]));
    assert_eq!(
        found(
            "use crate::{\n    codegen,\n    hir::{Ty, FnId},\n};",
            "driver"
        ),
        ok(&["codegen", "hir::Ty", "hir::FnId"])
    );
    assert_eq!(
        found("let id = crate::hir::FnId(callee);", "eval"),
        ok(&["hir::FnId"])
    );
    assert_eq!(found("use super::mir::Body;", "ty"), ok(&["mir::Body"]));
    assert_eq!(
        found("use super::super::mir::Body;", "ty::expr"),
        ok(&["mir::Body"])
    );
    assert_eq!(
        found("use super::Checker;", "ty::expr"),
        ok(&["ty::Checker"])
    );
    assert!(found("use super::super::X;", "ty")[0].is_err());
    assert_eq!(found("use crate::*;", "ty"), ok(&["*"]));
    assert_eq!(
        found("use crate::mir;\nfn f() { mir::build_body(); }", "codegen"),
        ok(&["mir", "mir::build_body"])
    );
    assert_eq!(
        found(
            "use crate::mir::{self, Body};
fn f() { mir::build_body(); }",
            "codegen"
        ),
        ok(&["mir", "mir::Body", "mir::build_body"])
    );
    assert!(found("pub(crate) fn f() {}", "ty").is_empty());
    assert_eq!(
        paths_after(
            "let c = ::inkwell::context::Context::create();",
            "inkwell",
            &[]
        )
        .len(),
        1
    );
    assert!(found("// use crate::mir;\nlet s = \"crate::mir\";", "ty").is_empty());

    let none = BTreeMap::new();
    let mir_reexports = reexports(&[Source {
        name: "src/mir/mod.rs".to_owned(),
        module: module("mir"),
        code: "pub use build::{build_body, build_expr_body};\npub use crate::memory::MemoryMap;"
            .to_owned(),
    }]);
    assert_eq!(
        mir_reexports.into_iter().collect::<Vec<_>>(),
        [
            (("mir".to_owned(), "build_body".to_owned()), "mir::build"),
            (
                ("mir".to_owned(), "build_expr_body".to_owned()),
                "mir::build"
            ),
        ]
    );
    let mut mir_reexports = BTreeMap::new();
    mir_reexports.insert(("mir".to_owned(), "build_body".to_owned()), "mir::build");
    assert!(check_use("codegen", &module("mir::Body"), &none).is_ok());
    assert!(check_use("codegen", &module("mir::build::X"), &none).is_err());
    assert!(check_use("codegen", &module("mir::build_body"), &mir_reexports).is_err());
    assert!(check_use("codegen", &module("hir::Ty"), &none).is_err());
    assert!(check_use("codegen", &module("span::Span"), &none).is_ok());
    assert!(check_use("ty", &module("*"), &none).is_err());
    assert!(check_use("codegen", &module("mir::*"), &none).is_err());
    assert!(check_use("ty", &module("ty::*"), &none).is_ok());
    assert!(check_use("eval", &module("hir::FnId"), &none).is_ok());
    assert!(check_use("eval", &module("hir::Ty"), &none).is_err());
    assert!(check_use("mir::build", &module("mir::Body"), &none).is_ok());

    let code = "let a = '\"'; // x.unwrap()\nlet b = \"//\"; /* y.unwrap() */ z.unwrap();";
    let stripped = strip(code);
    assert_eq!(stripped.chars().count(), code.chars().count());
    assert_eq!(stripped.lines().count(), 2);
    assert!(stripped.starts_with("let a = ' ';"));
    assert!(!stripped.contains("x.unwrap") && !stripped.contains("y.unwrap"));
    assert!(stripped.contains("let b = \"  \";") && stripped.contains("z.unwrap();"));
    assert_eq!(strip("let r = r#\"a \" b\"#;"), "let r = r#\"     \"#;");
    assert_eq!(strip("fn f<'a>(x: &'a u8) {}"), "fn f<'a>(x: &'a u8) {}");

    assert_eq!(
        cut_test_module("fn f() {}\n#[cfg(test)]\nmod tests {\n    fn g() {}\n}\n"),
        Ok("fn f() {}".to_owned())
    );
    assert!(cut_test_module("#[cfg(test)]\nmod tests {\n}\nfn after() {}\n").is_err());
    assert!(cut_test_module("#[cfg(test)]\nmod tests;\n").is_err());
    assert_eq!(
        cut_test_module("    #[cfg(test)]\n    mod helper {}\nfn f() {}"),
        Ok("    #[cfg(test)]\n    mod helper {}\nfn f() {}".to_owned())
    );

    assert_eq!(
        count_panics(
            "src/syntax/parser.rs",
            "x.unwrap(); self.expect(TokenKind::Semi)?;"
        ),
        1
    );
    assert_eq!(
        count_panics(
            "src/ty.rs",
            "y.expect(\"why\"); debug_assert!(z); self.expect(w);"
        ),
        2
    );
    assert_eq!(count_panics("src/ty.rs", ".map(Option::unwrap)"), 1);

    assert!(names_wrap_flag("build_int_nsw_add"));
    assert!(names_wrap_flag("set_no_signed_wrap_flag"));
    assert!(names_wrap_flag("LLVMBuildNSWAdd"));
    assert!(!names_wrap_flag("answer"));
    assert_eq!(
        integer_op("  %3 = mul nuw nsw i8 %1, %2"),
        Some(("mul", vec!["nuw", "nsw"]))
    );
    assert_eq!(
        integer_op("  %3 = trunc i32 %1 to i8"),
        Some(("trunc", vec!["i32", "%1"]))
    );
    assert_eq!(
        integer_op("  %3 = getelementptr inbounds nuw i8, ptr %0, i32 1"),
        None
    );
}

/// A file of `src/`: its name, its module path (empty for the crate root
/// and `main.rs`), and its code up to its test module, with comments and
/// literals blanked out.
struct Source {
    name: String,
    module: Vec<String>,
    code: String,
}

fn sources() -> Vec<Source> {
    let mut files = Vec::new();
    collect(&src_dir(), &mut files, &|path| {
        path.extension().is_some_and(|ext| ext == "rs")
    });
    files
        .iter()
        .map(|file| {
            let name = display(file);
            let text = fs::read_to_string(file).unwrap().replace("\r\n", "\n");
            let code = cut_test_module(&strip(&text)).unwrap_or_else(|e| panic!("{name}: {e}"));
            Source {
                name,
                module: module_path(file),
                code,
            }
        })
        .collect()
}

/// The text with comments and the contents of string and character
/// literals replaced by spaces, keeping every line break, so that offsets
/// and line numbers stay those of the source.
fn strip(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let blank = |c: char| if c == '\n' { '\n' } else { ' ' };
    let at = |i: usize| chars.get(i).copied();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let after_ident = i > 0 && is_ident(chars[i - 1]);
        if c == '/' && at(i + 1) == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                out.push(' ');
                i += 1;
            }
        } else if c == '/' && at(i + 1) == Some('*') {
            let mut depth = 0;
            while i < chars.len() {
                if chars[i] == '/' && at(i + 1) == Some('*') {
                    depth += 1;
                    out.push_str("  ");
                    i += 2;
                } else if chars[i] == '*' && at(i + 1) == Some('/') {
                    depth -= 1;
                    out.push_str("  ");
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    out.push(blank(chars[i]));
                    i += 1;
                }
            }
        } else if c == 'r'
            && (!after_ident || chars[i - 1] == 'b' && (i < 2 || !is_ident(chars[i - 2])))
            && matches!(at(i + 1), Some('"' | '#'))
            && raw_string_hashes(&chars, i + 1).is_some()
        {
            let hashes = raw_string_hashes(&chars, i + 1).unwrap();
            out.push('r');
            out.extend(std::iter::repeat_n('#', hashes));
            out.push('"');
            i += 2 + hashes;
            let end: String = std::iter::once('"')
                .chain(std::iter::repeat_n('#', hashes))
                .collect();
            while i < chars.len() && !chars[i..].starts_with(&end.chars().collect::<Vec<_>>()) {
                out.push(blank(chars[i]));
                i += 1;
            }
            out.push_str(&end);
            i += end.chars().count();
        } else if c == '"' {
            out.push('"');
            i += 1;
            while i < chars.len() {
                match chars[i] {
                    '\\' => {
                        out.push(' ');
                        out.extend(at(i + 1).map(blank));
                        i += 2;
                    }
                    '"' => {
                        out.push('"');
                        i += 1;
                        break;
                    }
                    other => {
                        out.push(blank(other));
                        i += 1;
                    }
                }
            }
        } else if c == '\'' && at(i + 1) == Some('\\') {
            let close = (i + 3..chars.len())
                .find(|&j| chars[j] == '\'')
                .unwrap_or(chars.len() - 1);
            out.push('\'');
            out.extend((i + 1..close).map(|_| ' '));
            out.push('\'');
            i = close + 1;
        } else if c == '\'' && at(i + 2) == Some('\'') {
            out.push_str("' '");
            i += 3;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// How many `#` open the raw string whose `#`s or quote start at `i`.
fn raw_string_hashes(chars: &[char], i: usize) -> Option<usize> {
    let hashes = chars[i..].iter().take_while(|&&c| c == '#').count();
    (chars.get(i + hashes) == Some(&'"')).then_some(hashes)
}

/// The code before the file's test module: a `#[cfg(test)]` at the start
/// of a line followed by `mod name {`, whose closing `}` at the start of a
/// line must end the file.
fn cut_test_module(code: &str) -> Result<String, String> {
    let lines: Vec<&str> = code.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let Some(next) = lines.get(i + 1) else {
            continue;
        };
        if line.trim_end() != "#[cfg(test)]" || !next.starts_with("mod ") {
            continue;
        }
        if !next.trim_end().ends_with('{') {
            return Err(format!(
                "line {}: a test module in a file of its own cannot be told apart",
                i + 2
            ));
        }
        let end = (i + 2..lines.len())
            .find(|&j| lines[j] == "}")
            .ok_or_else(|| format!("line {}: the test module does not close", i + 2))?;
        if lines[end + 1..].iter().any(|line| !line.trim().is_empty()) {
            return Err(format!(
                "line {}: the test module is not the last item of the file",
                i + 1
            ));
        }
        return Ok(lines[..i].join("\n"));
    }
    Ok(code.to_owned())
}

/// A path into another module, and the line it is on.
struct Use {
    line: usize,
    path: Result<Vec<String>, String>,
}

/// Every path in `code` that names a module of the crate, written in the
/// module `module`: through `crate::`, through `super::`, or through a
/// module the file imports by name. Each is the full path from the crate
/// root, or an error when it cannot be one.
fn uses(code: &str, module: &[String]) -> Vec<Use> {
    let mut found = paths_after(code, "crate", &[]);
    for start in path_starts(code, "super") {
        let mut rest = &code[start..];
        let mut up = 0;
        while let Some(after) = rest.strip_prefix("super::") {
            rest = after;
            up += 1;
        }
        let line = line_of(code, start);
        if up > module.len() {
            found.push(Use {
                line,
                path: Err("`super` reaches above the crate root".to_owned()),
            });
            continue;
        }
        for path in use_tree(rest) {
            let full = module[..module.len() - up]
                .iter()
                .cloned()
                .chain(path)
                .collect();
            found.push(Use {
                line,
                path: Ok(full),
            });
        }
    }
    for found in &mut found {
        if let Ok(path) = &mut found.path {
            path.retain(|segment| segment != "self");
        }
    }
    // A layer imported by name, as `use crate::mir;` or
    // `use crate::mir::{self, Body};`, is then used through paths that
    // start with its name.
    let imported: BTreeSet<Vec<String>> = found
        .iter()
        .filter_map(|found| found.path.as_ref().ok())
        .filter(|path| layer_of(path).is_some_and(|(_, len)| len == path.len()))
        .cloned()
        .collect();
    for path in imported {
        let name = path.last().unwrap().clone();
        found.extend(paths_after(code, &name, &path[..path.len() - 1]));
    }
    found.sort_by_key(|found| found.line);
    found
}

/// The paths of the `use` trees after each `word::` in `code` that starts
/// a path, each with `prefix` before it.
fn paths_after(code: &str, word: &str, prefix: &[String]) -> Vec<Use> {
    let mut found = Vec::new();
    for start in path_starts(code, word) {
        let rest = &code[start + word.len() + 2..];
        for path in use_tree(rest) {
            let path = prefix
                .iter()
                .cloned()
                .chain(word_unless_crate(word))
                .chain(path)
                .collect();
            found.push(Use {
                line: line_of(code, start),
                path: Ok(path),
            });
        }
    }
    found
}

fn word_unless_crate(word: &str) -> Option<String> {
    (word != "crate").then(|| word.to_owned())
}

/// Where `word::` starts a path in `code`: not inside a longer name and
/// not after another `::`.
fn path_starts(code: &str, word: &str) -> Vec<usize> {
    let pattern = format!("{word}::");
    code.match_indices(&pattern)
        .map(|(at, _)| at)
        .filter(|&at| {
            let before = &code[..at];
            let before = before.strip_suffix("::").unwrap_or(before);
            !before
                .chars()
                .next_back()
                .is_some_and(|c| is_ident(c) || c == ':')
        })
        .collect()
}

/// The paths of the `use` tree at the start of `text`, as `a::{b, c::d}`
/// or `a::B(x)`: segments up to the first character that continues none.
fn use_tree(text: &str) -> Vec<Vec<String>> {
    let mut cursor = Cursor { text, at: 0 };
    cursor.tree()
}

struct Cursor<'a> {
    text: &'a str,
    at: usize,
}

impl<'a> Cursor<'a> {
    fn rest(&self) -> &'a str {
        &self.text[self.at..]
    }

    fn skip_space(&mut self) {
        let rest = self.rest();
        self.at += rest.len() - rest.trim_start().len();
    }

    fn eat(&mut self, token: &str) -> bool {
        let found = self.rest().starts_with(token);
        if found {
            self.at += token.len();
        }
        found
    }

    fn segment(&mut self) -> Option<String> {
        if self.eat("*") {
            return Some("*".to_owned());
        }
        let len = self
            .rest()
            .find(|c: char| !is_ident(c))
            .unwrap_or(self.rest().len());
        let segment = &self.rest()[..len];
        if segment.is_empty() || segment.starts_with(|c: char| c.is_ascii_digit()) {
            return None;
        }
        self.at += len;
        Some(segment.to_owned())
    }

    fn tree(&mut self) -> Vec<Vec<String>> {
        if self.eat("{") {
            let mut paths = Vec::new();
            loop {
                self.skip_space();
                if self.eat("}") || self.rest().is_empty() {
                    break;
                }
                paths.extend(self.tree());
                self.skip_space();
                if !self.eat(",") {
                    self.skip_space();
                    self.eat("}");
                    break;
                }
            }
            return paths;
        }
        let Some(segment) = self.segment() else {
            return vec![Vec::new()];
        };
        if segment != "*" && self.eat("::") {
            return self
                .tree()
                .into_iter()
                .map(|path| std::iter::once(segment.clone()).chain(path).collect())
                .collect();
        }
        vec![vec![segment]]
    }
}

/// The module of [`LAYERS`] a path from the crate root falls in, with the
/// number of its segments: the longest one that starts the path.
fn layer_of(path: &[String]) -> Option<(&'static str, usize)> {
    LAYERS
        .iter()
        .map(|&(layer, _)| (layer, layer.split("::").count()))
        .filter(|&(layer, len)| {
            path.len() >= len && layer.split("::").zip(path).all(|(a, b)| a == b)
        })
        .max_by_key(|&(_, len)| len)
}

/// Items a module of [`LAYERS`] re-exports from one of its own submodules
/// that is a module of `LAYERS` too, as `mir` re-exports `build_body` from
/// `mir::build`: `(module, item) -> submodule`. An item re-exported from
/// any other module, as `mir` re-exports `MemoryMap`, is part of the
/// re-exporting layer's output and stays its own.
fn reexports(sources: &[Source]) -> BTreeMap<(String, String), &'static str> {
    let mut found = BTreeMap::new();
    for source in sources {
        let Some((layer, len)) = layer_of(&source.module) else {
            continue;
        };
        if len != source.module.len() {
            continue;
        }
        for (at, _) in source.code.match_indices("pub use ") {
            for path in use_tree(&source.code[at + "pub use ".len()..]) {
                let full: Vec<String> = match path.first().map(String::as_str) {
                    Some("crate") => path[1..].to_vec(),
                    Some("self") => source
                        .module
                        .iter()
                        .cloned()
                        .chain(path[1..].iter().cloned())
                        .collect(),
                    Some("super") | None => continue,
                    Some(_) => source
                        .module
                        .iter()
                        .cloned()
                        .chain(path.iter().cloned())
                        .collect(),
                };
                let Some(item) = full.last().filter(|item| *item != "*") else {
                    continue;
                };
                let inner = format!("{layer}::");
                if let Some((from, _)) =
                    layer_of(&full).filter(|&(from, _)| from.starts_with(&inner))
                {
                    found.insert((layer.to_owned(), item.clone()), from);
                }
            }
        }
    }
    found
}

/// Whether the module `from` may name `path`, a path from the crate root.
fn check_use(
    from: &str,
    path: &[String],
    reexports: &BTreeMap<(String, String), &'static str>,
) -> Result<(), String> {
    let (path, glob) = match path.split_last() {
        Some((last, init)) if last == "*" => (init, true),
        _ => (path, false),
    };
    if path.is_empty() {
        return Err("a glob import from the crate root hides which modules are used".to_owned());
    }
    let shown = path.join("::");
    let Some((mut to, len)) = layer_of(path) else {
        return Err(format!("`crate::{shown}` is in no module of `LAYERS`"));
    };
    if let Some(&source) = path
        .get(len)
        .and_then(|item| reexports.get(&(to.to_owned(), item.clone())))
    {
        to = source;
    }
    if to == from || to.starts_with(&format!("{from}::")) {
        return Ok(());
    }
    if glob {
        return Err(format!(
            "a glob import of `crate::{shown}` hides what `{from}` uses from another layer"
        ));
    }
    let allowed = LAYERS
        .iter()
        .find(|&&(layer, _)| layer == from)
        .is_some_and(|&(_, allowed)| allowed.contains(&to));
    if !SHARED.contains(&to) && !allowed {
        return Err(format!(
            "`{from}` uses `crate::{shown}`, of `{to}`, which its layer may not (ADR 0003, 0033, 0063)"
        ));
    }
    if ONLY_IDS.contains(&(from, to)) && !(path.len() == len + 1 && path[len].ends_with("Id")) {
        return Err(format!(
            "`{from}` may name only ids of `{to}`, not `crate::{shown}` (ADR 0063)"
        ));
    }
    Ok(())
}

/// The file whose `self.expect(` is the parser's own method, which reports
/// a diagnostic.
const PARSER: &str = "src/syntax/parser.rs";

/// The panics in a file's code: `unwrap()` and `expect(` on any value but
/// the parser itself in [`PARSER`], the same through
/// a path (`Option::unwrap`), and `panic!`, `unreachable!`,
/// `unimplemented!`, `todo!`, `assert!`, `assert_eq!` and `assert_ne!`.
/// `debug_assert!`, indexing, slicing and arithmetic overflow are not
/// counted.
fn count_panics(file: &str, code: &str) -> usize {
    const METHODS: &[&str] = &[".unwrap()", ".expect("];
    const PATHS: &[&str] = &["::unwrap", "::expect"];
    const MACROS: &[&str] = &[
        "panic!(",
        "unreachable!(",
        "unimplemented!(",
        "todo!(",
        "assert!(",
        "assert_eq!(",
        "assert_ne!(",
    ];
    let methods = METHODS
        .iter()
        .flat_map(|p| code.match_indices(p))
        .filter(|&(at, p)| !(file == PARSER && p == ".expect(" && code[..at].ends_with("self")))
        .count();
    let paths = PATHS
        .iter()
        .flat_map(|p| code.match_indices(p))
        .filter(|&(at, p)| {
            let after = code[at + p.len()..].chars().next();
            !after.is_some_and(is_ident)
        })
        .count();
    let macros = MACROS
        .iter()
        .flat_map(|p| code.match_indices(p))
        .filter(|&(at, _)| !code[..at].chars().next_back().is_some_and(is_ident))
        .count();
    methods + paths + macros
}

/// Whether a name in codegen is that of a wrap flag or of a builder method
/// that sets one, as `build_int_nsw_add` or `set_no_signed_wrap_flag`.
fn names_wrap_flag(name: &str) -> bool {
    if name.contains("NSW") || name.contains("NUW") {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    let joined = lower.replace('_', "");
    lower.split('_').any(|part| part == "nsw" || part == "nuw")
        || joined.contains("nosignedwrap")
        || joined.contains("nounsignedwrap")
}

/// The opcode and the two tokens after it, if a line of IR is an integer
/// operation that LLVM lets carry `nsw` or `nuw`.
fn integer_op(line: &str) -> Option<(&'static str, Vec<&str>)> {
    let tokens: Vec<&str> = line
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|token| !token.is_empty())
        .collect();
    let (at, op) = tokens.iter().enumerate().find_map(|(at, token)| {
        WRAPPING_OPS
            .iter()
            .find(|op| *op == token)
            .map(|op| (at, *op))
    })?;
    Some((op, tokens[at + 1..].iter().copied().take(2).collect()))
}

fn idents(line: &str) -> impl Iterator<Item = &str> {
    line.split(|c: char| !is_ident(c))
        .filter(|name| !name.is_empty())
}

/// The module path of a file of `src/`: `src/ty/expr.rs` is `ty::expr`,
/// `src/mir/mod.rs` is `mir`, and `lib.rs` and `main.rs` are the root.
fn module_path(file: &Path) -> Vec<String> {
    let relative = file.strip_prefix(src_dir()).unwrap();
    let mut parts: Vec<String> = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    let file = parts.pop().unwrap();
    let stem = file.trim_end_matches(".rs");
    let root = parts.is_empty() && (stem == "lib" || stem == "main");
    if stem != "mod" && !root {
        parts.push(stem.to_owned());
    }
    parts
}

/// The programs of `tests/ui` and `tests/e2e`: single files, and the
/// `main.ers` of each program of several files.
fn test_programs() -> Vec<PathBuf> {
    let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut programs = Vec::new();
    for dir in ["ui", "e2e"] {
        let dir = tests.join(dir);
        collect(&dir, &mut programs, &|path| {
            path.extension().is_some_and(|ext| ext == "ers")
                && (path.parent() == Some(dir.as_path())
                    || path.file_name().is_some_and(|name| name == "main.ers"))
        });
    }
    programs
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>, keep: &dyn Fn(&Path) -> bool) {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect(&path, out, keep);
        } else if keep(&path) {
            out.push(path);
        }
    }
}

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// A path relative to the repository, with `/` on every host.
fn display(path: &Path) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let relative = path.strip_prefix(root).unwrap_or(path);
    relative.to_string_lossy().replace('\\', "/")
}

fn line_of(code: &str, at: usize) -> usize {
    code[..at].matches('\n').count() + 1
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}
