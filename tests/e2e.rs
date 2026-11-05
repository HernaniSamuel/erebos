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

/// The files of `core::arch`, one per target (ADR 0080), by their path from
/// the repository's root.
const ARCH_FILES: [&str; 2] = ["rt/arch/riscv32.ers", "rt/arch/armv7m.ers"];

/// The portable items a file of `core::arch` lists in its header (ADR 0080):
/// the text after `// portable: ` on each line that has it, which is also the
/// start of the line that declares the item. Each must be declared in the
/// file as listed (`<item> {`), so that the list and the code cannot drift
/// apart.
fn portable_items(file: &str, text: &str) -> Result<Vec<String>, String> {
    let items: Vec<String> = text
        .lines()
        .filter_map(|line| line.strip_prefix("// portable: "))
        .map(|item| item.trim().to_owned())
        .collect();
    if items.is_empty() {
        return Err(format!("{file}: lists no portable item"));
    }
    for item in &items {
        let declaration = format!("{item} {{");
        if !text.lines().any(|line| line == declaration) {
            return Err(format!("{file}: no line `{declaration}`"));
        }
    }
    Ok(items)
}

/// The portable items every file of `core::arch` lists, given as `(file,
/// text)` pairs; an error when the files do not list the same.
fn portable_items_agreed(files: &[(&str, &str)]) -> Result<Vec<String>, String> {
    let mut agreed: Option<(&str, Vec<String>)> = None;
    for (file, text) in files {
        let items = portable_items(file, text)?;
        match &agreed {
            None => agreed = Some((file, items)),
            Some((first, expected)) if *expected != items => {
                return Err(format!(
                    "{file} lists {items:?} as portable, {first} lists {expected:?}"
                ));
            }
            Some(_) => {}
        }
    }
    agreed
        .map(|(_, items)| items)
        .ok_or_else(|| "no file of `core::arch`".to_owned())
}

/// The portable items `docs/reference.md` lists (ADR 0080): in its section
/// "`core`", each bullet that starts with a declaration in backticks, as
/// ``- `pub fn fence()`: ...``, is one.
fn reference_portable_items(reference: &str) -> Vec<String> {
    let section = reference
        .split("\n## `core` (ADR 0020, 0052)\n")
        .nth(1)
        .and_then(|rest| rest.split("\n## ").next())
        .unwrap_or("");
    section
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("- `pub "))
        .filter_map(|rest| rest.split('`').next())
        .map(|declaration| format!("pub {declaration}"))
        .collect()
}

/// The call of a portable item in a program: the items of `core::arch` that
/// have no parameter, as the fences and the interrupt functions do, and return
/// nothing or a value, which the program binds to a local named after the item. A later item with
/// another signature needs arguments here, and the test says so.
fn call_of(item: &str) -> Result<String, String> {
    let signature = item.strip_prefix("pub fn ").and_then(|rest| {
        let (name, returned) = rest.split_once("()")?;
        let returned = match returned {
            "" => None,
            _ => Some(returned.strip_prefix(" -> ")?),
        };
        Some((name, returned))
    });
    match signature {
        Some((name, returned))
            if !name.is_empty()
                && name.chars().all(|c| c.is_alphanumeric() || c == '_')
                && returned.is_none_or(|ty| {
                    !ty.is_empty() && ty.chars().all(|c| c.is_alphanumeric() || c == '_')
                }) =>
        {
            Ok(match returned {
                None => format!("    core::arch::{name}();\n"),
                Some(ty) => format!("    let {name}_result: {ty} = core::arch::{name}();\n"),
            })
        }
        _ => Err(format!(
            "`{item}` is not `pub fn name()` or `pub fn name() -> type`: teach `call_of` to call it"
        )),
    }
}

