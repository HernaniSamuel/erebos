//! Programs with an injected error are rejected (ADR 0069).
//!
//! A snippet that breaks one rule of the language is inserted into a valid
//! program of `tests/ui` or `tests/e2e`, and the compiler must report the
//! code of that rule.
//!
//! - `EREBOS_REJECTED_PROGRAMS`: how many base programs to inject into
//!   (default `DEFAULT_PROGRAMS`).
//! - `EREBOS_REJECTED_SEED`: the seed of the choice of programs and places
//!   (default 0).

mod common;

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use common::generator::Rng;
use erebos::driver::{self, OptLevel, Stage};

/// How many base programs a run injects into without
/// `EREBOS_REJECTED_PROGRAMS`.
const DEFAULT_PROGRAMS: usize = 40;

/// Where a snippet goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Place {
    /// Statements, inserted at a statement boundary inside a function body.
    Statements,
    /// Items, inserted at the end of the file.
    Items,
}

/// One rule of the language and the shortest text that breaks it.
struct Snippet {
    /// The code of the diagnostic the text must produce.
    code: &'static str,
    /// The stage whose output shows the error: the base is compiled to it.
    stage: Stage,
    place: Place,
    /// Uses only literals and names of its own (`injected_*`, `Inj*`).
    text: &'static str,
}

/// A catalog entry: the code, the stage, the place and the text.
macro_rules! s {
    ($code:literal, $stage:ident, $place:ident, $text:expr) => {
        Snippet {
            code: $code,
            stage: Stage::$stage,
            place: Place::$place,
            text: $text,
        }
    };
}

