//! Command-line interface. Argument parsing is done by hand: the surface is
//! too small to justify a dependency.

#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "the command line prints dumps and diagnostics"
)]

use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::process::ExitCode;

use erebos::driver::{self, OptLevel, Stage};

const USAGE: &str = "\
usage: erebos build <input.ers> [-o <output>] [-O] [--link <file.o>]... [--emit=<stage>]
       erebos svd <input.svd> [-o <output.ers>]

build:
  -o <output>      executable to write (default: <input> with .elf)
  --link <file.o>  an object file to link with, defining `extern fn`s
  -O               optimize (LLVM -O2)
  --emit=<stage>   print a compiler stage instead of building:
                   tokens, ast, hir, typeck, mir, linker-script, llvm-ir

svd:
  writes the Erebos module for an SVD file (ADR 0070)
  -o <output.ers>  module to write (default: next to the input, named after it)";

enum Command {
    Build(BuildArgs),
    Svd {
        input: PathBuf,
        output: Option<PathBuf>,
    },
}

struct BuildArgs {
    input: PathBuf,
    output: Option<PathBuf>,
    link: Vec<PathBuf>,
    opt: OptLevel,
    emit: Option<Stage>,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Command, String> {
    let svd = match args.next().as_deref() {
        Some("build") => false,
        Some("svd") => true,
        Some(other) => return Err(format!("unknown command `{other}`")),
        None => return Err("missing command".to_owned()),
    };

    let mut input = None;
    let mut output = None;
    let mut link = Vec::new();
    let mut opt = OptLevel::None;
    let mut emit = None;
    while let Some(arg) = args.next() {
        if arg == "-o" {
            let path = args.next().ok_or("`-o` needs a path")?;
            output = Some(PathBuf::from(path));
        } else if svd && arg.starts_with('-') {
            return Err(format!("unknown option `{arg}`"));
        } else if arg == "--link" {
            link.push(PathBuf::from(args.next().ok_or("`--link` needs a path")?));
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

    let input = input.ok_or("missing input file")?;
    if svd {
        return Ok(Command::Svd { input, output });
    }
    Ok(Command::Build(BuildArgs {
        input,
        output,
        link,
        opt,
        emit,
    }))
}

fn main() -> ExitCode {
    let command = match parse_args(std::env::args().skip(1)) {
        Ok(command) => command,
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let color = io::stderr().is_terminal();

    let result = match command {
        Command::Build(args) => match args.emit {
            Some(stage) => std::fs::read_to_string(&args.input)
                .map_err(|e| format!("error: cannot read `{}`: {e}", args.input.display()))
                .and_then(|text| driver::dump(&args.input, &text, stage, args.opt, color))
                .map(|dump| print!("{dump}")),
            None => {
                let output = args
                    .output
                    .unwrap_or_else(|| args.input.with_extension("elf"));
                driver::build(&args.input, &output, &args.link, args.opt, color)
            }
        },
        // Warnings are shown, and the module is written.
        Command::Svd { input, output } => {
            driver::svd(&input, output.as_deref(), color).map(|done| eprint!("{}", done.warnings))
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
