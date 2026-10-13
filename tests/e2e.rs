//! End-to-end tests: every `tests/e2e/*.ers` is compiled to an ELF and booted
//! on QEMU `virt` (ADR 0010).
//!
//! A program states what it should do in header comments:
//!
//! ```text
//! // expect-output: hi        (one line of UART output; may repeat)
//! // expect-exit: 0           (exit code written to the test finisher)
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

use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use erebos::driver::OptLevel;

/// How long a program that never exits is watched.
const RUN_FOREVER_CHECK: Duration = Duration::from_millis(1500);
/// How long a program that should exit gets to do so.
const EXIT_TIMEOUT: Duration = Duration::from_secs(10);

#[test]
fn e2e() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e");
    let mut programs: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "ers"))
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

struct Expectation {
    output: String,
    exit: Option<i32>,
}

fn expectation(source: &str) -> Expectation {
    let mut output = String::new();
    let mut exit = None;
    for line in source.lines() {
        if let Some(text) = line.strip_prefix("// expect-output:") {
            output.push_str(text.strip_prefix(' ').unwrap_or(text));
            output.push('\n');
        } else if let Some(code) = line.strip_prefix("// expect-exit:") {
            exit = Some(code.trim().parse().expect("expect-exit takes an integer"));
        }
    }
    Expectation { output, exit }
}

fn run(program: &Path, opt: OptLevel) -> Result<(), String> {
    let stem = program.file_stem().unwrap().to_string_lossy();
    let name = match opt {
        OptLevel::None => stem.to_string(),
        OptLevel::Speed => format!("{stem} (-O)"),
    };
    let file = match opt {
        OptLevel::None => stem.to_string(),
        OptLevel::Speed => format!("{stem}.opt"),
    };
    let expected = expectation(&fs::read_to_string(program).unwrap());
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("e2e");
    fs::create_dir_all(&work).unwrap();
    let elf = work.join(format!("{file}.elf"));
    let log = work.join(format!("{file}.qemu.log"));
    let _ = fs::remove_file(&log);

    erebos::driver::build(program, &elf, opt, false)
        .map_err(|e| format!("{name}: build failed\n{e}"))?;

    let mut qemu = Command::new(qemu_path())
        .args(["-machine", "virt", "-nographic", "-bios", "none"])
        .arg("-kernel")
        .arg(&elf)
        .args(["-d", "int,guest_errors", "-D"])
        .arg(&log)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{name}: cannot start QEMU ({e}); install it or set EREBOS_QEMU"))?;

    // Drain the UART output while QEMU runs, so a chatty program never
    // blocks on a full pipe.
    let mut stdout = qemu.stdout.take().unwrap();
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });

    let wait = if expected.exit.is_some() {
        EXIT_TIMEOUT
    } else {
        RUN_FOREVER_CHECK
    };
    let start = Instant::now();
    let mut status = None;
    while start.elapsed() < wait {
        status = qemu.try_wait().unwrap();
        if status.is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    if status.is_none() {
        qemu.kill().unwrap();
    }
    qemu.wait().unwrap();
    let output = String::from_utf8_lossy(&reader.join().unwrap()).replace("\r\n", "\n");

    let events = fs::read_to_string(&log).unwrap_or_default();
    if !events.is_empty() {
        let first: Vec<&str> = events.lines().take(5).collect();
        return Err(format!(
            "{name}: the CPU raised exceptions or guest errors:\n{}",
            first.join("\n")
        ));
    }

    match (expected.exit, status.map(|s| s.code())) {
        (None, None) => {}
        (None, Some(code)) => {
            return Err(format!(
                "{name}: exited ({code:?}) but has no `// expect-exit:`, so it should run forever"
            ));
        }
        (Some(_), None) => {
            return Err(format!(
                "{name}: did not exit within {EXIT_TIMEOUT:?}\noutput so far:\n{output}"
            ));
        }
        (Some(want), Some(got)) if got != Some(want) => {
            return Err(format!(
                "{name}: exit code {got:?}, expected {want}\noutput:\n{output}"
            ));
        }
        (Some(_), Some(_)) => {}
    }

    if output != expected.output {
        return Err(format!(
            "{name}: wrong output\n--- expected ---\n{}--- got ---\n{output}",
            expected.output
        ));
    }
    Ok(())
}

/// `EREBOS_QEMU` wins; otherwise `qemu-system-riscv32` from `PATH`.
fn qemu_path() -> PathBuf {
    env::var_os("EREBOS_QEMU")
        .map(PathBuf::from)
        .unwrap_or_else(|| "qemu-system-riscv32".into())
}
