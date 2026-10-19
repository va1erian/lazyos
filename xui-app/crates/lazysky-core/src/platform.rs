//! Everything OS-specific the core needs, behind one trait the front end
//! implements (docs/lazysky-plan.md §2.2). The core never names a LazyOS
//! crate or a Windows API.

use std::path::PathBuf;

pub use lazyhttps::Roots;

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
