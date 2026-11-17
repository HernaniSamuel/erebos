//! The oracles of the stack bound (ADR 0123, "Tests"). The bound is computed
//! from MIR and the frames LLVM measured, with assumptions about the calls MIR
//! does not show; these tests check it against the machine.
//!
//! - **The machine's calls**: every program of `tests/e2e` is built on both
//!   targets at both levels, and `llvm-objdump -d` of the executable is read.
//!   Every call, and every jump to another function, of a function that the
//!   report of `--emit=stack` lists must be one of the bodies its line says it
//!   reaches, or leave a function with `#[stack(N)]` whose number covers it. A
//!   function of the executable that is not in the report must be C's or a
//!   default handler.
//! - **The measured stack**: the programs `stack_measured*.ers` fill their stack
//!   with a pattern, run a deep path with a timer's handler preempting it, end
//!   it in a failed check or a fault, and print how many bytes of the pattern
//!   they overwrote. The number is at most the worst case the report gives and
//!   at least the 512-byte array on the path.

mod common;

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use common::board;
use common::qemu::{self, EXIT_TIMEOUT, Expectation};
use erebos::driver::{self, OptLevel, Stage};
use erebos::target::{Arch, Target};

/// The reset entry: a program that jumps to it from `asm` restarts.
const RESTART: &str = "_start";

/// The size of the array the measured programs hold on their path.
const BUFFER: u64 = 512;

/// A program of `tests/e2e` built for one target at one level.
struct Built {
    /// `name [target]` or `name [target] (-O)`, for messages.
    label: String,
    elf: PathBuf,
    /// The text of `--emit=stack`.
    report: String,
    /// The object files C contributed: the functions of the executable that
    /// no report line names, with the board's.
    objects: Vec<PathBuf>,
}

