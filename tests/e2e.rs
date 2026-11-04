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
use erebos::target::{RISCV32IMAC, THUMBV7M, Target};

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

    let mut objects = expected
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
    objects.extend(qemu::board_object(target, &work, opt).map_err(|e| format!("{name}: {e}"))?);
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

/// The three kinds of line QEMU logs for a correct run on `mps2-an385`
/// (ADR 0077) are removed from the event log, which is then empty.
#[test]
fn the_event_lines_of_a_correct_arm_run_are_removed() {
    let log = "Loaded reset SP 0x0 PC 0x0 from vector table\n\
               Loaded reset SP 0x20001000 PC 0x9 from vector table\n\
               Taking exception 16 [Semihosting call] on CPU 0\n\
               ...handling as semihosting call 0x20\n";
    assert_eq!(qemu::expected_events_removed(&THUMBV7M, log), "");
}

/// Any other line stays, whatever it resembles: another exception, a guest
/// error, a line that only contains one of the three.
#[test]
fn other_event_lines_stay() {
    let reset = "Loaded reset SP 0x20001000 PC 0x9 from vector table\n";
    for line in [
        "Taking exception 3 [Prefetch Abort]",
        "Taking exception 16 [Semihosting call",
        "Taking exception 17 [Semihosting call] ...",
        "Unassigned guest write to 0x40004014",
        " Loaded reset SP 0x1 PC 0x2 from vector table",
        "Loaded reset SP 0x1 PC 0x2 from vector table, and more",
        "Loaded reset SP 0x PC 0x2 from vector table",
        "Loaded reset SP 0xzz PC 0x2 from vector table",
        "it was ...handling as semihosting call 0x20",
        "...handling as semihosting call 20",
    ] {
        let log = format!("{reset}{line}\n{reset}");
        assert_eq!(
            qemu::expected_events_removed(&THUMBV7M, &log),
            format!("{line}\n"),
            "`{line}` must stay"
        );
    }
}

/// riscv32's log must be empty: nothing is removed from it.
#[test]
fn riscv32_keeps_every_event_line() {
    let log = "Taking exception 16 [Semihosting call] ...\n";
    assert_eq!(qemu::expected_events_removed(&RISCV32IMAC, log), log);
}

/// A program of one file, `main.ers`, beside `target`'s board (the program
/// declares `mod board;`), built by the compiler's command line; its
/// objects are those the board needs. What the process did.
fn build_by_command_line(
    name: &str,
    source: &str,
    target: &'static Target,
    opt: OptLevel,
) -> std::process::Output {
    let work = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("e2e-harness")
        .join(target.name);
    let dir = work.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("main.ers"), source).unwrap();
    fs::write(dir.join("board.ers"), board::source(target)).unwrap();
    let objects = qemu::board_object(target, &work, opt).unwrap();
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_erebos"));
    command
        .current_dir(&dir)
        .args(["build", "main.ers", "-o", "main.elf"])
        .args(["--target", target.name]);
    if opt == OptLevel::Speed {
        command.arg("-O");
    }
    if let Some(object) = &objects {
        command.arg("--link").arg(object);
    }
    command.output().unwrap()
}

/// A program that runs `body`, then stops QEMU, with `definitions` before
/// its entry function.
fn running(body: &str, definitions: &str) -> String {
    format!(
        "mod board;

{definitions}
entry fn main() -> ! {{
{body}    board::exit(0)
}}

panic fn on_panic() -> ! {{ loop {{}} }}
"
    )
}

/// The line and column where the first `asm(` of `source` starts, as
/// diagnostics show them.
fn asm_position(source: &str) -> String {
    let (line, text) = source
        .lines()
        .enumerate()
        .find(|(_, text)| text.contains("asm("))
        .unwrap();
    format!("main.ers:{}:{}", line + 1, text.find("asm(").unwrap() + 1)
}

/// A template the assembler rejects is `codegen::asm`, labelled at the `asm`
/// expression, with the compiler's own exit status: LLVM's default handler
/// would print a message of its own and end the process (ADR 0079, 0082).
#[test]
fn an_instruction_the_assembler_rejects_is_codegen_asm() {
    let source = running(
        "    bad();
",
        "fn bad() {
    asm(\"frobnicate x1, x2\");
}
",
    );
    for target in board::RUN_ON {
        for opt in [OptLevel::None, OptLevel::Speed] {
            let name = format!("unknown_instruction [{}] {opt:?}", target.name);
            let output = build_by_command_line("unknown_instruction", &source, target, opt);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(output.status.code(), Some(1), "{name}:\n{stderr}");
            assert_eq!(
                stderr.matches("codegen::asm").count(),
                1,
                "{name}:\n{stderr}"
            );
            assert!(
                stderr.contains(&asm_position(&source)) && stderr.contains("frobnicate"),
                "{name}: the label is not at the `asm`:\n{stderr}"
            );
            assert!(!stderr.contains("codegen::llvm"), "{name}:\n{stderr}");
        }
    }
}

