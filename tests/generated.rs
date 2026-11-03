//! Generated programs (ADR 0068): the test of the generator itself, and the
//! test that runs the programs against their two oracles.
//!
//! Every program calls each of its functions at compile time (`comptime`,
//! the evaluator) and at run time (LLVM, on QEMU), unoptimized and with
//! `-O`, on every target, and stops with exit code 0 and `generated ok`
//! when they agree. A program reaches its machine through `mod board;`: the
//! board of each target (ADR 0077) is written beside it as `board.ers`.
//!
//! - `EREBOS_GENERATED_PROGRAMS`: how many programs to run (default
//!   `DEFAULT_PROGRAMS`).
//! - `EREBOS_GENERATED_SEED`: the seed of the first one (default 0).

mod common;

use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use common::board;
use common::generator::{self, Feature, Uses};
use common::qemu::{self, Expectation};
use erebos::driver::{self, OptLevel, Stage};
use erebos::target;

/// How many seeds the test of the generator compiles.
const SEEDS: u64 = 300;

/// How many programs `generated_programs_agree` runs without
/// `EREBOS_GENERATED_PROGRAMS`.
const DEFAULT_PROGRAMS: u64 = 20;

/// How long a program gets to finish: far more than the 16 iterations of a
/// loop need, but short enough that a hang is reported.
const WAIT: Duration = Duration::from_secs(10);

fn work_dir() -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("generated");
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// The opaque inputs, compiled once per optimization level and shared by
/// every program (and by both tests).
fn opaque_object(opt: OptLevel) -> Result<PathBuf, String> {
    static OBJECTS: OnceLock<[Result<PathBuf, String>; 2]> = OnceLock::new();
    let objects = OBJECTS.get_or_init(|| {
        let work = work_dir();
        let source = work.join("opaque.c");
        fs::write(&source, generator::OPAQUE_C).unwrap();
        [OptLevel::None, OptLevel::Speed]
            .map(|opt| qemu::compile_c(&source, &[], &work, target::DEFAULT, opt))
    });
    match opt {
        OptLevel::None => objects[0].clone(),
        OptLevel::Speed => objects[1].clone(),
    }
}

/// Builds `source` (named `name`) at both levels for every target and runs
/// each on QEMU. The error is every disagreement or failure, one per
/// target and level. The files stay in the work directory only when
/// something failed.
fn run_both(name: &str, source: &str) -> Result<(), String> {
    let work = work_dir().join(name);
    fs::create_dir_all(&work).unwrap();
    let program = work.join(format!("{name}.ers"));
    fs::write(&program, source).unwrap();
    let expected = Expectation {
        targets: board::RUN_ON.to_vec(),
        output: "generated ok\n".into(),
        exit: Some(0),
        link_c: Vec::new(),
        c_flags: Vec::new(),
    };

    let mut failures = Vec::new();
    for &target in &expected.targets {
        fs::write(work.join("board.ers"), board::source(target)).unwrap();
        for (opt, label, file) in [
            (OptLevel::None, format!("{name} [{}]", target.name), "unopt"),
            (
                OptLevel::Speed,
                format!("{name} [{}] (-O)", target.name),
                "opt",
            ),
        ] {
            let elf = work.join(format!("{}.{file}.elf", target.name));
            let log = work.join(format!("{}.{file}.qemu.log", target.name));
            let result = opaque_object(opt).and_then(|object| {
                driver::build(&program, &elf, &[object], target, opt, false)
                    .map_err(|e| format!("{label}: build failed\n{e}"))?;
                let run = qemu::boot(&label, target, &elf, &log, WAIT)?;
                qemu::check(&label, &expected, &run)
            });
            if let Err(e) = result {
                failures.push(e);
            }
        }
    }
    if failures.is_empty() {
        let _ = fs::remove_dir_all(&work);
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}

fn env_number(var: &str, default: u64) -> u64 {
    match env::var(var) {
        Ok(text) => text
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("{var} must be a number, got `{text}`")),
        Err(_) => default,
    }
}

#[test]
fn generated_programs_agree() {
    let count = env_number("EREBOS_GENERATED_PROGRAMS", DEFAULT_PROGRAMS);
    let first = env_number("EREBOS_GENERATED_SEED", 0);
    let mut failures = Vec::new();
    for seed in first..first + count {
        let program = generator::generate(seed);
        if let Err(e) = run_both(&format!("seed-{seed}"), &program.source) {
            let dir = work_dir().join(format!("seed-{seed}"));
            failures.push(format!(
                "generated program of seed {seed} disagrees with its oracles:\n{e}\n\
                 source kept in {}\n\
                 run it again: EREBOS_GENERATED_SEED={seed} EREBOS_GENERATED_PROGRAMS=1 \
                 cargo test --test generated generated_programs_agree",
                dir.display()
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

/// The oracle can fail: the `comptime` result of a program changed by hand
/// is reported as the disagreement of the first function.
#[test]
fn a_changed_comptime_result_is_a_disagreement() {
    let program = generator::generate(1);
    let line = "const CT_0_0: u64 = ";
    let start = program.source.find(line).expect("the first comptime const") + line.len();
    let end = start + program.source[start..].find(";\n").unwrap();
    let mut source = program.source.clone();
    source.replace_range(
        start..end,
        &format!("({}) ^ 1", &program.source[start..end]),
    );

    let error = run_both("tampered", &source).expect_err("the changed program must disagree");
    // Exit code 1 is the first function; the log line names both values.
    assert!(
        error.contains("exit code Some(1), expected 0") && error.contains("#1 "),
        "unexpected report:\n{error}"
    );
    // Both levels report it.
    let target = erebos::target::DEFAULT.name;
    assert!(
        error.contains(&format!("tampered [{target}]:"))
            && error.contains(&format!("tampered [{target}] (-O):"))
    );
    let _ = fs::remove_dir_all(work_dir().join("tampered"));
}

#[test]
fn generator_makes_valid_programs() {
    assert_eq!(Feature::ALL.len(), Feature::COUNT);

    let mut uses = Uses::default();
    let mut sources = HashSet::new();
    for seed in 0..SEEDS {
        let program = generator::generate(seed);

        // The same seed gives the same text.
        assert_eq!(
            program.source,
            generator::generate(seed).source,
            "seed {seed} gave two different programs"
        );
        sources.insert(program.source.clone());

        if let Err(rendered) = driver::dump(
            Path::new("generated.ers"),
            &board::with_board(Path::new("generated.ers"), &program.source),
            Stage::Mir,
            erebos::target::DEFAULT,
            OptLevel::None,
            false,
        ) {
            panic!(
                "the generated program of seed {seed} is not valid:\n{rendered}\n--- program ---\n{}",
                program.source
            );
        }
        uses = uses.union(program.uses);
    }
    assert!(
        sources.len() as u64 > SEEDS * 9 / 10,
        "the seeds gave only {} different programs",
        sources.len()
    );

    // Every operator, type and construct of ADR 0068, section 1, is in at
    // least one program of the run.
    let missing: Vec<_> = Feature::ALL.iter().filter(|f| !uses.has(**f)).collect();
    assert!(
        missing.is_empty(),
        "no program of {SEEDS} seeds uses: {missing:?}"
    );
}