/// Builds `program` for `target` at `opt`, with its own C files, and takes its
/// report. The files are in a work directory of their own, `stack/<group>/`, so
/// that programs may be built in parallel and so that the tests, which cargo
/// runs in parallel, never build the same path (`board::stage` removes its
/// directory first).
fn build(
    group: &str,
    program: &Path,
    expected: &Expectation,
    target: &'static Target,
    opt: OptLevel,
) -> Result<Built, String> {
    let stem = board::program_name(program);
    let (label, file) = match opt {
        OptLevel::None => (format!("{stem} [{}]", target.name), stem.clone()),
        OptLevel::Speed => (
            format!("{stem} [{}] (-O)", target.name),
            format!("{stem}.opt"),
        ),
    };
    let work = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("stack")
        .join(group)
        .join(target.name);
    let own = work.join(&stem);
    fs::create_dir_all(&own).unwrap();
    let elf = work.join(format!("{file}.elf"));

    let mut objects = expected
        .link_c
        .iter()
        .map(|file| {
            qemu::compile_c(
                &program.with_file_name(file),
                &expected.c_flags,
                &own,
                target,
                opt,
            )
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("{label}: {e}"))?;
    objects.extend(qemu::board_object(target, &own, opt).map_err(|e| format!("{label}: {e}"))?);
    let staged = board::stage(program, target, &work.join("programs"))
        .map_err(|e| format!("{label}: {e}"))?;
    driver::build(&staged, &elf, &objects, target, opt, false)
        .map_err(|e| format!("{label}: build failed\n{e}"))?;
    let text = fs::read_to_string(&staged).unwrap();
    let report = driver::dump(&staged, &text, Stage::Stack, target, opt, false)
        .map_err(|e| format!("{label}: --emit=stack failed\n{e}"))?;
    Ok(Built {
        label,
        elf,
        report,
        objects,
    })
}

// ---------------------------------------------------------------- the report

/// One body line of the report (ADR 0123, "The report").
#[derive(Debug, Default)]
struct Body {
    frame: u64,
    /// The `N` of its `#[stack(N)]`.
    stack: Option<u64>,
    /// `None` when the report says `unknown`.
    depth: Option<u64>,
    /// The bodies its edges reach, each once, the inserted calls left out.
    calls: Vec<String>,
    /// Whether it has the inserted edges.
    inserted: bool,
}

#[derive(Debug)]
struct Report {
    /// The worst case, in bytes; `None` when the report says `unknown`.
    total: Option<u64>,
    /// The bodies by symbol.
    bodies: BTreeMap<String, Body>,
}

impl Report {
    fn parse(text: &str) -> Result<Report, String> {
        let first = text.lines().next().unwrap_or_default();
        let total = first
            .strip_prefix("worst case: ")
            .ok_or_else(|| format!("no `worst case:` line in the report:\n{text}"))?;
        let total = total.split(' ').next().unwrap().parse::<u64>().ok();
        let list = text
            .split_once("\nbodies:\n")
            .ok_or_else(|| format!("no `bodies:` in the report:\n{text}"))?
            .1;
        let mut bodies = BTreeMap::new();
        for line in list.lines() {
            let (symbol, rest) = line
                .split_once(": ")
                .ok_or_else(|| format!("a body line without `: `: {line}"))?;
            let (fields, calls) = match rest.split_once("; calls ") {
                Some((fields, calls)) => (fields, Some(calls)),
                None => (rest, None),
            };
            let mut body = Body::default();
            for field in fields.split(", ") {
                if let Some(n) = field.strip_prefix("frame ") {
                    body.frame = n.parse().map_err(|_| format!("bad frame in: {line}"))?;
                } else if let Some(n) = field.strip_prefix("stack ") {
                    body.stack = Some(n.parse().map_err(|_| format!("bad stack in: {line}"))?);
                } else if let Some(n) = field.strip_prefix("depth ") {
                    body.depth = n.parse().ok();
                } else if field != "not reached" {
                    return Err(format!("unknown field `{field}` in: {line}"));
                }
            }
            for call in calls.into_iter().flat_map(|calls| calls.split(", ")) {
                if call == "inserted calls" {
                    body.inserted = true;
                } else {
                    body.calls.push(call.to_owned());
                }
            }
            bodies.insert(symbol.to_owned(), body);
        }
        Ok(Report { total, bodies })
    }

    /// What `from` may call: its edges, and the inserted functions when it has
    /// the inserted edges. With `transitive`, also what those reach, which an
    /// optimized function may call after inlining a callee.
    fn reaches(&self, from: &str, inserted: &[String], transitive: bool) -> HashSet<String> {
        let mut found = HashSet::new();
        let mut work = vec![from.to_owned()];
        while let Some(name) = work.pop() {
            let Some(body) = self.bodies.get(&name) else {
                continue;
            };
            let targets = body.calls.iter().chain(
                body.inserted
                    .then_some(inserted)
                    .into_iter()
                    .flat_map(|list| list.iter()),
            );
            for target in targets {
                if found.insert(target.clone()) && transitive {
                    work.push(target.clone());
                }
            }
        }
        found
    }
}

/// The inserted functions (ADR 0123): the `#[export]` functions of `core` other
/// than `_start`, read from the source of `core`, which is where
/// `Mir::inserted` takes them. The oracle does not see MIR, so it checks the
/// report's depths agree: every body with the inserted edges is at least as deep
/// as any of them.
fn inserted_functions(target: &Target) -> Vec<String> {
    let arch = match target.arch {
        Arch::Riscv32 => "arch/riscv32.ers",
        Arch::Armv7m => "arch/armv7m.ers",
    };
    let mut names = Vec::new();
    for file in ["builtins.ers", arch] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("rt").join(file);
        let text = fs::read_to_string(&path).unwrap();
        let mut exported = false;
        for line in text.lines().map(str::trim) {
            if line == "#[export]" {
                exported = true;
            } else if exported && !line.starts_with("//") {
                // The first `fn` after the attribute, past `#[naked]` and
                // `#[stack(N)]` and the qualifiers.
                if let Some((_, rest)) = line.split_once("fn ") {
                    let name = rest.split('(').next().unwrap().trim();
                    if name != "_start" {
                        names.push(name.to_owned());
                    }
                    exported = false;
                }
            }
        }
    }
    names
}

