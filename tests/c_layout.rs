//! Struct layouts against C (ADR 0022): the layouts the compiler computes
//! for `tests/layout/structs.ers` must be the ones clang gives the C twins
//! in `tests/layout/structs.h` on the same target.
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
        for field in fields.trim_start_matches('{').split(", ") {
            let field = field.trim();
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
