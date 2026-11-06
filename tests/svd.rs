//! `erebos svd` (ADR 0070 to 0072): the SVD files of `tests/svd/*.svd` are
//! generated, and a snapshot holds the generated module followed by the
//! diagnostics, rendered plain as the ui snapshots are. A module that is
//! generated is also compiled: written beside a root file that declares it,
//! and type checked, so that a generator that writes invalid Erebos fails
//! here. The same file is generated twice, and must give the same bytes.
//!
//! A file may expect the compiler to reject what was generated, by naming
//! the codes in an XML comment at its top, `<!-- expect-error:
//! ty::peripheral-overlap -->`: the type check must then report those codes
//! and no others. The command itself is run through the compiler's binary.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use erebos::driver::{self, OptLevel, Stage};
use erebos::span::SourceMap;
use erebos::{diag, svd};

/// The root file a generated module is declared in: a program needs an entry
/// function, a panic function and a memory map to type check.
const ROOT: &str = "\
mod generated;

entry fn main() -> ! {
    loop {}
}

panic fn on_panic() -> ! {
    loop {}
}

memory {
    region IMAGE { origin: 0x8000_0000, size: 0x0400_0000 }
    region RAM { origin: 0x8400_0000, size: 0x0400_0000 }
    code in IMAGE;
    data in RAM;
    stack in RAM { size: 0x0010_0000 }
}
";

#[test]
fn svd() {
    insta::glob!("svd/*.svd", |path| {
        insta::assert_snapshot!(run(path));
    });
}

/// The text of the file at `path`, with `\n` line endings whatever the
/// checkout did to it, as the ui tests read theirs.
fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap().replace("\r\n", "\n")
}

/// The snapshot of the file at `path`.
fn run(path: &Path) -> String {
    let text = read(path);
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let (generated, diags, sources) = svd::generate(&name, &text);
    let diagnostics = diag::render(&diags.into_vec(), &sources, false);

    let (again, diags, sources) = svd::generate(&name, &text);
    assert_eq!(generated, again, "{name}: generated twice, two outputs");
    assert_eq!(
        diagnostics,
        diag::render(&diags.into_vec(), &sources, false),
        "{name}: generated twice, two sets of diagnostics"
    );

    match generated {
        Some(module) => {
            compile(path, &text, &module);
            format!("--- generated ---\n{module}--- diagnostics ---\n{diagnostics}")
        }
        None => format!("--- not generated ---\n{diagnostics}"),
    }
}

/// Type checks `module` beside a root file, and requires the errors the SVD
/// file `svd_text` expects, no more and no fewer.
fn compile(path: &Path, svd_text: &str, module: &str) {
    let stem = path.file_stem().unwrap().to_string_lossy();
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("svd")
        .join(stem.as_ref());
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("generated.ers"), module).unwrap();
    let root = dir.join("main.ers");
    fs::write(&root, ROOT).unwrap();

    let expected = expected_errors(svd_text);
    // The list of interrupts means something else on thumbv7m, where its
    // values are lines of the NVIC with a limit (ADR 0089): a module that has
    // one is checked there too.
    let mut targets = vec![erebos::target::DEFAULT];
    if module.lines().any(|line| line == "#[interrupts]") {
        targets.push(&erebos::target::THUMBV7M);
    }
    for target in targets {
        match driver::dump(&root, ROOT, Stage::Typeck, target, OptLevel::None, false) {
            Ok(_) => assert!(
                expected.is_empty(),
                "{}: the compiler did not report {expected:?} for {}",
                path.display(),
                target.name
            ),
            Err(rendered) => assert_eq!(
                error_codes(&rendered),
                expected,
                "{}: the generated module does not compile for {}:\n{rendered}",
                path.display(),
                target.name
            ),
        }
    }
}

/// The codes named by `<!-- expect-error: code -->` comments.
fn expected_errors(svd_text: &str) -> BTreeSet<String> {
    svd_text
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("<!-- expect-error:")?;
            Some(rest.strip_suffix("-->")?.trim().to_owned())
        })
        .collect()
}

/// The codes of rendered diagnostics: the line of a diagnostic that holds
/// its code alone, at the margin.
fn error_codes(rendered: &str) -> BTreeSet<String> {
    rendered
        .lines()
        .filter(|line| {
            let mut parts = line.split("::");
            let (Some(layer), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
                return false;
            };
            let word = |s: &str, ok: fn(char) -> bool| !s.is_empty() && s.chars().all(ok);
            word(layer, |c| c.is_ascii_lowercase())
                && word(name, |c| c.is_ascii_lowercase() || c == '-')
        })
        .map(str::to_owned)
        .collect()
}

