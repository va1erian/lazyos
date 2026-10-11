//! XRPC errors and rate limits (§7.3 steps 3 and 4).

use std::time::Duration;

use serde_json::Value;

/// The longest the client waits out a rate limit.
pub const MAX_RATE_WAIT: Duration = Duration::from_secs(60);

/// What went wrong, as far as the client acts on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The access token ran out: refresh and retry once.
    ExpiredToken,
    /// The token is not valid at all: sign in again.
    InvalidToken,
    /// HTTP 429: wait, then retry.
    RateLimited,
    /// A password the server refused, or a missing credential.
    AuthRequired,
    NotFound,
    Other,
}

/// An error answer from the server (or the transport's status without one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XrpcError {
    pub status: u16,
    /// The lexicon's `error` name (`ExpiredToken`, `InvalidRequest`, ...).
    pub code: String,
    pub message: String,
}

impl XrpcError {
    /// Reads an XRPC error body; a body that is not the usual
    /// `{"error","message"}` object still gives an error carrying the status.
    pub fn from_response(status: u16, body: &[u8]) -> XrpcError {
        let parsed: Option<Value> = serde_json::from_slice(body).ok();
        let field = |name: &str| {
            parsed
                .as_ref()
                .and_then(|v| v.get(name))
                .and_then(Value::as_str)
                .map(|s| s.chars().take(200).collect::<String>())
                .unwrap_or_default()
        };
        XrpcError {
            status,
            code: field("error"),
            message: field("message"),
        }
    }

    pub fn kind(&self) -> ErrorKind {
        match (self.status, self.code.as_str()) {
            (_, "ExpiredToken") => ErrorKind::ExpiredToken,
            (_, "InvalidToken") => ErrorKind::InvalidToken,
            (429, _) => ErrorKind::RateLimited,
            (401, _) | (_, "AuthenticationRequired" | "AuthFactorTokenRequired") => {
                ErrorKind::AuthRequired
            }
            (404, _) | (_, "NotFound" | "RecordNotFound") => ErrorKind::NotFound,
            _ => ErrorKind::Other,
        }
    }

    /// Whether `refreshSession` and one retry may fix it (§7.3 step 3).
    pub fn needs_refresh(&self) -> bool {
        matches!(self.status, 400 | 401) && self.kind() == ErrorKind::ExpiredToken
    }

    /// A line for a status bar.
    pub fn describe(&self) -> String {
        match (self.code.is_empty(), self.message.is_empty()) {
            (true, true) => format!("the server answered {}", self.status),
            (false, true) => format!("{} ({})", self.code, self.status),
            (true, false) => format!("{} ({})", self.message, self.status),
            // The server's own sentence reads best; the code is for logs.
            (false, false) => self.message.clone(),
        }
    }
}

impl std::fmt::Display for XrpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.describe())
    }
}

/// How long a 429 asks us to wait: `ratelimit-reset` is a Unix time, `now`
/// the caller's; a missing or absurd header waits one second, and the wait is
/// capped at [`MAX_RATE_WAIT`].
pub fn rate_limit_wait(headers: &[(String, String)], now_unix: u64) -> Duration {
    let reset = headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("ratelimit-reset"))
        .and_then(|(_, v)| v.trim().parse::<u64>().ok());
    let secs = reset.map_or(1, |at| at.saturating_sub(now_unix).max(1));
    Duration::from_secs(secs).min(MAX_RATE_WAIT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_tokens_need_a_refresh() {
        let e = XrpcError::from_response(
            400,
            br#"{"error":"ExpiredToken","message":"Token has expired"}"#,
        );
        assert_eq!(e.kind(), ErrorKind::ExpiredToken);
        assert!(e.needs_refresh());
        assert_eq!(e.describe(), "Token has expired");
    }

    #[test]
    fn a_body_that_is_not_json_still_carries_the_status() {
        let e = XrpcError::from_response(502, b"<html>bad gateway</html>");
        assert_eq!(e.kind(), ErrorKind::Other);
        assert_eq!(e.describe(), "the server answered 502");
        assert!(!e.needs_refresh());
    }

    #[test]
    fn rate_limit_waits_are_capped_and_never_zero() {
        let h = |v: &str| vec![("RateLimit-Reset".to_string(), v.to_string())];
        assert_eq!(rate_limit_wait(&h("1010"), 1000), Duration::from_secs(10));
        assert_eq!(rate_limit_wait(&h("999999999999"), 1000), MAX_RATE_WAIT);
        assert_eq!(rate_limit_wait(&h("900"), 1000), Duration::from_secs(1));
        assert_eq!(rate_limit_wait(&[], 1000), Duration::from_secs(1));
        assert_eq!(rate_limit_wait(&h("soon"), 1000), Duration::from_secs(1));
    }

    #[test]
    fn messages_are_bounded() {
        let long = "x".repeat(5000);
        let body = format!(r#"{{"error":"Oops","message":"{long}"}}"#);
        assert_eq!(
            XrpcError::from_response(500, body.as_bytes()).message.len(),
            200
        );
    }
}
