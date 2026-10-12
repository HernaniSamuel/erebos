//! End-to-end tests: every `tests/e2e/*.ers` is compiled to an ELF and booted
//! on QEMU `virt` (ADR 0010).
//!
//! M0 programs never exit, so a program passes if, after running for a
//! while, QEMU is still up and logged no exception or guest error. A
//! miscompiled program faults almost immediately and floods that log.
//! Once programs can write to the UART and the test finisher (M1), this
//! becomes "check the output and the exit code".

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

const RUN_TIME: Duration = Duration::from_millis(1500);

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
        .filter_map(|program| run(program).err())
        .collect();
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

fn run(program: &Path) -> Result<(), String> {
    let name = program.file_stem().unwrap().to_string_lossy();
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("e2e");
    fs::create_dir_all(&work).unwrap();
    let elf = work.join(format!("{name}.elf"));
    let log = work.join(format!("{name}.qemu.log"));
    let _ = fs::remove_file(&log);

    erebos::driver::build(program, &elf, false)
        .map_err(|e| format!("{name}: build failed\n{e}"))?;

    let mut qemu = Command::new(qemu_path())
        .args(["-machine", "virt", "-nographic", "-bios", "none"])
        .arg("-kernel")
        .arg(&elf)
        .args(["-d", "int,guest_errors", "-D"])
        .arg(&log)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{name}: cannot start QEMU ({e}); install it or set EREBOS_QEMU"))?;

    thread::sleep(RUN_TIME);
    let exited = qemu.try_wait().unwrap();
    if exited.is_none() {
        qemu.kill().unwrap();
    }
    let output = qemu.wait_with_output().unwrap();

    if let Some(status) = exited {
        return Err(format!(
            "{name}: QEMU exited early ({status}), but M0 programs never return\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let events = fs::read_to_string(&log).unwrap_or_default();
    if !events.is_empty() {
        let first: Vec<&str> = events.lines().take(5).collect();
        return Err(format!(
            "{name}: the CPU raised exceptions or guest errors:\n{}",
            first.join("\n")
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
