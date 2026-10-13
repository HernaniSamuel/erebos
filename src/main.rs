//! Command-line interface. Argument parsing is done by hand: the surface is
//! too small to justify a dependency.

use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::process::ExitCode;

use erebos::driver::{self, OptLevel, Stage};

const USAGE: &str = "\
usage: erebos build <input.ers> [-o <output>] [-O] [--emit=<stage>]

  -o <output>      executable to write (default: <input> with .elf)
  -O               optimize (LLVM -O2)
  --emit=<stage>   print a compiler stage instead of building:
                   tokens, ast, hir, typeck, mir, llvm-ir";

struct Args {
    input: PathBuf,
    output: Option<PathBuf>,
    opt: OptLevel,
    emit: Option<Stage>,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    match args.next().as_deref() {
        Some("build") => {}
        Some(other) => return Err(format!("unknown command `{other}`")),
        None => return Err("missing command".to_owned()),
    }

    let mut input = None;
    let mut output = None;
    let mut opt = OptLevel::None;
    let mut emit = None;
    while let Some(arg) = args.next() {
        if arg == "-o" {
            let path = args.next().ok_or("`-o` needs a path")?;
            output = Some(PathBuf::from(path));
        } else if arg == "-O" {
            opt = OptLevel::Speed;
        } else if let Some(stage) = arg.strip_prefix("--emit=") {
            emit = Some(stage.parse()?);
        } else if arg.starts_with('-') {
            return Err(format!("unknown option `{arg}`"));
        } else if input.replace(PathBuf::from(&arg)).is_some() {
            return Err("only one input file is supported".to_owned());
        }
    }

    Ok(Args {
        input: input.ok_or("missing input file")?,
        output,
        opt,
        emit,
    })
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let color = io::stderr().is_terminal();

    let result = match args.emit {
        Some(stage) => std::fs::read_to_string(&args.input)
            .map_err(|e| format!("error: cannot read `{}`: {e}", args.input.display()))
            .and_then(|text| {
                driver::dump(
                    &args.input.display().to_string(),
                    &text,
                    stage,
                    args.opt,
                    color,
                )
            })
            .map(|dump| print!("{dump}")),
        None => {
            let output = args
                .output
                .unwrap_or_else(|| args.input.with_extension("elf"));
            driver::build(&args.input, &output, args.opt, color)
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprint!("{e}");
            if !e.ends_with('\n') {
                eprintln!();
            }
            ExitCode::FAILURE
        }
    }
}