/// `main`-like consistency between the inserted functions read from `core` and
/// the report: each is a body, and a body with the inserted edges is at least
/// as deep as its own cost plus any of them. A set smaller than the compiler's
/// passes; one larger than it, or the compiler's missing one of them, fails.
fn check_inserted(report: &Report, inserted: &[String]) -> Vec<String> {
    let mut failures = Vec::new();
    for name in inserted {
        let Some(function) = report.bodies.get(name) else {
            failures.push(format!(
                "the inserted function `{name}` is not in the report"
            ));
            continue;
        };
        let Some(depth) = function.depth else {
            continue;
        };
        for (symbol, body) in &report.bodies {
            let cost = body.frame + body.stack.unwrap_or(0);
            if body.inserted && body.depth.is_some_and(|d| d < cost + depth) {
                failures.push(format!(
                    "`{symbol}` has the inserted edges but is not as deep as its cost {cost} \
                     plus `{name}`'s {depth}"
                ));
            }
        }
    }
    failures
}

// -------------------------------------------------------------- the machine

/// A call or a jump from one function to another, read from `llvm-objdump -d`.
#[derive(Debug, PartialEq)]
struct Transfer {
    from: String,
    to: String,
}

/// What `llvm-objdump -d` showed of an executable.
#[derive(Debug, Default)]
struct Machine {
    /// Every function, by the symbol of its header.
    functions: Vec<String>,
    /// The calls and jumps to another function that name their target.
    transfers: Vec<Transfer>,
}

/// Reads the disassembly: `<name>:` headers, and instructions that transfer
/// control (`j`, `jal`, `jr`, `jalr`, and `b`, `bl`, `bx`, `blx`, `cbz` and the
/// conditional branches) with a `<name+off>` after the operands. A target
/// that is a local label (`.Lpcrel_hi4`) is placed by its address. A transfer
/// within a function, or through a register without a name, is not one.
/// `80000024 <main>:` is the address and the name of a header.
fn header(line: &str) -> Option<(u64, &str)> {
    let (address, name) = line.strip_suffix(">:")?.split_once(" <")?;
    Some((u64::from_str_radix(address, 16).ok()?, name))
}

fn read_machine(disassembly: &str) -> Machine {
    // The headers, in the order of the text.
    let headers: Vec<(u64, &str)> = disassembly.lines().filter_map(header).collect();
    let labels: BTreeMap<&str, u64> = headers.iter().map(|&(a, n)| (n, a)).collect();
    let mut starts: Vec<(u64, &str)> = headers
        .iter()
        .copied()
        .filter(|(_, name)| !name.starts_with(".L"))
        .collect();
    starts.sort();
    let function_at = |address: u64| -> Option<&str> {
        let after = starts.partition_point(|&(start, _)| start <= address);
        after.checked_sub(1).map(|index| starts[index].1)
    };

    let mut machine = Machine::default();
    let mut current: Option<&str> = None;
    for line in disassembly.lines() {
        if let Some((_, name)) = header(line) {
            if !name.starts_with(".L") {
                current = Some(name);
                machine.functions.push(name.to_owned());
            }
            continue;
        }
        let Some(from) = current else { continue };
        let mut fields = line.split('\t');
        let (Some(head), Some(mnemonic)) = (fields.next(), fields.next()) else {
            continue;
        };
        // An instruction line starts with its address and a colon.
        let is_instruction = head
            .trim()
            .split_once(':')
            .is_some_and(|(address, _)| u64::from_str_radix(address, 16).is_ok());
        if !is_instruction {
            continue;
        }
        let mnemonic = mnemonic.trim();
        if !(mnemonic.starts_with('j') || mnemonic.starts_with('b') || mnemonic.starts_with("cb")) {
            continue;
        }
        let operands: String = fields.collect::<Vec<_>>().join("\t");
        let Some((_, after)) = operands.rsplit_once('<') else {
            continue;
        };
        let Some((target, _)) = after.split_once('>') else {
            continue;
        };
        let (name, offset) = match target.split_once("+0x") {
            Some((name, offset)) => (name, u64::from_str_radix(offset, 16).unwrap_or(0)),
            None => (target, 0),
        };
        let to = if name.starts_with(".L") {
            labels
                .get(name)
                .and_then(|address| function_at(address + offset))
        } else {
            Some(name)
        };
        if let Some(to) = to.filter(|to| *to != from) {
            machine.transfers.push(Transfer {
                from: from.to_owned(),
                to: to.to_owned(),
            });
        }
    }
    machine
}

