//! The cost of `match`es whose checks and code grew exponentially in M2.4b
//! (ADR 0042, 0043).
//!
//! Each case generates a program of a shape of ADR 0042 "Context" as
//! source text and compiles it up to a stage, as `tests/ui.rs` does. The
//! sizes are chosen so that the M2.4b compiler would take more than 2^30
//! steps on them, while the fixed one takes milliseconds.

use std::path::Path;

use erebos::driver::{self, OptLevel, Stage};

/// A pattern as source text, and the number of patterns it writes, as
/// ADR 0042 "Conformance tests" counts them: every pattern, each
/// alternative of an or-pattern, `_` and `let` included, and the
/// or-pattern itself.
#[derive(Clone)]
struct Pat {
    text: String,
    count: usize,
}

/// A pattern that holds no other: `_`, a literal, a variant without
/// payload.
fn leaf(text: &str) -> Pat {
    Pat {
        text: text.to_owned(),
        count: 1,
    }
}

/// `<name>(<subs>)`.
fn variant(name: &str, subs: Vec<Pat>) -> Pat {
    let texts: Vec<&str> = subs.iter().map(|sub| sub.text.as_str()).collect();
    Pat {
        text: format!("{name}({})", texts.join(", ")),
        count: 1 + subs.iter().map(|sub| sub.count).sum::<usize>(),
    }
}

/// `<alternatives>`, separated by `|`.
fn or(alternatives: Vec<Pat>) -> Pat {
    let texts: Vec<&str> = alternatives.iter().map(|alt| alt.text.as_str()).collect();
    Pat {
        text: texts.join(" | "),
        count: 1 + alternatives.iter().map(|alt| alt.count).sum::<usize>(),
    }
}

/// A generated program, and the number of patterns its `match` writes.
struct Program {
    text: String,
    patterns: usize,
}

/// A program declaring the enums of `decls`, on one line, and a function
/// `f` matching a value of type `ty` with `arms`, one pattern each, the
/// arm at index `i` giving `i`. The `match` starts at line 4, its first
/// arm at line 5.
fn program(decls: &str, ty: &str, arms: &[Pat]) -> Program {
    let text: String = arms
        .iter()
        .enumerate()
        .map(|(i, arm)| format!("        {} => {i},\n", arm.text))
        .collect();
    Program {
        text: format!(
            "{decls}\n\
             \n\
             fn f(x: {ty}) -> u8 {{\n    match x {{\n{text}    }}\n}}\n\
             \n\
             entry fn main() -> ! {{ loop {{}} }}\n\
             panic fn on_panic() -> ! {{ loop {{}} }}\n"
        ),
        patterns: arms.iter().map(|arm| arm.count).sum(),
    }
}

/// pairs(n): `enum P: u8 { A(bool, ..., bool) = 0 }` with `2n` positions,
/// and, for each `i < n`, one arm with `true` at positions `i` and `i + n`
/// and `_` elsewhere, then the arms of `rest`.
fn pairs(n: usize, rest: &[Pat]) -> Program {
    let mut arms: Vec<Pat> = (0..n)
        .map(|i| {
            let subs = (0..2 * n)
                .map(|j| leaf(if j == i || j == i + n { "true" } else { "_" }))
                .collect();
            variant("P::A", subs)
        })
        .collect();
    arms.extend_from_slice(rest);
    let positions = vec!["bool"; 2 * n].join(", ");
    program(&format!("enum P: u8 {{ A({positions}) = 0 }}"), "P", &arms)
}

/// `1 | 2 | 3`.
fn one_to_three() -> Pat {
    or(vec![leaf("1"), leaf("2"), leaf("3")])
}

/// An arm of ors(k): `Q::A(1 | 2 | 3, ..., 1 | 2 | 3)` with `k` positions,
/// followed by the patterns of `last`, each a position more.
fn ors_arm(k: usize, last: Vec<Pat>) -> Pat {
    let subs = std::iter::repeat_with(one_to_three)
        .take(k)
        .chain(last)
        .collect();
    variant("Q::A", subs)
}

/// `Q::A(_, ..., _, <last>)`, with `k` `_` before `last`.
fn wild_arm(k: usize, last: &str) -> Pat {
    let subs = std::iter::repeat_n(leaf("_"), k)
        .chain(std::iter::once(leaf(last)))
        .collect();
    variant("Q::A", subs)
}

/// ors(k): `enum Q: u8 { A(u8, ..., u8) = 0 }` with `k` positions, and
/// then those of `last`, matched with `arms`.
fn ors(k: usize, last: &[&str], arms: &[Pat]) -> Program {
    let positions: Vec<&str> = std::iter::repeat_n("u8", k)
        .chain(last.iter().copied())
        .collect();
    let decls = format!("enum Q: u8 {{ A({}) = 0 }}", positions.join(", "));
    program(&decls, "Q", arms)
}

