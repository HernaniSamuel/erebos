//! Reproducible builds (ADR 0027): compile-time execution sees nothing of
//! the host and zeroes the bytes it never wrote, so compiling the same
//! program twice gives identical executables.

use std::fs;
use std::path::Path;

use erebos::driver::{self, OptLevel};

#[test]
fn builds_are_identical() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    // Constants with padding, a table computed at compile time, and checks.
    let program = root.join("tests/e2e/crc_table.ers");
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("reproducible");
    fs::create_dir_all(&work).unwrap();
    for opt in [OptLevel::None, OptLevel::Speed] {
        let first = work.join("first.elf");
        let second = work.join("second.elf");
        driver::build(&program, &first, &[], opt, false).unwrap();
        driver::build(&program, &second, &[], opt, false).unwrap();
        assert!(
            fs::read(&first).unwrap() == fs::read(&second).unwrap(),
            "two builds of {} at {opt:?} differ",
            program.display()
        );
    }
}
