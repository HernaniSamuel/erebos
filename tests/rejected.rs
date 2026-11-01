//! Programs with an injected error are rejected (ADR 0069).
//!
//! A snippet that breaks one rule of the language is inserted into a valid
//! program of `tests/ui` or `tests/e2e`, and the compiler must report the
//! code of that rule.
//!
//! - `EREBOS_REJECTED_PROGRAMS`: how many base programs to inject into
//!   (default `DEFAULT_PROGRAMS`).
//! - `EREBOS_REJECTED_SEED`: the seed of the choice of programs and places
//!   (default 0).

mod common;

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use common::generator::Rng;
use erebos::driver::{self, OptLevel, Stage};

/// How many base programs a run injects into without
/// `EREBOS_REJECTED_PROGRAMS`.
const DEFAULT_PROGRAMS: usize = 150;

/// Where a snippet goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Place {
    /// Statements, inserted at a statement boundary inside a function body.
    Statements,
    /// Items, inserted at the end of the file.
    Items,
}

/// One rule of the language and the shortest text that breaks it.
struct Snippet {
    /// The code of the diagnostic the text must produce.
    code: &'static str,
    /// The stage whose output shows the error: the base is compiled to it.
    stage: Stage,
    place: Place,
    /// Uses only literals and names of its own (`injected_*`, `Inj*`).
    text: &'static str,
}

/// The checks on values known at compile time (`docs/reference.md`). The
/// names are of the form `injected_*` so that no base program has them.
const CATALOG: &[Snippet] = &[
    Snippet {
        code: "ty::shift-out-of-range",
        stage: Stage::Mir,
        place: Place::Statements,
        text: "let injected_shift: u32 = 1 << 32;",
    },
    Snippet {
        code: "ty::division-by-zero",
        stage: Stage::Mir,
        place: Place::Statements,
        text: "let injected_div: u32 = 7 / 0;",
    },
    Snippet {
        code: "ty::index-out-of-bounds",
        stage: Stage::Mir,
        place: Place::Statements,
        text: "let injected_array: [u8; 2] = [1, 2];\nlet injected_item: u8 = injected_array[2];",
    },
    Snippet {
        code: "ty::field-value-out-of-range",
        stage: Stage::Mir,
        place: Place::Items,
        text: "struct InjReg: u8 {\n    pub count: u8 at 0..=3 = 0,\n}\n\n\
               fn injected_field() -> InjReg {\n    InjReg { count: 16, .. }\n}",
    },
];

/// The valid programs to inject into: every root file of `tests/ui` and
/// `tests/e2e`, in a fixed order.
fn base_programs() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut paths = Vec::new();
    for dir in ["ui", "e2e"] {
        let mut entries: Vec<_> = fs::read_dir(root.join(dir))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            if path.extension().is_some_and(|e| e == "ers") {
                paths.push(path);
            } else if path.join("main.ers").is_file() {
                paths.push(path.join("main.ers"));
            }
        }
    }
    paths
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap().replace("\r\n", "\n")
}

/// Whether `text` compiles to `stage` without a diagnostic.
fn compiles(path: &Path, text: &str, stage: Stage) -> bool {
    driver::dump(path, text, stage, OptLevel::None, false).is_ok()
}

/// A block of braces while the text is scanned.
struct Block {
    /// It is the body of a function.
    function: bool,
    /// Its header (`if`, `while`, `match`) may name a value known at compile
    /// time, so that its code may never run and is not checked.
    maybe_unchecked: bool,
    /// A statement of it may not complete: what follows is dead code.
    dead: bool,
}

/// The names of the functions of `text` that return `!`.
fn never_functions(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|line| line.contains("-> !"))
        .filter_map(|line| line.split("fn ").nth(1))
        .filter_map(|rest| rest.split('(').next())
        .collect()
}

/// Whether the words of a statement mean that it may not complete, so that
/// what follows is dead code, which the checks of values known at compile
/// time do not look at: `return`, `break`, `continue`, a `loop`, a value of
/// type `!` or a call to a function that returns `!`.
fn diverges(code: &str, never: &[&str]) -> bool {
    code.contains(": !")
        || code
            .split(|k: char| !k.is_alphanumeric() && k != '_')
            .any(|w| matches!(w, "return" | "break" | "continue" | "loop") || never.contains(&w))
}

