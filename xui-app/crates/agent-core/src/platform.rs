//! Everything OS-specific the core needs, behind one trait the front end
//! implements (docs/ai-agent-plan.md §2). The core never names a LazyOS
//! crate or a Windows API. Later phases add `DiagSource`, `Docs` and `Secrets`
//! beside it.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub use lazyhttps::Roots;

pub trait Platform: Send + Sync {
    /// The trust anchors for every `https:` request.
    fn ca_roots(&self) -> Roots;
    /// Where `settings.toml`, the conversations and the drafts live
    /// (`fhs::app_data_dir(home, "os.lazy.agent")` on LazyOS).
    fn app_dir(&self) -> PathBuf;
    /// Seconds since the Unix epoch.
    fn now(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }
    /// Where requests go: the real service. A test platform points it at a
    /// scripted server (`Endpoint::for_testing`).
    fn endpoint(&self) -> crate::provider::Endpoint {
        crate::provider::Endpoint::fireworks()
    }
    /// Hands a link to the OS (the browser, or `mimed` on LazyOS). The window
    /// only calls it after the user confirmed the address.
    fn open_url(&self, url: &str) -> Result<(), String> {
        Err(format!("cannot open {url} here"))
    }
    /// One evidence line: a serial marker on LazyOS, stdout on a desktop.
    /// Never a key, a token or message text.
    fn log(&self, line: &str);
}