/// Every file of `core::arch` lists the same portable items, each declared as
/// listed, and a program calling all of them builds for every target
/// (ADR 0080), at both levels. The list is read from the files' headers, so a
/// new portable item needs no change here; one missing from a file, or with
/// another signature, fails. `docs/reference.md` must list each of them in
/// its section "`core`".
#[test]
fn portable_core_arch() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let texts: Vec<(&str, String)> = ARCH_FILES
        .iter()
        .map(|file| {
            let text = fs::read_to_string(root.join(file)).unwrap();
            (*file, text.replace("\r\n", "\n"))
        })
        .collect();
    let files: Vec<(&str, &str)> = texts.iter().map(|(f, t)| (*f, t.as_str())).collect();
    let items = portable_items_agreed(&files).unwrap();
    assert!(
        items.contains(&"pub fn fence()".to_owned())
            && items.contains(&"pub fn compiler_fence()".to_owned()),
        "the fences of ADR 0081 are portable: {items:?}"
    );

    let reference = fs::read_to_string(root.join("docs/reference.md"))
        .unwrap()
        .replace("\r\n", "\n");
    let listed = reference_portable_items(&reference);
    for item in &items {
        assert!(
            listed.contains(item),
            "`{item}` is portable but docs/reference.md does not list it (it lists {listed:?})"
        );
    }

    let calls: String = items.iter().map(|item| call_of(item).unwrap()).collect();
    let source = running(&calls, "");
    for target in board::RUN_ON {
        for opt in [OptLevel::None, OptLevel::Speed] {
            let output = build_by_command_line("portable_core_arch", &source, target, opt);
            assert!(
                output.status.success(),
                "[{}] {opt:?}:\n{}",
                target.name,
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

/// The agreement test fails on what it is meant to catch: a file that lists
/// another set, one that lists an item it does not declare or declares with
/// another signature, and one that lists none.
#[test]
fn portable_items_are_checked_against_the_files() {
    let fence = "// portable: pub fn fence()\npub fn fence() {\n}\n";
    let both = "// portable: pub fn fence()\n// portable: pub fn pause()\n\
                pub fn fence() {\n}\npub fn pause() {\n}\n";
    assert_eq!(
        portable_items_agreed(&[("a", both), ("b", both)]).unwrap(),
        ["pub fn fence()", "pub fn pause()"]
    );

    let error = portable_items_agreed(&[("a", both), ("b", fence)]).unwrap_err();
    assert!(
        error.starts_with("b lists") && error.contains("a lists"),
        "{error}"
    );

    // Listed, but declared with another signature.
    let other = "// portable: pub fn fence()\npub fn fence(x: u32) {\n}\n";
    let error = portable_items_agreed(&[("a", fence), ("b", other)]).unwrap_err();
    assert_eq!(error, "b: no line `pub fn fence() {`");

    // Listed, but not declared.
    let missing = "// portable: pub fn fence()\npub fn other() {\n}\n";
    assert!(portable_items_agreed(&[("a", missing)]).is_err());

    let none = "pub fn fence() {\n}\n";
    assert_eq!(
        portable_items_agreed(&[("a", none)]).unwrap_err(),
        "a: lists no portable item"
    );
    assert!(portable_items_agreed(&[]).is_err());

    // The reference's list is read from the bullets of its section "`core`":
    // an item outside that section, or not a declaration, is not one.
    let reference = "## Other\n\n- `pub fn elsewhere()`: not in `core`.\n\n\
                     ## `core` (ADR 0020, 0052)\n\n\
                     - `core` holds:\n  - `pub fn fence()`: a fence.\n\
                     - `pub fn pause()`: another.\n\n\
                     ## Next\n\n- `pub fn after()`: no.\n";
    assert_eq!(
        reference_portable_items(reference),
        ["pub fn fence()", "pub fn pause()"]
    );
    assert!(reference_portable_items("no section").is_empty());

    assert_eq!(
        call_of("pub fn fence()").unwrap(),
        "    core::arch::fence();\n"
    );
    assert!(call_of("pub fn wait(n: u32)").is_err());
    assert_eq!(
        call_of("pub fn id() -> u32").unwrap(),
        "    let id_result: u32 = core::arch::id();\n"
    );
    assert!(call_of("pub fn id() -> &u32").is_err());
    assert!(call_of("pub fn id() u32").is_err());
    assert!(call_of("pub fn id(x: u32) -> u32").is_err());
}
