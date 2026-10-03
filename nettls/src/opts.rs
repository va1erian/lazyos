//! What one run of the program should do, independent of which personality's
//! option names asked for it.

use std::path::PathBuf;
use std::time::Duration;

/// Which command-line dialect the binary speaks, chosen from `argv[0]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Personality {
    Fetch,
    Curl,
    Wget,
}

impl Personality {
    /// The personality for `argv[0]`: its basename `curl` or `wget` (an
    /// `.elf` suffix, as build outputs carry, is ignored); anything else is
    /// `fetch`, the tool's own name.
    pub fn from_argv0(argv0: &str) -> Personality {
        let base = argv0.rsplit('/').next().unwrap_or(argv0);
        let base = base.strip_suffix(".elf").unwrap_or(base);
        match base {
            "curl" => Personality::Curl,
            "wget" => Personality::Wget,
            _ => Personality::Fetch,
        }
    }

    /// The name used to prefix messages.
    pub fn name(self) -> &'static str {
        match self {
            Personality::Fetch => "fetch",
            Personality::Curl => "curl",
            Personality::Wget => "wget",
        }
    }
}

/// Where the response body goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Output {
    /// Standard output (curl's and fetch's default, wget's `-O -`).
    Stdout,
    /// A named file, created or truncated (`-o FILE`, wget `-O FILE`).
    File(PathBuf),
    /// The URL's last path segment, overwritten (curl `-O`).
    RemoteName,
    /// The URL's last path segment or `index.html`, never overwriting an
    /// existing file (`name.1`, `name.2`, ...): wget's default.
    WgetName,
}

/// One request, as every personality's parser produces it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    pub personality: Personality,
    pub url: String,
    /// Send `HEAD` and print the response headers instead of a body.
    pub head: bool,
    /// Print the response headers before the body (curl `-i`).
    pub include: bool,
    pub output: Output,
    /// Follow redirects (curl/fetch `-L`; wget's default).
    pub follow: bool,
    pub max_redirs: u32,
    /// Limit on resolving + connecting + the TLS handshake of each hop.
    pub connect_timeout: Option<Duration>,
    /// Limit on the whole run, across redirects.
    pub max_time: Option<Duration>,
    /// TLS handshake details, the chain and the exchanged headers on stderr.
    pub verbose: bool,
    /// No progress and no error messages (curl `-s`, wget `-q`).
    pub quiet: bool,
    /// Error messages even when quiet (curl `-S`).
    pub show_error: bool,
    /// An HTTP status >= 400 is a failure and writes no body (curl `-f`).
    pub fail: bool,
    /// Print each hop's response headers to stderr (wget `-S`).
    pub server_response: bool,
    /// Extra request headers, already validated as `name: value`.
    pub headers: Vec<(String, String)>,
    pub user_agent: Option<String>,
    /// curl `-w` template, printed after the transfer.
    pub write_out: Option<String>,
    /// A CA bundle to trust instead of the system one.
    pub cacert: Option<PathBuf>,
}

impl Options {
    /// The defaults of `personality` for `url`; parsers adjust from here.
    pub fn new(personality: Personality, url: String) -> Options {
        let wget = personality == Personality::Wget;
        Options {
            personality,
            url,
            head: false,
            include: false,
            output: if wget {
                Output::WgetName
            } else {
                Output::Stdout
            },
            follow: wget,
            max_redirs: match personality {
                Personality::Curl => 50,
                Personality::Wget => 20,
                Personality::Fetch => 10,
            },
            connect_timeout: None,
            max_time: None,
            verbose: false,
            quiet: false,
            show_error: false,
            fail: false,
            server_response: false,
            headers: Vec::new(),
            user_agent: None,
            write_out: None,
            cacert: None,
        }
    }
}

/// What the command line asked for.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Run(Box<Options>),
    /// Print this text to stdout and exit 0 (`--help`, `--version`).
    Print(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn personality_follows_argv0_basename() {
        assert_eq!(
            Personality::from_argv0("/system/bin/curl"),
            Personality::Curl
        );
        assert_eq!(Personality::from_argv0("wget"), Personality::Wget);
        assert_eq!(
            Personality::from_argv0("target/nettls/fetch.elf"),
            Personality::Fetch
        );
        assert_eq!(Personality::from_argv0("./curl.elf"), Personality::Curl);
        assert_eq!(Personality::from_argv0("/bin/curlish"), Personality::Fetch);
        assert_eq!(Personality::from_argv0(""), Personality::Fetch);
    }

    #[test]
    fn defaults_differ_by_personality() {
        let w = Options::new(Personality::Wget, "u".into());
        assert!(w.follow);
        assert_eq!(w.output, Output::WgetName);
        let c = Options::new(Personality::Curl, "u".into());
        assert!(!c.follow);
        assert_eq!(c.output, Output::Stdout);
        assert_eq!(c.max_redirs, 50);
    }
}
