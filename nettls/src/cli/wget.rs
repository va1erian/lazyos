//! The `wget` personality: a subset of GNU wget's options. wget follows
//! redirects and saves to the URL's file name by default.

use super::args::{self, Spec};
use super::{one_url, HELP_FOOTER};
use crate::opts::{Command, Options, Output, Personality};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Id {
    Output,
    Quiet,
    Verbose,
    Debug,
    ServerResponse,
    MaxRedirect,
    Timeout,
    UserAgent,
    Header,
    CaCertificate,
    NoCheckCertificate,
    Help,
    Version,
}

const SPECS: &[Spec<Id>] = &[
    Spec {
        short: Some('O'),
        long: Some("output-document"),
        takes_value: true,
        id: Id::Output,
    },
    Spec {
        short: Some('q'),
        long: Some("quiet"),
        takes_value: false,
        id: Id::Quiet,
    },
    Spec {
        short: Some('v'),
        long: Some("verbose"),
        takes_value: false,
        id: Id::Verbose,
    },
    Spec {
        short: Some('d'),
        long: Some("debug"),
        takes_value: false,
        id: Id::Debug,
    },
    Spec {
        short: Some('S'),
        long: Some("server-response"),
        takes_value: false,
        id: Id::ServerResponse,
    },
    Spec {
        short: None,
        long: Some("max-redirect"),
        takes_value: true,
        id: Id::MaxRedirect,
    },
    Spec {
        short: Some('T'),
        long: Some("timeout"),
        takes_value: true,
        id: Id::Timeout,
    },
    Spec {
        short: Some('U'),
        long: Some("user-agent"),
        takes_value: true,
        id: Id::UserAgent,
    },
    Spec {
        short: None,
        long: Some("header"),
        takes_value: true,
        id: Id::Header,
    },
    Spec {
        short: None,
        long: Some("ca-certificate"),
        takes_value: true,
        id: Id::CaCertificate,
    },
    Spec {
        short: None,
        long: Some("no-check-certificate"),
        takes_value: false,
        id: Id::NoCheckCertificate,
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

const HELP: &str = "Usage: wget [options] URL
 -O, --output-document FILE  write to FILE ('-' for standard output)
 -q, --quiet                 no output
 -v, --verbose               the default progress output (accepted)
 -d, --debug                 TLS handshake and certificate chain on stderr
 -S, --server-response       print the server's response headers
     --max-redirect N        redirect limit (default 20)
 -T, --timeout S             limit on connecting and on the whole transfer
 -U, --user-agent UA         User-Agent header
     --header 'N: V'         extra request header
     --ca-certificate FILE   trust the CAs in FILE instead of the system bundle
Without -O the body is saved under the URL's file name (index.html for a
directory), never overwriting an existing file (name.1, name.2, ...).
";

/// Parse wget's arguments (without `argv[0]`).
pub fn parse(argv: &[String]) -> Result<Command, String> {
    let parsed = args::tokenize(argv, SPECS)?;
    let mut opts = Options::new(Personality::Wget, String::new());
    for (id, v) in parsed.options {
        match id {
            Id::Output => {
                let file = args::value(v);
                opts.output = if file == "-" {
                    Output::Stdout
                } else {
                    Output::File(file.into())
                };
            }
            Id::Quiet => opts.quiet = true,
            Id::Verbose => {}
            Id::Debug => opts.verbose = true,
            Id::ServerResponse => opts.server_response = true,
            Id::MaxRedirect => opts.max_redirs = args::redirect_limit(&args::value(v))?,
            Id::Timeout => {
                // wget's -T bounds each network step; the closest honest
                // mapping is both the connect limit and the overall limit.
                let limit = args::seconds(&args::value(v))?;
                opts.connect_timeout = limit;
                opts.max_time = limit;
            }
            Id::UserAgent => opts.user_agent = Some(args::value(v)),
            Id::Header => opts.headers.push(args::header(&args::value(v))?),
            Id::CaCertificate => opts.cacert = Some(args::value(v).into()),
            Id::NoCheckCertificate => {
                return Err(super::insecure_refused(
                    "--no-check-certificate",
                    "--ca-certificate FILE",
                ))
            }
            Id::Help => return Ok(Command::Print(format!("{HELP}{HELP_FOOTER}"))),
            Id::Version => return Ok(Command::Print(super::version("wget"))),
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
    fn defaults_follow_and_save_by_name() {
        let o = run(&["https://example.com/a.txt"]).unwrap();
        assert!(o.follow);
        assert_eq!(o.max_redirs, 20);
        assert_eq!(o.output, Output::WgetName);
    }

    #[test]
    fn options() {
        let o = run(&[
            "-O",
            "-",
            "-q",
            "-S",
            "--max-redirect=2",
            "-T",
            "3",
            "-U",
            "ua",
            "--header",
            "Accept: text/plain",
            "--ca-certificate",
            "ca.pem",
            "-d",
            "http://h/",
        ])
        .unwrap();
        assert_eq!(o.output, Output::Stdout);
        assert!(o.quiet && o.server_response && o.verbose);
        assert_eq!(o.max_redirs, 2);
        assert_eq!(o.connect_timeout, Some(Duration::from_secs(3)));
        assert_eq!(o.max_time, Some(Duration::from_secs(3)));
        assert_eq!(o.user_agent.as_deref(), Some("ua"));
        assert_eq!(o.headers, vec![("Accept".into(), "text/plain".into())]);
        assert_eq!(o.cacert, Some("ca.pem".into()));
        let o = run(&["-Ofile.bin", "u"]).unwrap();
        assert_eq!(o.output, Output::File("file.bin".into()));
    }

    #[test]
    fn no_check_certificate_is_refused() {
        let err = run(&["--no-check-certificate", "https://h/"]).unwrap_err();
        assert!(err.contains("not supported"), "{err}");
    }
}