/// What the oracle of the machine's calls finds wrong, one string each. `c` is
/// the set of C's functions. With `optimized`, a function may call what a body
/// it inlined calls, so it may call anything its line reaches (through
/// other lines), not only what it names.
fn check_machine(
    report: &Report,
    inserted: &[String],
    machine: &Machine,
    c: &HashSet<String>,
    optimized: bool,
) -> Vec<String> {
    let mut failures = check_inserted(report, inserted);
    for function in &machine.functions {
        let compiler = function.starts_with("__erebos_unhandled");
        if !report.bodies.contains_key(function) && !c.contains(function) && !compiler {
            failures.push(format!(
                "the function `{function}` of the executable is in no line of the report, \
                 and is neither C's nor a default handler"
            ));
        }
    }
    let mut reached: BTreeMap<&str, HashSet<String>> = BTreeMap::new();
    for Transfer { from, to } in &machine.transfers {
        // Calls from C, which an `extern fn`'s line stands for, are inside its
        // `N` (ADR 0122), and the default handlers are not in the report.
        // A jump to `_start` is a restart: it sets the stack pointer to the top
        // of the stack, so what runs next is the thread again and not a callee.
        let Some(body) = report.bodies.get(from).filter(|_| !c.contains(from)) else {
            continue;
        };
        if to == RESTART {
            continue;
        }
        let allowed = reached
            .entry(from)
            .or_insert_with(|| report.reaches(from, inserted, optimized));
        let covered = body
            .stack
            .zip(report.bodies.get(to).and_then(|callee| callee.depth))
            .is_some_and(|(n, depth)| depth <= n);
        if !allowed.contains(to) && !covered {
            failures.push(format!(
                "`{from}` calls or jumps to `{to}`, which its line does not reach{}",
                match body.stack {
                    Some(n) => format!(" (and its `#[stack({n})]` does not cover it)"),
                    None => String::new(),
                }
            ));
        }
    }
    failures
}

/// The functions an object file defines, from `llvm-readelf --symbols`.
fn functions_of(object: &Path) -> HashSet<String> {
    let readelf = common::readelf_path();
    let output = Command::new(&readelf)
        .arg("--symbols")
        .arg(object)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "cannot run `{}` ({e}); install LLVM or set EREBOS_READELF",
                readelf.display()
            )
        });
    assert!(output.status.success(), "llvm-readelf failed");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            // Num: Value Size Type Bind Vis Ndx Name
            (fields.len() == 8 && fields[3] == "FUNC" && fields[6] != "UND")
                .then(|| fields[7].to_owned())
        })
        .collect()
}

/// The disassembly of an executable's code.
fn disassemble(elf: &Path) -> String {
    let objdump = common::objdump_path();
    let output = Command::new(&objdump)
        .arg("-d")
        .arg(elf)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "cannot run `{}` ({e}); install LLVM or set EREBOS_OBJDUMP",
                objdump.display()
            )
        });
    assert!(output.status.success(), "llvm-objdump failed");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The fewest transfers a build of `tests/e2e` shows: the smallest program has
