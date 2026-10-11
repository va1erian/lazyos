//! The XRPC client the `api` thread drives (docs/lazysky-plan.md §7.3):
//! sign in, call with the access token, refresh once on an expired token,
//! wait out a 429.
//!
//! One [`Api`] belongs to the `api` thread. Secrets (the password, the JWTs)
//! are never put in an error message or a log line.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use super::http::{Http, Response};
use super::url;
use crate::proto::error::{rate_limit_wait, ErrorKind, XrpcError};
use crate::proto::{parse_json, Session};

/// How often a rate-limit wait looks at the shutdown flag.
const WAIT_STEP: Duration = Duration::from_millis(100);
/// A request is retried after a refresh once and after a rate limit twice.
const MAX_RATE_WAITS: u32 = 2;

/// Why a call produced no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// There is no session: the caller must sign in.
    NotSignedIn,
    /// The session could not be renewed: the caller must sign in again.
    SessionExpired,
    /// The server refused the request.
    Server(XrpcError),
    /// No answer, or one that makes no sense.
    Transport(String),
}

impl ApiError {
    /// A line for a status bar.
    pub fn describe(&self) -> String {
        match self {
            ApiError::NotSignedIn => "not signed in".into(),
            ApiError::SessionExpired => "the session expired; sign in again".into(),
            ApiError::Server(e) => e.describe(),
            ApiError::Transport(why) => why.clone(),
        }
    }
}

/// What the client tells its owner while a call is in progress.
pub trait Notes {
    /// A line for a status bar ("rate limited, waiting 12 s").
    fn status(&self, line: &str);
}

pub struct Api {
    http: Http,
    service: String,
    session: Option<Session>,
    closing: Arc<AtomicBool>,
}

impl Api {
    pub fn new(http: Http, service: &str, closing: Arc<AtomicBool>) -> Api {
        Api {
            http,
            service: service.trim_end_matches('/').to_string(),
            session: None,
            closing,
        }
    }

    pub fn session(&self) -> Option<&Session> {
        self.session.as_ref()
    }

    pub fn sign_out(&mut self) {
        self.session = None;
    }

    /// Points the next sign-in at `service`.
    pub fn set_service(&mut self, service: &str) {
        self.service = service.trim_end_matches('/').to_string();
    }

    /// `com.atproto.server.createSession`. The handle may carry a leading
    /// `@`; the password is an app password.
    pub fn sign_in(&mut self, handle: &str, password: &str) -> Result<&Session, ApiError> {
        let identifier = handle.trim().trim_start_matches('@');
        if identifier.is_empty() || password.is_empty() {
            return Err(ApiError::Transport(
                "enter a handle and an app password".into(),
            ));
        }
        let url = format!("{}/xrpc/com.atproto.server.createSession", self.service);
        let body = json!({"identifier": identifier, "password": password});
        let response = self
            .http
            .post(
                &url,
                &[("content-type", "application/json")],
                body.to_string().as_bytes(),
            )
            .map_err(ApiError::Transport)?;
        let value = ok_json(&response)?;
        let session = Session::from_json(&value, &self.service).map_err(ApiError::Transport)?;
        Ok(self.session.insert(session))
    }

    /// `GET <pds>/xrpc/<nsid>?...` with the bearer token.
    pub fn get(
        &mut self,
        nsid: &str,
        query: &[(&str, &str)],
        notes: &dyn Notes,
    ) -> Result<Value, ApiError> {
        self.call(nsid, query, None, notes)
    }

    /// `POST <pds>/xrpc/<nsid>` with a JSON body and the bearer token.
    pub fn post(&mut self, nsid: &str, body: &Value, notes: &dyn Notes) -> Result<Value, ApiError> {
        let bytes = body.to_string().into_bytes();
        self.call(nsid, &[], Some(("application/json", bytes)), notes)
    }

