//! Running a program on QEMU `virt` (ADR 0010), shared by `tests/e2e.rs`
//! and the generated programs (ADR 0068).

use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use erebos::driver::OptLevel;

/// How long a program that never exits is watched.
pub const RUN_FOREVER_CHECK: Duration = Duration::from_millis(1500);
/// How long a program that should exit gets to do so.
pub const EXIT_TIMEOUT: Duration = Duration::from_secs(10);

/// What a program says it should do, from the header comments of its root
/// file (see `tests/e2e.rs`).
pub struct Expectation {
    pub output: String,
    pub exit: Option<i32>,
    pub link_c: Vec<String>,
    pub c_flags: Vec<String>,
}

pub fn expectation(source: &str) -> Expectation {
    let mut output = String::new();
    let mut exit = None;
    let mut link_c = Vec::new();
    let mut c_flags = Vec::new();
    for line in source.lines() {
        if let Some(file) = line.strip_prefix("// link-c:") {
            link_c.push(file.trim().to_owned());
        }
        if let Some(flags) = line.strip_prefix("// c-flags:") {
            c_flags.extend(flags.split_whitespace().map(str::to_owned));
        }
        if let Some(text) = line.strip_prefix("// expect-output:") {
            output.push_str(text.strip_prefix(' ').unwrap_or(text));
            output.push('\n');
        } else if let Some(code) = line.strip_prefix("// expect-exit:") {
            exit = Some(code.trim().parse().expect("expect-exit takes an integer"));
        }
    }
    Expectation {
        output,
        exit,
        link_c,
        c_flags,
    }
}

/// What came out of one boot of a program.
pub struct Run {
    /// The exit status, or `None` when QEMU was still up after the wait
    /// and was killed.
    pub status: Option<ExitStatus>,
    /// The UART output, with `\n` line ends.
    pub output: String,
    /// The CPU exceptions and guest errors QEMU logged; empty when none.
    pub events: String,
}

/// Boots `elf` on QEMU, logging events to `log`, and waits `wait` for it
/// to stop; a program still running then is killed. `name` labels errors.
pub fn boot(name: &str, elf: &Path, log: &Path, wait: Duration) -> Result<Run, String> {
    let _ = fs::remove_file(log);
    let mut qemu = Command::new(qemu_path())
        .args(["-machine", "virt", "-nographic", "-bios", "none"])
        .arg("-kernel")
        .arg(elf)
        .args(["-d", "int,guest_errors", "-D"])
        .arg(log)
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
    let events = fs::read_to_string(log).unwrap_or_default();
    Ok(Run {
        status,
        output,
        events,
    })
}

/// Checks a boot against what the program expected.
pub fn check(name: &str, expected: &Expectation, run: &Run) -> Result<(), String> {
    let output = &run.output;
    if !run.events.is_empty() {
        let first: Vec<&str> = run.events.lines().take(5).collect();
        return Err(format!(
            "{name}: the CPU raised exceptions or guest errors:\n{}",
            first.join("\n")
        ));
    }

    match (expected.exit, run.status.map(|s| s.code())) {
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

    if *output != expected.output {
        return Err(format!(
            "{name}: wrong output\n--- expected ---\n{}--- got ---\n{output}",
            expected.output
        ));
    }
    Ok(())
}

/// Compiles a C file for the target with clang, optimized like the program,
/// with the program's extra `flags`.
pub fn compile_c(
    source: &Path,
    flags: &[String],
    work: &Path,
    opt: OptLevel,
) -> Result<PathBuf, String> {
    let stem = source.file_stem().unwrap().to_string_lossy();
    let (level, suffix) = match opt {
        OptLevel::None => ("-O0", ""),
        OptLevel::Speed => ("-O2", ".opt"),
    };
    let object = work.join(format!("{stem}{suffix}.c.o"));
    let clang = super::clang_path();
    let output = Command::new(&clang)
        .args(super::CLANG_TARGET)
        .args(["-std=c11", "-Wall", "-Werror", "-c", level])
        .args(flags)
        .arg(source)
        .arg("-o")
        .arg(&object)
        .output()
        .map_err(|e| {
            format!(
                "cannot run `{}` ({e}); install clang or set EREBOS_CLANG",
                clang.display()
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "clang failed on {}:\n{}",
            source.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(object)
}

/// `EREBOS_QEMU` wins; otherwise `qemu-system-riscv32` from `PATH`.
fn qemu_path() -> PathBuf {
    env::var_os("EREBOS_QEMU")
        .map(PathBuf::from)
        .unwrap_or_else(|| "qemu-system-riscv32".into())
}
