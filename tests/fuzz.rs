//! Mutation fuzzing: the compiler must answer every input with output or
//! diagnostics, never by panicking.
//!
//! Programs from `tests/ui` and `tests/e2e` are mutated (spans deleted,
//! snippets inserted, lines duplicated or swapped between programs) and
//! compiled up to LLVM IR. The seed is fixed, so failures reproduce; set
//! `EREBOS_FUZZ_ITERATIONS` to run more inputs locally.

use std::env;
use std::fs;
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;

use erebos::driver::{self, OptLevel, Stage};

const DEFAULT_ITERATIONS: usize = 2000;
const SEED: u64 = 0x5EED_E4EB_0500_0001;

/// Fragments that tend to reach unusual compiler paths.
const SNIPPETS: &[&str] = &[
    "return",
    "break",
    "continue",
    "loop {}",
    "{",
    "}",
    "(",
    ")",
    ";",
    "-",
    "!",
    "*",
    "as u8",
    "as *mut u8",
    "as usize",
    "if true { 1 } else { 2 }",
    "0",
    "b'a'",
    "true",
    "x",
    "while false {}",
    "/ 0",
    "% x",
    "-128",
    "()",
    "let y: u8 = 1;",
    "*p = 1;",
    "f()",
    "as bool",
    "&&",
    "||",
    "<<",
    "1 << 40",
    "loop { break 5; }",
    "fn",
    "entry",
    "panic",
    "->",
    "!",
    "u64",
    "/ b'\\0'",
    "_start",
    "[",
    "]",
    "[0]",
    "[i]",
    "[0; 4]",
    "[1, 2]",
    "[u8; 4]",
    "[u8; 0]",
    "&raw const",
    "&raw mut",
    "&",
    "*const volatile [u32; 2]",
    "memcpy",
];

/// xorshift64: small, deterministic, good enough to pick mutations.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn seeds() -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut paths: Vec<_> = ["ui", "e2e"]
        .iter()
        .flat_map(|dir| fs::read_dir(root.join(dir)).unwrap())
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "ers"))
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|path| fs::read_to_string(path).unwrap().replace("\r\n", "\n"))
        .collect()
}

/// Cuts at a character boundary at or before `i`.
fn floor_char(text: &str, mut i: usize) -> usize {
    i = i.min(text.len());
    while !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn mutate(rng: &mut Rng, sources: &[String]) -> String {
    let mut text = sources[rng.below(sources.len())].clone();
    for _ in 0..=rng.below(3) {
        match rng.below(4) {
            0 if text.len() > 2 => {
                let start = floor_char(&text, rng.below(text.len()));
                let end = floor_char(&text, start + 1 + rng.below(12));
                text.replace_range(start..end, "");
            }
            1 => {
                let at = floor_char(&text, rng.below(text.len() + 1));
                let snippet = SNIPPETS[rng.below(SNIPPETS.len())];
                text.insert_str(at, &format!(" {snippet} "));
            }
            2 => {
                let mut lines: Vec<&str> = text.lines().collect();
                if !lines.is_empty() {
                    let i = rng.below(lines.len());
                    lines.insert(i, lines[i]);
                }
                text = lines.join("\n");
            }
            _ => {
                let other = &sources[rng.below(sources.len())];
                let other_lines: Vec<&str> = other.lines().collect();
                let mut lines: Vec<&str> = text.lines().collect();
                if !lines.is_empty() && !other_lines.is_empty() {
                    let i = rng.below(lines.len());
                    lines[i] = other_lines[rng.below(other_lines.len())];
                }
                text = lines.join("\n");
            }
        }
    }
    text
}

#[test]
fn compiler_never_panics() {
    let iterations = env::var("EREBOS_FUZZ_ITERATIONS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(DEFAULT_ITERATIONS);
    let sources = seeds();
    let mut rng = Rng(SEED);

    // Panics are expected to be caught here; keep their messages out of the
    // test output and report the input instead.
    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));

    let mut failure = None;
    for i in 0..iterations {
        let input = mutate(&mut rng, &sources);
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            driver::dump("fuzz.ers", &input, Stage::LlvmIr, OptLevel::None, false)
        }));
        if let Err(payload) = result {
            let message = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            failure = Some((i, message, input));
            break;
        }
    }
    panic::set_hook(default_hook);

    if let Some((i, message, input)) = failure {
        let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("fuzz-crash.ers");
        fs::write(&path, &input).unwrap();
        panic!(
            "the compiler panicked on input #{i}: {message}\ninput saved to {}\n--- input ---\n{input}",
            path.display()
        );
    }
}
