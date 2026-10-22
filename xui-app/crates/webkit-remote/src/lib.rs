//! The seam's [`Engine`] over webd's full views (docs/webd-plan.md, phase P4): LazyWeb+'s window
//! drives a page that `webd`'s WebKit paints in another process, through one
//! [`FullWebView`](xui_webd_client::FullWebView) made through the browser scope.
//!
//! Every call comes from the window's thread (a Messenger handle belongs to the thread that made
//! it) and returns without waiting for the page; a call to webd itself is bounded by the transport.
//! Whatever webd says is read by [`RemoteEngine::pump`], which the window calls when the view's
//! event channel is readable (or on a timer) and which turns it into the seam's
//! [`Mailbox`](xui_webkit::mailbox::Mailbox): frames and deltas copied straight out of the shared
//! slots into the mailbox ([`sink`]), the page's state, the news ([`news`]).
//!
//! What the in-process engine decided on the spot is decided as follows:
//!
//! * **Navigations to other schemes** (`mailto:`, the browser's own `x-lazyweb:` commands) are held
//!   by webd (`Handoff.Ask`) and refused here at once; the window's policy
//!   ([`Options::navigator`]) turns them into the news the in-process policy posted (a launch, a
//!   command), with the window's own real-input lock.
//! * **The view** is made when the page view first has a size, again after it is lost: a page
//!   process that ended leaves the page blank until the next load (as before); webd going away is
//!   retried every second and the page loaded again ([`life`]).
//! * **Internal pages** larger than webd takes as an address (`data:` URLs over 8 KiB) are given
//!   as HTML with a short base address of their own, and their address is told back as the
//!   original one, so the window's own pages stay recognisable ([`alias`]).
//! * **Pointer moves** are sent at most every [`MOVE_INTERVAL_MS`]; the newest waits (the plan's
//!   merging, issue #890, now in the client's own process).

mod alias;
mod input;
mod life;
pub mod news;
mod sink;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use messenger_generated::os_lazy_webd_view_v1 as wire;
use xui_webd_client::transport::ScopedTransport;
use xui_webd_client::{FullWebView, Scope, ViewConfig};
use xui_webkit::abi::NavKind;
use xui_webkit::engine::NotifyAnswer;
use xui_webkit::events::Event;
use xui_webkit::mailbox::{Mailbox, PageState};

pub use life::Health;
pub use xui_webd_client::SlotPlan;

/// The shortest time between two pointer moves sent to webd.
pub const MOVE_INTERVAL_MS: u64 = 8;
/// How often a view webd could not make is asked for again.
pub const RETRY_MS: u64 = 1000;

/// A navigation webd held because the view's policy does not load it (a scheme for another program,
/// one of the browser's commands): what the window's policy needs to decide.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Navigation {
    pub url: String,
    pub kind: NavKind,
    /// WebKit's own word: a click or a key started it.
    pub gesture: bool,
    /// The page on screen when it was asked (the window's own pages may run commands).
    pub committed_url: String,
}

/// What the window gives the engine.
pub struct Options {
    /// The view's configuration (the browser's: `browser` profile, the web, hand-offs asked).
    pub config: ViewConfig,
    /// The slots to attach (`SlotPlan`); fewer are used when the process cannot have them.
    pub slots: SlotPlan,
    /// The window's policy for a held navigation: the news to post, if any. The navigation itself
    /// never loads in the view.
    pub navigator: Box<dyn Fn(&Navigation) -> Option<Event>>,
    /// The person's kept decisions about notifications, told to webd whenever a view is made
    /// (webd keeps them only while it runs).
    pub permissions: Vec<(String, NotifyAnswer)>,
    /// Told the view's event channel whenever it changes (a new view, none): the window parks on it.
    pub watch: Box<dyn Fn(Option<u64>)>,
    /// Asks the window to call [`RemoteEngine::pump`] soon (a pointer move waits its turn).
    pub wake: Box<dyn Fn()>,
}

