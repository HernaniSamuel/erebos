//! Struct layouts against C (ADR 0022): the layouts the compiler computes
//! for `tests/layout/structs.ers` must be the ones clang gives the C twins
//! in `tests/layout/structs.h` on the same target. The twin of an enum with
//! payloads is a union of structs that each start with the tag (ADR 0039);
//! that of an `Option(T)` or `Result(T, E)` (ADR 0045) is named as
//! `c_name` names it.
//!
//! The compiler's layouts come from `--emit=typeck`; each becomes a
//! `_Static_assert` on `sizeof`, `_Alignof` and `offsetof`, and clang checks
//! them all. Needs clang, found like `ld.lld`: `EREBOS_CLANG`, then the LLVM
//! we build against (`LLVM_SYS_221_PREFIX`), then `PATH`.

mod common;

use std::fmt::Write;
use std::fs;
use std::path::Path;
use std::process::Command;

use erebos::driver::{self, OptLevel, Stage};

#[test]
fn struct_layouts_match_c() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/layout");
    let source = fs::read_to_string(dir.join("structs.ers")).unwrap();
    let dump = driver::dump("structs.ers", &source, Stage::Typeck, OptLevel::None, false)
        .unwrap_or_else(|diags| panic!("structs.ers does not compile:\n{diags}"));

    let mut checks = String::from("#include <stddef.h>\n#include \"structs.h\"\n\n");
    let mut structs = 0;
    for line in dump.lines() {
        // `[#[...] ]struct Name { field: Type at 4, ... } size 12, align 4`
        let Some(rest) = line
            .split("struct ")
            .nth(1)
            .filter(|_| !line.starts_with(' '))
        else {
            continue;
        };
        let (name, rest) = rest.split_once(' ').unwrap();
        let (fields, sizes) = rest.rsplit_once("} ").unwrap();
        let (size, align) = sizes
            .strip_prefix("size ")
            .and_then(|s| s.split_once(", align "))
            .unwrap();
        let what = |what: &str| format!("\"{name}: {what} differs from C\"");
        writeln!(
            checks,
            "_Static_assert(sizeof(struct {name}) == {size}, {});",
            what("size")
        )
        .unwrap();
        writeln!(
            checks,
            "_Static_assert(_Alignof(struct {name}) == {align}, {});",
            what("alignment")
        )
        .unwrap();
        // A field's type may be `Result(u32, Mode)`.
        for field in split_top_level(fields.trim_start_matches('{')) {
            let Some((field_name, at)) = field.split_once(": ") else {
                continue;
            };
            let offset = at.rsplit_once(" at ").unwrap().1;
            writeln!(
                checks,
                "_Static_assert(offsetof(struct {name}, {field_name}) == {offset}, {});",
                what(&format!("offset of `{field_name}`"))
            )
            .unwrap();
        }
        structs += 1;
    }
    assert!(structs >= 10, "found only {structs} structs in:\n{dump}");

    let mut enums = 0;
    for line in dump.lines() {
        // `enum Name: u8 { A = 0, B(u8 at 1, u16 at 2) = 1 } size 4, align 2`;
        // enums without payloads have no size, being their integer type.
        let Some(rest) = line
            .strip_prefix("enum ")
            .filter(|_| line.contains("} size "))
        else {
            continue;
        };
        let (name, rest) = rest.split_once(": ").unwrap();
        let name = c_name(name);
        let (variants, sizes) = rest.rsplit_once("} ").unwrap();
        let (size, align) = sizes
            .strip_prefix("size ")
            .and_then(|s| s.split_once(", align "))
            .unwrap();
        let what = |what: &str| format!("\"{name}: {what} differs from C\"");
        writeln!(
            checks,
            "_Static_assert(sizeof(union {name}) == {size}, {});",
            what("size")
        )
        .unwrap();
        writeln!(
            checks,
            "_Static_assert(_Alignof(union {name}) == {align}, {});",
            what("alignment")
        )
        .unwrap();
        let variants = variants.split_once("{ ").unwrap().1;
        for variant in split_top_level(variants) {
            // `B(u8 at 1, u16 at 2) = 1`, or `A = 0` without payload.
            let Some((variant_name, payload)) = variant.split_once('(') else {
                continue;
            };
            let payload = payload.rsplit_once(')').unwrap().0;
            writeln!(
                checks,
                "_Static_assert(offsetof(union {name}, {variant_name}.tag) == 0, {});",
                what(&format!("offset of the tag of `{variant_name}`"))
            )
            .unwrap();
            for (i, value) in split_top_level(payload).iter().enumerate() {
                let offset = value.rsplit_once(" at ").unwrap().1;
                writeln!(
                    checks,
                    "_Static_assert(offsetof(union {name}, {variant_name}.p{i}) == {offset}, {});",
                    what(&format!("offset of payload value {i} of `{variant_name}`"))
                )
                .unwrap();
            }
        }
        enums += 1;
    }
    assert!(
        enums >= 3,
        "found only {enums} enums with payloads in:\n{dump}"
    );

    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("c_layout");
    fs::create_dir_all(&work).unwrap();
    let c_file = work.join("checks.c");
    fs::write(&c_file, &checks).unwrap();

    let clang = common::clang_path();
    let output = Command::new(&clang)
        .args(common::CLANG_TARGET)
        // C23, for enums with a fixed underlying type (ADR 0035).
        .args(["-std=c23", "-fsyntax-only", "-I"])
        .arg(&dir)
        .arg(&c_file)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "cannot run `{}` ({e}); install clang or set EREBOS_CLANG",
                clang.display()
            )
        });
    assert!(
        output.status.success(),
        "clang disagrees with the compiler's layouts:\n{}\n--- checks ---\n{checks}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The name of the C twin of the enum `name`: itself, or for an
/// `Option(T)` or `Result(T, E)`, its name and arguments run together,
/// each capitalized, `()` as `Unit`, an array `[T; N]` as `<T>x<N>`:
/// `Result(u32, Mode)` is `ResultU32Mode`, `Option([u32; 0])` is
/// `OptionU32x0` (not `OptionU320`, which reads as a 320-bit integer).
fn c_name(name: &str) -> String {
    if !name.contains('(') {
        return name.to_owned();
    }
    name.replace("()", "Unit")
        .replace(['[', ']'], "")
        .replace("; ", "x")
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(|part| part[..1].to_ascii_uppercase() + &part[1..])
        .collect()
}

/// `text` split at the `, ` outside parentheses: the variants of an enum,
/// the values of a payload, or the fields of a struct, whose types may
/// hold `()` or type arguments.
fn split_top_level(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    for (i, c) in text.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(text[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(text[start..].trim());
    parts
}