// ---- the ESP32-C3

/// The ESP32-C3's SVD file (ADR 0070), whole, is generated and the module is
/// built, with a program that reaches a few registers, into a riscv32
/// executable at both optimization levels. The executable is not run: the
/// official QEMU has no ESP32-C3 machine. The snapshot is a summary of what
/// was written and the warnings; the generated text is not kept.
#[test]
fn the_esp32c3_is_generated_and_built() {
    let chips = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/svd/chips");
    let name = "esp32c3.svd";
    let text = read(&chips.join(name));
    let (generated, diags, sources) = svd::generate(name, &text);
    let diags = diags.into_vec();
    let Some(module) = generated else {
        panic!(
            "{name} was not generated:\n{}",
            diag::render(&diags, &sources, false)
        );
    };

    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("svd_chips")
        .join("esp32c3");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("generated.ers"), &module).unwrap();
    let root = dir.join("esp32c3_main.ers");
    fs::write(&root, read(&chips.join("esp32c3_main.ers"))).unwrap();
    for (opt, elf) in [
        (OptLevel::None, "esp32c3.elf"),
        (OptLevel::Speed, "esp32c3.opt.elf"),
    ] {
        driver::build(
            &root,
            &dir.join(elf),
            &[],
            erebos::target::DEFAULT,
            opt,
            false,
        )
        .unwrap_or_else(|rendered| {
            panic!("the generated module does not build with {opt:?}:\n{rendered}")
        });
    }

    // What the program and the file's survey (ADR 0067, 0071) rely on.
    for (what, line) in [
        (
            "RNG over APB_CTRL",
            "pub peripheral RNG: rng::Rng at 0x6002_6000 over APB_CTRL;",
        ),
        (
            "AES's register CONTINUE",
            "pub write continue_: Continue at 0xA8,",
        ),
        ("AES's field CONTINUE", "pub continue_: bool at 0 = false,"),
        (
            "SHA's register CONTINUE",
            "pub write continue_: Continue at 0x14,",
        ),
        ("SHA's field CONTINUE", "pub continue_: u32 at 1..=31 = 0,"),
    ] {
        assert!(
            module.lines().any(|l| l.trim() == line),
            "{what}: no line `{line}` in the generated module"
        );
    }

    insta::assert_snapshot!(summary(&module, &diags, &sources));
}

/// The device, the number of things written, and the warnings: each with its
/// code and message, then a line per label with its position in the SVD file
/// and its text. Not `diag::render`: it shows every line between two labels
/// of one warning, and the ESP32-C3's `RTC_CNTL` warning has two labels
/// 1 700 lines apart (a snapshot of 83 KB).
fn summary(module: &str, diags: &[diag::Diagnostic], sources: &SourceMap) -> String {
    let device = module
        .lines()
        .find_map(|line| line.strip_prefix("// Device: "))
        .map(|device| device.trim_end_matches('.'))
        .expect("the header names the device");

    // What the module declares, from its layout: items at the margin or one
    // level in, entries two levels in.
    let (mut types, mut peripherals, mut registers, mut fields) = (0, 0, 0, 0);
    let mut inside = "";
    // The variants of the chip's interrupts, which follow `#[interrupts]`.
    let (mut interrupts, mut listing) = (0, false);
    for line in module.lines() {
        if line == "#[interrupts]" {
            listing = true;
        } else if listing && line == "}" {
            listing = false;
        } else if listing && line.starts_with("    ") && !line.trim_start().starts_with("//") {
            interrupts += 1;
        } else if line.starts_with("pub peripheral ") {
            peripherals += 1;
        } else if let Some(item) = line.strip_prefix("    pub ") {
            inside = item.split(' ').next().unwrap_or("");
            types += usize::from(inside == "peripheral");
        } else if line.starts_with("        pub ") {
            match inside {
                "peripheral" => registers += 1,
                "struct" => fields += 1,
                _ => {}
            }
        }
    }

    let mut out = format!(
        "device: {device}\n\
         peripheral types: {types}\n\
         peripherals: {peripherals}\n\
         registers: {registers}\n\
         fields: {fields}\n\
         interrupts: {interrupts}\n\
         warnings: {}\n",
        diags.len()
    );
    for diag in diags {
        out.push_str(&format!("{}: {}\n", diag.code, diag.message));
        for label in &diag.labels {
            let file = sources.file(label.span.file);
            let before = &file.text[..label.span.lo as usize];
            let line = before.matches('\n').count() + 1;
            let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
            out.push_str(&format!(
                "  {}:{line}:{column}: {}\n",
                file.name, label.message
            ));
        }
    }
    out
}

