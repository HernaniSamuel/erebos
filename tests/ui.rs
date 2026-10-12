//! Snapshot tests over `tests/ui/*.ers`.
//!
//! Each file is compiled up to a stage and the stage's dump (or the rendered
//! diagnostics) is snapshotted. A first line `// emit: <stage>` picks the
//! stage; otherwise the last stage is used.

use std::fs;

use erebos::driver::{self, Stage};

#[test]
fn ui() {
    insta::glob!("ui/*.ers", |path| {
        let text = fs::read_to_string(path).unwrap().replace("\r\n", "\n");
        let stage = text
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("// emit:"))
            .map(|name| name.trim().parse::<Stage>().unwrap())
            .unwrap_or(*Stage::ALL.last().unwrap());

        let name = path.file_name().unwrap().to_string_lossy();
        let output = match driver::dump(&name, &text, stage, false) {
            Ok(dump) => format!("--- {stage} ---\n{dump}"),
            Err(diags) => format!("--- errors ---\n{diags}"),
        };
        insta::assert_snapshot!(output);
    });
}
