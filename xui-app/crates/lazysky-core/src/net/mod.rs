//! HTTPS for the core: the shared `lazyhttps` crate (TLS, roots, resolver,
//! capped blocking requests; docs/ai-agent-plan.md §2), the Bluesky API
//! client and URL encoding.

pub mod api;
pub mod url;

pub use lazyhttps::{roots, Http, HttpOptions, Response};

/// The client's options: the shared defaults with LazySky's `User-Agent`.
pub fn http_options() -> HttpOptions {
    HttpOptions {
        user_agent: concat!("LazySky/", env!("CARGO_PKG_VERSION")).to_string(),
        ..HttpOptions::default()
    }
}