// ---- the modules kept beside the e2e programs

/// The e2e programs that declare a module generated from an SVD file kept
/// beside it: the directory under `tests/e2e/` and the file's stem. QEMU
/// `virt`'s devices (ADR 0070), and the UART of each target driven by its
/// interrupt (ADR 0092).
const KEPT: [(&str, &str); 3] = [
    ("svd_virt", "virt"),
    ("uart_interrupt_riscv32", "virt"),
    ("uart_interrupt_armv7m", "an385"),
];

/// Each kept module (`virt.ers`, `an385.ers`), the one an e2e program
/// declares, is what the generator writes from its SVD file today: it is kept
/// in the repository, so a change to the generator that alters it shows in
/// the diff.
#[test]
fn kept_modules_are_what_the_generator_writes_from_their_svd_files() {
    for (directory, stem) in KEPT {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/e2e")
            .join(directory);
        let name = format!("{stem}.svd");
        let (generated, diags, sources) = svd::generate(&name, &read(&dir.join(&name)));
        let diags = diags.into_vec();
        let Some(module) = generated else {
            panic!(
                "{directory}/{name} was not generated:\n{}",
                diag::render(&diags, &sources, false)
            );
        };
        assert!(
            diags.is_empty(),
            "{directory}/{name} gave diagnostics:\n{}",
            diag::render(&diags, &sources, false)
        );
        assert!(
            module == read(&dir.join(format!("{stem}.ers"))),
            "tests/e2e/{directory}/{stem}.ers is not what the generator writes from \
             {name}; regenerate it with `erebos svd tests/e2e/{directory}/{name}`"
        );
    }
}

// ---- the command

