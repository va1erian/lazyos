//! A client's side of `os.lazy.webd.view.v1` (and its scopes `net.v1` and `browser.v1`) over the
//! in-memory fabric: a service built with a profile folder, a Downloads folder in the temp
//! directory, a recording `Outside` (hand-offs and toasts) and a clock the test moves. The typed
//! requests are in `full_calls.rs`, the decoded events in `full_events.rs`.
//!
//! The light helpers stay on `Rig` (`fx.rig.create_plain`, ...); the full ones are named apart
//! (`create_view`, `attach_slot`, ...) so a test never sends a light request to a full view by
//! accident.

#![allow(dead_code)]

use std::cell::RefCell;
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use libmessenger::Object;
use messenger_generated::os_lazy_webd_browser_v1 as browser_wire;
use messenger_generated::os_lazy_webd_net_v1 as net_wire;
use messenger_generated::os_lazy_webd_view_v1 as wire;
use xui_webd::events::EventQueue;
use xui_webd::fullconfig::Places;
use xui_webd::ident::Origin;
use xui_webd::mem::MemFabric;
use xui_webd::outside::{Outside, ToastNews};
use xui_webd::recording::RecordingEngine;
use xui_webd::Service;

use super::{outcome, parcel, Outcome, Rig, SESSION};

// Tests name the event type through this module with the rest of the full rig.
#[allow(unused_imports)]
pub use super::full_events::{decode_full_event, FEv};

/// Where the browser profile would live (nothing is written there: the engine is a recording double).
pub const PROFILES: &str = "/webd-test/profiles";

/// Which interface a `CreateView` goes through: what the view may reach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    View,
    Net,
    Browser,
}

impl Via {
    pub fn id(self) -> u64 {
        match self {
            Via::View => wire::INTERFACE_ID,
            Via::Net => net_wire::INTERFACE_ID,
            Via::Browser => browser_wire::INTERFACE_ID,
        }
    }
}