/// The snippets, one or more for a code of `docs/reference.md`. The names
/// are of the form `injected_*` so that no base program has them.
const CATALOG: &[Snippet] = &[
    Snippet {
        code: "ty::shift-out-of-range",
        stage: Stage::Mir,
        place: Place::Statements,
        text: "let injected_shift: u32 = 1 << 32;",
    },
    Snippet {
        code: "ty::division-by-zero",
        stage: Stage::Mir,
        place: Place::Statements,
        text: "let injected_div: u32 = 7 / 0;",
    },
    Snippet {
        code: "ty::index-out-of-bounds",
        stage: Stage::Mir,
        place: Place::Statements,
        text: "let injected_array: [u8; 2] = [1, 2];\nlet injected_item: u8 = injected_array[2];",
    },
    Snippet {
        code: "ty::field-value-out-of-range",
        stage: Stage::Mir,
        place: Place::Items,
        text: "struct InjReg: u8 {\n    pub count: u8 at 0..=3 = 0,\n}\n\n\
               fn injected_field() -> InjReg {\n    InjReg { count: 16, .. }\n}",
    },
    // Lexer.
    s!(
        "lexer::unexpected-character",
        Ast,
        Statements,
        "let injected_a: u32 = 1 $;"
    ),
    s!(
        "lexer::unterminated-comment",
        Ast,
        Statements,
        "/* injected"
    ),
    s!(
        "lexer::empty-int",
        Ast,
        Statements,
        "let injected_a: u32 = 0x;"
    ),
    s!(
        "lexer::invalid-digit",
        Ast,
        Statements,
        "let injected_a: u32 = 0b2;"
    ),
    s!(
        "lexer::leading-zero",
        Ast,
        Statements,
        "let injected_a: u32 = 012;"
    ),
    s!(
        "lexer::int-too-large",
        Ast,
        Statements,
        "let injected_a: u64 = 99999999999999999999;"
    ),
    s!(
        "lexer::invalid-byte",
        Ast,
        Statements,
        "let injected_a: u8 = b'\\q';"
    ),
    s!(
        "lexer::unterminated-byte",
        Ast,
        Statements,
        "let injected_a: u8 = b'a;"
    ),
    s!(
        "lexer::unterminated-byte-string",
        Ast,
        Statements,
        "let injected_a: u8 = b\"abc;"
    ),
    // Parser.
    s!(
        "parser::unexpected-token",
        Ast,
        Statements,
        "let injected_a: u32 = ;"
    ),
    s!("parser::unclosed-block", Ast, Items, "fn injected_f() {"),
    s!(
        "parser::let-without-type",
        Ast,
        Statements,
        "let injected_a = 1;"
    ),
    s!(
        "parser::chained-comparison",
        Ast,
        Statements,
        "let injected_a: bool = 1 < 2 < 3;"
    ),
    s!(
        "parser::local-const",
        Ast,
        Statements,
        "const INJ_A: u32 = 1;"
    ),
    s!(
        "parser::local-static",
        Ast,
        Statements,
        "static INJ_A: u32 = 1;"
    ),
    s!("parser::local-mod", Ast, Statements, "mod injected_m {}"),
    s!(
        "parser::local-use",
        Ast,
        Statements,
        "use core::injected_m;"
    ),
    s!(
        "parser::local-memory",
        Ast,
        Statements,
        "memory { region INJ { origin: 0, size: 16 } }"
    ),
    s!(
        "parser::local-peripheral",
        Ast,
        Statements,
        "peripheral InjP {}"
    ),
    s!(
        "parser::misplaced-pub",
        Ast,
        Items,
        "pub entry fn injected_entry() -> ! {\n    loop {}\n}"
    ),
    s!(
        "parser::enum-without-type",
        Ast,
        Items,
        "enum InjE { A = 1 }"
    ),
    s!(
        "parser::variant-without-value",
        Ast,
        Items,
        "enum InjE: u8 { A }"
    ),
    s!(
        "parser::empty-payload",
        Ast,
        Items,
        "enum InjE: u8 { A() = 1 }"
    ),
    // Names and items.
    s!(
        "resolve::unknown-name",
        Hir,
        Statements,
        "let injected_a: u32 = injected_missing;"
    ),
    s!(
        "resolve::unknown-type",
        Hir,
        Statements,
        "let injected_a: InjMissing = 1;"
    ),
    s!(
        "resolve::unknown-path",
        Hir,
        Statements,
        "let injected_a: u32 = injected_m::x;"
    ),
    s!(
        "resolve::unknown-function",
        Hir,
        Statements,
        "injected_missing();"
    ),
    s!(
        "resolve::not-a-function",
        Hir,
        Statements,
        "let injected_a: u32 = 1;\ninjected_a();"
    ),
    s!(
        "resolve::type-argument-count",
        Hir,
        Statements,
        "let injected_a: Option = 1;"
    ),
    s!(
        "resolve::super-at-root",
        Hir,
        Items,
        "fn injected_f() {\n    injected_x = super::x;\n}"
    ),
    s!(
        "resolve::misused-module",
        Hir,
        Items,
        "mod injected_m {}\nfn injected_f() {\n    let injected_a: injected_m = 1;\n}"
    ),
    s!(
        "resolve::duplicate-item",
        Hir,
        Items,
        "fn injected_dup() {}\nfn injected_dup() {}"
    ),
    s!(
        "resolve::duplicate-type",
        Hir,
        Items,
        "struct u32 { a: u8 }"
    ),
    s!(
        "resolve::use-variant",
        Hir,
        Items,
        "enum InjE: u8 { A = 1 }\nuse InjE::A;"
    ),
    s!(
        "resolve::use-cycle",
        Hir,
        Items,
        "mod injected_m {\n    pub use super::injected_n::a;\n}\n\
         mod injected_n {\n    pub use super::injected_m::a;\n}"
    ),
    s!(
        "resolve::private-item",
        Hir,
        Items,
        "mod injected_m {\n    fn hidden() {}\n}\nfn injected_f() {\n    injected_m::hidden();\n}"
    ),
    s!(
        "resolve::private-reexport",
        Hir,
        Items,
        "mod injected_m {\n    fn hidden() {}\n    pub mod n {\n        pub use super::hidden;\n    }\n}"
    ),
    s!(
        "resolve::private-field",
        Hir,
        Items,
        "mod injected_m {\n    pub struct S { a: u8 }\n}\nfn injected_f() {\n    \
         let injected_s: injected_m::S = injected_m::S { a: 1 };\n}"
    ),
    s!(
        "resolve::duplicate-param",
        Hir,
        Items,
        "fn injected_f(a: u8, a: u8) {}"
    ),
    s!(
        "resolve::mut-extern-param",
        Hir,
        Items,
        "extern fn injected_f(mut a: u8);"
    ),
    s!(
        "resolve::duplicate-field",
        Hir,
        Items,
        "struct InjS { a: u8, a: u8 }"
    ),
    s!(
        "resolve::unknown-field",
        Hir,
        Items,
        "struct InjS { a: u8 }\nfn injected_f() {\n    let injected_s: InjS = InjS { a: 1, b: 2 };\n}"
    ),
    s!(
        "resolve::missing-fields",
        Hir,
        Items,
        "struct InjS { a: u8 }\nfn injected_f() {\n    let injected_s: InjS = InjS { };\n}"
    ),
    s!(
        "resolve::missing-default",
        Hir,
        Items,
        "struct InjS { a: u8 }\nfn injected_f() {\n    let injected_s: InjS = InjS { .. };\n}"
    ),
    s!(
        "resolve::bit-struct-type",
        Hir,
        Items,
        "struct InjB: i32 {\n    a: u8 at 0..=3,\n}"
    ),
    s!(
        "resolve::empty-bit-struct",
        Hir,
        Items,
        "struct InjB: u8 {}"
    ),
    s!(
        "resolve::write-effect-default",
        Hir,
        Items,
        "struct InjB: u8 {\n    a: u8 at 0..=3 write 1 clears = 1,\n}"
    ),
    s!(
        "resolve::enum-type",
        Hir,
        Items,
        "enum InjE: bool { A = 1 }"
    ),
    s!(
        "resolve::duplicate-variant",
        Hir,
        Items,
        "enum InjE: u8 { A = 1, A = 2 }"
    ),
    s!("resolve::empty-enum", Hir, Items, "enum InjE: u8 {}"),
    s!(
        "resolve::unknown-variant",
        Hir,
        Items,
        "enum InjE: u8 { A = 1 }\nfn injected_f() {\n    let injected_e: InjE = InjE::B;\n}"
    ),
    s!(
        "resolve::not-an-enum",
        Hir,
        Items,
        "struct InjS { a: u8 }\nfn injected_f() {\n    let injected_e: u8 = InjS::B;\n}"
    ),
    s!("resolve::reserved-name", Hir, Items, "fn __injected() {}"),
    s!(
        "resolve::unknown-attribute",
        Hir,
        Items,
        "#[injected]\nstruct InjS { a: u8 }"
    ),
    s!(
        "resolve::misplaced-attribute",
        Hir,
        Items,
        "#[packed]\nstruct InjB: u8 {\n    a: u8 at 0..=3,\n}"
    ),
    s!(
        "resolve::attribute-argument",
        Hir,
        Items,
        "#[align]\nstruct InjS { a: u8 }"
    ),
    s!(
        "resolve::duplicate-attribute",
        Hir,
        Items,
        "#[packed]\n#[packed]\nstruct InjS { a: u8 }"
    ),
    s!(
        "resolve::conflicting-attributes",
        Hir,
        Items,
        "#[packed]\n#[align(8)]\nstruct InjS { a: u8 }"
    ),
    s!(
        "resolve::outside-loop",
        Hir,
        Items,
        "fn injected_f() {\n    break;\n}"
    ),
    s!(
        "resolve::break-value-in-while",
        Hir,
        Items,
        "fn injected_f() {\n    while true {\n        break 1;\n    }\n}"
    ),
    s!(
        "resolve::return-in-comptime",
        Hir,
        Items,
        "const INJ_C: u32 = comptime { return 1; };"
    ),
    // Types and checks on bodies.
    s!(
        "ty::mismatched-types",
        Typeck,
        Statements,
        "let injected_a: u8 = true;"
    ),
    s!("ty::untyped-literal", Typeck, Statements, "1;"),
    s!(
        "ty::literal-out-of-range",
        Typeck,
        Statements,
        "let injected_a: u8 = 256;"
    ),
    s!(
        "ty::invalid-operand",
        Typeck,
        Statements,
        "let injected_a: bool = true + false;"
    ),
    s!(
        "ty::negate-unsigned",
        Typeck,
        Statements,
        "let injected_n: u32 = 1;\nlet injected_a: u32 = -injected_n;"
    ),
    s!(
        "ty::invalid-cast",
        Typeck,
        Statements,
        "let injected_n: u32 = 1;\nlet injected_a: bool = injected_n as bool;"
    ),
    s!(
        "ty::pointer-cast-width",
        Typeck,
        Statements,
        "let injected_n: u32 = 1;\nlet injected_p: *const u8 = injected_n as *const u8;"
    ),
    s!(
        "ty::deref-non-pointer",
        Typeck,
        Statements,
        "let injected_n: u32 = 1;\nlet injected_a: u32 = *injected_n;"
    ),
    s!(
        "ty::cannot-index",
        Typeck,
        Statements,
        "let injected_n: u32 = 1;\nlet injected_a: u32 = injected_n[0];"
    ),
    s!(
        "ty::no-fields",
        Typeck,
        Statements,
        "let injected_n: u32 = 1;\nlet injected_a: u32 = injected_n.x;"
    ),
    s!(
        "ty::unknown-field",
        Typeck,
        Items,
        "struct InjS { a: u8 }\nfn injected_f(s: InjS) -> u8 {\n    s.b\n}"
    ),
    s!(
        "ty::write-through-const",
        Typeck,
        Statements,
        "let injected_n: u32 = 1;\nlet injected_p: *const u32 = &raw const injected_n;\n*injected_p = 2;"
    ),
    s!("ty::not-assignable", Typeck, Statements, "1 = 2;"),
    s!(
        "ty::assign-immutable",
        Typeck,
        Statements,
        "let injected_n: u32 = 1;\ninjected_n = 2;"
    ),
    s!(
        "ty::not-a-place",
        Typeck,
        Statements,
        "let injected_p: *const u32 = &raw const 1;"
    ),
    s!(
        "ty::address-of-immutable",
        Typeck,
        Statements,
        "let injected_n: u32 = 1;\nlet injected_p: *mut u32 = &raw mut injected_n;"
    ),
    s!("ty::never-in-memory", Typeck, Items, "struct InjN { a: ! }"),
    s!(
        "ty::too-large",
        Typeck,
        Items,
        "struct InjBig { a: [u8; 0x8000_0000] }"
    ),
    s!(
        "ty::infinite-size",
        Typeck,
        Items,
        "struct InjI { a: InjI }"
    ),
    s!("ty::unsized", Typeck, Items, "struct InjU { a: [u8] }"),
    s!(
        "ty::invalid-align",
        Typeck,
        Items,
        "#[align(3)]\nstruct InjA { a: u8 }"
    ),
    s!(
        "ty::aligned-in-packed",
        Typeck,
        Items,
        "#[align(8)]\nstruct InjA { a: u8 }\n#[packed]\nstruct InjP { a: InjA }"
    ),
    s!(
        "ty::not-comptime",
        Typeck,
        Statements,
        "let injected_n: u32 = 1;\nlet injected_a: u32 = comptime { injected_n };"
    ),
    s!(
        "ty::cycle",
        Typeck,
        Items,
        "const INJ_A: u32 = INJ_B;\nconst INJ_B: u32 = INJ_A;"
    ),
    s!(
        "ty::duplicate-discriminant",
        Typeck,
        Items,
        "enum InjE: u8 { A = 1, B = 1 }"
    ),
    s!(
        "ty::match-type",
        Typeck,
        Statements,
        "let injected_a: [u8; 1] = [1];\nmatch injected_a {\n    _ => {}\n}"
    ),
    s!(
        "ty::non-exhaustive-match",
        Typeck,
        Statements,
        "let injected_n: u8 = 1;\nmatch injected_n {\n    1 => {}\n}"
    ),
    s!(
        "ty::unreachable-pattern",
        Typeck,
        Statements,
        "let injected_n: u8 = 1;\nmatch injected_n {\n    _ => {}\n    1 => {}\n}"
    ),
    s!(
        "ty::try-operand",
        Typeck,
        Items,
        "fn injected_f() -> Option(u8) {\n    let injected_n: u32 = 1;\n    let injected_a: u32 = injected_n?;\n    .None\n}"
    ),
    s!(
        "ty::try-return",
        Typeck,
        Items,
        "fn injected_f() {\n    let injected_o: Option(u8) = .None;\n    let injected_a: u8 = injected_o?;\n}"
    ),
    s!(
        "ty::if-without-else",
        Typeck,
        Statements,
        "let injected_a: u32 = if true { 1 };"
    ),
    s!(
        "ty::wrong-arg-count",
        Typeck,
        Items,
        "fn injected_g() {}\nfn injected_f() {\n    injected_g(1);\n}"
    ),
    s!("ty::param-type", Typeck, Items, "fn injected_f(a: ()) {}"),
    s!("ty::untyped-variant", Typeck, Statements, ".V;"),
    s!(
        "ty::wrong-payload-count",
        Typeck,
        Items,
        "enum InjE: u8 { A(u8) = 1 }\nfn injected_f() {\n    let injected_e: InjE = InjE::A(1, 2);\n}"
    ),
    s!(
        "ty::private-field",
        Typeck,
        Items,
        "mod injected_m {\n    pub struct S { pub a: u8, b: u8 }\n    pub fn make() -> S {\n        S { a: 1, b: 2 }\n    }\n}\n\
         fn injected_f() -> u8 {\n    injected_m::make().b\n}"
    ),
    s!(
        "ty::private-variant",
        Typeck,
        Items,
        "mod injected_m {\n    enum InjE: u8 { A = 1 }\n    pub fn injected_g(e: InjE) {}\n}\n\
         fn injected_f() {\n    injected_m::injected_g(.A);\n}"
    ),
    s!(
        "ty::slice-pointer-index",
        Typeck,
        Items,
        "fn injected_f(s: *const [u8]) -> u8 {\n    s[0]\n}"
    ),
    s!(
        "ty::packed-field-address",
        Typeck,
        Items,
        "#[packed]\nstruct InjS { a: u8, b: u32 }\nfn injected_f(p: *const InjS) {\n    \
         let injected_q: *const u32 = &raw const (*p).b;\n}"
    ),
    s!(
        "ty::volatile-too-wide",
        Typeck,
        Items,
        "fn injected_f(p: *mut volatile u64) -> u64 {\n    *p\n}"
    ),
    s!(
        "ty::volatile-aggregate",
        Typeck,
        Items,
        "struct InjS { a: u8 }\nfn injected_f(p: *const volatile InjS) -> InjS {\n    *p\n}"
    ),
    s!(
        "ty::volatile-packed-field",
        Typeck,
        Items,
        "#[packed]\nstruct InjS { a: u8, b: u32 }\nfn injected_f(p: *const volatile InjS) -> u32 {\n    (*p).b\n}"
    ),
    s!("resolve::module-file", Hir, Items, "mod injected_missing;"),
    s!(
        "resolve::duplicate-symbol",
        Hir,
        Items,
        "mod injected_m {\n    #[export]\n    pub fn injected_x() {}\n}\n#[export]\nfn injected_x() {}"
    ),
    s!(
        "resolve::not-a-constant",
        Hir,
        Statements,
        "let injected_n: u8 = 1;\nmatch injected_n {\n    injected_n => {}\n    _ => {}\n}"
    ),
    s!(
        "resolve::duplicate-binding",
        Hir,
        Items,
        "enum InjE: u8 { A(u8, u8) = 1 }\nfn injected_f(e: InjE) {\n    match e {\n        \
         InjE::A(let a: u8, let a: u8) => {}\n    }\n}"
    ),
    s!(
        "resolve::inconsistent-binding",
        Hir,
        Items,
        "enum InjE: u8 { A(u8) = 1, B = 2 }\nfn injected_f(e: InjE) {\n    match e {\n        \
         InjE::A(let a: u8) | InjE::B => {}\n    }\n}"
    ),
    s!(
        "resolve::multiple-entries",
        Hir,
        Items,
        "entry fn injected_e1() -> ! {\n    loop {}\n}\nentry fn injected_e2() -> ! {\n    loop {}\n}"
    ),
    s!(
        "resolve::multiple-panics",
        Hir,
        Items,
        "panic fn injected_p1() -> ! {\n    loop {}\n}\npanic fn injected_p2() -> ! {\n    loop {}\n}"
    ),
    s!(
        "resolve::duplicate-memory",
        Hir,
        Items,
        "memory {\n    region INJ { origin: 0x9000_0000, size: 16 }\n}\nmemory {\n    region INJ { origin: 0x9000_0000, size: 16 }\n}"
    ),
    s!(
        "resolve::duplicate-region",
        Hir,
        Items,
        "memory {\n    region INJ { origin: 0x9000_0000, size: 16 }\n    region INJ { origin: 0x9000_0100, size: 16 }\n}"
    ),
    s!(
        "resolve::unknown-region",
        Hir,
        Items,
        "memory {\n    region INJ { origin: 0x9000_0000, size: 16 }\n    code in INJ_NONE;\n}"
    ),
    s!(
        "resolve::duplicate-placement",
        Hir,
        Items,
        "memory {\n    region INJ { origin: 0x9000_0000, size: 16 }\n    code in INJ;\n    code in INJ;\n}"
    ),
    s!(
        "resolve::missing-placement",
        Hir,
        Items,
        "memory {\n    region INJ { origin: 0x9000_0000, size: 16 }\n}"
    ),
    s!(
        "resolve::stack-with-code",
        Hir,
        Items,
        "memory {\n    region INJ { origin: 0x9000_0000, size: 64 }\n    code in INJ;\n    stack in INJ { size: 16 }\n}"
    ),
    s!(
        "ty::unknown-variant",
        Typeck,
        Items,
        "enum InjE: u8 { A = 1 }\nfn injected_f() {\n    let injected_e: InjE = .B;\n}"
    ),
    s!(
        "ty::constant-pattern-type",
        Typeck,
        Items,
        "const INJ_P: *const u8 = 0 as *const u8;\nfn injected_f(p: *const u8) {\n    match p {\n        INJ_P => {}\n        _ => {}\n    }\n}"
    ),
    // Compile-time evaluation.
    s!(
        "eval::assertion-failed",
        Mir,
        Statements,
        "let injected_a: u32 = comptime {\n    assert(false);\n    1\n};"
    ),
    s!(
        "eval::panic",
        Mir,
        Items,
        "fn injected_zero() -> u32 {\n    0\n}\nconst INJ_C: u32 = comptime { 7 / injected_zero() };"
    ),
    s!(
        "eval::call-depth",
        Mir,
        Items,
        "fn injected_r(n: u32) -> u32 {\n    injected_r(n)\n}\nconst INJ_C: u32 = comptime { injected_r(1) };"
    ),
    s!(
        "eval::extern-call",
        Mir,
        Items,
        "extern fn injected_x() -> u32;\nconst INJ_C: u32 = comptime { injected_x() };"
    ),
    s!(
        "eval::static-mut",
        Mir,
        Items,
        "static mut INJ_S: u32 = 0;\nconst INJ_C: u32 = comptime { INJ_S };"
    ),
    s!(
        "eval::extern-static",
        Mir,
        Items,
        "extern static INJ_S: u32;\nconst INJ_C: u32 = comptime { INJ_S };"
    ),
    s!(
        "eval::read-only-write",
        Mir,
        Items,
        "static INJ_S: u32 = 0;\nconst INJ_C: u32 = comptime {\n    let p: *mut u32 = &raw const INJ_S as *mut u32;\n    *p = 1;\n    0\n};"
    ),
    s!(
        "eval::invalid-access",
        Mir,
        Items,
        "const INJ_C: u32 = comptime {\n    let p: *const u32 = 0x1000 as *const u32;\n    *p\n};"
    ),
    s!(
        "eval::pointer-to-int",
        Mir,
        Items,
        "const INJ_C: u32 = comptime {\n    let n: u32 = 1;\n    let a: usize = &raw const n as usize;\n    a as u32\n};"
    ),
    s!(
        "eval::volatile-access",
        Mir,
        Items,
        "const INJ_C: u32 = comptime {\n    let p: *const volatile u32 = 0x1000 as *const volatile u32;\n    *p\n};"
    ),
    s!(
        "eval::pointer-escapes",
        Mir,
        Items,
        "const INJ_C: *const u32 = comptime {\n    let n: u32 = 1;\n    &raw const n\n};"
    ),
    // Bit-field structs.
    s!(
        "ty::bit-field-type",
        Typeck,
        Items,
        "struct InjB: u8 {\n    a: i8 at 0..=3,\n}"
    ),
    s!(
        "ty::reversed-bit-range",
        Typeck,
        Items,
        "struct InjB: u8 {\n    a: u8 at 3..=0,\n}"
    ),
    s!(
        "ty::bit-field-outside",
        Typeck,
        Items,
        "struct InjB: u8 {\n    a: u8 at 4..=9,\n}"
    ),
    s!(
        "ty::bit-field-width",
        Typeck,
        Items,
        "struct InjB: u8 {\n    a: bool at 0..=1,\n}"
    ),
    s!(
        "ty::bit-field-overlap",
        Typeck,
        Items,
        "struct InjB: u8 {\n    a: u8 at 0..=3,\n    b: u8 at 3..=5,\n}"
    ),
    s!(
        "ty::bit-field-offset",
        Typeck,
        Items,
        "struct InjB: u8 {\n    a: u8 at 0..=3,\n}\nfn injected_f() -> usize {\n    offset_of(InjB, a)\n}"
    ),
    s!(
        "ty::bit-field-address",
        Typeck,
        Items,
        "struct InjB: u8 {\n    a: u8 at 0..=3,\n}\nfn injected_f(p: *mut InjB) {\n    \
         let injected_q: *mut u8 = &raw mut (*p).a;\n}"
    ),
    // Peripherals.
    s!(
        "resolve::duplicate-register",
        Hir,
        Items,
        "peripheral InjP {\n    read a: u8 at 0,\n    read a: u8 at 1,\n}"
    ),
    s!(
        "resolve::not-a-peripheral-type",
        Hir,
        Items,
        "peripheral INJ_P: u32 at 0x1000;"
    ),
    s!(
        "ty::register-type",
        Typeck,
        Items,
        "peripheral InjP {\n    read a: u64 at 0,\n}"
    ),
    s!(
        "ty::misaligned-register",
        Typeck,
        Items,
        "peripheral InjP {\n    read a: u32 at 1,\n}"
    ),
    s!(
        "ty::register-overlap",
        Typeck,
        Items,
        "peripheral InjP {\n    read write a: u8 at 0,\n    read write b: u8 at 0,\n}"
    ),
    s!(
        "ty::misaligned-peripheral",
        Typeck,
        Items,
        "peripheral InjP {\n    read a: u32 at 0,\n}\nperipheral INJ_P: InjP at 0x1001;"
    ),
    s!(
        "ty::peripheral-out-of-range",
        Typeck,
        Items,
        "peripheral InjP {\n    read a: u32 at 0,\n}\nperipheral INJ_P: InjP at 0xFFFF_FFFC;\n\
         peripheral INJ_Q: InjP at 0xFFFF_FFFE;"
    ),
    s!(
        "ty::peripheral-overlap",
        Typeck,
        Items,
        "peripheral InjP {\n    read a: u32 at 0,\n}\nperipheral INJ_P: InjP at 0xE000_0000;\n\
         peripheral INJ_Q: InjP at 0xE000_0000;"
    ),
    s!(
        "ty::peripheral-by-value",
        Typeck,
        Items,
        "peripheral InjP {\n    read a: u32 at 0,\n}\nfn injected_f(p: InjP) {}"
    ),
    s!(
        "ty::volatile-peripheral",
        Typeck,
        Items,
        "peripheral InjP {\n    read a: u32 at 0,\n}\nfn injected_f(p: *mut volatile InjP) {}"
    ),
    s!(
        "ty::peripheral-index",
        Typeck,
        Items,
        "peripheral InjP {\n    read a: u32 at 0,\n}\nfn injected_f(p: *mut InjP) {\n    let injected_x: InjP = p[1];\n}"
    ),
    s!(
        "ty::unknown-register",
        Typeck,
        Items,
        "peripheral InjP {\n    read a: u32 at 0,\n}\nfn injected_f(p: *mut InjP) -> u32 {\n    (*p).zz\n}"
    ),
    s!(
        "ty::private-register",
        Typeck,
        Items,
        "mod injected_m {\n    pub peripheral InjP {\n        read a: u32 at 0,\n    }\n}\n\
         fn injected_f(p: *mut injected_m::InjP) -> u32 {\n    (*p).a\n}"
    ),
    s!(
        "ty::write-to-read-only",
        Typeck,
        Items,
        "peripheral InjP {\n    read a: u32 at 0,\n}\nfn injected_f(p: *mut InjP) {\n    (*p).a = 1;\n}"
    ),
    s!(
        "ty::read-of-write-only",
        Typeck,
        Items,
        "peripheral InjP {\n    write a: u32 at 0,\n}\nfn injected_f(p: *mut InjP) -> u32 {\n    (*p).a\n}"
    ),
    s!(
        "ty::register-address",
        Typeck,
        Items,
        "peripheral InjP {\n    read a: u32 at 0,\n}\nfn injected_f(p: *mut InjP) {\n    \
         let injected_q: *const u32 = &raw const (*p).a;\n}"
    ),
    s!(
        "ty::read-with-effects",
        Typeck,
        Items,
        "struct InjB: u8 {\n    f: bool at 0,\n}\nperipheral InjP {\n    read write r: InjB at 0 read clears,\n}\n\
         fn injected_f(p: *mut InjP) {\n    (*p).r.f = true;\n}"
    ),
    s!(
        "ty::compound-write-effect",
        Typeck,
        Items,
        "struct InjB: u8 {\n    f: bool at 0 write 1 clears,\n}\nperipheral InjP {\n    read write r: InjB at 0,\n}\n\
         fn injected_f(p: *mut InjP) {\n    (*p).r.f |= true;\n}"
    ),
    // Register arrays, clusters and `over`.
    s!(
        "resolve::missing-step",
        Hir,
        Items,
        "peripheral InjB {\n    read a: u32 at 0,\n}\nperipheral InjP {\n    c: [InjB; 2] at 0,\n}"
    ),
    s!(
        "resolve::misplaced-step",
        Hir,
        Items,
        "peripheral InjP {\n    read a: u32 at 0 step 4,\n}"
    ),
    s!(
        "resolve::not-a-peripheral",
        Hir,
        Items,
        "const INJ_C: u32 = 1;\nperipheral InjP {\n    read a: u32 at 0,\n}\n\
         peripheral INJ_P: InjP at 0x1000 over INJ_C;"
    ),
    s!(
        "resolve::over-itself",
        Hir,
        Items,
        "peripheral InjP {\n    read a: u32 at 0,\n}\nperipheral INJ_P: InjP at 0x1000 over INJ_P;"
    ),
    s!(
        "ty::misaligned-step",
        Typeck,
        Items,
        "peripheral InjP {\n    read a: [u32; 2] at 0 step 6,\n}"
    ),
    s!(
        "ty::step-too-small",
        Typeck,
        Items,
        "peripheral InjP {\n    read a: [u32; 2] at 0 step 0,\n}"
    ),
];

