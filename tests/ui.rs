//! Snapshot tests over `tests/ui/*.ers`.
//!
//! Each file is compiled up to a stage and the stage's dump (or the rendered
//! diagnostics) is snapshotted. Header comments on the first lines pick the
//! stage (`// emit: <stage>`; otherwise the last stage is used) and, with
//! `// opt: both`, also snapshot the stage with optimizations (`-O`).

use std::fs;

use erebos::driver::{self, OptLevel, Stage};

#[test]
fn ui() {
    insta::glob!("ui/*.ers", |path| {
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

        let name = path.file_name().unwrap().to_string_lossy();
        let mut output = String::new();
        for &opt in levels {
            let flag = if opt == OptLevel::Speed { " -O" } else { "" };
            output += &match driver::dump(&name, &text, stage, opt, false) {
                Ok(dump) => format!("--- {stage}{flag} ---\n{dump}"),
                Err(diags) => format!("--- errors{flag} ---\n{diags}"),
            };
        }
        insta::assert_snapshot!(output);
    });
}