    /// `POST <pds>/xrpc/<nsid>` with a raw body of `content_type` (a blob
    /// upload).
    pub fn post_bytes(
        &mut self,
        nsid: &str,
        content_type: &str,
        bytes: Vec<u8>,
        notes: &dyn Notes,
    ) -> Result<Value, ApiError> {
        self.call(nsid, &[], Some((content_type, bytes)), notes)
    }

    /// Who is signed in (the repo records are written to).
    pub fn did(&self) -> Option<&str> {
        self.session.as_ref().map(|s| s.did.as_str())
    }

    fn call(
        &mut self,
        nsid: &str,
        query: &[(&str, &str)],
        body: Option<(&str, Vec<u8>)>,
        notes: &dyn Notes,
    ) -> Result<Value, ApiError> {
        let mut refreshed = false;
        let mut waits = 0;
        loop {
            let session = self.session.as_ref().ok_or(ApiError::NotSignedIn)?;
            let url = url::with_query(&session.xrpc(nsid), query);
            let bearer = format!("Bearer {}", session.access_jwt);
            let response = match &body {
                None => self.http.get(&url, &[("authorization", &bearer)]),
                Some((content_type, bytes)) => self.http.post(
                    &url,
                    &[("authorization", &bearer), ("content-type", content_type)],
                    bytes,
                ),
            }
            .map_err(ApiError::Transport)?;
            if (200..300).contains(&response.status) {
                return parse_json(&response.body).map_err(ApiError::Transport);
            }
            let error = XrpcError::from_response(response.status, &response.body);
            if error.needs_refresh() && !refreshed {
                refreshed = true;
                self.refresh()?;
                continue;
            }
            if error.kind() == ErrorKind::RateLimited && waits < MAX_RATE_WAITS {
                waits += 1;
                let wait = rate_limit_wait(&response.headers, now_unix());
                notes.status(&format!("Rate limited; waiting {} s", wait.as_secs()));
                if !self.wait(wait) {
                    return Err(ApiError::Transport("stopped".into()));
                }
                continue;
            }
            return Err(ApiError::Server(error));
        }
    }

    /// `com.atproto.server.refreshSession` with the refresh token.
    fn refresh(&mut self) -> Result<(), ApiError> {
        let current = self.session.as_ref().ok_or(ApiError::NotSignedIn)?;
        let url = current.xrpc("com.atproto.server.refreshSession");
        let bearer = format!("Bearer {}", current.refresh_jwt);
        let pds = current.pds.clone();
        let response = self
            .http
            .post(&url, &[("authorization", &bearer)], b"")
            .map_err(ApiError::Transport)?;
        // Only the server saying the refresh token is no good ends the
        // session. A 5xx, a rate limit or an unreadable answer is a bad
        // moment, not a sign-out: keep the session and let the caller's
        // error say so (the next call tries the refresh again).
        if (200..300).contains(&response.status) {
            let renewed = parse_json(&response.body)
                .ok()
                .and_then(|v| Session::from_json(&v, &pds).ok());
            return match renewed {
                Some(session) => {
                    self.session = Some(session);
                    Ok(())
                }
                None => Err(ApiError::Transport("the refresh answer was not a session".into())),
            };
        }
        let error = XrpcError::from_response(response.status, &response.body);
        if matches!(response.status, 400 | 401) {
            self.session = None;
            return Err(ApiError::SessionExpired);
        }
        Err(ApiError::Server(error))
    }

    /// Sleeps `total`, waking to see a shutdown; `false` if one came.
    fn wait(&self, total: Duration) -> bool {
        let mut left = total;
        while !left.is_zero() {
            if self.closing.load(Ordering::SeqCst) {
                return false;
            }
            let step = left.min(WAIT_STEP);
            thread::sleep(step);
            left -= step;
        }
        true
    }
}

/// The JSON of a 2xx answer, or the server's error.
fn ok_json(response: &Response) -> Result<Value, ApiError> {
    if (200..300).contains(&response.status) {
        parse_json(&response.body).map_err(ApiError::Transport)
    } else {
        Err(ApiError::Server(XrpcError::from_response(
            response.status,
            &response.body,
        )))
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