/// A function with a bad template called from three places, which `-O`
/// inlines into three copies of it, is one report, not three (ADR 0079).
#[test]
fn a_bad_template_called_from_three_places_is_reported_once() {
    let source = running(
        "    bad();
    bad();
    bad();
",
        "fn bad() {
    asm(\"frobnicate x1, x2\");
}
",
    );
    for target in board::RUN_ON {
        for opt in [OptLevel::None, OptLevel::Speed] {
            let name = format!("three_calls [{}] {opt:?}", target.name);
            let output = build_by_command_line("three_calls", &source, target, opt);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(output.status.code(), Some(1), "{name}:\n{stderr}");
            assert_eq!(
                stderr.matches("codegen::asm").count(),
                1,
                "{name}:\n{stderr}"
            );
            assert!(stderr.contains(&asm_position(&source)), "{name}:\n{stderr}");
        }
    }
}

/// A label that is not local, in a function that `-O` inlines twice, is
/// defined twice in the object: LLVM cannot say which `asm` expression it
/// comes from, so the report has no label. Without inlining the program
/// builds (ADR 0079).
#[test]
fn a_label_defined_by_inlining_is_codegen_asm_without_a_label() {
    let source = "mod board;

static mut COUNT: u32 = 0;

fn once() {
    asm(\"shared_label: nop\");
    COUNT = COUNT + 1;
}

entry fn main() -> ! {
    once();
    once();
    board::exit(0)
}

panic fn on_panic() -> ! { loop {} }
";
    for target in board::RUN_ON {
        let name = format!("shared_label [{}]", target.name);
        let output = build_by_command_line("shared_label", source, target, OptLevel::None);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "{name} builds without inlining:\n{stderr}"
        );

        let output = build_by_command_line("shared_label", source, target, OptLevel::Speed);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{name} -O:\n{stderr}");
        assert_eq!(
            stderr.matches("codegen::asm").count(),
            1,
            "{name} -O:\n{stderr}"
        );
        assert!(
            stderr.contains("shared_label") && !stderr.contains("main.ers:"),
            "{name} -O: the report has a label, or not the message:\n{stderr}"
        );
    }
}

/// More `reg` operands than registers: LLVM finds none for the rest, which is
/// a report of the `asm` expression, not `codegen::llvm` (ADR 0079).
#[test]
fn more_operands_than_registers_is_codegen_asm() {
    let operands = "in(reg) a, ".repeat(16);
    let placeholders = "{}".repeat(16);
    let definitions = format!(
        "fn many(a: u32) {{
    asm(\"nop {placeholders}\", {});
}}
",
        operands.trim_end_matches(", ")
    );
    let source = running(
        "    many(1);
",
        &definitions,
    );
    let target = &THUMBV7M;
    for opt in [OptLevel::None, OptLevel::Speed] {
        let name = format!("too_many_registers [{}] {opt:?}", target.name);
        let output = build_by_command_line("too_many_registers", &source, target, opt);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{name}:\n{stderr}");
        assert_eq!(
            stderr.matches("codegen::asm").count(),
            1,
            "{name}:\n{stderr}"
        );
        assert!(stderr.contains(&asm_position(&source)), "{name}:\n{stderr}");
        assert!(!stderr.contains("codegen::llvm"), "{name}:\n{stderr}");
    }
}

/// The rendered `codegen::asm` (ADR 0079), as the command line prints it: two
/// templates the assembler rejects reported in the order of the source and
/// not of the calls, a label LLVM cannot place, and an `asm` that finds no
/// register. The paths are those of the program, never the work directory's.
#[test]
fn codegen_asm_is_rendered() {
    let rendered = |name: &str, source: &str, target: &'static Target, opt: OptLevel| {
        let output = build_by_command_line(name, source, target, opt);
        assert_eq!(output.status.code(), Some(1), "{name}");
        String::from_utf8(output.stderr).unwrap()
    };

    // The second function is called first, and the first has an immediate the
    // assembler refuses on riscv32.
    let two = running(
        "    second(1);
    first();
",
        "fn first() {
    asm(\"frobnicate x1, x2\");
}

fn second(x: u32) {
    asm(\"addi a0, {}, 99999\", in(reg) x, out(\"a0\") _);
}
",
    );
    insta::assert_snapshot!(
        "codegen_asm_two_in_source_order",
        rendered("render_two", &two, &RISCV32IMAC, OptLevel::None)
    );

    let label = running(
        "    once();
    once();
",
        "static mut COUNT: u32 = 0;

fn once() {
    asm(\"shared_label: nop\");
    COUNT = COUNT + 1;
}
",
    );
    insta::assert_snapshot!(
        "codegen_asm_without_a_label",
        rendered("render_label", &label, &RISCV32IMAC, OptLevel::Speed)
    );

    let registers = running(
        "    many(1);
",
        &format!(
            "fn many(a: u32) {{
    asm(\"nop {}\", {});
}}
",
            "{}".repeat(16),
            vec!["in(reg) a"; 16].join(", ")
        ),
    );
    insta::assert_snapshot!(
        "codegen_asm_without_registers",
        rendered("render_registers", &registers, &THUMBV7M, OptLevel::None)
    );
}