/// The valid programs to inject into: every root file of `tests/ui` and
/// `tests/e2e`, in a fixed order.
fn base_programs() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut paths = Vec::new();
    for dir in ["ui", "e2e"] {
        let mut entries: Vec<_> = fs::read_dir(root.join(dir))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            if path.extension().is_some_and(|e| e == "ers") {
                paths.push(path);
            } else if path.join("main.ers").is_file() {
                paths.push(path.join("main.ers"));
            }
        }
    }
    paths
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap().replace("\r\n", "\n")
}

/// A group of codes that have no snippet, and why.
struct Exempt {
    codes: &'static [&'static str],
    reason: &'static str,
}

/// The codes of `docs/reference.md` that the catalog does not have.
const EXEMPT: &[Exempt] = &[
    Exempt {
        codes: &["resolve::missing-entry", "resolve::missing-panic"],
        reason: "the error is the absence of an item, and a snippet can only add text; the base programs have the item (`tests/ui/resolve_missing_entry.ers` and `resolve_missing_panic.ers` cover them)",
    },
    Exempt {
        codes: &["resolve::call-entry"],
        reason: "the call names the program's entry function, whose name a snippet does not know; an `entry fn` of its own is `resolve::multiple-entries`",
    },
    Exempt {
        codes: &[
            "ty::special-fn-params",
            "ty::panic-param",
            "ty::entry-returns",
            "ty::panic-returns",
        ],
        reason: "they are about the program's one `entry fn` or `panic fn`; a snippet that adds a second one is `resolve::multiple-entries` or `multiple-panics`, reported before type checking, so the rule is never reached",
    },
    Exempt {
        codes: &[
            "ty::empty-region",
            "ty::region-out-of-range",
            "ty::region-overlap",
            "ty::misaligned-code",
            "ty::stack-size",
        ],
        reason: "they are about the program's `memory` block, and a program has one: a second block in a snippet is `resolve::duplicate-memory` in a base that has one, which stops before these checks",
    },
    Exempt {
        codes: &["eval::step-limit"],
        reason: "a loop of 10,000,000 steps takes seconds in a debug build, in every program it is injected into; `tests/ui/eval_comptime_loop.ers` covers it",
    },
    Exempt {
        codes: &[
            "eval::pointer-compare",
            "eval::dangling-pointer",
            "eval::uninit-read",
            "eval::pointer-out-of-bounds",
        ],
        reason: "they need an object, a pointer into it and a use, so a snippet would be a whole program of its own, which `tests/ui/eval_*.ers` already are",
    },
    Exempt {
        codes: &["codegen::llvm"],
        reason: "an environment problem (LLVM rejecting the target), not a rule of the language",
    },
    Exempt {
        codes: &["link::no-memory-map", "link::region-overflow"],
        reason: "they depend on the program as a whole (no `memory` block; what the linker places in a region) and are found when building, not by a stage of `dump`",
    },
];