/// Whether the header of a block names something that may be known at
/// compile time: `true`, `false` or a constant (written in capitals).
fn names_a_constant(header: &str) -> bool {
    header
        .split(|k: char| !k.is_alphanumeric() && k != '_')
        .any(|w| {
            matches!(w, "true" | "false")
                || (w.len() > 1
                    && w.chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
        })
}

/// The offsets at which a statement may be inserted: just after the `{` that
/// opens the body of a function and just after each `;` that ends a line
/// inside one, each at the start of the next line. A place is left out when
/// the statement would be dead code (after a statement that diverges, in its
/// block) or inside a block whose condition may be a constant, where the
/// compiler does not check it (reference, "Checks on values known at
/// compile time"). Strings, character literals and comments are skipped.
fn statement_boundaries(text: &str) -> Vec<usize> {
    let never = never_functions(text);
    let mut stack: Vec<Block> = Vec::new();
    let mut header = String::new();
    let mut comment = 0usize;
    let mut points = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        // The last significant character of the line, and whether it opened
        // a function body.
        let mut last = None;
        // The line without comments and strings.
        let mut code = String::new();
        while i < chars.len() {
            let c = chars[i];
            let next = chars.get(i + 1).copied();
            if comment > 0 {
                if c == '*' && next == Some('/') {
                    comment -= 1;
                    i += 1;
                } else if c == '/' && next == Some('*') {
                    comment += 1;
                    i += 1;
                }
            } else if c == '/' && next == Some('/') {
                break;
            } else if c == '/' && next == Some('*') {
                comment = 1;
                i += 1;
            } else if c == '"' {
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    i += if chars[i] == '\\' { 2 } else { 1 };
                }
                last = Some(('"', false));
            } else if c == '\'' {
                // A character literal is `'x'` or `'\x'` with a short escape.
                let close = chars[i + 1..]
                    .iter()
                    .take(12)
                    .position(|&k| k == '\'')
                    .filter(|&p| p > 0 && !(p == 1 && chars[i + 1] == '\\'));
                if let Some(p) = close {
                    i += p + 1;
                }
                last = Some(('\'', false));
            } else if c == '{' {
                let function = header
                    .split(|k: char| !k.is_alphanumeric() && k != '_')
                    .any(|w| w == "fn");
                stack.push(Block {
                    function,
                    maybe_unchecked: !function && names_a_constant(&header),
                    dead: false,
                });
                code.push(c);
                header.clear();
                last = Some(('{', function));
            } else if c == '}' {
                stack.pop();
                code.push(c);
                header.clear();
                last = Some(('}', false));
            } else if c == ';' {
                code.push(c);
                header.clear();
                last = Some((';', false));
            } else {
                header.push(c);
                code.push(c);
                if !c.is_whitespace() {
                    last = Some((c, false));
                }
            }
            i += 1;
        }
        let diverging = diverges(&code, &never);
        let usable =
            stack.iter().any(|b| b.function) && !stack.iter().any(|b| b.maybe_unchecked || b.dead);
        if line.ends_with('\n') && usable {
            match last {
                Some(('{', true)) => points.push(offset + line.len()),
                Some((';', _)) if !diverging => points.push(offset + line.len()),
                _ => {}
            }
        }
        // The statements of this line are done: what follows them in the
        // block is dead when they diverge.
        if diverging && let Some(block) = stack.last_mut() {
            block.dead = true;
        }
        offset += line.len();
    }
    points
}

/// `base` with the snippet inserted, and the line it starts on.
fn inject(base: &str, snippet: &Snippet, rng: &mut Rng) -> Option<(String, usize)> {
    let at = match snippet.place {
        Place::Statements => {
            let points = statement_boundaries(base);
            if points.is_empty() {
                return None;
            }
            rng.pick(&points)
        }
        Place::Items => base.len(),
    };
    let mut text = String::with_capacity(base.len() + snippet.text.len() + 2);
    text.push_str(&base[..at]);
    if snippet.place == Place::Items && !base.ends_with('\n') {
        text.push('\n');
    }
    let line = text.matches('\n').count() + 1;
    text.push_str(snippet.text);
    text.push('\n');
    text.push_str(&base[at..]);
    Some((text, line))
}

/// The result of a run.
struct Report {
    /// How many snippets were injected and compiled.
    injections: usize,
    /// One message per injection that did not give its code.
    failures: Vec<String>,
}

