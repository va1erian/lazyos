//! The window's engines on LazyOS: one `RemoteEngine`, a full view of webd, for each tab
//! (docs/lazyweb-plan.md 2.1, 2.3). A tab's view is made in the web process of its first address's
//! host (`ViewConfig.process_key`, browser profile on disk only: webd refuses a key for a profile
//! kept in memory), so tabs of one site share a process and other sites are isolated.
//!
//! The window parks its loop on one descriptor, the front tab's event channel; the tabs behind it
//! are read on the window's beat (`Msg::Engine`, a quarter of a second), which is slow enough not to
//! matter for what they tell (a title, a download's progress) and cheap. A tab brought to the
//! front hands the loop its channel ([`Engines::front`]).

use super::*;

use std::collections::BTreeMap;

use crate::app::{Engines, Fresh};
use crate::tabs::TabId;

/// The event channels of the tabs' views, and which one the loop is parked on.
struct Watcher {
    state: RefCell<(TabId, BTreeMap<TabId, Option<u64>>)>,
    park: Box<dyn Fn(Option<u64>)>,
}

impl Watcher {
    /// Tab `tab`'s view has this event channel now (or none).
    fn report(&self, tab: TabId, channel: Option<u64>) {
        let front = {
            let mut state = self.state.borrow_mut();
            state.1.insert(tab, channel);
            state.0 == tab
        };
        if front {
            (self.park)(channel);
        }
    }

    /// Tab `tab` is in front: the loop parks on its channel.
    fn front(&self, tab: TabId) {
        let channel = {
            let mut state = self.state.borrow_mut();
            state.0 = tab;
            state.1.get(&tab).copied().flatten()
        };
        (self.park)(channel);
    }

    fn forget(&self, tab: TabId) {
        self.state.borrow_mut().1.remove(&tab);
    }
}

/// Makes and reads the engines of the window's tabs.
pub(super) struct LazyEngines {
    /// The configuration of a tab's view: the browser's, with the agent the person chose.
    pub(super) config: ViewConfig,
    /// Whether a tab's process key is honoured (a profile kept on disk).
    pub(super) keyed: bool,
    pub(super) internal: Internal,
    pub(super) permissions: Permissions,
    /// Wakes the window's loop for an engine that wants to be called soon.
    pub(super) wake: Rc<dyn Fn()>,
}

/// The engines made so far, and the watcher; shared by the window's `Engines` and the helpers'
/// jobs, which run on the front tab's engine.
pub(super) struct Fleet {
    made: LazyEngines,
    engines: RefCell<BTreeMap<TabId, Rc<RemoteEngine<WebdTransport>>>>,
    front: Cell<TabId>,
    watcher: Rc<Watcher>,
}

impl Fleet {
    pub(super) fn new(made: LazyEngines, park: Box<dyn Fn(Option<u64>)>) -> Rc<Fleet> {
        let watcher = Rc::new(Watcher {
            state: RefCell::new((0, BTreeMap::new())),
            park,
        });
        Rc::new(Fleet {
            made,
            engines: RefCell::new(BTreeMap::new()),
            front: Cell::new(0),
            watcher,
        })
    }

    /// The front tab's engine: what the ad blocker's worker and the shell's calls run on.
    pub(super) fn current(&self) -> Option<Rc<RemoteEngine<WebdTransport>>> {
        self.engines.borrow().get(&self.front.get()).cloned()
    }
}

impl Engines for Fleet {
    fn make(&self, tab: TabId, process_key: &str) -> Fresh {
        let mailbox = Arc::new(Mailbox::default());
        let mut config = self.made.config.clone();
        if self.made.keyed {
            config.process_key = process_key.to_owned();
        }
        let (watcher, wake) = (Rc::clone(&self.watcher), Rc::clone(&self.made.wake));
        let engine = Rc::new(RemoteEngine::new(
            WebdTransport::full(),
            Arc::clone(&mailbox),
            Options {
                config,
                slots: SlotPlan::default(),
                navigator: navigator(self.made.internal.clone()),
                permissions: decisions(&self.made.permissions),
                watch: Box::new(move |channel| watcher.report(tab, channel)),
                wake: Box::new(move || wake()),
            },
        ));
        self.engines.borrow_mut().insert(tab, Rc::clone(&engine));
        Fresh { engine, mailbox }
    }

    fn front(&self, tab: TabId) {
        self.front.set(tab);
        self.watcher.front(tab);
    }

    fn gone(&self, tab: TabId) {
        self.engines.borrow_mut().remove(&tab);
        self.watcher.forget(tab);
    }

    fn pump(&self) -> bool {
        let all: Vec<_> = self.engines.borrow().values().cloned().collect();
        all.iter()
            .fold(false, |again, engine| engine.pump() | again)
    }
}
