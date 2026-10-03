//! The `fetch` personality (docs/tls-plan.md §7):
//! `fetch [-I] [-o FILE] [-L] [--max-redirs N] [--timeout S] [-v] URL`.

use super::args::{self, Spec};
use super::{one_url, HELP_FOOTER};
use crate::opts::{Command, Options, Output, Personality};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Id {
    Head,
    Output,
    Location,
    MaxRedirs,
    Timeout,
    Verbose,
    CaCert,
    Help,
    Version,
}

const SPECS: &[Spec<Id>] = &[
    Spec {
        short: Some('I'),
        long: Some("head"),
        takes_value: false,
        id: Id::Head,
    },
    Spec {
        short: Some('o'),
        long: Some("output"),
        takes_value: true,
        id: Id::Output,
    },
    Spec {
        short: Some('L'),
        long: Some("location"),
        takes_value: false,
        id: Id::Location,
    },
    Spec {
        short: None,
        long: Some("max-redirs"),
        takes_value: true,
        id: Id::MaxRedirs,
    },
    Spec {
        short: None,
        long: Some("timeout"),
        takes_value: true,
        id: Id::Timeout,
    },
    Spec {
        short: Some('v'),
        long: Some("verbose"),
        takes_value: false,
        id: Id::Verbose,
    },
    Spec {
        short: None,
        long: Some("cacert"),
        takes_value: true,
        id: Id::CaCert,
    },
    Spec {
        short: Some('h'),
        long: Some("help"),
        takes_value: false,
        id: Id::Help,
    },
    Spec {
        short: Some('V'),
        long: Some("version"),
        takes_value: false,
        id: Id::Version,
    },
];

const HELP: &str = "Usage: fetch [-I] [-o FILE] [-L] [--max-redirs N] [--timeout S] [-v] URL
 -I, --head          HEAD request; print the response headers
 -o, --output FILE   write the body to FILE instead of standard output
 -L, --location      follow redirects (never https -> http)
     --max-redirs N  redirect limit (default 10)
     --timeout S     limit on the whole transfer, in seconds
 -v, --verbose       TLS handshake, certificate chain and headers on stderr
     --cacert FILE   trust the CAs in FILE instead of the system bundle
The same program answers to the names curl and wget, with their options.
";

/// Parse fetch's arguments (without `argv[0]`).
pub fn parse(argv: &[String]) -> Result<Command, String> {
    let parsed = args::tokenize(argv, SPECS)?;
    let mut opts = Options::new(Personality::Fetch, String::new());
    for (id, v) in parsed.options {
        match id {
            Id::Head => opts.head = true,
            Id::Output => opts.output = Output::File(args::value(v).into()),
            Id::Location => opts.follow = true,
            Id::MaxRedirs => opts.max_redirs = args::redirect_limit(&args::value(v))?,
            Id::Timeout => opts.max_time = args::seconds(&args::value(v))?,
            Id::Verbose => opts.verbose = true,
            Id::CaCert => opts.cacert = Some(args::value(v).into()),
            Id::Help => return Ok(Command::Print(format!("{HELP}{HELP_FOOTER}"))),
            Id::Version => return Ok(Command::Print(super::version("fetch"))),
        }
    }
    opts.url = one_url(parsed.positional)?;
    Ok(Command::Run(Box::new(opts)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn run(list: &[&str]) -> Result<Options, String> {
        let argv: Vec<String> = list.iter().map(|s| s.to_string()).collect();
        match parse(&argv)? {
            Command::Run(o) => Ok(*o),
            Command::Print(_) => Err("printed".into()),
        }
    }

    #[test]
    fn plan_surface() {
        let o = run(&[
            "-I",
            "-o",
            "f",
            "-L",
            "--max-redirs",
            "4",
            "--timeout",
            "7",
            "-v",
            "u",
        ])
        .unwrap();
        assert!(o.head && o.follow && o.verbose);
        assert_eq!(o.output, Output::File("f".into()));
        assert_eq!(o.max_redirs, 4);
        assert_eq!(o.max_time, Some(Duration::from_secs(7)));
        assert_eq!(o.url, "u");
    }

    #[test]
    fn defaults_and_refusals() {
        let o = run(&["https://h/"]).unwrap();
        assert!(!o.follow);
        assert_eq!(o.output, Output::Stdout);
        assert!(run(&["-k", "u"]).is_err());
        assert!(run(&["--insecure", "u"]).is_err());
    }
}
