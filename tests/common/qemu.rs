//! Running a program on QEMU (ADR 0010, 0077), shared by `tests/e2e.rs`
//! and the generated programs (ADR 0068).

use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use erebos::driver::OptLevel;
use erebos::target::{Arch, Target};

/// How long a program that never exits is watched.
pub const RUN_FOREVER_CHECK: Duration = Duration::from_millis(1500);
/// How long a program that should exit gets to do so.
pub const EXIT_TIMEOUT: Duration = Duration::from_secs(10);

/// What a program says it should do, from the header comments of its root
/// file (see `tests/e2e.rs`).
pub struct Expectation {
    /// The targets the program runs on: those of its `// targets:` header,
    /// or every target.
    pub targets: Vec<&'static Target>,
    pub output: String,
    pub exit: Option<i32>,
    pub link_c: Vec<String>,
    pub c_flags: Vec<String>,
}

pub fn expectation(source: &str) -> Result<Expectation, String> {
    let mut targets = Vec::new();
    let mut output = String::new();
    let mut exit = None;
    let mut link_c = Vec::new();
    let mut c_flags = Vec::new();
    for line in source.lines() {
        if let Some(names) = line.strip_prefix("// targets:") {
            targets.extend(super::board::targets_named(names)?);
        }
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
    if targets.is_empty() {
        targets = super::board::targets_named("")?;
    }
    Ok(Expectation {
        targets,
        output,
        exit,
        link_c,
        c_flags,
    })
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

/// Boots `elf` on the QEMU machine of `target`, logging events to `log`,
/// and waits `wait` for it to stop; a program still running then is
/// killed. `name` labels errors.
pub fn boot(
    name: &str,
    target: &Target,
    elf: &Path,
    log: &Path,
    wait: Duration,
) -> Result<Run, String> {
    let _ = fs::remove_file(log);
    let machine: &[&str] = match target.arch {
        Arch::Riscv32 => &["-machine", "virt", "-nographic", "-bios", "none"],
        // A Cortex-M3 that stops QEMU through semihosting (ADR 0074, 0077).
        Arch::Armv7m => &[
            "-machine",
            "mps2-an385",
            "-nographic",
            "-semihosting-config",
            "enable=on,target=native",
        ],
    };
    let mut qemu = Command::new(qemu_path(target))
        .args(machine)
        .arg("-kernel")
        .arg(elf)
        .args(["-d", "int,guest_errors", "-D"])
        .arg(log)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            format!(
                "{name}: cannot start QEMU ({e}); install it or set {}",
                qemu_variable(target)
            )
        })?;

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
    let events = expected_events_removed(target, &fs::read_to_string(log).unwrap_or_default());
    Ok(Run {
        status,
        output,
        events,
    })
}

/// The event log of a correct run, without the lines that QEMU writes for
/// it on `target` (ADR 0077). On `mps2-an385`, QEMU 11 logs under `-d int`
/// the reset vector it loads (twice per boot) and each semihosting request;
/// these are the only lines removed, each matched from the start of the
/// line, and every other line, including any other `Taking exception`,
/// stays. riscv32's log must be empty as it is.
pub fn expected_events_removed(target: &Target, log: &str) -> String {
    match target.arch {
        Arch::Riscv32 => log.to_owned(),
        Arch::Armv7m => log
            .lines()
            .filter(|line| !is_armv7m_event_of_a_correct_run(line))
            .flat_map(|line| [line, "\n"])
            .collect(),
    }
}

/// The three kinds of line of ADR 0077's table.
fn is_armv7m_event_of_a_correct_run(line: &str) -> bool {
    line.strip_prefix("Loaded reset SP 0x")
        .is_some_and(is_reset_vector_tail)
        || line.starts_with("Taking exception 16 [Semihosting call]")
        || line.starts_with("...handling as semihosting call 0x")
}

/// `<hex> PC 0x<hex> from vector table`, what follows `Loaded reset SP 0x`.
fn is_reset_vector_tail(tail: &str) -> bool {
    let hex =
        |text: &str| -> bool { !text.is_empty() && text.bytes().all(|b| b.is_ascii_hexdigit()) };
    tail.strip_suffix(" from vector table")
        .and_then(|rest| rest.split_once(" PC 0x"))
        .is_some_and(|(sp, pc)| hex(sp) && hex(pc))
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

/// Compiles a C file for `target` with clang, optimized like the program,
/// with the program's extra `flags`.
pub fn compile_c(
    source: &Path,
    flags: &[String],
    work: &Path,
    target: &Target,
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
        .args(super::clang_target(target))
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

/// Compiles `target`'s board C file, if its board has one (ADR 0077), into
/// `work`, for the program to link: `None` when it has none.
pub fn board_object(
    target: &Target,
    work: &Path,
    opt: OptLevel,
) -> Result<Option<PathBuf>, String> {
    super::board::c_file(target)
        .map(|source| compile_c(&source, &[], work, target, opt))
        .transpose()
}

/// The environment variable that names `target`'s QEMU.
fn qemu_variable(target: &Target) -> &'static str {
    match target.arch {
        Arch::Riscv32 => "EREBOS_QEMU",
        Arch::Armv7m => "EREBOS_QEMU_ARM",
    }
}

/// The variable of `qemu_variable` wins; otherwise the system emulator of
/// the architecture from `PATH`.
fn qemu_path(target: &Target) -> PathBuf {
    let default = match target.arch {
        Arch::Riscv32 => "qemu-system-riscv32",
        Arch::Armv7m => "qemu-system-arm",
    };
    env::var_os(qemu_variable(target))
        .map(PathBuf::from)
        .unwrap_or_else(|| default.into())
}
