//! End-to-end tests: every `tests/e2e/*.ers` is compiled to an ELF and booted
//! on QEMU, on each target it runs on (ADR 0010, 0077). So is every
//! `tests/e2e/*/main.ers`, the root file of a program of several files
//! (ADR 0049), which holds the headers; that program is named after its
//! directory.
//!
//! A program reaches its machine through `mod board;`: the harness copies
//! the program into a work directory and puts the target's board module
//! (`tests/boards/<target>.ers`) beside its root file as `board.ers`. A
//! program that has a `board.ers` of its own is an error.
//!
//! A program states what it should do in header comments:
//!
//! ```text
//! // targets: riscv32imac-unknown-none-elf   (the targets it runs on, by
//! //                           name, separated by spaces or commas; without
//! //                           this header, every target)
//! // expect-output: hi        (one line of UART output; may repeat)
//! // expect-exit: 0           (exit code written to the test finisher)
//! // link-c: helpers.c        (a C file, next to the root file, compiled
//! //                           with clang and linked in; may repeat)
//! // c-flags: -O1 -DX         (extra clang flags for those C files)
//! ```
//!
//! With `expect-exit`, the program must stop QEMU through `board::exit`
//! with that code, having printed exactly the expected lines. Without it,
//! the program must run forever: after a while QEMU must still be up.
//! Either way, the CPU must raise no exception or guest error; a
//! miscompiled program faults almost immediately and floods that log.
//!
//! Every program runs twice per target: unoptimized and with `-O`. The
//! optimizer exploits any undefined behavior in the generated IR, so
//! agreement between the two is evidence that the IR means what the
//! language says.

mod common;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use common::board;
use common::qemu::{self, EXIT_TIMEOUT, Expectation, RUN_FOREVER_CHECK};
use erebos::driver::OptLevel;
use erebos::target::Target;

#[test]
fn e2e() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e");
    let mut programs: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter_map(|path| {
            if path.is_dir() {
                Some(path.join("main.ers")).filter(|main| main.is_file())
            } else {
                Some(path).filter(|path| path.extension().is_some_and(|ext| ext == "ers"))
            }
        })
        .collect();
    programs.sort();
    assert!(!programs.is_empty(), "no programs in {}", dir.display());

    let mut failures = Vec::new();
    for program in &programs {
        let expected = match qemu::expectation(&fs::read_to_string(program).unwrap()) {
            Ok(expected) => expected,
            Err(e) => {
                failures.push(format!("{}: {e}", board::program_name(program)));
                continue;
            }
        };
        for target in &expected.targets {
            for opt in [OptLevel::None, OptLevel::Speed] {
                failures.extend(run(program, &expected, target, opt).err());
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

fn run(
    program: &Path,
    expected: &Expectation,
    target: &'static Target,
    opt: OptLevel,
) -> Result<(), String> {
    // A program of several files is named after its directory; a
    // `main.ers` among the single files is `main`.
    let stem = board::program_name(program);
    let name = match opt {
        OptLevel::None => format!("{stem} [{}]", target.name),
        OptLevel::Speed => format!("{stem} [{}] (-O)", target.name),
    };
    let file = match opt {
        OptLevel::None => stem.clone(),
        OptLevel::Speed => format!("{stem}.opt"),
    };
    let work = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("e2e")
        .join(target.name);
    fs::create_dir_all(&work).unwrap();
    let elf = work.join(format!("{file}.elf"));
    let log = work.join(format!("{file}.qemu.log"));

    let objects = expected
        .link_c
        .iter()
        .map(|file| {
            qemu::compile_c(
                &program.with_file_name(file),
                &expected.c_flags,
                &work,
                target,
                opt,
            )
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("{name}: {e}"))?;
    let staged = board::stage(program, target, &work.join("programs"))
        .map_err(|e| format!("{name}: {e}"))?;
    erebos::driver::build(&staged, &elf, &objects, target, opt, false)
        .map_err(|e| format!("{name}: build failed\n{e}"))?;

    let wait = if expected.exit.is_some() {
        EXIT_TIMEOUT
    } else {
        RUN_FOREVER_CHECK
    };
    let run = qemu::boot(&name, target, &elf, &log, wait)?;
    qemu::check(&name, expected, &run)
}

/// A program with a `board.ers` of its own is refused, with a message
/// that names the file.
#[test]
fn a_program_with_its_own_board_is_refused() {
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("e2e-harness");
    let dir = work.join("own_board");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("main.ers"), "mod board;\n").unwrap();
    fs::write(
        dir.join("board.ers"),
        "pub fn putc(byte: u8) {}
",
    )
    .unwrap();
    let error = board::stage(
        &dir.join("main.ers"),
        erebos::target::DEFAULT,
        &work.join("out"),
    )
    .expect_err("a program with its own board.ers must be refused");
    assert!(
        error.contains("board.ers") && error.contains("own_board"),
        "unexpected message: {error}"
    );
}

/// A `// targets:` header with a name that is no target fails, naming it
/// and the targets there are.
#[test]
fn an_unknown_target_in_the_header_fails() {
    let error = qemu::expectation(
        "// targets: riscv32imac-unknown-none-elf, z80
",
    )
    .err()
    .expect("an unknown target must fail");
    assert!(
        error.contains("`z80`") && error.contains("riscv32imac-unknown-none-elf"),
        "unexpected message: {error}"
    );
}

/// The header chooses targets, and a program without one runs on all.
#[test]
fn the_targets_header_chooses_targets() {
    let names = |source: &str| -> Vec<&'static str> {
        let expected = qemu::expectation(source).unwrap();
        expected.targets.iter().map(|t| t.name).collect()
    };
    assert_eq!(
        names("// targets: riscv32imac-unknown-none-elf\n"),
        ["riscv32imac-unknown-none-elf"]
    );
    let all: Vec<&str> = board::RUN_ON.iter().map(|t| t.name).collect();
    assert_eq!(names("// expect-exit: 0\n"), all);
}