/// A scratch folder that goes away with the test (the Downloads folder).
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(name: &str) -> TempDir {
        let path = std::env::temp_dir().join(format!("webd-full-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        TempDir(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct OutsideState {
    opened: Vec<String>,
    open_result: bool,
    toasts: Vec<(u64, String, String)>,
    closed: Vec<u64>,
    news: Vec<ToastNews>,
    next: u64,
    refuse_toasts: bool,
}

/// The recording `Outside`: what webd handed to the system. Clones share one log.
#[derive(Clone, Default)]
pub struct RecOutside(Rc<RefCell<OutsideState>>);

impl RecOutside {
    /// Whether `open_url` reports the address as handed over.
    pub fn set_open_result(&self, handed: bool) {
        self.0.borrow_mut().open_result = handed;
    }

    pub fn refuse_toasts(&self, refuse: bool) {
        self.0.borrow_mut().refuse_toasts = refuse;
    }

    pub fn opened(&self) -> Vec<String> {
        self.0.borrow().opened.clone()
    }

    /// `(toast id, title, body)` of every toast shown.
    pub fn toasts(&self) -> Vec<(u64, String, String)> {
        self.0.borrow().toasts.clone()
    }

    pub fn closed(&self) -> Vec<u64> {
        self.0.borrow().closed.clone()
    }

    /// The shell reports something about a toast; the next `pump` hears it.
    pub fn news(&self, news: ToastNews) {
        self.0.borrow_mut().news.push(news);
    }
}

impl Outside for RecOutside {
    fn open_url(&self, url: &str) -> bool {
        let mut state = self.0.borrow_mut();
        state.opened.push(url.to_owned());
        state.open_result
    }

    fn toast(&self, title: &str, body: &str) -> Option<u64> {
        let mut state = self.0.borrow_mut();
        if state.refuse_toasts {
            return None;
        }
        state.next += 1;
        let id = 100 + state.next;
        state.toasts.push((id, title.to_owned(), body.to_owned()));
        Some(id)
    }

    fn close_toast(&self, toast: u64) {
        self.0.borrow_mut().closed.push(toast);
    }

    fn toast_news(&self) -> Vec<ToastNews> {
        std::mem::take(&mut self.0.borrow_mut().news)
    }
}

/// The rig for the full interface: a `Rig` (reachable through `Deref`) whose service has places, an
/// `Outside` and a clock.
pub struct Fx {
    pub rig: Rig,
    /// The queue's clock (what the engine's decisions and the input stamps read), in ms.
    pub clock: Arc<AtomicU64>,
    pub outside: RecOutside,
    pub dir: TempDir,
}

impl Deref for Fx {
    type Target = Rig;
    fn deref(&self) -> &Rig {
        &self.rig
    }
}

impl DerefMut for Fx {
    fn deref_mut(&mut self) -> &mut Rig {
        &mut self.rig
    }
}

/// A view config for each of the common cases (the generated `Default` is a `Web` policy).
pub fn data_only() -> wire::ViewConfig {
    wire::ViewConfig {
        scheme_policy: wire::SCHEME_POLICY_DATA_ONLY,
        ..wire::ViewConfig::default()
    }
}

pub fn web() -> wire::ViewConfig {
    wire::ViewConfig {
        scheme_policy: wire::SCHEME_POLICY_WEB,
        ..wire::ViewConfig::default()
    }
}

pub fn listed(origins: &[&str]) -> wire::ViewConfig {
    wire::ViewConfig {
        scheme_policy: wire::SCHEME_POLICY_ORIGINS,
        origins: origins.iter().map(|o| (*o).to_owned()).collect(),
        ..wire::ViewConfig::default()
    }
}

/// The `browser` profile on disk, `Web` policy.
pub fn browser() -> wire::ViewConfig {
    wire::ViewConfig {
        profile: "browser".into(),
        ..web()
    }
}

impl Fx {
    pub fn new(name: &str) -> Fx {
        let clock = Arc::new(AtomicU64::new(1000));
        let read = clock.clone();
        let queue = Arc::new(EventQueue::new().with_clock(move || read.load(Ordering::SeqCst)));
        let engine = RecordingEngine::new();
        let fabric = MemFabric::new();
        let dir = TempDir::new(name);
        let outside = RecOutside::default();
        let places = Places {
            profiles: Some(PROFILES.into()),
            downloads: Some(dir.0.clone()),
        };
        let service = Service::new(engine.clone(), fabric.clone(), queue.clone(), SESSION)
            .with_places(places)
            .with_outside(Box::new(outside.clone()));
        Fx {
            rig: Rig {
                service,
                engine,
                fabric,
                queue,
            },
            clock,
            outside,
            dir,
        }
    }

    pub fn set_clock(&self, ms: u64) {
        self.clock.store(ms, Ordering::SeqCst);
    }

    /// Send a request on `interface` with the given objects.
    pub fn send(
        &mut self,
        from: Origin,
        interface: u64,
        method: u32,
        body: Vec<u8>,
        objects: Vec<Object>,
    ) -> Outcome {
        let call = xui_webd::Call {
            origin: from,
            parcel: parcel(interface, method, body),
            objects,
        };
        outcome(&self.rig.service.handle(call))
    }

    /// A request on `os.lazy.webd.view.v1` with no objects.
    pub fn call(&mut self, from: Origin, method: u32, body: Vec<u8>) -> Outcome {
        self.send(from, wire::INTERFACE_ID, method, body, vec![])
    }

    /// `CreateView` through `via` with a fresh event channel: the view and its channel.
    pub fn create_view(
        &mut self,
        from: Origin,
        via: Via,
        config: wire::ViewConfig,
        width: u32,
        height: u32,
    ) -> Result<(u64, u64), i64> {
        let channel = self.rig.fabric.new_channel();
        let args = wire::CreateViewArgs {
            config,
            width,
            height,
            events: channel,
        };
        let (body, objects) = wire::encode_create_view_args(&args).unwrap();
        let body = self.send(from, via.id(), wire::METHOD_CREATEVIEW, body, objects)?;
        Ok((wire::decode_create_view_reply(&body).unwrap().view, channel))
    }

    /// A full view that cannot fail to be made (the defaults of the case).
    pub fn view(
        &mut self,
        from: Origin,
        via: Via,
        config: wire::ViewConfig,
        width: u32,
        height: u32,
    ) -> (u64, u64) {
        self.create_view(from, via, config, width, height).unwrap()
    }

    pub fn pump(&mut self, now: u64) {
        self.rig.service.pump(now);
    }
}