/// A directory of its own for one test of the command, empty.
fn workdir(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("svd_cli")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Runs `erebos svd` with `args` in `dir`.
fn erebos_svd(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_erebos"))
        .arg("svd")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn sample(name: &str) -> String {
    read(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/svd")
            .join(name),
    )
}

#[test]
fn the_output_is_named_after_the_input_and_written_beside_it() {
    let dir = workdir("default_output");
    for (input, output) in [
        ("esp-32.svd", "esp_32.ers"),
        ("match.svd", "match_.ers"),
        ("32bit.svd", "_32bit.ers"),
        ("plain", "plain.ers"),
        ("a.b.svd", "a_b.ers"),
    ] {
        let text = sample("arrays.svd");
        fs::write(dir.join(input), &text).unwrap();
        let run = erebos_svd(&dir, &[input]);
        assert!(run.status.success(), "{input}: {}", stderr(&run));
        let written = read(&dir.join(output));
        assert_eq!(
            Some(written),
            svd::generate(input, &text).0,
            "{input}: the module written is the one generated, with the input's name in its header"
        );
    }
}

#[test]
fn an_input_in_another_directory_gets_its_output_there() {
    let dir = workdir("other_directory");
    fs::create_dir(dir.join("chips")).unwrap();
    fs::write(dir.join("chips/dev.svd"), sample("arrays.svd")).unwrap();
    let run = erebos_svd(&dir, &["chips/dev.svd"]);
    assert!(run.status.success(), "{}", stderr(&run));
    assert!(dir.join("chips/dev.ers").is_file());
    assert!(!dir.join("dev.ers").exists());
}

#[test]
fn o_names_the_output_and_replaces_what_is_there() {
    let dir = workdir("named_output");
    fs::write(dir.join("dev.svd"), sample("arrays.svd")).unwrap();
    fs::write(dir.join("mine.ers"), "old").unwrap();
    let run = erebos_svd(&dir, &["dev.svd", "-o", "mine.ers"]);
    assert!(run.status.success(), "{}", stderr(&run));
    assert!(read(&dir.join("mine.ers")).starts_with("// Generated by `erebos svd`"));
    assert!(!dir.join("dev.ers").exists());
}

#[test]
fn an_output_that_is_the_input_is_refused_and_the_input_is_kept() {
    let dir = workdir("same_file");
    let text = sample("arrays.svd");
    fs::write(dir.join("dev.svd"), &text).unwrap();
    // Named twice, and spelled another way.
    for args in [
        &["dev.svd", "-o", "dev.svd"][..],
        &["dev.svd", "-o", "./dev.svd"],
    ] {
        let run = erebos_svd(&dir, args);
        assert!(!run.status.success(), "{args:?}");
        assert!(
            stderr(&run).contains("is the input file"),
            "{}",
            stderr(&run)
        );
        assert_eq!(read(&dir.join("dev.svd")), text, "{args:?}");
    }
    // The default name can be the input's: `dev.ers` made from `dev.ers`.
    fs::write(dir.join("dev.ers"), &text).unwrap();
    let run = erebos_svd(&dir, &["dev.ers"]);
    assert!(!run.status.success());
    assert!(
        stderr(&run).contains("is the input file"),
        "{}",
        stderr(&run)
    );
    assert_eq!(read(&dir.join("dev.ers")), text);
}

#[test]
fn an_error_writes_nothing_and_fails() {
    let dir = workdir("error");
    fs::write(dir.join("dev.svd"), sample("errors_xml.svd")).unwrap();
    let run = erebos_svd(&dir, &["dev.svd"]);
    assert!(!run.status.success());
    assert!(stderr(&run).contains("svd::xml"), "{}", stderr(&run));
    assert!(!dir.join("dev.ers").exists());
    // An output that was there is not touched.
    fs::write(dir.join("kept.ers"), "kept").unwrap();
    let run = erebos_svd(&dir, &["dev.svd", "-o", "kept.ers"]);
    assert!(!run.status.success());
    assert_eq!(read(&dir.join("kept.ers")), "kept");
}

#[test]
fn a_file_that_is_not_utf8_is_an_error() {
    let dir = workdir("not_utf8");
    fs::write(dir.join("dev.svd"), b"<device>\xFF\xFE</device>").unwrap();
    let run = erebos_svd(&dir, &["dev.svd"]);
    assert!(!run.status.success());
    assert!(stderr(&run).contains("svd::xml"), "{}", stderr(&run));
    assert!(stderr(&run).contains("not UTF-8"), "{}", stderr(&run));
    assert!(!dir.join("dev.ers").exists());
}

#[test]
fn warnings_are_shown_and_the_output_is_written() {
    let dir = workdir("warnings");
    fs::write(dir.join("dev.svd"), sample("registers_unsupported.svd")).unwrap();
    let run = erebos_svd(&dir, &["dev.svd"]);
    assert!(run.status.success(), "{}", stderr(&run));
    assert!(
        stderr(&run).contains("svd::not-declared"),
        "{}",
        stderr(&run)
    );
    assert!(dir.join("dev.ers").is_file());
    assert!(run.stdout.is_empty());
}

#[test]
fn the_command_line_is_checked() {
    let dir = workdir("usage");
    for (args, message) in [
        (&[][..], "missing input file"),
        (&["a.svd", "b.svd"], "only one input file"),
        (&["a.svd", "-O"], "unknown option `-O`"),
        (&["a.svd", "-o"], "`-o` needs a path"),
    ] {
        let run = erebos_svd(&dir, args);
        assert_eq!(run.status.code(), Some(2), "{args:?}");
        assert!(stderr(&run).contains(message), "{args:?}: {}", stderr(&run));
    }
    let run = erebos_svd(&dir, &["missing.svd"]);
    assert_eq!(run.status.code(), Some(1));
    assert!(stderr(&run).contains("cannot read"), "{}", stderr(&run));
    // A name with nothing left for a module's name.
    fs::write(dir.join("@@.svd"), sample("arrays.svd")).unwrap();
    let run = erebos_svd(&dir, &["@@.svd"]);
    assert_eq!(run.status.code(), Some(1));
    assert!(stderr(&run).contains("name the output"), "{}", stderr(&run));
}

/// `n` peripherals of one register at one address, as one list: each shares
/// bytes with every earlier one.
fn stacked(n: u32) -> String {
    format!(
        "<device><name>D</name><size>32</size><access>read-write</access><peripherals>\
         <peripheral><name>P%s</name><dim>{n}</dim><dimIncrement>0</dimIncrement>\
         <baseAddress>0x40000000</baseAddress><registers><register><name>R</name>\
         <addressOffset>0</addressOffset></register></registers></peripheral>\
         </peripherals></device>"
    )
}

#[test]
fn peripherals_stacked_on_one_address_get_an_output_linear_in_their_number() {
    // Each of the 4 096 would name every earlier one (80 MB of comments).
    let n = 4096;
    let (generated, diags, _) = svd::generate("stacked.svd", &stacked(n));
    assert!(diags.into_vec().is_empty());
    let module = generated.unwrap();
    assert!(module.len() < n as usize * 512, "{} bytes", module.len());
    // The comments wrap: read them as one line.
    let text = module.replace("\n// svd: ", " ");
    let last = format!(
        "found from the bytes it shares with `P0`, `P1`, `P2`, `P3`, `P4`, `P5`, `P6`, `P7` \
         and {} more.",
        n - 1 - 8
    );
    assert!(text.contains(&last), "the last one counts the rest");
}