/// three (riscv32 at `-O`). Fewer means that the output of `llvm-objdump` is no
/// longer read as it was written for, and the oracle would pass with nothing
/// checked.
const MIN_TRANSFERS: usize = 3;

/// The oracle must have looked at something: the reset entry is among the
/// functions read, and there are calls and jumps.
fn check_not_vacuous(machine: &Machine) -> Vec<String> {
    let mut failures = Vec::new();
    if !machine.functions.iter().any(|f| f == RESTART) {
        failures.push(format!(
            "`{RESTART}` is not among the {} functions read from `llvm-objdump -d`; \
             its output is not in the format the oracle reads",
            machine.functions.len()
        ));
    }
    if machine.transfers.len() < MIN_TRANSFERS {
        failures.push(format!(
            "only {} calls or jumps between functions were read from `llvm-objdump -d`, \
             fewer than {MIN_TRANSFERS}; its output is not in the format the oracle reads",
            machine.transfers.len()
        ));
    }
    failures
}

/// Checks the calls of one build.
fn check_calls(built: &Built, target: &Target, opt: OptLevel) -> Vec<String> {
    let report = match Report::parse(&built.report) {
        Ok(report) => report,
        Err(e) => return vec![format!("{}: {e}", built.label)],
    };
    let inserted = inserted_functions(target);
    let machine = read_machine(&disassemble(&built.elf));
    let c: HashSet<String> = built.objects.iter().flat_map(|o| functions_of(o)).collect();
    let mut failures = check_not_vacuous(&machine);
    failures.extend(check_machine(
        &report,
        &inserted,
        &machine,
        &c,
        matches!(opt, OptLevel::Speed),
    ));
    failures
        .into_iter()
        .map(|failure| format!("{}: {failure}", built.label))
        .collect()
}

/// Every program of `tests/e2e`, on each target it runs on at both levels: each
/// call and jump the machine makes between functions is one the bound's graph
/// has (ADR 0123). The programs are built on several threads.
#[test]
fn every_call_of_the_machine_is_an_edge_of_the_bound() {
    let programs = board::e2e_programs();
    let next = AtomicUsize::new(0);
    let failures = Mutex::new(Vec::new());
    let threads = thread::available_parallelism()
        .map_or(4, usize::from)
        .min(8);
    thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                while let Some(program) = programs.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let mut mine = Vec::new();
                    match qemu::expectation(&fs::read_to_string(program).unwrap()) {
                        Err(e) => mine.push(format!("{}: {e}", board::program_name(program))),
                        Ok(expected) => {
                            for target in &expected.targets {
                                for opt in [OptLevel::None, OptLevel::Speed] {
                                    match build("calls", program, &expected, target, opt) {
                                        Ok(built) => mine.extend(check_calls(&built, target, opt)),
                                        Err(e) => mine.push(e),
                                    }
                                }
                            }
                        }
                    }
                    failures.lock().unwrap().extend(mine);
                }
            });
        }
    });
    let mut failures = failures.into_inner().unwrap();
    failures.sort();
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

// ------------------------------------------------------------ the unit tests

fn report_of(lines: &[&str]) -> Report {
    Report::parse(&format!(
        "worst case: 100 bytes\nthread\nbodies:\n{}\n",
        lines.join("\n")
    ))
    .unwrap()
}

fn machine_of(text: &str) -> Machine {
    read_machine(text)
}

const RISCV: &str = "\
80000000 <_start>:
80000000: 30047073     \tcsrci\tmstatus, 0x8
80000004: 00000317     \tauipc\tt1, 0x0
80000008: 01c30067     \tjr\t0x1c(t1) <main>

80000024 <main>:
80000024: 1141         \taddi\tsp, sp, -0x10
80000026: 00000097     \tauipc\tra, 0x0
8000002a: 000080e7     \tjalr\t0x2(ra) <helper>
8000002e: 0000         \tj\t0x80000030 <main+0xc>
80000030: 00008067     \tjalr\t0x0(ra) <memcpy>
80000034: 8082         \tret