/// Injects every snippet of `catalog` into `programs` base programs taken
/// from `bases` in an order fixed by `seed`; a base that does not compile
/// to a snippet's stage is skipped for that snippet.
fn run(catalog: &[Snippet], bases: &[PathBuf], programs: usize, seed: u64) -> Report {
    let mut rng = Rng::new(seed);
    let mut order: Vec<usize> = (0..bases.len()).collect();
    for i in (1..order.len()).rev() {
        order.swap(i, rng.below(i + 1));
    }

    let mut report = Report {
        injections: 0,
        failures: Vec::new(),
    };
    let mut used = 0;
    for index in order {
        if used == programs {
            break;
        }
        let path = &bases[index];
        let base = read(path);
        let mut valid: HashMap<String, bool> = HashMap::new();
        let mut injected = false;
        for snippet in catalog {
            let ok = *valid
                .entry(snippet.stage.name().to_owned())
                .or_insert_with(|| compiles(path, &base, snippet.stage));
            if !ok {
                continue;
            }
            let Some((text, line)) = inject(&base, snippet, &mut rng) else {
                continue;
            };
            injected = true;
            report.injections += 1;
            let shown = path
                .strip_prefix(env!("CARGO_MANIFEST_DIR"))
                .unwrap_or(path);
            match driver::dump(path, &text, snippet.stage, OptLevel::None, false) {
                Ok(_) => report.failures.push(format!(
                    "{}:{line}: `{}` was accepted ({}):\n{}",
                    shown.display(),
                    snippet.code,
                    snippet.stage,
                    snippet.text
                )),
                Err(rendered) if !rendered.contains(snippet.code) => {
                    report.failures.push(format!(
                        "{}:{line}: `{}` was not reported ({}); the snippet:\n{}\nthe diagnostics:\n{rendered}",
                        shown.display(),
                        snippet.code,
                        snippet.stage,
                        snippet.text
                    ));
                }
                Err(_) => {}
            }
        }
        if injected {
            used += 1;
        }
    }
    report
}

fn env_number(var: &str, default: u64) -> u64 {
    match env::var(var) {
        Ok(text) => text
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("{var} must be a number, got `{text}`")),
        Err(_) => default,
    }
}

#[test]
fn injected_errors_are_rejected() {
    let programs = env_number("EREBOS_REJECTED_PROGRAMS", DEFAULT_PROGRAMS as u64) as usize;
    let seed = env_number("EREBOS_REJECTED_SEED", 0);
    let report = run(CATALOG, &base_programs(), programs, seed);
    assert!(
        report.injections >= programs.min(CATALOG.len()),
        "only {} injections were made; the base programs are not found",
        report.injections
    );
    assert!(
        report.failures.is_empty(),
        "{} of {} injected errors were not reported (seed {seed}; run again with \
         EREBOS_REJECTED_SEED={seed} EREBOS_REJECTED_PROGRAMS={programs}):\n\n{}",
        report.failures.len(),
        report.injections,
        report.failures.join("\n\n")
    );
}

/// The check can fail: a snippet that is not wrong, or that is wrong in
/// another way than its code says, is reported.
#[test]
fn a_broken_catalog_entry_is_reported() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e");
    let bases = vec![root.join("hello.ers")];

    // Valid text under the code of an error: the compiler accepts it.
    let accepted = [Snippet {
        code: "ty::division-by-zero",
        stage: Stage::Mir,
        place: Place::Statements,
        text: "let injected_div: u32 = 7 / 1;",
    }];
    let report = run(&accepted, &bases, 1, 0);
    assert_eq!(report.injections, 1);
    assert_eq!(report.failures.len(), 1, "{:?}", report.failures);
    assert!(report.failures[0].contains("was accepted"));

    // An error under another code: rejected, but not for the code asked.
    let wrong_code = [Snippet {
        code: "ty::shift-out-of-range",
        stage: Stage::Mir,
        place: Place::Statements,
        text: "let injected_div: u32 = 7 / 0;",
    }];
    let report = run(&wrong_code, &bases, 1, 0);
    assert_eq!(report.injections, 1);
    assert_eq!(report.failures.len(), 1, "{:?}", report.failures);
    assert!(report.failures[0].contains("was not reported"));
}

#[test]
fn statement_boundaries_are_inside_functions() {
    let text = "\
const A: u32 = 1;
struct S { x: u32 }
fn f(x: u32) -> u32 { // comment {
    let s = \"{\";
    let c = '{';
    let y = x;
    /* { */
    if y == 0 {
        return 1;
    }
    y
}

fn g(x: u32) {
    let a = x;
    if LIMIT > 2 {
        let b = x;
    }
    stop();
    let c = x;
}

fn stop() -> ! {
    loop {}
}
";
    let points = statement_boundaries(text);
    let lines: Vec<usize> = points
        .iter()
        .map(|p| text[..*p].matches('\n').count() + 1)
        .collect();
    // In `f`: after its opening and each `let`, not after the `return`, whose
    // block ends there. In `g`: after its opening and the first `let`; not
    // in the `if` on a constant, nor after the call to `stop`, which never
    // returns. The opening of `stop` is a place too.
    assert_eq!(lines, vec![4, 5, 6, 7, 15, 16, 24]);
}
