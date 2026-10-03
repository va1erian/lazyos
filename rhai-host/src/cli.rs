//! Command-line parsing for `rhai`: pure (no I/O), so it is unit-tested.

use rhai_lazy::{Config, Limits};

pub const USAGE: &str = "\
usage: rhai [options] [-e EXPR | SCRIPT | -] [ARG...]
       rhai                      interactive REPL
       rhai -e EXPR [ARG...]     evaluate EXPR and print its value
       rhai SCRIPT [ARG...]      run a script file (a #! first line is ignored)
       rhai - [ARG...]           run the script read from standard input

options:
  -e, --eval EXPR        evaluate EXPR (the result is printed unless it is ())
  -q, --quiet            REPL: no banner, prompts or echo (for `... | rhai -q`)
      --sandbox          disable eval() and import
      --max-ops N        operations before a script is stopped (0 = unlimited)
      --max-call-levels N  nested function calls (1..=MAX)
      --max-expr-depth N expression nesting depth
      --max-string N     bytes in one string (0 = unlimited)
      --max-array N      elements in one array (0 = unlimited)
      --max-map N        entries in one map (0 = unlimited)
      --max-io N         bytes one read may load (files, stdin; at least 1)
  -h, --help             show this text
  -V, --version          show the version

The script sees os::args() (the ARGs), env(), exit(n), clock(), sleep(ms),
read(path), write(path, text), ls(path) and stdin_text(). On LazyOS it also
sees msg::: msg::interfaces(), msg::services(), msg::describe(name) and
msg::connect(interface[, service]), whose methods call the service
(msg::connect(\"os.lazy.confd.v1\").info()), msg::subscribe(filter),
msg::publish(topic, value), msg::on(filter, |e| ...), msg::serve(name,
interface, #{Method: |args| ...}) and msg::run([ms]) / msg::stop(), and
one generated module per interface: sys::confd::get(path),
sys::confd::on_changed(|e| ...), ... (msg::describe names each one).
exit status: the script's exit(n), else 0 on success, 1 on error, 2 on misuse.";

/// The deepest call nesting a flag may request. The guest's main thread has a
/// 1 MiB stack; the default (32) is well inside it and this ceiling was
/// measured in the guest (see `docs/architecture/userland.md`).
pub const MAX_CALL_LEVELS: usize = 64;

/// What to run.
#[derive(Debug, PartialEq, Eq)]
pub enum Mode {
    Repl,
    Eval(String),
    Script(String),
    Stdin,
    Help,
    Version,
}

/// A parsed command line.
#[derive(Debug)]
pub struct Options {
    pub mode: Mode,
    /// The script's arguments (`os::args()`).
    pub args: Vec<String>,
    pub quiet: bool,
    pub config: Config,
}

fn number<T: std::str::FromStr>(flag: &str, value: Option<String>) -> Result<T, String> {
    let value = value.ok_or_else(|| format!("{flag} needs a value"))?;
    value
        .parse()
        .map_err(|_| format!("{flag}: `{value}` is not a valid number"))
}

fn positive(flag: &str, value: usize) -> Result<usize, String> {
    if value == 0 {
        Err(format!("{flag} must be at least 1"))
    } else {
        Ok(value)
    }
}

/// Parse `argv[1..]`. Options end at the first positional argument (the
/// script, or `-`) or at `--`; everything after belongs to the script.
pub fn parse<I: IntoIterator<Item = String>>(argv: I) -> Result<Options, String> {
    let mut argv = argv.into_iter();
    let mut options = Options {
        mode: Mode::Repl,
        args: Vec::new(),
        quiet: false,
        config: Config::default(),
    };
    let limits: &mut Limits = &mut options.config.limits;
    while let Some(arg) = argv.next() {
        // `--flag=value` is the same as `--flag value`.
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => {
                (flag.to_string(), Some(value.to_string()))
            }
            _ => (arg.clone(), None),
        };
        let mut value = || inline.clone().or_else(|| argv.next());
        match flag.as_str() {
            "-h" | "--help" => {
                options.mode = Mode::Help;
                return Ok(options);
            }
            "-V" | "--version" => {
                options.mode = Mode::Version;
                return Ok(options);
            }
            "-q" | "--quiet" => options.quiet = true,
            "--sandbox" => options.config.sandbox = true,
            "-e" | "--eval" => {
                let expr = value().ok_or_else(|| format!("{flag} needs an expression"))?;
                options.mode = Mode::Eval(expr);
            }
            "--max-ops" => limits.max_operations = number(&flag, value())?,
            "--max-call-levels" => {
                let n = positive(&flag, number(&flag, value())?)?;
                if n > MAX_CALL_LEVELS {
                    return Err(format!("{flag} may be at most {MAX_CALL_LEVELS}"));
                }
                limits.max_call_levels = n;
            }
            "--max-expr-depth" => {
                let n = positive(&flag, number(&flag, value())?)?;
                limits.max_expr_depth = n;
                limits.max_fn_expr_depth = n;
            }
            "--max-string" => limits.max_string_size = number(&flag, value())?,
            "--max-array" => limits.max_array_size = number(&flag, value())?,
            "--max-map" => limits.max_map_size = number(&flag, value())?,
            "--max-io" => limits.max_io_bytes = positive(&flag, number(&flag, value())?)?,
            "--" => {
                // The next word is the script (unless -e already chose a mode).
                if matches!(options.mode, Mode::Repl) {
                    if let Some(script) = argv.next() {
                        options.mode = Mode::Script(script);
                    }
                }
                options.args.extend(argv);
                return Ok(options);
            }
            "-" if matches!(options.mode, Mode::Repl) => {
                options.mode = Mode::Stdin;
                options.args.extend(argv);
                return Ok(options);
            }
            unknown if unknown.starts_with('-') && unknown != "-" => {
                return Err(format!("unknown option {unknown} (try --help)"));
            }
            _ => {
                // First positional: the script, or (after -e) the first ARG.
                if matches!(options.mode, Mode::Repl) {
                    options.mode = Mode::Script(arg);
                } else {
                    options.args.push(arg);
                }
                options.args.extend(argv);
                return Ok(options);
            }
        }
    }
    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_words(words: &[&str]) -> Result<Options, String> {
        parse(words.iter().map(|w| w.to_string()))
    }

    #[test]
    fn no_arguments_is_the_repl() {
        let o = parse_words(&[]).unwrap();
        assert_eq!(o.mode, Mode::Repl);
        assert!(!o.quiet);
    }

    #[test]
    fn eval_takes_the_expression_and_trailing_args() {
        let o = parse_words(&["-e", "1+2", "a", "b"]).unwrap();
        assert_eq!(o.mode, Mode::Eval("1+2".into()));
        assert_eq!(o.args, ["a", "b"]);
        let o = parse_words(&["--eval=x", "y"]).unwrap();
        assert_eq!(o.mode, Mode::Eval("x".into()));
        assert_eq!(o.args, ["y"]);
    }

    #[test]
    fn a_script_takes_everything_after_it_as_arguments() {
        let o = parse_words(&["--sandbox", "s.rhai", "-e", "--max-ops", "x"]).unwrap();
        assert_eq!(o.mode, Mode::Script("s.rhai".into()));
        assert_eq!(o.args, ["-e", "--max-ops", "x"]);
        assert!(o.config.sandbox);
    }

    #[test]
    fn dash_reads_the_script_from_stdin() {
        let o = parse_words(&["-", "arg"]).unwrap();
        assert_eq!(o.mode, Mode::Stdin);
        assert_eq!(o.args, ["arg"]);
    }

    #[test]
    fn double_dash_ends_options() {
        let o = parse_words(&["--", "-weird-name.rhai", "x"]).unwrap();
        assert_eq!(o.mode, Mode::Script("-weird-name.rhai".into()));
        assert_eq!(o.args, ["x"]);
        let o = parse_words(&["-e", "1", "--", "-x"]).unwrap();
        assert_eq!(o.args, ["-x"]);
    }

    #[test]
    fn limits_are_configurable() {
        let o = parse_words(&[
            "--max-ops",
            "99",
            "--max-call-levels=8",
            "--max-string",
            "10",
            "--max-array",
            "11",
            "--max-map",
            "12",
            "--max-io=13",
            "--max-expr-depth",
            "14",
            "-q",
        ])
        .unwrap();
        let l = o.config.limits;
        assert_eq!(
            (
                l.max_operations,
                l.max_call_levels,
                l.max_string_size,
                l.max_array_size
            ),
            (99, 8, 10, 11)
        );
        assert_eq!(
            (l.max_map_size, l.max_io_bytes, l.max_expr_depth),
            (12, 13, 14)
        );
        assert!(o.quiet);
    }

    #[test]
    fn bad_input_is_a_usage_error() {
        for bad in [
            &["--bogus"][..],
            &["-e"],
            &["--max-ops"],
            &["--max-ops", "abc"],
            &["--max-ops", "-1"],
            &["--max-call-levels", "0"],
            &["--max-call-levels", "65"],
            &["--max-io", "0"],
            &["--max-expr-depth", "0"],
        ] {
            assert!(parse_words(bad).is_err(), "{bad:?} was accepted");
        }
    }

    #[test]
    fn help_and_version_win() {
        assert_eq!(parse_words(&["-h"]).unwrap().mode, Mode::Help);
        assert_eq!(parse_words(&["--version"]).unwrap().mode, Mode::Version);
    }
}