80000036 <helper>:
80000036: 8082         \tret

80000038 <memcpy>:
80000038: a001         \tj\t0x80000038 <memcpy>

8000003a <c_function>:
8000003a: 0000         \tj\t0x80000036 <helper>
8000003c: 8082         \tret

8000003e <__erebos_unhandled>:
8000003e: 0000         \tj\t0x80000024 <main>
";

/// The reader finds the calls and jumps, by name, and none of a jump within a
/// function; a local label is placed by its address.
#[test]
fn the_machine_reader_finds_calls_and_jumps_between_functions() {
    let machine = machine_of(RISCV);
    let pairs: Vec<(&str, &str)> = machine
        .transfers
        .iter()
        .map(|t| (t.from.as_str(), t.to.as_str()))
        .collect();
    assert_eq!(
        pairs,
        [
            ("_start", "main"),
            ("main", "helper"),
            ("main", "memcpy"),
            ("c_function", "helper"),
            ("__erebos_unhandled", "main"),
        ]
    );
    assert_eq!(
        machine.functions,
        [
            "_start",
            "main",
            "helper",
            "memcpy",
            "c_function",
            "__erebos_unhandled"
        ]
    );
    let local = "\
80000000 <f>:
80000000: 0000         \tnop

80000004 <.Lpcrel_hi1>:
80000004: 0000         \tj\t0x80000008 <.Lpcrel_hi1+0x4>
80000008: 0000         \tj\t0x8000000c <g>

8000000c <g>:
8000000c: 8082         \tret
";
    let machine = machine_of(local);
    assert_eq!(
        machine.transfers,
        [Transfer {
            from: "f".into(),
            to: "g".into()
        }]
    );
    assert_eq!(machine.functions, ["f", "g"]);
}

/// An output the reader does not understand (no tabs, other headers) makes the
/// oracle fail instead of pass with nothing checked.
#[test]
fn a_disassembly_the_reader_does_not_understand_fails_the_oracle() {
    assert!(check_not_vacuous(&machine_of(RISCV)).is_empty());
    let other = RISCV
        .replace('\t', " ")
        .replace(" <", " [")
        .replace(">:", "]:");
    let failures = check_not_vacuous(&machine_of(&other));
    assert_eq!(failures.len(), 2, "{failures:?}");
    assert!(
        failures[0].contains("`_start` is not among"),
        "{failures:?}"
    );
    assert!(failures[1].contains("only 0 calls"), "{failures:?}");
    // Functions but too few transfers.
    let few = "80000000 <_start>:\n80000000: 30047073     \tcsrci\tmstatus, 0x8\n";
    let failures = check_not_vacuous(&machine_of(few));
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].contains("only 0 calls"), "{failures:?}");
}

/// The ARM form: `bl`, `b.w` and a conditional branch with a trailing comment.
#[test]
fn the_machine_reader_reads_thumb() {
    let text = "\
00000040 <f>:
      40: b081         \tsub\tsp, #0x4
      42: f7ff ffc3    \tbl\t0x98 <g>       @ imm = #-0x7a
      46: d202         \tbhs\t0x4e <f+0xe>      @ imm = #0x4
      48: f000 b800    \tb.w\t0x98 <h>
      4c: 4770         \tbx\tlr

00000098 <g>:
      98: 4770         \tbx\tlr
";
    let machine = machine_of(text);
    let pairs: Vec<(&str, &str)> = machine
        .transfers
        .iter()
        .map(|t| (t.from.as_str(), t.to.as_str()))
        .collect();
    assert_eq!(pairs, [("f", "g"), ("f", "h")]);
}

const REPORT: [&str; 4] = [
    "_start: frame 0, stack 0, depth 112; calls main",
    "main: frame 16, depth 112; calls helper, inserted calls",
    "helper: frame 0, depth 0",
    "memcpy: frame 32, depth 32",
];

