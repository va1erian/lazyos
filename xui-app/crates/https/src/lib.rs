//! HTTPS for LazySky and LazyAgent: one rustls configuration, a ureq
//! connector, a resolver that stays on the calling thread, a capped blocking
//! request path and a streaming one (docs/ai-agent-plan.md §2 and §5.2).
//!
//! `tls.rs`, `roots.rs` and `resolve.rs` are adaptations of LazyWeb's
//! `xui-app/web/src/fetch/{tls,roots,resolve}.rs`; LazyWeb is declared
//! GPL-2.0-only, so they are copied, not depended on. Keep them in step.
//! This crate is GPL-3.0-or-later and shared by the two apps that may link
//! it; it names no xui type and no LazyOS crate.

mod http;
mod resolve;
pub mod roots;
mod tls;

use std::path::PathBuf;
use std::sync::Arc;

pub use http::{Http, HttpOptions, Policy, Response, StreamResponse, Watch, REFUSED};

/// Where the TLS trust anchors come from.
#[derive(Clone, Debug)]
pub enum Roots {
    /// A PEM bundle on disk (LazyOS: `fhs::etc::CA_BUNDLE`).
    Bundle(PathBuf),
    /// A PEM bundle in memory (tests; a throwaway CA).
    Pem(Arc<Vec<u8>>),
    /// The host's trust store (feature `native-roots`, desktop only).
    Native,
}

/// Installs `nettls-crypto` as the process's default rustls provider (the
/// GPLv2-compatible one, docs/tls-plan.md §3.2). Call it once, before any
/// thread starts; `false` means another provider was already installed. The
/// crate's own TLS configuration names its provider explicitly, so this only
/// matters to other rustls users in the same process.
pub fn install_tls_provider() -> bool {
    rustls::crypto::CryptoProvider::install_default(nettls_crypto::provider()).is_ok()
}