/// Whether `text` compiles to `stage` without a diagnostic.
fn compiles(path: &Path, text: &str, stage: Stage) -> bool {
    driver::dump(path, text, stage, OptLevel::None, false).is_ok()
}

/// A block of braces while the text is scanned.
struct Block {
    /// It is the body of a function.
    function: bool,
    /// Its header (`if`, `while`, `match`) may name a value known at compile
    /// time, so that its code may never run and is not checked.
    maybe_unchecked: bool,
    /// A statement of it may not complete: what follows is dead code.
    dead: bool,
}

/// The names of the functions of `text` that return `!`.
fn never_functions(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|line| line.contains("-> !"))
        .filter_map(|line| line.split("fn ").nth(1))
        .filter_map(|rest| rest.split('(').next())
        .collect()
}

/// Whether the words of a statement mean that it may not complete, so that
/// what follows is dead code, which the checks of values known at compile
/// time do not look at: `return`, `break`, `continue`, a `loop`, a value of
/// type `!` or a call to a function that returns `!`.
fn diverges(code: &str, never: &[&str]) -> bool {
    code.contains(": !")
        || code
            .split(|k: char| !k.is_alphanumeric() && k != '_')
            .any(|w| matches!(w, "return" | "break" | "continue" | "loop") || never.contains(&w))
}