/// A call that the line does not name is reported; one that the line names, an
/// inserted function when the line has the inserted edges, a function C
/// defines, and a default handler are not.
#[test]
fn a_call_the_report_does_not_show_is_reported() {
    let report = report_of(&REPORT);
    let inserted = vec!["memcpy".to_owned()];
    let c: HashSet<String> = ["c_function".to_owned()].into();
    let machine = machine_of(RISCV);
    // `c_function` is C's (skipped), `__erebos_unhandled` is the compiler's.
    assert_eq!(
        check_machine(&report, &inserted, &machine, &c, false),
        Vec::<String>::new()
    );

    // Without the inserted edges `main` may not call `memcpy`.
    let report = report_of(&[
        REPORT[0],
        "main: frame 16, depth 16; calls helper",
        REPORT[2],
        REPORT[3],
    ]);
    let failures = check_machine(&report, &inserted, &machine, &c, false);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].contains("`main` calls or jumps to `memcpy`"),
        "{failures:?}"
    );

    // A function of the executable in no line, neither C's nor a handler.
    let failures = check_machine(
        &report_of(&REPORT),
        &inserted,
        &machine,
        &HashSet::new(),
        false,
    );
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].contains("`c_function`"), "{failures:?}");
}

/// After inlining, a function calls what a callee of its line calls: the
/// optimized level accepts it, the unoptimized one does not.
#[test]
fn an_inlined_callee_s_calls_are_accepted_only_when_optimized() {
    let text = "\
00000000 <a>:
       0: f7ff ffc3    \tbl\t0x10 <c>

00000010 <c>:
      10: 4770         \tbx\tlr

00000020 <b>:
      20: 4770         \tbx\tlr
";
    let report = report_of(&[
        "a: frame 0, depth 8; calls b",
        "b: frame 8, depth 8; calls c",
        "c: frame 0, depth 0",
    ]);
    let machine = machine_of(text);
    let c = HashSet::new();
    assert_eq!(check_machine(&report, &[], &machine, &c, true).len(), 0);
    let failures = check_machine(&report, &[], &machine, &c, false);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].contains("`a` calls or jumps to `c`"),
        "{failures:?}"
    );
}

/// A function with `#[stack(N)]` may make a call its line does not name when
/// `N` is at least the callee's depth, and not otherwise.
#[test]
fn a_declared_stack_covers_a_call_only_when_it_is_large_enough() {
    let text = "\
00000000 <naked>:
       0: f7ff ffc3    \tbl\t0x10 <deep>

00000010 <deep>:
      10: 4770         \tbx\tlr
";
    let machine = machine_of(text);
    let c = HashSet::new();
    let report = |n: u64| {
        report_of(&[
            format!("naked: frame 0, stack {n}, depth {n}").as_str(),
            "deep: frame 24, depth 24",
        ])
    };
    assert!(check_machine(&report(24), &[], &machine, &c, false).is_empty());
    let failures = check_machine(&report(16), &[], &machine, &c, false);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].contains("#[stack(16)]"), "{failures:?}");
}

/// The inserted functions are read from `core`, and the report must agree: a
/// body with the inserted edges is at least as deep as any of them.
#[test]
fn the_inserted_functions_are_those_the_report_is_deep_for() {
    let riscv = inserted_functions(&erebos::target::RISCV32IMAC);
    assert!(riscv.contains(&"memcpy".to_owned()), "{riscv:?}");
    assert!(riscv.contains(&"__divdi3".to_owned()), "{riscv:?}");
    assert!(!riscv.contains(&"_start".to_owned()), "{riscv:?}");
    let arm = inserted_functions(&erebos::target::THUMBV7M);
    assert!(arm.contains(&"__aeabi_uldivmod".to_owned()), "{arm:?}");
    assert!(arm.contains(&"__aeabi_memcpy4".to_owned()), "{arm:?}");

    let inserted = vec!["memcpy".to_owned()];
    let shallow = report_of(&[
        "main: frame 16, depth 16; calls inserted calls",
        "memcpy: frame 32, depth 32",
    ]);
    let failures = check_inserted(&shallow, &inserted);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].contains("`main`"), "{failures:?}");
    let deep = report_of(&[
        "main: frame 16, depth 48; calls inserted calls",
        "memcpy: frame 32, depth 32",
    ]);
    assert!(check_inserted(&deep, &inserted).is_empty());
    let failures = check_inserted(&deep, &["memset".to_owned()]);
    assert!(failures[0].contains("not in the report"), "{failures:?}");
}