/// The rendered diagnostics of `text` compiled up to `stage`; the program
/// must fail.
fn errors(text: &str, stage: Stage) -> String {
    match driver::dump(
        Path::new("match_cost.ers"),
        text,
        stage,
        OptLevel::None,
        false,
    ) {
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

/// The number of MIR blocks of `f` in `program`, whose scrutinee's enum
/// has `variants` variants, which must be at most 4 × (p + v), p counting
/// the patterns of its `match` and v those variants (ADR 0042).
fn assert_linear(program: &Program, variants: usize) {
    let dump = match driver::dump(
        Path::new("match_cost.ers"),
        &program.text,
        Stage::Mir,
        OptLevel::None,
        false,
    ) {
        Ok(dump) => dump,
        Err(diags) => panic!("expected no diagnostics, got:\n{diags}"),
    };
    let start = dump
        .find("\nfn f(")
        .unwrap_or_else(|| panic!("no `fn f` in:\n{dump}"));
    let body = &dump[start..];
    let body = &body[..body.find("\n}\n").expect("a body ends")];
    let blocks = body
        .lines()
        .filter(|line| {
            line.trim()
                .strip_prefix("bb")
                .and_then(|rest| rest.strip_suffix(": {"))
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
        .count();
    let bound = 4 * (program.patterns + variants);
    assert!(
        blocks <= bound,
        "{blocks} blocks, more than 4 × ({} + {variants}) = {bound}",
        program.patterns
    );
}

#[test]
fn pairs_without_a_wildcard_arm_lists_three_witnesses_and_more() {
    // 2^32 witnesses, which the M2.4b compiler enumerated to count them.
    let diags = only(&pairs(32, &[]).text, "ty::non-exhaustive-match");
    let label = diags
        .lines()
        .find_map(|line| line.split_once("missing: ").map(|(_, list)| list))
        .unwrap_or_else(|| panic!("no `missing` label in:\n{diags}"));
    assert_eq!(label.matches("`P::A(").count(), 3, "{label}");
    assert!(label.ends_with("` and more"), "{label}");
    // The message lists the same, on lines miette wraps at 100 columns.
    let message = message(&diags);
    assert_eq!(
        message,
        format!("non-exhaustive `match`: {label} not covered"),
        "{diags}"
    );
}

/// The message of the one diagnostic in `diags`, its wrapped lines joined.
fn message(diags: &str) -> String {
    let mut lines = diags.lines().skip_while(|line| !line.starts_with("  × "));
    let first = lines
        .next()
        .unwrap_or_else(|| panic!("no message in:\n{diags}"));
    std::iter::once(&first["  × ".len()..])
        .chain(lines.map_while(|line| line.strip_prefix("  │ ")))
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn ors_written_twice_is_unreachable() {
    // The M2.4b compiler tried the 3^32 combinations of the second arm's
    // alternatives.
    let arm = ors_arm(32, vec![]);
    let text = ors(32, &[], &[arm.clone(), arm, leaf("_")]).text;
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
    let three_or_four = || ors_arm(31, vec![or(vec![leaf("3"), leaf("4")])]);
    let arms = [ors_arm(32, vec![]), three_or_four(), leaf("_")];
    let text = ors(32, &[], &arms).text;
    let diags = only(&text, "ty::unreachable-pattern");
    assert!(diags.contains("unreachable pattern"), "{diags}");
    // The alternative `3`, the last pattern of line 6 but `4`.
    let column = 9 + three_or_four().text.find("3 | 4)").unwrap();
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
        ors_arm(32, vec![leaf("true")]),
        wild_arm(32, "false"),
        wild_arm(32, "true"),
    ];
    let text = ors(32, &["bool"], &arms).text;
    if let Err(diags) = driver::dump(
        Path::new("match_cost.ers"),
        &text,
        Stage::Typeck,
        OptLevel::None,
        false,
    ) {
        panic!("expected no diagnostics, got:\n{diags}");
    }
}

#[test]
fn pairs_code_is_linear() {
    // The M2.4b decision tree copied each arm under every case of the
    // positions before its own: 2064 blocks for pairs(10) already.
    assert_linear(&pairs(32, &[leaf("_")]), 1);
}

#[test]
fn ors_code_is_linear() {
    // The M2.4b decision tree made each combination of alternatives an
    // arm of its own: 3289 blocks for ors(8) already.
    let arms = [ors_arm(32, vec![]), leaf("_")];
    assert_linear(&ors(32, &[], &arms), 1);
}

#[test]
fn nested_or_pattern_of_variants_code_is_linear() {
    // `N::A(N1::A(N2::A(1 | 2) | N2::B(3)) | N1::B(N2::A(4))) | N::B(_)`
    // and a `_` arm, over `N`, `N1` and `N2`, each of two variants.
    let decls = "enum N2: u8 { A(u8) = 0, B(u8) = 1 } \
                 enum N1: u8 { A(N2) = 0, B(N2) = 1 } \
                 enum N: u8 { A(N1) = 0, B(N1) = 1 }";
    let n2 = or(vec![
        variant("N2::A", vec![or(vec![leaf("1"), leaf("2")])]),
        variant("N2::B", vec![leaf("3")]),
    ]);
    let n1 = or(vec![
        variant("N1::A", vec![n2]),
        variant("N1::B", vec![variant("N2::A", vec![leaf("4")])]),
    ]);
    let arm = or(vec![
        variant("N::A", vec![n1]),
        variant("N::B", vec![leaf("_")]),
    ]);
    assert_linear(&program(decls, "N", &[arm, leaf("_")]), 2);
}
