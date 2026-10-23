//! The program's life: the window, its engine in webd, the helpers' threads, and what is left to do
//! once the window is closed.

use super::*;

/// Wakes the window for work the other threads queued (set once the window's proxy exists; work
/// queued before is woken for then).
#[derive(Clone, Default)]
struct Waker(Arc<OnceLock<Box<dyn Fn() + Send + Sync>>>);

impl Waker {
    fn wake(&self) {
        if let Some(wake) = self.0.get() {
            wake();
        }
    }

    fn set(&self, wake: impl Fn() + Send + Sync + 'static) {
        let _ = self.0.set(Box::new(wake));
        self.wake();
    }
}

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

pub(super) fn run() -> i32 {
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
    let Some(backend) = connect() else {
        return 1;
    };
    let (width, height) = backend.window_size(WINDOW);
    backend.set_size_hints(420, 300, 0, 0);
    backend.on_first_frame(|| trace::pass!(Channel::LazyWeb, "UP"));
    let theme = backend.desktop_theme();
    let waker = Waker::default();
    let (relay, jobs) = relay::relay({
        let waker = waker.clone();
        move || waker.wake()
    });
    // Settings at their defaults until `confd` answers (`Blocker::attach`, on a thread).
    let blocker = Blocker::volatile();
    start_blocker(&blocker, profile.as_ref(), memory_only, relay);
    let internal = Internal::default();
    let choices = prefs::load();
    let quit_profile = profile.clone();
    let window_blocker = blocker.clone();
    let spec = PlatformSpec::new(APP_NAME).size(Dip(width as f32), Dip(height as f32));
    let window_backend = Rc::clone(&backend);
    let outcome = run_app(Rc::clone(&backend) as Rc<dyn Backend>, spec, move |ui| {
        if let Some(theme) = theme {
            ui.set_theme(theme);
        }
        let proxy = ui.proxy();
        waker.set({
            let proxy = proxy.clone();
            move || {
                let _ = proxy.send(Msg::Jobs);
            }
        });
        main_thread::install({
            let proxy = proxy.clone();
            move || {
                let _ = proxy.send(Msg::Jobs);
            }
        });
        // The shell's toasts for web notifications, on a thread of their own that wakes this window.
        let toasts = ShellWorker::spawn(LazyShell::default(), {
            let proxy = proxy.clone();
            move || {
                let _ = proxy.send(Msg::Shell);
            }
        })
        .map(|worker| Rc::new(worker.with_turn(main_thread::run)) as Rc<dyn Toasts>);
        let mailbox = Arc::new(Mailbox::default());
        let engine = Rc::new(RemoteEngine::new(
            WebdTransport::full(),
            Arc::clone(&mailbox),
            Options {
                config: browser_config(memory_only),
                slots: SlotPlan::default(),
                navigator: navigator(internal.clone()),
                permissions: decisions(&permissions),
                watch: watch(&window_backend),
                wake: Box::new(move || {
                    let _ = proxy.send(Msg::Engine);
                }),
            },
        ));
        let traces = xui_app::probe::enabled();
        crate::allow_named_markers(traces);
        let focus_backend = Rc::clone(&window_backend);
        let setup = Setup {
            url: first,
            visits: visits(profile.as_ref()),
            transfers: Transfers::new(folders.downloads.clone(), Some(Box::new(LazyToaster))),
            internal: internal.clone(),
            permissions: permissions.clone(),
            toasts,
            blocker: window_blocker,
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
                pump: {
                    let engine = Rc::clone(&engine);
                    Rc::new(move || engine.pump())
                },
                jobs: {
                    let engine = Rc::clone(&engine);
                    Rc::new(move || {
                        jobs.run(engine.as_ref());
                        main_thread::run();
                    })
                },
            },
        };
        Browser::build(ui, engine as Rc<dyn Engine>, mailbox, setup)
            .expect("the LazyWeb window's widgets built")
    });
    backend.unbind();
    // The history and the session queued last are written before the marker goes (`disk.rs`).
    if !crate::disk::global().flush(SETTINGS_FLUSH) {
        crate::marker("DISK:FAIL:flush:timeout");
    }
    // A clean quit: the next start is not a crash (`session.rs`).
    if let Some(profile) = &quit_profile {
        crate::session::end(profile);
    }
    // The list updater's `curl` must not outlive the window.
    let left = !stop_children();
    if left {
        crate::marker("CHILD:curl:ALIVE");
    }
    // A switch flipped just before the window closed is written before the process exits.
    if !blocker.flush(SETTINGS_FLUSH) {
        crate::marker("BLOCK:SETTINGS:FAIL:flush:timeout");
    }
    let status = i32::from(xui_app::launch::finish(Channel::LazyWeb, outcome));
    if status == 0 && !left {
        trace::pass!(Channel::LazyWeb, "QUIT");
        0
    } else if status == 0 {
        1
    } else {
        status
    }
}