/// The lines of the report are read back as the compiler writes them.
#[test]
fn the_report_is_read_back() {
    let report = Report::parse(
        "worst case: 1120 bytes, 4096 declared\nthread context: 512 bytes\nbodies:\n\
         tick: frame 0, depth 208; calls inserted calls\n\
         leaf: frame 0, stack 8, depth 8\n\
         divide: frame 16, depth unknown, not reached; calls on_panic, inserted calls\n\
         core.arch.__erebos_trap: frame 0, stack 64, depth 64\n",
    )
    .unwrap();
    assert_eq!(report.total, Some(1120));
    assert_eq!(report.bodies.len(), 4);
    let tick = &report.bodies["tick"];
    assert!(tick.inserted && tick.calls.is_empty() && tick.depth == Some(208));
    assert_eq!(report.bodies["leaf"].stack, Some(8));
    let divide = &report.bodies["divide"];
    assert_eq!((divide.frame, divide.depth), (16, None));
    assert_eq!(divide.calls, ["on_panic"]);
    assert!(report.bodies.contains_key("core.arch.__erebos_trap"));
    assert_eq!(
        Report::parse("worst case: unknown\nbodies:\n")
            .unwrap()
            .total,
        None
    );
}

// --------------------------------------------------------- the measured stack

/// The programs that measure their stack, on every target they run on at both
/// levels: the number they print is at most the report's worst case, and at
/// least the 512-byte array of their path, so that a count that is always 0
/// fails (ADR 0123, "Tests").
#[test]
fn the_measured_stack_is_within_the_bound() {
    let programs: Vec<PathBuf> = board::e2e_programs()
        .into_iter()
        .filter(|program| board::program_name(program).starts_with("stack_measured"))
        .collect();
    // Two paths on each target: a failed check and a fault.
    assert_eq!(programs.len(), 4, "{programs:?}");
    let mut failures = Vec::new();
    for program in &programs {
        let expected = qemu::expectation(&fs::read_to_string(program).unwrap()).unwrap();
        assert!(expected.measures_stack, "{}", program.display());
        for target in &expected.targets {
            for opt in [OptLevel::None, OptLevel::Speed] {
                failures.extend(measure(program, &expected, target, opt).err());
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

fn measure(
    program: &Path,
    expected: &Expectation,
    target: &'static Target,
    opt: OptLevel,
) -> Result<(), String> {
    let built = build("measured", program, expected, target, opt)?;
    let label = &built.label;
    let report = Report::parse(&built.report).map_err(|e| format!("{label}: {e}"))?;
    let total = report
        .total
        .ok_or_else(|| format!("{label}: the report has no worst case:\n{}", built.report))?;
    let log = built.elf.with_extension("qemu.log");
    let run = qemu::boot(
        label,
        target,
        &built.elf,
        &log,
        EXIT_TIMEOUT,
        expected.takes_exceptions,
    )?;
    qemu::check(label, expected, &run)?;
    let used = qemu::measured_stack(&run.output)
        .ok_or_else(|| format!("{label}: no `stack <N>` in the output:\n{}", run.output))?;
    if used > total {
        return Err(format!(
            "{label}: the program used {used} bytes of stack, more than the bound {total}"
        ));
    }
    if used < BUFFER {
        return Err(format!(
            "{label}: the program used {used} bytes of stack, fewer than its {BUFFER}-byte array"
        ));
    }
    Ok(())
}