/// Whether the header of a block names something that may be known at
/// compile time: `true`, `false` or a constant (written in capitals).
fn names_a_constant(header: &str) -> bool {
    header
        .split(|k: char| !k.is_alphanumeric() && k != '_')
        .any(|w| {
            matches!(w, "true" | "false")
                || (w.len() > 1
                    && w.chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
        })
}

/// The offsets at which a statement may be inserted: just after the `{` that
/// opens the body of a function and just after each `;` that ends a line
/// inside one, each at the start of the next line. A place is left out when
/// the statement would be dead code (after a statement that diverges, in its
/// block) or inside a block whose condition may be a constant, where the
/// compiler does not check it (reference, "Checks on values known at
/// compile time"). Strings, character literals and comments are skipped.
fn statement_boundaries(text: &str) -> Vec<usize> {
    let never = never_functions(text);
    let mut stack: Vec<Block> = Vec::new();
    let mut header = String::new();
    let mut comment = 0usize;
    let mut points = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        // The last significant character of the line, and whether it opened
        // a function body.
        let mut last = None;
        // The line without comments and strings.
        let mut code = String::new();
        while i < chars.len() {
            let c = chars[i];
            let next = chars.get(i + 1).copied();
            if comment > 0 {
                if c == '*' && next == Some('/') {
                    comment -= 1;
                    i += 1;
                } else if c == '/' && next == Some('*') {
                    comment += 1;
                    i += 1;
                }
            } else if c == '/' && next == Some('/') {
                break;
            } else if c == '/' && next == Some('*') {
                comment = 1;
                i += 1;
            } else if c == '"' {
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    i += if chars[i] == '\\' { 2 } else { 1 };
                }
                last = Some(('"', false));
            } else if c == '\'' {
                // A character literal is `'x'` or `'\x'` with a short escape.
                let close = chars[i + 1..]
                    .iter()
                    .take(12)
                    .position(|&k| k == '\'')
                    .filter(|&p| p > 0 && !(p == 1 && chars[i + 1] == '\\'));
                if let Some(p) = close {
                    i += p + 1;
                }
                last = Some(('\'', false));
            } else if c == '{' {
                let function = header
                    .split(|k: char| !k.is_alphanumeric() && k != '_')
                    .any(|w| w == "fn");
                stack.push(Block {
                    function,
                    maybe_unchecked: !function && names_a_constant(&header),
                    dead: false,
                });
                code.push(c);
                header.clear();
                last = Some(('{', function));
            } else if c == '}' {
                stack.pop();
                code.push(c);
                header.clear();
                last = Some(('}', false));
            } else if c == ';' {
                code.push(c);
                header.clear();
                last = Some((';', false));
            } else {
                header.push(c);
                code.push(c);
                if !c.is_whitespace() {
                    last = Some((c, false));
                }
            }
            i += 1;
        }
        let diverging = diverges(&code, &never);
        let usable =
            stack.iter().any(|b| b.function) && !stack.iter().any(|b| b.maybe_unchecked || b.dead);
        if line.ends_with('\n') && usable {
            match last {
                Some(('{', true)) => points.push(offset + line.len()),
                Some((';', _)) if !diverging => points.push(offset + line.len()),
                _ => {}
            }
        }
        // The statements of this line are done: what follows them in the
        // block is dead when they diverge.
        if diverging && let Some(block) = stack.last_mut() {
            block.dead = true;
        }
        offset += line.len();
    }
    points
}

