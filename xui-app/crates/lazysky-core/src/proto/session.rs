//! `createSession` / `refreshSession` answers (§7.3 Login).
//!
//! The JWTs stay in memory and never reach a log: `Debug` redacts them.

use std::fmt;

use serde_json::Value;

use super::text;

#[derive(Clone, PartialEq, Eq)]
pub struct Session {
    pub access_jwt: String,
    pub refresh_jwt: String,
    pub did: String,
    pub handle: String,
    /// The PDS the account lives on; the login service when the DID document
    /// names none.
    pub pds: String,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("did", &self.did)
            .field("handle", &self.handle)
            .field("pds", &self.pds)
            .field("access_jwt", &"<redacted>")
            .field("refresh_jwt", &"<redacted>")
            .finish()
    }
}

impl Session {
    /// Reads a `createSession` or `refreshSession` answer. `service` is the
    /// base URL the request went to (the fallback PDS).
    pub fn from_json(v: &Value, service: &str) -> Result<Session, String> {
        let need = |key: &str| {
            text(v, key)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| format!("the session answer has no {key}"))
        };
        let pds = pds_endpoint(v).unwrap_or_else(|| service.trim_end_matches('/').to_string());
        Ok(Session {
            access_jwt: need("accessJwt")?,
            refresh_jwt: need("refreshJwt")?,
            did: need("did")?,
            handle: need("handle")?,
            pds,
        })
    }

    /// `<pds>/xrpc/<nsid>`.
    pub fn xrpc(&self, nsid: &str) -> String {
        format!("{}/xrpc/{nsid}", self.pds)
    }
}

/// The `#atproto_pds` service endpoint of the answer's DID document. Only an
/// `https:` (or, for a local harness, `http:`) endpoint is believed.
fn pds_endpoint(v: &Value) -> Option<String> {
    let services = v.get("didDoc")?.get("service")?.as_array()?;
    let endpoint = services
        .iter()
        .find(|s| text(s, "id").is_some_and(|id| id.ends_with("#atproto_pds")))
        .and_then(|s| text(s, "serviceEndpoint"))?;
    let ok = endpoint.starts_with("https://") || endpoint.starts_with("http://");
    ok.then(|| endpoint.trim_end_matches('/').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn answer(extra: Value) -> Value {
        let mut v = json!({
            "accessJwt": "access.secret.jwt", "refreshJwt": "refresh.secret.jwt",
            "did": "did:plc:abc", "handle": "alice.test"
        });
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        v
    }

    #[test]
    fn the_pds_comes_from_the_did_document() {
        let v = answer(json!({"didDoc": {"service": [
            {"id": "#other", "serviceEndpoint": "https://elsewhere.test"},
            {"id": "#atproto_pds", "serviceEndpoint": "https://pds.example.test/"}
        ]}}));
        let s = Session::from_json(&v, "https://bsky.social").unwrap();
        assert_eq!(s.pds, "https://pds.example.test");
        assert_eq!(
            s.xrpc("app.bsky.feed.getTimeline"),
            "https://pds.example.test/xrpc/app.bsky.feed.getTimeline"
        );
    }

    #[test]
    fn without_a_did_document_the_service_is_the_pds() {
        let s = Session::from_json(&answer(json!({})), "https://bsky.social/").unwrap();
        assert_eq!(s.pds, "https://bsky.social");
    }

    #[test]
    fn a_hostile_endpoint_is_ignored() {
        let v = answer(json!({"didDoc": {"service": [
            {"id": "#atproto_pds", "serviceEndpoint": "javascript:alert(1)"}]}}));
        assert_eq!(
            Session::from_json(&v, "https://bsky.social").unwrap().pds,
            "https://bsky.social"
        );
    }

    #[test]
    fn debug_never_shows_a_token() {
        let s = Session::from_json(&answer(json!({})), "https://x.test").unwrap();
        let shown = format!("{s:?}");
        assert!(!shown.contains("secret"), "{shown}");
    }

    #[test]
    fn a_missing_token_is_an_error() {
        let err =
            Session::from_json(&json!({"did": "d", "handle": "h"}), "https://x.test").unwrap_err();
        assert!(err.contains("accessJwt"), "{err}");
    }
}
