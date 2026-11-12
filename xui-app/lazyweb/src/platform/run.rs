//! The program's life: the window, its engine in webd, the helpers' threads, and what is left to do
//! once the window is closed.

use super::*;
use xui_work::Sink;

/// The ad blocker's worker (a thread that fetches, converts and hands the lists to the engine through
/// `relay`), started with the window: nothing waits for it, a page loads unblocked until a list is
/// ready.
fn start_blocker(blocker: &Blocker, profile: Option<&PathBuf>, memory_only: bool, relay: Relay) {
    let Some(profile) = profile.filter(|_| !memory_only) else {
        // No home folder, so nowhere for a list to live, or the page's data kept in memory (webd keeps
        // content blocking for a profile on disk only): say so, rather than leaving the shield at
        // "preparing" for ever.
        for list in blocker.lists() {
            blocker.update_list(list.id, |l| {
                l.state = crate::blocker::ListState::Unavailable
            });
        }
        return;
    };
    let now = Arc::new(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    });
    let worker = Worker::new(
        profile.join("lists"),
        Arc::new(relay),
        Arc::new(CurlRunner),
        blocker.clone(),
        now,
    );
    let commands = worker.spawn();
    // The settings' store first (a service that may take a while to come up), then the lists that
    // settings leave on; on a thread of its own, since a Messenger call waits.
    let attach = blocker.clone();
    let opened = std::thread::Builder::new()
        .name(String::from("confd"))
        .spawn({
            let commands = commands.clone();
            move || {
                attach.attach(settings_store());
                let _ = commands.send(BlockerCmd::Start);
            }
        });
    if opened.is_err() {
        let _ = commands.send(BlockerCmd::Start);
    }
}

/// The view's event channel as a descriptor the window's loop parks on (`watch_fd`): the window
/// reads the view the moment webd writes, not on a timer.
fn watch(backend: &Rc<LazyOSBackend>) -> Box<dyn Fn(Option<u64>)> {
    let backend = Rc::clone(backend);
    let held: RefCell<Option<OwnedFd>> = RefCell::new(None);
    Box::new(move |channel| {
        let fd = channel.and_then(|handle| lazyos_sys::msg::endpoint_fd(handle, true).ok());
        // SAFETY: `endpoint_fd` just made this descriptor and nothing else owns it.
        let owned = fd.map(|fd| unsafe { OwnedFd::from_raw_fd(fd) });
        backend.watch_fd(owned.as_ref().map_or(-1, |fd| fd.as_raw_fd()));
        // The old descriptor closes after the loop forgot it.
        *held.borrow_mut() = owned;
    })
}

/// What is left to do once the window is closed and the display released: the history and the
/// session queued last are written before the `QUIT` marker goes (`disk.rs`), the list updater's
/// `curl` must not outlive the window, and a switch flipped just before the window closed is
/// written before the process exits. Whether nothing was left behind.
fn wind_down(blocker: &Blocker, profile: Option<&PathBuf>) -> bool {
    if !crate::disk::global().flush(SETTINGS_FLUSH) {
        crate::marker("DISK:FAIL:flush:timeout");
    }
    // A clean quit: the next start is not a crash (`session.rs`).
    if let Some(profile) = profile {
        crate::session::end(profile);
    }
    let left = !stop_children();
    if left {
        crate::marker("CHILD:curl:ALIVE");
    }
    if !blocker.flush(SETTINGS_FLUSH) {
        crate::marker("BLOCK:SETTINGS:FAIL:flush:timeout");
    }
    !left
}

pub(super) fn run() -> ! {
    let first = first_address(&std::env::args().collect::<Vec<_>>());
    let folders = folders();
    let profile = folders.profile.clone();
    let memory_only = profile
        .as_ref()
        .is_some_and(|dir| crate::memory::enabled(dir));
    let permissions = match &profile {
        Some(dir) => Permissions::open(dir.join(NOTIFY_FILE)),
        None => Permissions::in_memory(),
    };
    // Settings at their defaults until `confd` answers (`Blocker::attach`, on a thread).
    let blocker = Blocker::volatile();
    let exit_blocker = blocker.clone();
    let exit_profile = profile.clone();
    // An app that opens at login starts while the core apps are still being installed: wait for
    // the compositor (LazyShell retries the same way).
    xui_app::app(Channel::LazyWeb, APP_NAME)
        .size(WINDOW.0, WINDOW.1)
        .min_size(420, 300)
        .wait_for_display(CONNECT_WAIT)
        .at_exit(move || wind_down(&exit_blocker, exit_profile.as_ref()))
        .run(move |ui, cx| {
            let backend = cx.backend();
            let proxy = ui.proxy();
            // The doorbell for work other threads queue for the window (the blocker's relay,
            // the main-thread turns): one message, one wake per burst.
            let jobs_bell = Sink::bell(&proxy, Msg::Jobs);
            let (relay, jobs) = relay::relay({
                let bell = jobs_bell.clone();
                move || {
                    bell.ring();
                }
            });
            start_blocker(&blocker, profile.as_ref(), memory_only, relay);
            let internal = Internal::default();
            let choices = prefs::load();
            // The agent the person chose last (View > User Agent): the view is made with it.
            let mut config = browser_config(memory_only);
            config.user_agent = useragent::PRESETS[profile.as_deref().map_or(0, useragent::load)]
                .agent
                .to_owned();
            main_thread::install(move || {
                jobs_bell.ring();
            });
            // The shell's toasts for web notifications, on a thread of their own that wakes this window.
            let toasts = ShellWorker::spawn(LazyShell::default(), {
                let bell = Sink::bell(&proxy, Msg::Shell);
                move || {
                    bell.ring();
                }
            })
            .map(|worker| Rc::new(worker.with_turn(main_thread::run)) as Rc<dyn Toasts>);
            // One engine per tab, made by the window as tabs open; the loop parks on the front tab's.
            let fleet = Fleet::new(
                LazyEngines {
                    config,
                    keyed: !memory_only,
                    internal: internal.clone(),
                    permissions: permissions.clone(),
                    wake: Rc::new({
                        let bell = Sink::bell(&proxy, Msg::Engine);
                        move || {
                            bell.ring();
                        }
                    }),
                },
                watch(backend),
            );
            let traces = xui_app::probe::enabled();
            crate::allow_named_markers(traces);
            let focus_backend = Rc::clone(backend);
            let setup = Setup {
                url: first,
                visits: visits(profile.as_ref()),
                transfers: Transfers::new(folders.downloads.clone(), Some(Box::new(LazyToaster))),
                internal: internal.clone(),
                permissions: permissions.clone(),
                toasts,
                blocker,
                host: Host {
                    traces,
                    focused: Rc::new(move || focus_backend.focused()),
                    launcher: Rc::new(LazyLauncher),
                    profile: profile.clone(),
                    prefs: choices,
                    probe: Box::new(|window, name, rect| {
                        xui_app::probe::widget(
                            window,
                            name,
                            rect.left,
                            rect.top,
                            rect.width(),
                            rect.height(),
                        );
                    }),
                    engines: Rc::clone(&fleet) as Rc<dyn Engines>,
                    jobs: {
                        let fleet = Rc::clone(&fleet);
                        Rc::new(move || {
                            // The ad blocker's calls run on the front tab's engine: the rule lists are
                            // the browser profile's, shared by its views.
                            if let Some(engine) = fleet.current() {
                                jobs.run(engine.as_ref());
                            }
                            main_thread::run();
                        })
                    },
                },
            };
            Browser::build(ui, setup)
        })
}