/// The configuration of LazyWeb+'s view: the `browser` profile (on disk, or in memory with
/// `memory_only`, #940), the web, the browser's own questions, frames with damage.
pub fn browser_config(memory_only: bool) -> ViewConfig {
    ViewConfig {
        profile: String::from("browser"),
        ephemeral: memory_only,
        scheme_policy: wire::SCHEME_POLICY_WEB,
        origins: Vec::new(),
        scripts: true,
        handoff: wire::HANDOFF_ASK,
        downloads: true,
        popups: true,
        // webd keeps content blocking and notifications with a profile on disk only.
        blocking: !memory_only,
        notifications: !memory_only,
        user_agent_suffix: String::new(),
        deltas: true,
        dialogs: true,
        notify_in_client: true,
    }
}

/// What the engine keeps between calls.
struct Inner<T: ScopedTransport> {
    view: FullWebView<T>,
    /// The page view's size, once known.
    size: Option<(u32, u32)>,
    /// A load asked for while there was no view.
    pending: Option<String>,
    /// The page as last posted to the mailbox.
    state: PageState,
    /// Added to the view's count of finished loads, so a new view's count goes on from the old one's.
    finished_base: u32,
    /// Why there is no view now, and when to ask again.
    health: Health,
    retry_at: u64,
    /// Calls made before the view existed (the blocker's), run once it does.
    deferred: Vec<Box<dyn FnOnce(&mut FullWebView<T>)>>,
    blocking: Option<(bool, Vec<String>)>,
    permissions: BTreeMap<String, NotifyAnswer>,
    aliases: alias::Aliases,
    moves: input::Moves,
    /// The event channel last told to `watch`.
    channel: Option<u64>,
}

/// The engine: one view of webd, its picture in the window's mailbox.
pub struct RemoteEngine<T: ScopedTransport> {
    inner: RefCell<Inner<T>>,
    mailbox: Arc<Mailbox>,
    navigator: Box<dyn Fn(&Navigation) -> Option<Event>>,
    watch: Box<dyn Fn(Option<u64>)>,
    wake: Box<dyn Fn()>,
    started: Instant,
}

impl<T: ScopedTransport> RemoteEngine<T> {
    /// An engine over `transport` posting into `mailbox`. No view is made before the first
    /// [`Engine::resize`](xui_webkit::engine::Engine::resize).
    pub fn new(transport: T, mailbox: Arc<Mailbox>, options: Options) -> RemoteEngine<T> {
        let mut view = FullWebView::new(transport, Scope::Browser, options.config);
        view.set_slot_plan(options.slots);
        RemoteEngine {
            inner: RefCell::new(Inner {
                view,
                size: None,
                pending: None,
                state: PageState::default(),
                finished_base: 0,
                health: Health::Waiting,
                retry_at: 0,
                deferred: Vec::new(),
                blocking: None,
                permissions: options.permissions.into_iter().collect(),
                aliases: alias::Aliases::default(),
                moves: input::Moves::default(),
                channel: None,
            }),
            mailbox,
            navigator: options.navigator,
            watch: options.watch,
            wake: options.wake,
            started: Instant::now(),
        }
    }

    /// Milliseconds since the engine was made: the clock of the retries and the pointer.
    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// Reads what webd sent and hands it to the mailbox; sends a pointer move that waited; asks for
    /// the view again when it is due. Returns whether the window should call again soon
    /// ([`MOVE_INTERVAL_MS`]) rather than only when the channel is readable.
    pub fn pump(&self) -> bool {
        let now = self.now_ms();
        self.retry_if_due(now);
        self.flush_move(now, false);
        let events = {
            let mut inner = self.inner.borrow_mut();
            let mut sink = sink::MailboxSink::new(&self.mailbox);
            inner.view.poll_into(&mut sink)
        };
        for event in events {
            self.news(event);
        }
        self.notice_channel();
        let inner = self.inner.borrow();
        inner.moves.waiting() || inner.health.retrying()
    }

    /// The view's health, for the window's status and the traces.
    pub fn health(&self) -> Health {
        self.inner.borrow().health
    }

    /// The view's counters (frames, deltas, bands, slots left out), for the traces.
    pub fn counters(&self) -> xui_webd_client::FullCounters {
        self.inner.borrow().view.counters()
    }

    /// Tells `watch` when the view's event channel changed.
    fn notice_channel(&self) {
        let now = self.inner.borrow().view.events_handle();
        let changed = {
            let mut inner = self.inner.borrow_mut();
            std::mem::replace(&mut inner.channel, now) != now
        };
        if changed {
            (self.watch)(now);
        }
    }
}

mod engine;