/// `base` with the snippet inserted, and the line it starts on.
fn inject(base: &str, snippet: &Snippet, rng: &mut Rng) -> Option<(String, usize)> {
    let at = match snippet.place {
        Place::Statements => {
            let points = statement_boundaries(base);
            if points.is_empty() {
                return None;
            }
            rng.pick(&points)
        }
        Place::Items => base.len(),
    };
    let mut text = String::with_capacity(base.len() + snippet.text.len() + 2);
    text.push_str(&base[..at]);
    if snippet.place == Place::Items && !base.ends_with('\n') {
        text.push('\n');
    }
    let line = text.matches('\n').count() + 1;
    text.push_str(snippet.text);
    text.push('\n');
    text.push_str(&base[at..]);
    Some((text, line))
}

/// The result of a run.
struct Report {
    /// How many snippets were injected and compiled.
    injections: usize,
    /// One message per injection that did not give its code.
    failures: Vec<String>,
}

/// Injects every snippet of `catalog` into `programs` base programs taken
/// from `bases` in an order fixed by `seed`; a base that does not compile
/// to a snippet's stage is skipped for that snippet.
fn run(catalog: &[Snippet], bases: &[PathBuf], programs: usize, seed: u64) -> Report {
    let mut rng = Rng::new(seed);
    let mut order: Vec<usize> = (0..bases.len()).collect();
    for i in (1..order.len()).rev() {
        order.swap(i, rng.below(i + 1));
    }

    let mut report = Report {
        injections: 0,
        failures: Vec::new(),
    };
    let mut used = 0;
    for index in order {
        if used == programs {
            break;
        }
        let path = &bases[index];
        let base = read(path);
        let mut valid: HashMap<String, bool> = HashMap::new();
        let mut injected = false;
        for snippet in catalog {
            let ok = *valid
                .entry(snippet.stage.name().to_owned())
                .or_insert_with(|| compiles(path, &base, snippet.stage));
            if !ok {
                continue;
            }
            let Some((text, line)) = inject(&base, snippet, &mut rng) else {
                continue;
            };
            injected = true;
            report.injections += 1;
            let shown = path
                .strip_prefix(env!("CARGO_MANIFEST_DIR"))
                .unwrap_or(path);
            match driver::dump(path, &text, snippet.stage, OptLevel::None, false) {
                Ok(_) => report.failures.push(format!(
                    "{}:{line}: `{}` was accepted ({}):\n{}",
                    shown.display(),
                    snippet.code,
                    snippet.stage,
                    snippet.text
                )),
                Err(rendered) if !rendered.contains(snippet.code) => {
                    report.failures.push(format!(
                        "{}:{line}: `{}` was not reported ({}); the snippet:\n{}\nthe diagnostics:\n{rendered}",
                        shown.display(),
                        snippet.code,
                        snippet.stage,
                        snippet.text
                    ));
                }
                Err(_) => {}
            }
        }
        if injected {
            used += 1;
        }
    }
    report
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
fn injected_errors_are_rejected() {
    let programs = env_number("EREBOS_REJECTED_PROGRAMS", DEFAULT_PROGRAMS as u64) as usize;
    let seed = env_number("EREBOS_REJECTED_SEED", 0);
    let report = run(CATALOG, &base_programs(), programs, seed);
    assert!(
        report.injections >= programs.min(CATALOG.len()),
        "only {} injections were made; the base programs are not found",
        report.injections
    );
    assert!(
        report.failures.is_empty(),
        "{} of {} injected errors were not reported (seed {seed}; run again with \
         EREBOS_REJECTED_SEED={seed} EREBOS_REJECTED_PROGRAMS={programs}):\n\n{}",
        report.failures.len(),
        report.injections,
        report.failures.join("\n\n")
    );
}

/// The check can fail: a snippet that is not wrong, or that is wrong in
/// another way than its code says, is reported.
#[test]
fn a_broken_catalog_entry_is_reported() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e");
    let bases = vec![root.join("hello.ers")];

    // Valid text under the code of an error: the compiler accepts it.
    let accepted = [Snippet {
        code: "ty::division-by-zero",
        stage: Stage::Mir,
        place: Place::Statements,
        text: "let injected_div: u32 = 7 / 1;",
    }];
    let report = run(&accepted, &bases, 1, 0);
    assert_eq!(report.injections, 1);
    assert_eq!(report.failures.len(), 1, "{:?}", report.failures);
    assert!(report.failures[0].contains("was accepted"));

    // An error under another code: rejected, but not for the code asked.
    let wrong_code = [Snippet {
        code: "ty::shift-out-of-range",
        stage: Stage::Mir,
        place: Place::Statements,
        text: "let injected_div: u32 = 7 / 0;",
    }];
    let report = run(&wrong_code, &bases, 1, 0);
    assert_eq!(report.injections, 1);
    assert_eq!(report.failures.len(), 1, "{:?}", report.failures);
    assert!(report.failures[0].contains("was not reported"));
}

