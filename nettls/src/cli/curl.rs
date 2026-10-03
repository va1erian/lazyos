//! The `curl` personality: a subset of curl's options with curl's meanings.

use super::args::{self, Spec};
use super::{one_url, HELP_FOOTER};
use crate::opts::{Command, Options, Output, Personality};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Id {
    Silent,
    ShowError,
    Location,
    Output,
    RemoteName,
    Head,
    Include,
    Verbose,
    Fail,
    MaxRedirs,
    ConnectTimeout,
    MaxTime,
    Header,
    UserAgent,
    WriteOut,
    CaCert,
    Insecure,
    Help,
    Version,
}

const SPECS: &[Spec<Id>] = &[
    Spec {
        short: Some('s'),
        long: Some("silent"),
        takes_value: false,
        id: Id::Silent,
    },
    Spec {
        short: Some('S'),
        long: Some("show-error"),
        takes_value: false,
        id: Id::ShowError,
    },
    Spec {
        short: Some('L'),
        long: Some("location"),
        takes_value: false,
        id: Id::Location,
    },
    Spec {
        short: Some('o'),
        long: Some("output"),
        takes_value: true,
        id: Id::Output,
    },
    Spec {
        short: Some('O'),
        long: Some("remote-name"),
        takes_value: false,
        id: Id::RemoteName,
    },
    Spec {
        short: Some('I'),
        long: Some("head"),
        takes_value: false,
        id: Id::Head,
    },
    Spec {
        short: Some('i'),
        long: Some("include"),
        takes_value: false,
        id: Id::Include,
    },
    Spec {
        short: Some('v'),
        long: Some("verbose"),
        takes_value: false,
        id: Id::Verbose,
    },
    Spec {
        short: Some('f'),
        long: Some("fail"),
        takes_value: false,
        id: Id::Fail,
    },
    Spec {
        short: None,
        long: Some("max-redirs"),
        takes_value: true,
        id: Id::MaxRedirs,
    },
    Spec {
        short: None,
        long: Some("connect-timeout"),
        takes_value: true,
        id: Id::ConnectTimeout,
    },
    Spec {
        short: Some('m'),
        long: Some("max-time"),
        takes_value: true,
        id: Id::MaxTime,
    },
    Spec {
        short: Some('H'),
        long: Some("header"),
        takes_value: true,
        id: Id::Header,
    },
    Spec {
        short: Some('A'),
        long: Some("user-agent"),
        takes_value: true,
        id: Id::UserAgent,
    },
    Spec {
        short: Some('w'),
        long: Some("write-out"),
        takes_value: true,
        id: Id::WriteOut,
    },
    Spec {
        short: None,
        long: Some("cacert"),
        takes_value: true,
        id: Id::CaCert,
    },
    Spec {
        short: Some('k'),
        long: Some("insecure"),
        takes_value: false,
        id: Id::Insecure,
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

const HELP: &str = "Usage: curl [options] URL
 -s, --silent             no progress or error messages
 -S, --show-error         error messages even with -s
 -L, --location           follow redirects
 -o, --output FILE        write the body to FILE
 -O, --remote-name        write the body to the URL's file name
 -I, --head               HEAD request; print the response headers
 -i, --include            print the response headers before the body
 -v, --verbose            TLS handshake, certificate chain and headers on stderr
 -f, --fail               fail (exit 22) on HTTP status >= 400, writing no body
     --max-redirs N       redirect limit (default 50)
     --connect-timeout S  limit on connecting, per hop
 -m, --max-time S         limit on the whole transfer
 -H, --header 'N: V'      extra request header
 -A, --user-agent UA      User-Agent header
 -w, --write-out FMT      print FMT afterwards (%{http_code}, %{url_effective}, ...)
     --cacert FILE        trust the CAs in FILE instead of the system bundle
";

/// Parse curl's arguments (without `argv[0]`).
pub fn parse(argv: &[String]) -> Result<Command, String> {
    let parsed = args::tokenize(argv, SPECS)?;
    let mut opts = Options::new(Personality::Curl, String::new());
    for (id, v) in parsed.options {
        match id {
            Id::Silent => opts.quiet = true,
            Id::ShowError => opts.show_error = true,
            Id::Location => opts.follow = true,
            Id::Output => opts.output = Output::File(args::value(v).into()),
            Id::RemoteName => opts.output = Output::RemoteName,
            Id::Head => opts.head = true,
            Id::Include => opts.include = true,
            Id::Verbose => opts.verbose = true,
            Id::Fail => opts.fail = true,
            Id::MaxRedirs => opts.max_redirs = args::redirect_limit(&args::value(v))?,
            Id::ConnectTimeout => opts.connect_timeout = args::seconds(&args::value(v))?,
            Id::MaxTime => opts.max_time = args::seconds(&args::value(v))?,
            Id::Header => opts.headers.push(args::header(&args::value(v))?),
            Id::UserAgent => opts.user_agent = Some(args::value(v)),
            Id::WriteOut => opts.write_out = Some(args::value(v)),
            Id::CaCert => opts.cacert = Some(args::value(v).into()),
            Id::Insecure => return Err(super::insecure_refused("-k/--insecure", "--cacert FILE")),
            Id::Help => return Ok(Command::Print(format!("{HELP}{HELP_FOOTER}"))),
            Id::Version => return Ok(Command::Print(super::version("curl"))),
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
    fn combined_short_flags() {
        let o = run(&["-sSLo", "out.html", "https://example.com/"]).unwrap();
        assert!(o.quiet && o.show_error && o.follow);
        assert_eq!(o.output, Output::File("out.html".into()));
        assert_eq!(o.url, "https://example.com/");
        let o = run(&["-sSL", "https://example.com/"]).unwrap();
        assert!(o.quiet && o.show_error && o.follow);
        assert_eq!(o.output, Output::Stdout);
    }

    #[test]
    fn everything_else() {
        let o = run(&[
            "-I",
            "-i",
            "-v",
            "-f",
            "-O",
            "--max-redirs",
            "3",
            "--connect-timeout",
            "2",
            "-m",
            "9.5",
            "-H",
            "X-A: 1",
            "-A",
            "ua/1",
            "-w",
            "%{http_code}",
            "--cacert",
            "ca.pem",
            "http://h/x",
        ])
        .unwrap();
        assert!(o.head && o.include && o.verbose && o.fail);
        assert_eq!(o.output, Output::RemoteName);
        assert_eq!(o.max_redirs, 3);
        assert_eq!(o.connect_timeout, Some(Duration::from_secs(2)));
        assert_eq!(o.max_time, Some(Duration::from_millis(9500)));
        assert_eq!(o.headers, vec![("X-A".into(), "1".into())]);
        assert_eq!(o.user_agent.as_deref(), Some("ua/1"));
        assert_eq!(o.write_out.as_deref(), Some("%{http_code}"));
        assert_eq!(o.cacert, Some("ca.pem".into()));
        assert!(!o.follow, "curl follows only with -L");
    }

    #[test]
    fn insecure_is_refused() {
        for flag in ["-k", "--insecure", "-sk"] {
            let err = run(&[flag, "https://h/"]).unwrap_err();
            assert!(err.contains("not supported"), "{err}");
        }
    }

    #[test]
    fn url_count_and_unknowns() {
        assert!(run(&[]).unwrap_err().contains("no URL"));
        assert!(run(&["a", "b"]).is_err());
        assert!(run(&["--proxy", "x", "u"]).is_err());
        assert!(matches!(
            parse(&["--help".to_string()]),
            Ok(Command::Print(_))
        ));
    }
}
