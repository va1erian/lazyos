//! A world for the full views' tests: the service over the fake web (`web::FakeWeb`), with a
//! profiles root (only named: the fake engine keeps cookies in memory) and a real Downloads folder
//! in the host's temporary directory, removed when the test ends.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use messenger_generated::os_lazy_webd_view_v1 as wire;
use xui_webd::events::EventQueue;
use xui_webd::fullconfig::Places;
use xui_webd_client::full::{FullWebView, Scope, ViewConfig};

use super::web::FakeWeb;
use super::{world_with, SharedWorld, TestTransport};

/// The service's profiles root: an absolute path the service accepts and never touches (the fake
/// engine keeps the profile in memory).
const PROFILES: &str = "/webd-test/profiles";

pub type Full = FullWebView<TestTransport<FakeWeb>>;

/// The fake web, the service over it, and the Downloads folder.
pub struct Web {
    pub world: SharedWorld<FakeWeb>,
    pub web: FakeWeb,
    pub downloads: PathBuf,
}

impl Drop for Web {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.downloads);
    }
}

pub fn web() -> Web {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let downloads = std::env::temp_dir().join(format!(
        "webd-client-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let queue = Arc::new(EventQueue::new());
    let web = FakeWeb::new(queue.clone());
    let places = Places {
        profiles: Some(PathBuf::from(PROFILES)),
        downloads: Some(downloads.clone()),
    };
    let world = world_with(web.clone(), queue, places);
    Web {
        world,
        web,
        downloads,
    }
}

impl Web {
    /// A full view of the app labelled `label`, not created yet.
    pub fn view(&self, label: u32, scope: Scope, config: ViewConfig) -> Full {
        FullWebView::new(TestTransport::full(&self.world, label), scope, config)
    }
}

/// A view on the network with its own profile.
pub fn net_config() -> ViewConfig {
    ViewConfig {
        scheme_policy: wire::SCHEME_POLICY_WEB,
        scripts: true,
        handoff: wire::HANDOFF_ASK,
        popups: true,
        dialogs: true,
        ..ViewConfig::default()
    }
}

/// A browser view: the `browser` profile, downloads, blocking and deltas.
pub fn browser_config() -> ViewConfig {
    ViewConfig {
        profile: "browser".into(),
        downloads: true,
        blocking: true,
        deltas: true,
        ..net_config()
    }
}

/// A view with no network.
pub fn data_config() -> ViewConfig {
    ViewConfig {
        scheme_policy: wire::SCHEME_POLICY_DATA_ONLY,
        ..ViewConfig::default()
    }
}
