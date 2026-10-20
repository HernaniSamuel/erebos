//! Snapshot tests over `tests/ui/*.ers` and `tests/ui/*/main.ers`.
//!
//! Each file is compiled up to a stage and the stage's dump (or the rendered
//! diagnostics) is snapshotted. Header comments on the first lines pick the
//! stage (`// emit: <stage>`; otherwise the last stage is used) and, with
//! `// opt: both`, also snapshot the stage with optimizations (`-O`).
//!
//! A directory with a `main.ers` is a program of several files: `main.ers`
//! is its root file, holding the headers, and the files of its modules sit
//! beside it and below (ADR 0049). It is snapshotted under the directory's
//! name.

use std::fs;
use std::path::Path;

use erebos::driver::{self, OptLevel, Stage};

#[test]
fn ui() {
    insta::glob!("ui/*.ers", |path| {
        let output = compile(path);
        insta::assert_snapshot!(output);
    });
    insta::glob!("ui/*/main.ers", |path| {
        let dir = path.parent().unwrap().file_name().unwrap();
        let mut settings = insta::Settings::clone_current();
        settings.set_snapshot_suffix(dir.to_string_lossy());
        let output = compile(path);
        settings.bind(|| insta::assert_snapshot!(output));
    });
}

/// The dumps or diagnostics of the program whose root file is `path`, at
/// the stage and levels its headers ask for.
fn compile(path: &Path) -> String {
    let text = fs::read_to_string(path).unwrap().replace("\r\n", "\n");
    let headers: Vec<&str> = text
        .lines()
        .take_while(|line| line.starts_with("//"))
        .collect();
    let stage = headers
        .iter()
        .find_map(|line| line.strip_prefix("// emit:"))
        .map(|name| name.trim().parse::<Stage>().unwrap())
        .unwrap_or(*Stage::ALL.last().unwrap());
    let levels: &[OptLevel] = if headers.contains(&"// opt: both") {
        &[OptLevel::None, OptLevel::Speed]
    } else {
        &[OptLevel::None]
    };

    let mut output = String::new();
    for &opt in levels {
        let flag = if opt == OptLevel::Speed { " -O" } else { "" };
        output += &match driver::dump(path, &text, stage, opt, false) {
            Ok(dump) => format!("--- {stage}{flag} ---\n{dump}"),
            Err(diags) => format!("--- errors{flag} ---\n{diags}"),
        };
    }
    output
}
