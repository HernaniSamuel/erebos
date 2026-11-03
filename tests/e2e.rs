//! End-to-end tests: every `tests/e2e/*.ers` is compiled to an ELF and booted
//! on QEMU `virt` (ADR 0010). So is every `tests/e2e/*/main.ers`, the root
//! file of a program of several files (ADR 0049), which holds the headers;
//! that program is named after its directory.
//!
//! A program states what it should do in header comments:
//!
//! ```text
//! // expect-output: hi        (one line of UART output; may repeat)
//! // expect-exit: 0           (exit code written to the test finisher)
//! // link-c: helpers.c        (a C file, next to the root file, compiled
//! //                           with clang and linked in; may repeat)
//! // c-flags: -O1 -DX         (extra clang flags for those C files)
//! ```
//!
//! With `expect-exit`, the program must stop QEMU through the test finisher
//! with that code, having printed exactly the expected lines. Without it,
//! the program must run forever: after a while QEMU must still be up.
//! Either way, the CPU must raise no exception or guest error; a
//! miscompiled program faults almost immediately and floods that log.
//!
//! Every program runs twice: unoptimized and with `-O`. The optimizer
//! exploits any undefined behavior in the generated IR, so agreement between
//! the two is evidence that the IR means what the language says.

mod common;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use common::qemu::{self, EXIT_TIMEOUT, RUN_FOREVER_CHECK};
use erebos::driver::OptLevel;

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

    let failures: Vec<String> = programs
        .iter()
        .flat_map(|program| [OptLevel::None, OptLevel::Speed].map(|opt| run(program, opt)))
        .filter_map(Result::err)
        .collect();
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

fn run(program: &Path, opt: OptLevel) -> Result<(), String> {
    // A program of several files is named after its directory; a
    // `main.ers` among the single files is `main`.
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e");
    let named = if program.file_name().unwrap() == "main.ers" && program.parent().unwrap() != dir {
        program.parent().unwrap().file_name().unwrap()
    } else {
        program.file_stem().unwrap()
    };
    let stem = named.to_string_lossy();
    let name = match opt {
        OptLevel::None => stem.to_string(),
        OptLevel::Speed => format!("{stem} (-O)"),
    };
    let file = match opt {
        OptLevel::None => stem.to_string(),
        OptLevel::Speed => format!("{stem}.opt"),
    };
    let expected = qemu::expectation(&fs::read_to_string(program).unwrap());
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("e2e");
    fs::create_dir_all(&work).unwrap();
    let elf = work.join(format!("{file}.elf"));
    let log = work.join(format!("{file}.qemu.log"));

    let objects = expected
        .link_c
        .iter()
        .map(|file| qemu::compile_c(&program.with_file_name(file), &expected.c_flags, &work, opt))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("{name}: {e}"))?;
    erebos::driver::build(program, &elf, &objects, erebos::target::DEFAULT, opt, false).map_err(
        |e| {
            format!(
                "{name}: build failed
{e}"
            )
        },
    )?;

    let wait = if expected.exit.is_some() {
        EXIT_TIMEOUT
    } else {
        RUN_FOREVER_CHECK
    };
    let run = qemu::boot(&name, &elf, &log, wait)?;
    qemu::check(&name, &expected, &run)
}
