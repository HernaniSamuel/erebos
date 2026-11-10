//! The analysis of moves runs where ADR 0099 puts it, inside `query`, before
//! compile-time evaluation can run a body: in `mir_body` for a function, and
//! in `comptime_value` for a `comptime` expression. Each program is compiled
//! as a whole, as `tests/ui.rs` does; the analysis is never called by the
//! test. A body that was evaluated would add `eval::panic`, since each one
//! reaches a division by zero after its use of a moved value.

use std::path::Path;

use erebos::driver::{self, OptLevel, Stage};

/// The diagnostics of `source`, which must be rejected.
fn errors(source: &str) -> String {
    match driver::dump(
        Path::new("main.ers"),
        source,
        Stage::Mir,
        driver::DEFAULT_TARGET,
        OptLevel::None,
        false,
    ) {
        Ok(_) => panic!("the program was accepted"),
        Err(errors) => errors,
    }
}

const PRELUDE: &str = "
struct Handle { n: u32 }
fn consume(h: Handle) {}
fn zero() -> u32 { 0 }
entry fn main() -> ! { loop {} }
panic fn on_panic() -> ! { loop {} }
";

#[test]
fn a_function_is_analysed_before_a_constant_evaluates_it() {
    let source = format!(
        "{PRELUDE}
fn twice() -> u32 {{
    let h: Handle = Handle {{ n: 1 }};
    consume(h);
    consume(h);
    10 / zero()
}}
const FROM_BODY: u32 = twice();
"
    );
    let errors = errors(&source);
    assert_eq!(
        errors.matches("analysis::use-after-move").count(),
        1,
        "{errors}"
    );
    assert!(!errors.contains("eval::"), "{errors}");
}

#[test]
fn a_comptime_expression_is_analysed_once_before_it_is_evaluated() {
    let source = format!(
        "{PRELUDE}
fn value() -> u32 {{
    comptime {{
        let h: Handle = Handle {{ n: 1 }};
        consume(h);
        consume(h);
        10 / zero()
    }}
}}
fn first() -> u32 {{ value() }}
fn second() -> u32 {{ value() }}
"
    );
    let errors = errors(&source);
    assert_eq!(
        errors.matches("analysis::use-after-move").count(),
        1,
        "{errors}"
    );
    assert!(!errors.contains("eval::"), "{errors}");
}

#[test]
fn a_body_that_passes_is_evaluated() {
    let source = format!(
        "{PRELUDE}
fn once() -> u32 {{
    let h: Handle = Handle {{ n: 1 }};
    consume(h);
    10 / zero()
}}
const FROM_BODY: u32 = once();
"
    );
    let errors = errors(&source);
    assert!(!errors.contains("analysis::"), "{errors}");
    assert!(errors.contains("eval::panic"), "{errors}");
}
