//! Compiles each file named on the command line as a Rhai script and reports
//! parse errors: `tools/docs/check_snippets.py` uses it to keep the code
//! blocks of the documentation valid Rhai. Nothing is run. An argument
//! `@list` names a file holding one path per line (a command line is too
//! short for every block of the docs on Windows).

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut engine = rhai::Engine::new();
    // The player is a release build, whose Rhai allows expressions nested 64
    // deep (32 inside a function); a debug build of this example halves both,
    // which would reject scripts the player runs.
    engine.set_max_expr_depths(64, 32);
    let mut failed = false;
    let mut paths = Vec::new();
    for arg in std::env::args().skip(1) {
        match arg.strip_prefix('@') {
            Some(list) => match std::fs::read_to_string(list) {
                Ok(text) => paths.extend(text.lines().map(String::from)),
                Err(error) => {
                    eprintln!("{list}: {error}");
                    return ExitCode::from(2);
                }
            },
            None => paths.push(arg),
        }
    }
    for path in paths {
        let source = match std::fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                eprintln!("{path}: {error}");
                failed = true;
                continue;
            }
        };
        if let Err(error) = engine.compile(&source) {
            eprintln!("{path}: {error}");
            failed = true;
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
