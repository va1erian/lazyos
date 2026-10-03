//! Why a run failed, the message for it, and the exit code each personality
//! uses (curl's numbers for `curl` and `fetch`, GNU wget's for `wget`).

use crate::opts::Personality;

/// A TLS failure, already explained for a person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TlsFailure {
    /// One line: what went wrong (`unknown issuer`, `certificate expired`).
    pub reason: String,
    /// The certificate was rejected (curl 60), not the handshake (curl 35).
    pub certificate: bool,
    /// Extra lines for a clock problem: system time against the validity.
    pub clock: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    Usage(String),
    BadUrl(String),
    UnsupportedScheme(String),
    /// The CA bundle could not be used; no connection was attempted.
    Roots(String),
    Resolve(String),
    Connect(String),
    Timeout,
    Tls(TlsFailure),
    TooManyRedirects(u32),
    /// An `https` page redirected to plain `http`.
    Downgrade(String),
    /// A redirect without a usable `Location`.
    BadRedirect(String),
    /// HTTP status >= 400 where that is a failure (curl `-f`, wget).
    HttpStatus(u16, String),
    BodyTooLarge(u64),
    Write(String),
    Recv(String),
}

impl Failure {
    /// curl's exit code for this failure (also used by `fetch`).
    pub fn curl_code(&self) -> i32 {
        match self {
            Failure::UnsupportedScheme(_) | Failure::Downgrade(_) => 1,
            Failure::Usage(_) => 2,
            Failure::BadUrl(_) => 3,
            Failure::Resolve(_) => 6,
            Failure::Connect(_) => 7,
            Failure::BadRedirect(_) => 8,
            Failure::HttpStatus(..) => 22,
            Failure::Write(_) => 23,
            Failure::Timeout => 28,
            Failure::Tls(t) if t.certificate => 60,
            Failure::Tls(_) => 35,
            Failure::TooManyRedirects(_) => 47,
            Failure::Recv(_) => 56,
            Failure::BodyTooLarge(_) => 63,
            Failure::Roots(_) => 77,
        }
    }

    /// GNU wget's exit code for this failure.
    pub fn wget_code(&self) -> i32 {
        match self {
            Failure::Usage(_) => 2,
            Failure::Write(_) => 3,
            Failure::Resolve(_) | Failure::Connect(_) | Failure::Timeout | Failure::Recv(_) => 4,
            Failure::Tls(_) | Failure::Roots(_) => 5,
            Failure::HttpStatus(..) | Failure::BadRedirect(_) => 8,
            _ => 1,
        }
    }

    pub fn code(&self, personality: Personality) -> i32 {
        match personality {
            Personality::Wget => self.wget_code(),
            _ => self.curl_code(),
        }
    }

    /// The message, without the program-name prefix.
    pub fn message(&self) -> String {
        match self {
            Failure::Usage(m) => m.clone(),
            Failure::BadUrl(m) => format!("bad URL: {m}"),
            Failure::UnsupportedScheme(s) => {
                format!("protocol \"{s}\" is not supported (only https and http)")
            }
            Failure::Roots(m) => m.clone(),
            Failure::Resolve(host) => format!("could not resolve host: {host}"),
            Failure::Connect(m) => format!("could not connect: {m}"),
            Failure::Timeout => "operation timed out".into(),
            Failure::Tls(t) => match &t.clock {
                Some(clock) => format!("TLS: {}\n{clock}", t.reason),
                None => format!("TLS: {}", t.reason),
            },
            Failure::TooManyRedirects(n) => format!("maximum ({n}) redirects followed"),
            Failure::Downgrade(to) => format!(
                "refusing to follow a redirect from https to plain http ({to}): \
                 the page would no longer be protected"
            ),
            Failure::BadRedirect(m) => format!("bad redirect: {m}"),
            Failure::HttpStatus(code, reason) => {
                format!("the server returned HTTP {code} {reason}")
            }
            Failure::BodyTooLarge(cap) => {
                format!(
                    "response body exceeds the {} MiB limit",
                    cap / (1024 * 1024)
                )
            }
            Failure::Write(m) => format!("cannot write output: {m}"),
            Failure::Recv(m) => format!("receiving data failed: {m}"),
        }
    }

    /// The line(s) printed on stderr: `curl: (6) ...`, `wget: ...`.
    pub fn render(&self, personality: Personality) -> String {
        match personality {
            Personality::Curl => format!("curl: ({}) {}", self.curl_code(), self.message()),
            other => format!("{}: {}", other.name(), self.message()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tls(certificate: bool) -> Failure {
        Failure::Tls(TlsFailure {
            reason: "r".into(),
            certificate,
            clock: None,
        })
    }

    #[test]
    fn curl_codes_match_curl() {
        assert_eq!(Failure::Resolve("h".into()).curl_code(), 6);
        assert_eq!(Failure::Connect("x".into()).curl_code(), 7);
        assert_eq!(Failure::HttpStatus(404, "Not Found".into()).curl_code(), 22);
        assert_eq!(Failure::Timeout.curl_code(), 28);
        assert_eq!(tls(false).curl_code(), 35);
        assert_eq!(tls(true).curl_code(), 60);
        assert_eq!(Failure::TooManyRedirects(3).curl_code(), 47);
    }

    #[test]
    fn wget_codes_match_wget() {
        assert_eq!(Failure::Resolve("h".into()).wget_code(), 4);
        assert_eq!(tls(true).wget_code(), 5);
        assert_eq!(Failure::HttpStatus(500, "".into()).wget_code(), 8);
        assert_eq!(Failure::Usage("x".into()).wget_code(), 2);
    }

    #[test]
    fn render_prefixes() {
        let f = Failure::Resolve("example.invalid".into());
        assert_eq!(
            f.render(Personality::Curl),
            "curl: (6) could not resolve host: example.invalid"
        );
        assert_eq!(
            f.render(Personality::Wget),
            "wget: could not resolve host: example.invalid"
        );
    }
}
