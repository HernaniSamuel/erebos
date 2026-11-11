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
    s!(
        "parser::string-outside-asm",
        Ast,
        Statements,
        "let injected_a: u32 = \"abc\";"
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
    // Types and checks on bodies. They are compiled to `Mir`, the last stage
    // an erroneous program reaches, and not to `Typeck`: every function that
    // type checks is lowered whatever errors the items have (ADR 0032), so a
    // snippet that declares an item names it in a function of its own
    // (`injected_use`), and a panic of MIR building or of the validator on the
    // faulty item fails the test (see `ty::copy-field-not-copy`, whose panic
    // the `Typeck` stage hid).
    s!(
        "ty::mismatched-types",
        Mir,
        Statements,
        "let injected_a: u8 = true;"
    ),
    s!("ty::untyped-literal", Mir, Statements, "1;"),
    s!(
        "ty::literal-out-of-range",
        Mir,
        Statements,
        "let injected_a: u8 = 256;"
    ),
    s!(
        "ty::invalid-operand",
        Mir,
        Statements,
        "let injected_a: bool = true + false;"
    ),
    s!(
        "ty::negate-unsigned",
        Mir,
        Statements,
        "let injected_n: u32 = 1;\nlet injected_a: u32 = -injected_n;"
    ),
    s!(
        "ty::invalid-cast",
        Mir,
        Statements,
        "let injected_n: u32 = 1;\nlet injected_a: bool = injected_n as bool;"
    ),
    s!(
        "ty::pointer-cast-width",
        Mir,
        Statements,
        "let injected_n: u32 = 1;\nlet injected_p: *const u8 = injected_n as *const u8;"
    ),
    s!(
        "ty::deref-non-pointer",
        Mir,
        Statements,
        "let injected_n: u32 = 1;\nlet injected_a: u32 = *injected_n;"
    ),
    s!(
        "ty::cannot-index",
        Mir,
        Statements,
        "let injected_n: u32 = 1;\nlet injected_a: u32 = injected_n[0];"
    ),
    s!(
        "ty::no-fields",
        Mir,
        Statements,
        "let injected_n: u32 = 1;\nlet injected_a: u32 = injected_n.x;"
    ),
    s!(
        "ty::unknown-field",
        Mir,
        Items,
        "struct InjS { a: u8 }\nfn injected_f(s: InjS) -> u8 {\n    s.b\n}"
    ),
    s!(
        "ty::write-through-const",
        Mir,
        Statements,
        "let injected_n: u32 = 1;\nlet injected_p: *const u32 = &raw const injected_n;\n*injected_p = 2;"
    ),
    s!("ty::not-assignable", Mir, Statements, "1 = 2;"),
    s!(
        "ty::assign-immutable",
        Mir,
        Statements,
        "let injected_n: u32 = 1;\ninjected_n = 2;"
    ),
    s!(
        "ty::not-a-place",
        Mir,
        Statements,
        "let injected_p: *const u32 = &raw const 1;"
    ),
    s!(
        "ty::address-of-immutable",
        Mir,
        Statements,
        "let injected_n: u32 = 1;\nlet injected_p: *mut u32 = &raw mut injected_n;"
    ),
    s!(
        "ty::mut-borrow-of-immutable",
        Mir,
        Statements,
        "let injected_n: u32 = 1;
let injected_r: &mut u32 = &mut injected_n;"
    ),
    s!(
        "ty::reference-position",
        Mir,
        Items,
        "struct InjHolder { injected_r: &u32 }
fn injected_use(injected_h: InjHolder) {}"
    ),
    s!(
        "ty::reference-to-static-mut",
        Mir,
        Items,
        "static mut INJ_COUNTER: u32 = 0;
fn injected_use() {
    let injected_r: &u32 = &INJ_COUNTER;
}"
    ),
    s!(
        "ty::reference-to-volatile",
        Mir,
        Items,
        "fn injected_use(injected_p: *mut volatile u32) {
    let injected_r: &u32 = &*injected_p;
}"
    ),
    s!(
        "ty::never-in-memory",
        Mir,
        Items,
        "struct InjN { a: ! }\nfn injected_use(injected_n: InjN) {}"
    ),
    s!(
        "ty::too-large",
        Mir,
        Items,
        "struct InjBig { a: [u8; 0x8000_0000] }\nfn injected_use(injected_b: InjBig) {}"
    ),
    s!(
        "ty::infinite-size",
        Mir,
        Items,
        "struct InjI { a: InjI }\nfn injected_use(injected_i: InjI) {}"
    ),
    s!(
        "ty::unsized",
        Mir,
        Items,
        "struct InjU { a: [u8] }\nfn injected_use(injected_u: InjU) {}"
    ),
    s!(
        "ty::copy-field-not-copy",
        Mir,
        Items,
        "struct InjHandle { id: u32 }
#[copy]
struct InjCopy { h: InjHandle }
fn injected_copy(injected_c: InjCopy) {}"
    ),
    s!(
        "ty::non-copy-constant",
        Mir,
        Items,
        "struct InjHandle2 { id: u32 }
const INJ_C: InjHandle2 = InjHandle2 { id: 1 };
fn injected_use() -> u32 {
    INJ_C.id
}"
    ),
    s!(
        "ty::non-copy-repeat",
        Mir,
        Items,
        "struct InjHandle3 { id: u32 }
fn injected_f() {
    let injected_a: [InjHandle3; 2] = [InjHandle3 { id: 1 }; 2];
}"
    ),
    s!(
        "ty::move-out-of-place",
        Mir,
        Items,
        "struct InjHandle4 { id: u32 }
fn injected_f(injected_p: *mut InjHandle4) {
    let injected_h: InjHandle4 = *injected_p;
}"
    ),
    s!(
        "ty::non-copy-binding-in-or-pattern",
        Mir,
        Items,
        "struct InjHandle5 { id: u32 }
enum InjInner: u8 { A(InjHandle5) = 0, B(InjHandle5) = 1 }
enum InjOuter: u8 { Pair(InjInner, u32) = 0 }
fn injected_f(injected_o: InjOuter) -> u32 {
    match injected_o {
        .Pair(.A(let injected_b: InjHandle5) | .B(let injected_b: InjHandle5), 0) => 1,
        _ => 0,
    }
}"
    ),
    s!(
        "ty::linear-static",
        Mir,
        Items,
        "#[linear]
struct InjLinear { id: u32 }
static INJ_S: InjLinear = InjLinear { id: 1 };
fn injected_use() {}"
    ),
    s!(
        "ty::linear-array",
        Mir,
        Items,
        "#[linear]
struct InjLinear2 { id: u32 }
fn injected_use(injected_a: [InjLinear2; 2]) {}"
    ),
    s!(
        "ty::linear-through-pointer",
        Mir,
        Items,
        "#[linear]
struct InjLinear3 { id: u32 }
fn injected_f(injected_p: *mut InjLinear3, injected_t: InjLinear3) {
    *injected_p = injected_t;
}"
    ),
    s!(
        "ty::end-type",
        Mir,
        Items,
        "fn injected_f(injected_n: u32) {
    end(injected_n);
}"
    ),
    s!(
        "ty::end-private-field",
        Mir,
        Items,
        "mod injected_dma {
    #[linear]
    pub struct InjTransfer { injected_channel: u32 }
}
fn injected_f(injected_t: injected_dma::InjTransfer) {
    end(injected_t);
}"
    ),
    s!(
        "ty::invalid-align",
        Mir,
        Items,
        "#[align(3)]\nstruct InjA { a: u8 }\nfn injected_use(injected_a: InjA) {}"
    ),
    s!(
        "ty::aligned-in-packed",
        Mir,
        Items,
        "#[align(8)]\nstruct InjA { a: u8 }\n#[packed]\nstruct InjP { a: InjA }\nfn injected_use(injected_p: InjP) {}"
    ),
    s!(
        "ty::not-comptime",
        Mir,
        Statements,
        "let injected_n: u32 = 1;\nlet injected_a: u32 = comptime { injected_n };"
    ),
    s!(
        "ty::cycle",
        Mir,
        Items,
        "const INJ_A: u32 = INJ_B;\nconst INJ_B: u32 = INJ_A;\nfn injected_use() -> u32 {\n    INJ_A\n}"
    ),
    s!(
        "ty::duplicate-discriminant",
        Mir,
        Items,
        "enum InjE: u8 { A = 1, B = 1 }\nfn injected_use(injected_e: InjE) -> u32 {\n    match injected_e {\n        .A => 1,\n        .B => 2,\n    }\n}"
    ),
    s!(
        "ty::match-type",
        Mir,
        Statements,
        "let injected_a: [u8; 1] = [1];\nmatch injected_a {\n    _ => {}\n}"
    ),
    s!(
        "ty::non-exhaustive-match",
        Mir,
        Statements,
        "let injected_n: u8 = 1;\nmatch injected_n {\n    1 => {}\n}"
    ),
    s!(
        "ty::unreachable-pattern",
        Mir,
        Statements,
        "let injected_n: u8 = 1;\nmatch injected_n {\n    _ => {}\n    1 => {}\n}"
    ),
    s!(
        "ty::try-operand",
        Mir,
        Items,
        "fn injected_f() -> Option(u8) {\n    let injected_n: u32 = 1;\n    let injected_a: u32 = injected_n?;\n    .None\n}"
    ),
    s!(
        "ty::try-return",
        Mir,
        Items,
        "fn injected_f() {\n    let injected_o: Option(u8) = .None;\n    let injected_a: u8 = injected_o?;\n}"
    ),
    s!(
        "ty::if-without-else",
        Mir,
        Statements,
        "let injected_a: u32 = if true { 1 };"
    ),
    s!(
        "ty::wrong-arg-count",
        Mir,
        Items,
        "fn injected_g() {}\nfn injected_f() {\n    injected_g(1);\n}"
    ),
    s!("ty::param-type", Mir, Items, "fn injected_f(a: ()) {}"),
    s!("ty::untyped-variant", Mir, Statements, ".V;"),
    s!(
        "ty::wrong-payload-count",
        Mir,
        Items,
        "enum InjE: u8 { A(u8) = 1 }\nfn injected_f() {\n    let injected_e: InjE = InjE::A(1, 2);\n}"
    ),
    s!(
        "ty::private-field",
        Mir,
        Items,
        "mod injected_m {\n    pub struct S { pub a: u8, b: u8 }\n    pub fn make() -> S {\n        S { a: 1, b: 2 }\n    }\n}\n\
         fn injected_f() -> u8 {\n    injected_m::make().b\n}"
    ),
    s!(
        "ty::private-variant",
        Mir,
        Items,
        "mod injected_m {\n    enum InjE: u8 { A = 1 }\n    pub fn injected_g(e: InjE) {}\n}\n\
         fn injected_f() {\n    injected_m::injected_g(.A);\n}"
    ),
    s!(
        "ty::slice-pointer-index",
        Mir,
        Items,
        "fn injected_f(s: *const [u8]) -> u8 {\n    s[0]\n}"
    ),
    s!(
        "ty::packed-field-address",
        Mir,
        Items,
        "#[packed]\nstruct InjS { a: u8, b: u32 }\nfn injected_f(p: *const InjS) {\n    \
         let injected_q: *const u32 = &raw const (*p).b;\n}"
    ),
    s!(
        "ty::volatile-too-wide",
        Mir,
        Items,
        "fn injected_f(p: *mut volatile u64) -> u64 {\n    *p\n}"
    ),
    s!(
        "ty::volatile-aggregate",
        Mir,
        Items,
        "struct InjS { a: u8 }\nfn injected_f(p: *const volatile InjS) -> InjS {\n    *p\n}"
    ),
    s!(
        "ty::volatile-packed-field",
        Mir,
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
    // Handlers. The bases are built for the default target, riscv32, so the
    // paths name its vectors; a base for thumbv7m does not compile and is
    // skipped. A base that serves the vector of a snippet adds a
    // `resolve::duplicate-vector`, which does not hide the code asked for.
    s!(
        "resolve::not-a-vector",
        Hir,
        Items,
        "struct InjS { a: u8 }\ninterrupt fn injected_h() at InjS {}"
    ),
    s!(
        "resolve::duplicate-vector",
        Hir,
        Items,
        "interrupt fn injected_h1() at core::arch::Vector::MachineSoftware {}\n\
         interrupt fn injected_h2() at core::arch::Vector::MachineSoftware {}"
    ),
    s!(
        "resolve::call-interrupt",
        Hir,
        Items,
        "interrupt fn injected_h() at core::arch::Vector::MachineExternal {}\n\
         fn injected_f() {\n    injected_h();\n}"
    ),
    // Two lists are an error whether or not the base has one: a base with one
    // gets the second and third reported against it.
    s!(
        "resolve::duplicate-interrupts",
        Hir,
        Items,
        "#[interrupts]\nenum InjI1: u32 { A = 0 }\n#[interrupts]\nenum InjI2: u32 { B = 0 }"
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
        Mir,
        Items,
        "enum InjE: u8 { A = 1 }\nfn injected_f() {\n    let injected_e: InjE = .B;\n}"
    ),
    s!(
        "ty::constant-pattern-type",
        Mir,
        Items,
        "const INJ_P: *const u8 = 0 as *const u8;\nfn injected_f(p: *const u8) {\n    match p {\n        INJ_P => {}\n        _ => {}\n    }\n}"
    ),
    // The analyses of one body.
    s!(
        "analysis::use-after-move",
        Mir,
        Items,
        "struct InjHandle6 { id: u32 }
fn injected_take(injected_h: InjHandle6) {}
fn injected_f(injected_h: InjHandle6) {
    injected_take(injected_h);
    injected_take(injected_h);
}"
    ),
    s!(
        "analysis::borrow-conflict",
        Mir,
        Items,
        "fn injected_f(mut injected_x: u32) -> u32 {
    let injected_r: &mut u32 = &mut injected_x;
    injected_x = 2;
    *injected_r = 3;
    injected_x
}"
    ),
    s!(
        "analysis::linear-not-consumed",
        Mir,
        Items,
        "#[linear]
struct InjLinear4 { id: u32 }
fn injected_f(injected_t: InjLinear4) {}"
    ),
    s!(
        "analysis::linear-overwritten",
        Mir,
        Items,
        "#[linear]
struct InjLinear5 { id: u32 }
fn injected_take(injected_t: InjLinear5) {
    end(injected_t);
}
fn injected_f(mut injected_t: InjLinear5) {
    injected_t = InjLinear5 { id: 2 };
    injected_take(injected_t);
}"
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
    // Inline assembly (ADR 0079). The templates are never assembled: the
    // stages stop before the object (`codegen::asm` is exempt below).
    s!("ty::asm-template", Mir, Statements, "asm(\"nop {}\");"),
    s!(
        "ty::asm-register",
        Mir,
        Statements,
        "asm(\"nop\", in(\"injected_register\") 1 as u32);"
    ),
    s!(
        "ty::asm-operand-type",
        Mir,
        Statements,
        "asm(\"nop {}\", in(reg) true);"
    ),
    s!(
        "ty::asm-sym",
        Mir,
        Items,
        "const INJ_C: u32 = 1;
fn injected_f() {
    asm(\"nop {}\", sym INJ_C);
}"
    ),
    s!(
        "ty::asm-options",
        Mir,
        Statements,
        "asm(\"nop\", options(nomem, readonly));"
    ),
    s!(
        "ty::naked-body",
        Mir,
        Items,
        "#[naked]
fn injected_f() {
    asm(\"nop\");
}"
    ),
    s!(
        "eval::asm",
        Mir,
        Items,
        "fn injected_f() {
    asm(\"nop\");
}
const INJ_C: u32 = comptime {
    injected_f();
    0
};"
    ),
    // Bit-field structs.
    s!(
        "ty::bit-field-type",
        Mir,
        Items,
        "struct InjB: u8 {\n    a: i8 at 0..=3,\n}\nfn injected_use(injected_b: InjB) {}"
    ),
    s!(
        "ty::reversed-bit-range",
        Mir,
        Items,
        "struct InjB: u8 {\n    a: u8 at 3..=0,\n}\nfn injected_use(injected_b: InjB) {}"
    ),
    s!(
        "ty::bit-field-outside",
        Mir,
        Items,
        "struct InjB: u8 {\n    a: u8 at 4..=9,\n}\nfn injected_use(injected_b: InjB) {}"
    ),
    s!(
        "ty::bit-field-width",
        Mir,
        Items,
        "struct InjB: u8 {\n    a: bool at 0..=1,\n}\nfn injected_use(injected_b: InjB) {}"
    ),
    s!(
        "ty::bit-field-overlap",
        Mir,
        Items,
        "struct InjB: u8 {\n    a: u8 at 0..=3,\n    b: u8 at 3..=5,\n}\nfn injected_use(injected_b: InjB) {}"
    ),
    s!(
        "ty::bit-field-offset",
        Mir,
        Items,
        "struct InjB: u8 {\n    a: u8 at 0..=3,\n}\nfn injected_f() -> usize {\n    offset_of(InjB, a)\n}"
    ),
    s!(
        "ty::bit-field-address",
        Mir,
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
        Mir,
        Items,
        "peripheral InjP {\n    read a: u64 at 0,\n}\nfn injected_use(p: *mut InjP) -> u64 {\n    (*p).a\n}"
    ),
    s!(
        "ty::misaligned-register",
        Mir,
        Items,
        "peripheral InjP {\n    read a: u32 at 1,\n}\nfn injected_use(p: *mut InjP) -> u32 {\n    (*p).a\n}"
    ),
    s!(
        "ty::register-overlap",
        Mir,
        Items,
        "peripheral InjP {\n    read write a: u8 at 0,\n    read write b: u8 at 0,\n}\nfn injected_use(p: *mut InjP) -> u8 {\n    (*p).a\n}"
    ),
    s!(
        "ty::misaligned-peripheral",
        Mir,
        Items,
        "peripheral InjP {\n    read a: u32 at 0,\n}\nperipheral INJ_P: InjP at 0x1001;\nfn injected_use() -> u32 {\n    INJ_P.a\n}"
    ),
    s!(
        "ty::peripheral-out-of-range",
        Mir,
        Items,
        "peripheral InjP {\n    read a: u32 at 0,\n}\nperipheral INJ_P: InjP at 0xFFFF_FFFC;\n\
         peripheral INJ_Q: InjP at 0xFFFF_FFFE;\nfn injected_use() -> u32 {\n    INJ_Q.a\n}"
    ),
    s!(
        "ty::peripheral-overlap",
        Mir,
        Items,
        "peripheral InjP {\n    read a: u32 at 0,\n}\nperipheral INJ_P: InjP at 0xE000_0000;\n\
         peripheral INJ_Q: InjP at 0xE000_0000;\nfn injected_use() -> u32 {\n    INJ_Q.a\n}"
    ),
    s!(
        "ty::peripheral-by-value",
        Mir,
        Items,
        "peripheral InjP {\n    read a: u32 at 0,\n}\nfn injected_f(p: InjP) {}"
    ),
    s!(
        "ty::volatile-peripheral",
        Mir,
        Items,
        "peripheral InjP {\n    read a: u32 at 0,\n}\nfn injected_f(p: *mut volatile InjP) {}"
    ),
    s!(
        "ty::peripheral-index",
        Mir,
        Items,
        "peripheral InjP {\n    read a: u32 at 0,\n}\nfn injected_f(p: *mut InjP) {\n    let injected_x: InjP = p[1];\n}"
    ),
    s!(
        "ty::unknown-register",
        Mir,
        Items,
        "peripheral InjP {\n    read a: u32 at 0,\n}\nfn injected_f(p: *mut InjP) -> u32 {\n    (*p).zz\n}"
    ),
    s!(
        "ty::private-register",
        Mir,
        Items,
        "mod injected_m {\n    pub peripheral InjP {\n        read a: u32 at 0,\n    }\n}\n\
         fn injected_f(p: *mut injected_m::InjP) -> u32 {\n    (*p).a\n}"
    ),
    s!(
        "ty::write-to-read-only",
        Mir,
        Items,
        "peripheral InjP {\n    read a: u32 at 0,\n}\nfn injected_f(p: *mut InjP) {\n    (*p).a = 1;\n}"
    ),
    s!(
        "ty::read-of-write-only",
        Mir,
        Items,
        "peripheral InjP {\n    write a: u32 at 0,\n}\nfn injected_f(p: *mut InjP) -> u32 {\n    (*p).a\n}"
    ),
    s!(
        "ty::register-address",
        Mir,
        Items,
        "peripheral InjP {\n    read a: u32 at 0,\n}\nfn injected_f(p: *mut InjP) {\n    \
         let injected_q: *const u32 = &raw const (*p).a;\n}"
    ),
    s!(
        "ty::read-with-effects",
        Mir,
        Items,
        "struct InjB: u8 {\n    f: bool at 0,\n}\nperipheral InjP {\n    read write r: InjB at 0 read clears,\n}\n\
         fn injected_f(p: *mut InjP) {\n    (*p).r.f = true;\n}"
    ),
    s!(
        "ty::compound-write-effect",
        Mir,
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
        Mir,
        Items,
        "peripheral InjP {\n    read a: [u32; 2] at 0 step 6,\n}\nfn injected_use(p: *mut InjP) -> u32 {\n    (*p).a[1]\n}"
    ),
    s!(
        "ty::step-too-small",
        Mir,
        Items,
        "peripheral InjP {\n    read a: [u32; 2] at 0 step 0,\n}\nfn injected_use(p: *mut InjP) -> u32 {\n    (*p).a[1]\n}"
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
        codes: &["ty::interrupt-params", "ty::interrupt-return"],
        reason: "a handler needs a vector, and between them the bases of `tests/ui` and `tests/e2e` serve every vector of riscv32: a snippet clashes with a base that serves its vector, `resolve::duplicate-vector`, reported before type checking, which hides these; `tests/ui/interrupt_type_errors.ers` and `_thumbv7m.ers` cover them",
    },
    Exempt {
        codes: &[
            "ty::interrupts-enum",
            "ty::interrupt-number",
            "ty::interrupt-not-a-vector",
        ],
        reason: "each needs an enum with `#[interrupts]` of its own, and a program has one: a base that has a list (`tests/ui/chip_interrupts_*.ers`, `tests/e2e/uart_loopback_riscv32.ers`) gets `resolve::duplicate-interrupts` from the snippet's, reported before type checking, which hides these; `ty::interrupt-number` is also about thumbv7m, and the bases are built for riscv32, where 496 is accepted; `tests/ui/chip_interrupts_*.ers` cover them",
    },
    Exempt {
        codes: &[
            "analysis::unprotected-static",
            "analysis::unmaskable-static",
        ],
        reason: "a snippet cannot break either rule alone: rule 1 needs a place the thread context reaches, rule 2 a place an unmaskable handler reaches and another context too, and a snippet's statements land in any function; a handler in a snippet clashes with a base that serves its vector, `resolve::duplicate-vector`, which hides the code, as for `ty::interrupt-return`; the `tests/ui/shared_static_*.ers` cover it (ADR 0093)",
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
        codes: &["codegen::asm"],
        reason: "the assembler reads the template when the object is written, and `driver::dump` has no object stage; the harness tests of `tests/e2e.rs` cover it",
    },
    Exempt {
        codes: &["codegen::llvm"],
        reason: "an environment problem (LLVM rejecting the target), not a rule of the language",
    },
    Exempt {
        codes: &[
            "svd::xml",
            "svd::not-svd",
            "svd::missing-element",
            "svd::invalid-number",
            "svd::invalid-dim",
            "svd::unknown-reference",
            "svd::derived-cycle",
            "svd::too-large",
            "svd::invalid-name",
            "svd::renamed",
            "svd::not-declared",
            "svd::invalid-value",
            "svd::overlap",
        ],
        reason: "they are the generator's, reported on an SVD file, which a snippet of Erebos does not hold; `tests/svd/` and the unit tests of `src/svd/` cover them",
    },
    Exempt {
        codes: &["link::no-memory-map", "link::region-overflow"],
        reason: "they depend on the program as a whole (no `memory` block; what the linker places in a region) and are found when building, not by a stage of `dump`",
    },
];

/// Whether `text` compiles to `stage` without a diagnostic.
fn compiles(path: &Path, text: &str, stage: Stage) -> bool {
    driver::dump(
        path,
        &common::board::with_board(path, text, erebos::target::DEFAULT),
        stage,
        erebos::target::DEFAULT,
        OptLevel::None,
        false,
    )
    .is_ok()
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
    /// It is the body of a naked function, which is one `asm` expression and
    /// nothing else (ADR 0079): a statement added to it is its own error.
    naked: bool,
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
                    naked: function && header.contains("#[naked]"),
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
        let usable = stack.iter().any(|b| b.function)
            && !stack.iter().any(|b| b.maybe_unchecked || b.dead || b.naked);
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
            match driver::dump(
                path,
                &common::board::with_board(path, &text, erebos::target::DEFAULT),
                snippet.stage,
                erebos::target::DEFAULT,
                OptLevel::None,
                false,
            ) {
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

/// The body of a naked function is one `asm` and nothing else, so no place
/// is inside it; the function after it has its places.
#[test]
fn a_naked_body_has_no_statement_boundaries() {
    let text = "#[naked]
#[export]
fn naked(a: u32) -> u32 {
    asm(\"ret\", options(noreturn));
}

fn other(a: u32) {
    let b = a;
}
";
    let points = statement_boundaries(text);
    let lines: Vec<usize> = points
        .iter()
        .map(|p| text[..*p].matches('\n').count() + 1)
        .collect();
    assert_eq!(lines, vec![8, 9]);
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
