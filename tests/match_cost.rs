//! The cost of checking `match`es whose search grew exponentially in M2.4b
//! (ADR 0043).
//!
//! Each case generates a program of a shape of ADR 0042 "Context" as
//! source text and compiles it up to a stage, as `tests/ui.rs` does. The
//! sizes are chosen so that the M2.4b compiler would take more than 2^30
//! steps on them, while the fixed one takes milliseconds.

use erebos::driver::{self, OptLevel, Stage};

/// A program declaring `enum <name>: u8 { A(<positions>) = 0 }`, and a
/// function matching a value of it with `arms`, one pattern each, the arm
/// at index `i` giving `i`. The `match` starts at line 4, its first arm at
/// line 5.
fn program(name: &str, positions: &[&str], arms: &[String]) -> String {
    let positions = positions.join(", ");
    let arms: String = arms
        .iter()
        .enumerate()
        .map(|(i, arm)| format!("        {arm} => {i},\n"))
        .collect();
    format!(
        "enum {name}: u8 {{ A({positions}) = 0 }}\n\
         \n\
         fn f(x: {name}) -> u8 {{\n    match x {{\n{arms}    }}\n}}\n\
         \n\
         entry fn main() -> ! {{ loop {{}} }}\n\
         panic fn on_panic() -> ! {{ loop {{}} }}\n"
    )
}

/// pairs(n): `enum P: u8 { A(bool, ..., bool) = 0 }` with `2n` positions,
/// and, for each `i < n`, one arm with `true` at positions `i` and `i + n`
/// and `_` elsewhere.
fn pairs(n: usize) -> String {
    let arms: Vec<String> = (0..n)
        .map(|i| {
            let subs: Vec<&str> = (0..2 * n)
                .map(|j| if j == i || j == i + n { "true" } else { "_" })
                .collect();
            format!("P::A({})", subs.join(", "))
        })
        .collect();
    program("P", &vec!["bool"; 2 * n], &arms)
}

/// An arm of ors(k): `Q::A(1 | 2 | 3, ..., 1 | 2 | 3)` with `k` positions,
/// followed by the patterns of `last`, each a position more.
fn ors_arm(k: usize, last: &[&str]) -> String {
    let subs: Vec<&str> = std::iter::repeat_n("1 | 2 | 3", k)
        .chain(last.iter().copied())
        .collect();
    format!("Q::A({})", subs.join(", "))
}

/// `Q::A(_, ..., _, <last>)`, with `k` `_` before `last`.
fn wild_arm(k: usize, last: &str) -> String {
    let subs: Vec<&str> = std::iter::repeat_n("_", k)
        .chain(std::iter::once(last))
        .collect();
    format!("Q::A({})", subs.join(", "))
}

/// ors(k): `enum Q: u8 { A(u8, ..., u8) = 0 }` with `k` positions, and
/// then those of `last`, matched with `arms`.
fn ors(k: usize, last: &[&str], arms: &[String]) -> String {
    let positions: Vec<&str> = std::iter::repeat_n("u8", k)
        .chain(last.iter().copied())
        .collect();
    program("Q", &positions, arms)
}

/// The rendered diagnostics of `text` compiled up to `stage`; the program
/// must fail.
fn errors(text: &str, stage: Stage) -> String {
    match driver::dump("match_cost.ers", text, stage, OptLevel::None, false) {
        Ok(dump) => panic!("expected diagnostics, got:\n{dump}"),
        Err(diags) => diags,
    }
}

/// The rendered diagnostics of `text`, a program of one `match` that
/// reports `code` once and nothing else, up to type checking.
fn only(text: &str, code: &str) -> String {
    let diags = errors(text, Stage::Typeck);
    assert_eq!(diags.matches("ty::").count(), 1, "{diags}");
    assert_eq!(diags.matches(code).count(), 1, "{diags}");
    diags
}

/// Whether the rendered `diags` underline the source line `line`.
fn underlined(diags: &str, line: usize) -> bool {
    let source = format!(" {line} │ ");
    let mut lines = diags.lines();
    while let Some(text) = lines.next() {
        if text.starts_with(&source) {
            return lines.next().is_some_and(|next| next.contains('─'));
        }
    }
    false
}

#[test]
fn pairs_without_a_wildcard_arm_lists_three_witnesses_and_more() {
    // 2^32 witnesses, which the M2.4b compiler enumerated to count them.
    let diags = only(&pairs(32), "ty::non-exhaustive-match");
    let label = diags
        .lines()
        .find_map(|line| line.split_once("missing: ").map(|(_, list)| list))
        .unwrap_or_else(|| panic!("no `missing` label in:\n{diags}"));
    assert_eq!(label.matches("`P::A(").count(), 3, "{label}");
    assert!(label.ends_with("` and more"), "{label}");
}

#[test]
fn ors_written_twice_is_unreachable() {
    // The M2.4b compiler tried the 3^32 combinations of the second arm's
    // alternatives.
    let arm = ors_arm(32, &[]);
    let text = ors(32, &[], &[arm.clone(), arm, "_".to_owned()]);
    let diags = only(&text, "ty::unreachable-pattern");
    assert!(diags.contains("unreachable arm"), "{diags}");
    // The second arm, at line 6, is labelled with the first, at line 5.
    assert!(diags.contains("[match_cost.ers:6:9]"), "{diags}");
    assert!(
        diags.contains("every value it matches is matched here"),
        "{diags}"
    );
    assert!(underlined(&diags, 5), "{diags}");
}

#[test]
fn ors_with_a_dead_last_alternative() {
    // `Q::A(1 | 2 | 3, ..., 1 | 2 | 3, 3 | 4)` after the arm with
    // `1 | 2 | 3` there: its alternative `3` is dead, `4` is not.
    let arms = [ors_arm(32, &[]), ors_arm(31, &["3 | 4"]), "_".to_owned()];
    let text = ors(32, &[], &arms);
    let diags = only(&text, "ty::unreachable-pattern");
    assert!(diags.contains("unreachable pattern"), "{diags}");
    // The alternative `3`, the last pattern of line 6 but `4`.
    let column = 9 + ors_arm(31, &["3 | 4"]).find("3 | 4)").unwrap();
    assert!(
        diags.contains(&format!("[match_cost.ers:6:{column}]")),
        "{diags}"
    );
}

#[test]
fn ors_with_a_last_bool_is_exhaustive() {
    // `A(1 | 2 | 3, ..., 1 | 2 | 3, true)`, `A(_, ..., _, false)` and
    // `A(_, ..., _, true)`: no witness, no dead arm. The M2.4b compiler
    // examined the 3^32 combinations of the first arm's alternatives.
    let arms = [
        ors_arm(32, &["true"]),
        wild_arm(32, "false"),
        wild_arm(32, "true"),
    ];
    let text = ors(32, &["bool"], &arms);
    if let Err(diags) = driver::dump(
        "match_cost.ers",
        &text,
        Stage::Typeck,
        OptLevel::None,
        false,
    ) {
        panic!("expected no diagnostics, got:\n{diags}");
    }
}
