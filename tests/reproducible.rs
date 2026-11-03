//! Reproducible builds (ADR 0027): compile-time execution sees nothing of
//! the host and zeroes the bytes it never wrote, so compiling the same
//! program twice gives identical executables, for every target (ADR 0073).
//! Nothing runs here; each program is staged with each target's own board
//! (ADR 0077), and a board's C file is linked in.

mod common;

use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::process::Command;

use erebos::driver::{self, OptLevel};
use erebos::target::TARGETS;

#[test]
fn builds_are_identical() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("reproducible");
    fs::create_dir_all(&work).unwrap();
    // Constants with padding, a table computed at compile time, and checks;
    // statics with padding and pointers to other statics (ADR 0030).
    for program in ["tests/e2e/crc_table.ers", "tests/e2e/static_pointers.ers"] {
        for target in TARGETS {
            // Staged as the e2e tests do (ADR 0077): the programs declare
            // `mod board;`.
            let program = common::board::stage(
                &root.join(program),
                target,
                &work.join("programs").join(target.name),
            )
            .unwrap();
            for opt in [OptLevel::None, OptLevel::Speed] {
                let first = work.join("first.elf");
                let second = work.join("second.elf");
                let objects: Vec<_> = common::qemu::board_object(target, &work, opt)
                    .unwrap()
                    .into_iter()
                    .collect();
                driver::build(&program, &first, &objects, target, opt, false).unwrap();
                driver::build(&program, &second, &objects, target, opt, false).unwrap();
                assert!(
                    fs::read(&first).unwrap() == fs::read(&second).unwrap(),
                    "two builds of {} for {} at {opt:?} differ",
                    program.display(),
                    target.name
                );
            }
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
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("reproducible");
    fs::create_dir_all(&work).unwrap();
    for target in TARGETS {
        // The program and its board, copied to
        // `<work>/programs/<target>/modules`.
        let staged = common::board::stage(
            &root.join("tests/e2e/modules/main.ers"),
            target,
            &work.join("programs").join(target.name),
        )
        .unwrap();
        let program = staged.parent().unwrap();
        let parent = program.parent().unwrap();
        for opt in [None, Some("-O")] {
            // The board's C file, if it has one, is built beside the program.
            let object = common::qemu::board_object(
                target,
                &work,
                if opt.is_some() {
                    OptLevel::Speed
                } else {
                    OptLevel::None
                },
            )
            .unwrap();
            let builds = [
                (parent, "modules/main.ers", work.join("modules-root.elf")),
                (program, "main.ers", work.join("modules-dir.elf")),
            ];
            for (dir, input, output) in &builds {
                let result = Command::new(env!("CARGO_BIN_EXE_erebos"))
                    .current_dir(dir)
                    .args(["build", input, "-o"])
                    .arg(output)
                    .args(["--target", target.name])
                    .args(opt)
                    .args(
                        object
                            .iter()
                            .flat_map(|object| [OsStr::new("--link"), object.as_os_str()]),
                    )
                    .output()
                    .unwrap();
                assert!(
                    result.status.success(),
                    "building {input} for {} from {} failed:\n{}",
                    target.name,
                    dir.display(),
                    String::from_utf8_lossy(&result.stderr)
                );
            }
            assert!(
                fs::read(&builds[0].2).unwrap() == fs::read(&builds[1].2).unwrap(),
                "the builds of tests/e2e/modules for {} at {opt:?} differ",
                target.name
            );
        }
    }
}
