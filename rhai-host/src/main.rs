//! `rhai`: the Rhai scripting engine as an ordinary LazyOS command (issue #319).
//!
//! A static `x86_64-unknown-linux-musl` `std` program, thin on purpose: the
//! bindings, limits and REPL logic live in `libs/rhai-lazy` (host-tested); this
//! file only parses the command line and wires the process's stdio to it. It
//! composes with `sh`: it reads stdin, writes stdout/stderr and exits with the
//! script's status, so `echo hi | rhai -e 'print(stdin_text())'` just works.

mod cli;
mod editor;
mod repl_io;
mod sys;

use std::io::{self, Write};
use std::rc::Rc;

use cli::{Mode, Options};
use rhai_lazy::{build_engine, eval_source, Engine, Host, Outcome, Scope};
use sys::StdHost;

/// Write one line to stderr, ignoring failure (`eprintln!` would panic).
fn complain(message: &str) {
    let _ = writeln!(io::stderr(), "rhai: {message}");
}

/// Read every argument as UTF-8; a script path or expression that is not
/// valid UTF-8 cannot be handled faithfully, so it is refused.
fn utf8_args() -> Result<Vec<String>, String> {
    std::env::args_os()
        .skip(1)
        .map(|arg| {
            arg.into_string()
                .map_err(|bad| format!("argument {bad:?} is not valid UTF-8"))
        })
        .collect()
}

/// Run one piece of source and turn the outcome into an exit status.
/// `print_result` is set for `-e`, which shows the final value.
fn run_source(engine: &Engine, source: &str, print_result: bool, label: &str) -> i32 {
    match eval_source(engine, &mut Scope::new(), source) {
        Outcome::Value(value) => {
            if print_result && !value.is_unit() {
                let shown = writeln!(io::stdout(), "{value}").and_then(|()| io::stdout().flush());
                if shown.is_err() {
                    return 0;
                }
            }
            0
        }
        Outcome::Exit(code) => code,
        Outcome::Failed(message) => {
            complain(&format!("{label}{message}"));
            1
        }
        // The reader of our stdout went away: stop quietly, like a filter.
        Outcome::OutputClosed => 0,
    }
}

/// Load a script's text through the same bounded, UTF-8-checked path `os::read` uses.
fn load(host: &StdHost, path: Option<&str>, max: usize) -> Result<String, String> {
    let (name, bytes) = match path {
        Some(path) => (path, host.read_file(path, max)),
        None => ("standard input", host.read_stdin(max)),
    };
    let bytes = bytes.map_err(|e| format!("{name}: {e}"))?;
    String::from_utf8(bytes).map_err(|_| format!("{name}: not valid UTF-8"))
}

fn run(options: Options) -> i32 {
    let host = Rc::new(StdHost::new(options.args.clone()));
    let engine = build_engine(host.clone(), &options.config);
    let max_io = options.config.limits.max_io_bytes;
    let script = |path: Option<&str>| match load(&host, path, max_io) {
        Ok(source) => {
            let label = path.map_or(String::new(), |p| format!("{p}: "));
            run_source(&engine, &source, false, &label)
        }
        Err(message) => {
            complain(&message);
            1
        }
    };
    match &options.mode {
        Mode::Eval(expr) => run_source(&engine, expr, true, ""),
        Mode::Script(path) => script(Some(path)),
        Mode::Stdin => script(None),
        Mode::Repl => repl_io::run(
            &engine,
            if options.quiet {
                repl_io::Style::Batch
            } else {
                repl_io::Style::Interactive
            },
            io::stdin().lock(),
            io::stdout(),
            io::stderr(),
        ),
        Mode::Help | Mode::Version => 0,
    }
}

fn main() {
    let options = match utf8_args().and_then(cli::parse) {
        Ok(options) => options,
        Err(message) => {
            complain(&message);
            std::process::exit(2);
        }
    };
    let code = match options.mode {
        Mode::Help => {
            let _ = writeln!(io::stdout(), "{}", cli::USAGE);
            0
        }
        Mode::Version => {
            let _ = writeln!(
                io::stdout(),
                "rhai {} (engine {})",
                env!("CARGO_PKG_VERSION"),
                rhai_lazy::ENGINE_VERSION
            );
            0
        }
        _ => run(options),
    };
    let _ = io::stdout().flush();
    std::process::exit(code);
}
