//! HTTPS for the core: one rustls configuration, a ureq connector, a
//! resolver that stays on the calling thread, and a capped blocking GET.
//!
//! `tls.rs`, `roots.rs` and `resolve.rs` are adaptations of LazyWeb's
//! `xui-app/web/src/fetch/{tls,roots,resolve}.rs`; LazyWeb is declared
//! GPL-2.0-only, so they are copied, not depended on. Keep them in step.

pub mod api;
pub mod http;
mod resolve;
pub mod roots;
pub(crate) mod tls;
pub mod url;

pub use http::{Http, HttpOptions, Response};
