//! Everything OS-specific the core needs, behind one trait the front end
//! implements (docs/lazysky-plan.md §2.2). The core never names a LazyOS
//! crate or a Windows API.

use std::path::PathBuf;
use std::sync::Arc;

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

pub trait Platform: Send + Sync {
    /// The trust anchors for every `https:` request.
    fn ca_roots(&self) -> Roots;
    /// Where `settings.toml` and `session.bin` live.
    fn app_dir(&self) -> PathBuf;
    /// The default folder for saved pictures.
    fn pictures_dir(&self) -> PathBuf;
    /// Hands a link to the OS (the browser, or `mimed` on LazyOS).
    fn open_url(&self, url: &str) -> Result<(), String>;
    /// One evidence line: a serial marker on LazyOS, stdout on a desktop.
    fn log(&self, line: &str);
}
