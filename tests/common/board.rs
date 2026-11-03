//! The board modules of the tests (ADR 0077): `tests/boards/<target>.ers`
//! describes the QEMU machine a target's tests run on, and a program that
//! declares `mod board;` reaches its machine through it.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use erebos::target::{self, Target};

/// The name the harness gives a board next to a program's root file.
const BOARD_FILE: &str = "board.ers";

/// The source of `target`'s board module.
pub fn source(target: &Target) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/boards")
        .join(format!("{}.ers", target.name));
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read the board {}: {e}", path.display()))
}

/// The targets a `// targets:` header names, or every target when
/// `names` is empty. Names are separated by spaces or commas.
pub fn targets_named(names: &str) -> Result<Vec<&'static Target>, String> {
    let mut found = Vec::new();
    for name in names
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|name| !name.is_empty())
    {
        let Some(target) = target::by_name(name) else {
            let known: Vec<&str> = target::TARGETS.iter().map(|t| t.name).collect();
            return Err(format!(
                "`// targets:` names an unknown target `{name}`; the targets are {}",
                known.join(", ")
            ));
        };
        found.push(target);
    }
    if found.is_empty() {
        found.extend(target::TARGETS.iter().copied());
    }
    Ok(found)
}

/// Copies a program into `work/<name>` and puts `target`'s board beside its
/// root file as `board.ers`; returns the root file there. `program` is the
/// root file, either `tests/e2e/<name>.ers` or `tests/e2e/<name>/main.ers`
/// (then its whole directory is copied, keeping the names of its files,
/// which panic messages print). A program that already has a `board.ers`
/// is an error.
pub fn stage(program: &Path, target: &Target, work: &Path) -> Result<PathBuf, String> {
    let dir = program.parent().unwrap();
    let own = dir.join(BOARD_FILE);
    if own.exists() {
        return Err(format!(
            "{} is a board of the program's own, but the harness supplies `board.ers` \
             (ADR 0077); rename it",
            own.display()
        ));
    }
    let name = program_name(program);
    let staged = work.join(&name);
    let _ = fs::remove_dir_all(&staged);
    fs::create_dir_all(&staged).map_err(|e| format!("{}: {e}", staged.display()))?;
    let root = if program.file_name().unwrap() == "main.ers" && is_program_dir(dir) {
        copy_dir(dir, &staged)?;
        staged.join("main.ers")
    } else {
        let root = staged.join(program.file_name().unwrap());
        fs::copy(program, &root).map_err(|e| format!("{}: {e}", program.display()))?;
        root
    };
    fs::write(staged.join(BOARD_FILE), source(target))
        .map_err(|e| format!("{}: {e}", staged.display()))?;
    Ok(root)
}

/// What a program is called: its file's stem, or its directory's name
/// when it is the `main.ers` of a directory of `tests/e2e`.
pub fn program_name(program: &Path) -> String {
    let dir = program.parent().unwrap();
    if program.file_name().unwrap() == "main.ers" && is_program_dir(dir) {
        dir.file_name().unwrap().to_string_lossy().into_owned()
    } else {
        program.file_stem().unwrap().to_string_lossy().into_owned()
    }
}

/// A directory of programs of several files lies one level below
/// `tests/e2e`, not in it.
fn is_program_dir(dir: &Path) -> bool {
    dir.parent().is_some_and(|parent| parent.ends_with("e2e"))
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    for entry in fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            fs::create_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))?;
            copy_dir(&path, &target)?;
        } else {
            fs::copy(&path, &target).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    Ok(())
}

/// For the tests that read a program as text and compile it where it
/// lies (`dump` with the path of the root file): the source with its
/// `mod board;` line emptied and the default target's board declared
/// inline at the end, so that the lines of the program stay where they
/// are. A source without that line, or whose directory has a `board.ers`
/// of its own, is returned unchanged.
pub fn with_board(path: &Path, text: &str) -> String {
    if path.with_file_name(BOARD_FILE).exists() {
        return text.to_owned();
    }
    let mut found = false;
    let mut out: Vec<&str> = Vec::new();
    for line in text.lines() {
        if line == "mod board;" {
            found = true;
            out.push("");
        } else {
            out.push(line);
        }
    }
    if !found {
        return text.to_owned();
    }
    let mut result = out.join("\n");
    result.push_str("\nmod board {\n");
    result.push_str(&source(target::DEFAULT));
    result.push_str("}\n");
    result
}
