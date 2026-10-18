//! The cost of checking `match`es whose search grew exponentially in M2.4b
//! (ADR 0043).
//!
//! Each case generates a program of a shape of ADR 0042 "Context" as
//! source text and compiles it up to a stage, as `tests/ui.rs` does. The
//! sizes are chosen so that the M2.4b compiler would take more than 2^30
//! steps on them, while the fixed one takes milliseconds.

use erebos::driver::{self, OptLevel, Stage};

/// pairs(n): `enum P: u8 { A(bool, ..., bool) = 0 }` with `2n` positions,
/// and, for each `i < n`, one arm with `true` at positions `i` and `i + n`
/// and `_` elsewhere.
fn pairs(n: usize) -> String {
    let positions = vec!["bool"; 2 * n].join(", ");
    let mut arms = String::new();
    for i in 0..n {
        let subs: Vec<&str> = (0..2 * n)
            .map(|j| if j == i || j == i + n { "true" } else { "_" })
            .collect();
        arms += &format!("        P::A({}) => {i},\n", subs.join(", "));
    }
    format!(
        "enum P: u8 {{ A({positions}) = 0 }}\n\
         \n\
         fn f(p: P) -> u8 {{\n    match p {{\n{arms}    }}\n}}\n\
         \n\
         entry fn main() -> ! {{ loop {{}} }}\n\
         panic fn on_panic() -> ! {{ loop {{}} }}\n"
    )
}

/// The rendered diagnostics of `text` compiled up to `stage`; the program
/// must fail.
fn errors(text: &str, stage: Stage) -> String {
    match driver::dump("match_cost.ers", text, stage, OptLevel::None, false) {
        Ok(dump) => panic!("expected diagnostics, got:\n{dump}"),
        Err(diags) => diags,
    }
}

#[test]
fn pairs_without_a_wildcard_arm_lists_three_witnesses_and_more() {
    // 2^32 witnesses, which the M2.4b compiler enumerated to count them.
    let diags = errors(&pairs(32), Stage::Typeck);
    assert_eq!(diags.matches("ty::").count(), 1, "{diags}");
    assert_eq!(
        diags.matches("ty::non-exhaustive-match").count(),
        1,
        "{diags}"
    );
    let label = diags
        .lines()
        .find_map(|line| line.split_once("missing: ").map(|(_, list)| list))
        .unwrap_or_else(|| panic!("no `missing` label in:\n{diags}"));
    assert_eq!(label.matches("`P::A(").count(), 3, "{label}");
    assert!(label.ends_with("` and more"), "{label}");
}