#[test]
fn statement_boundaries_are_inside_functions() {
    let text = "\
const A: u32 = 1;
struct S { x: u32 }
fn f(x: u32) -> u32 { // comment {
    let s = \"{\";
    let c = '{';
    let y = x;
    /* { */
    if y == 0 {
        return 1;
    }
    y
}

fn g(x: u32) {
    let a = x;
    if LIMIT > 2 {
        let b = x;
    }
    stop();
    let c = x;
}

fn stop() -> ! {
    loop {}
}
";
    let points = statement_boundaries(text);
    let lines: Vec<usize> = points
        .iter()
        .map(|p| text[..*p].matches('\n').count() + 1)
        .collect();
    // In `f`: after its opening and each `let`, not after the `return`, whose
    // block ends there. In `g`: after its opening and the first `let`; not
    // in the `if` on a constant, nor after the call to `stop`, which never
    // returns. The opening of `stop` is a place too.
    assert_eq!(lines, vec![4, 5, 6, 7, 15, 16, 24]);
}

/// The codes of the table of the section "Diagnostics" of the reference.
fn reference_codes() -> Vec<String> {
    let reference = read(&Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/reference.md"));
    let section = reference
        .split(
            "
## Diagnostics
",
        )
        .nth(1)
        .expect("docs/reference.md has no section `Diagnostics`");
    let section = section
        .split(
            "
## ",
        )
        .next()
        .unwrap();
    let mut codes = Vec::new();
    for row in section.lines().filter(|l| l.starts_with("| `")) {
        let cell = row.split('|').nth(1).unwrap();
        for code in cell.split('`').skip(1).step_by(2) {
            codes.push(code.to_owned());
        }
    }
    codes
}

/// What is wrong with the lists for the codes of the reference: a code in
/// neither, in both, or a name that is not a code.
fn rule_problems(codes: &[String], catalog: &[Snippet], exempt: &[Exempt]) -> Vec<String> {
    let mut problems = Vec::new();
    for code in codes {
        let snippet = catalog.iter().any(|s| s.code == code);
        let reason = exempt.iter().any(|e| e.codes.contains(&code.as_str()));
        if !snippet && !reason {
            problems.push(format!(
                "`{code}` has neither a snippet in CATALOG nor a reason in EXEMPT"
            ));
        } else if snippet && reason {
            problems.push(format!("`{code}` has a snippet and is also in EXEMPT"));
        }
    }
    // A name that is not in the table is a typo or a code that was removed.
    for snippet in catalog {
        if !codes.iter().any(|c| c == snippet.code) {
            problems.push(format!(
                "the snippet of `{}` names a code that is not in the reference",
                snippet.code
            ));
        }
    }
    for group in exempt {
        if group.reason.is_empty() {
            problems.push(format!("{:?} are exempt with no reason", group.codes));
        }
        for code in group.codes {
            if !codes.iter().any(|c| c == code) {
                problems.push(format!(
                    "EXEMPT names `{code}`, which is not in the reference"
                ));
            }
        }
    }
    problems
}

#[test]
fn every_code_has_a_rule() {
    let codes = reference_codes();
    assert!(
        codes.len() > 100,
        "only {} codes were read from the table of `Diagnostics`",
        codes.len()
    );
    let problems = rule_problems(&codes, CATALOG, EXEMPT);
    assert!(
        problems.is_empty(),
        "{}",
        problems.join(
            "
"
        )
    );
}

/// The check can fail: a code in neither list, in both, or a name that is no
/// code of the reference, is reported.
#[test]
fn a_code_without_a_rule_is_reported() {
    let codes = vec!["ty::first".to_owned(), "ty::second".to_owned()];
    let snippet = |code| Snippet {
        code,
        stage: Stage::Typeck,
        place: Place::Items,
        text: "",
    };
    let exempt = |codes| Exempt {
        codes,
        reason: "because",
    };

    let ok = rule_problems(&codes, &[snippet("ty::first")], &[exempt(&["ty::second"])]);
    assert!(ok.is_empty(), "{ok:?}");

    let missing = rule_problems(&codes, &[snippet("ty::first")], &[]);
    assert_eq!(missing.len(), 1, "{missing:?}");
    assert!(missing[0].contains("ty::second"));

    let both = rule_problems(
        &codes,
        &[snippet("ty::first"), snippet("ty::second")],
        &[exempt(&["ty::second"])],
    );
    assert_eq!(both.len(), 1, "{both:?}");

    let unknown = rule_problems(
        &codes,
        &[snippet("ty::first"), snippet("ty::typo")],
        &[exempt(&["ty::second", "ty::gone"])],
    );
    assert_eq!(unknown.len(), 2, "{unknown:?}");
}
