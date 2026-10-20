//! Reproducible builds (ADR 0027): compile-time execution sees nothing of
//! the host and zeroes the bytes it never wrote, so compiling the same
//! program twice gives identical executables.

use std::fs;
use std::path::Path;
use std::process::Command;

use erebos::driver::{self, OptLevel};

#[test]
fn builds_are_identical() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("reproducible");
    fs::create_dir_all(&work).unwrap();
    // Constants with padding, a table computed at compile time, and checks;
    // statics with padding and pointers to other statics (ADR 0030).
    for program in ["tests/e2e/crc_table.ers", "tests/e2e/static_pointers.ers"] {
        let program = root.join(program);
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
}

/// A program of several files builds to the same bytes from two working
/// directories, named by two different paths: its files are found from the
/// root file's directory and named from it (ADR 0049). The compiler runs
/// as a process, since a test cannot change its own working directory
/// without changing that of the tests running beside it.
#[test]
fn modules_build_the_same_from_any_directory() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let program = root.join("tests/e2e/modules");
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("reproducible");
    fs::create_dir_all(&work).unwrap();
    for opt in [None, Some("-O")] {
        let builds = [
            (
                root,
                "tests/e2e/modules/main.ers",
                work.join("modules-root.elf"),
            ),
            (program.as_path(), "main.ers", work.join("modules-dir.elf")),
        ];
        for (dir, input, output) in &builds {
            let result = Command::new(env!("CARGO_BIN_EXE_erebos"))
                .current_dir(dir)
                .args(["build", input, "-o"])
                .arg(output)
                .args(opt)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "building {input} from {} failed:\n{}",
                dir.display(),
                String::from_utf8_lossy(&result.stderr)
            );
        }
        assert!(
            fs::read(&builds[0].2).unwrap() == fs::read(&builds[1].2).unwrap(),
            "the builds of tests/e2e/modules at {opt:?} differ"
        );
    }
}
