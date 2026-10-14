//! The window's line to the shell for web notifications: every call is made on WebKit's main thread.

use super::*;

/// The shell's event channel for web notifications. Messenger handles belong to the thread that
/// made them: this lives on WebKit's main thread (`on_main`), which is also the thread the shell
/// accepts the app's calls from.
struct Listening {
    listener: Option<notify::Listener>,
    retry_at: Option<Instant>,
}

thread_local! {
    static LISTENING: RefCell<Listening> = const { RefCell::new(Listening { listener: None, retry_at: None }) };
}

impl Listening {
    /// Asks for the event channel when there is none (before the first toast, so no event is
    /// missed; again, a little later, when the shell restarted).
    fn listen(&mut self) {
        if self.listener.is_some() || self.retry_at.is_some_and(|at| Instant::now() < at) {
            return;
        }
        match notify::listen() {
            Ok(listener) => {
                self.listener = Some(listener);
                self.retry_at = None;
            }
            Err(_) => self.retry_at = Some(Instant::now() + Duration::from_secs(2)),
        }
    }

    fn poll(&mut self) -> Vec<ShellEvent> {
        self.listen();
        let Some(listener) = self.listener.as_mut() else {
            return Vec::new();
        };
        match listener.poll() {
            Ok(events) => events
                .into_iter()
                .filter_map(|event| match event {
                    notify::Event::Activated { id } => Some(ShellEvent::Activated { toast: id }),
                    notify::Event::Closed { id, reason } => Some(ShellEvent::Closed {
                        toast: id,
                        reason: match reason {
                            notify::CloseReason::User => Reason::User,
                            notify::CloseReason::Timeout => Reason::Timeout,
                            notify::CloseReason::Replaced => Reason::Replaced,
                            notify::CloseReason::Sender => Reason::Sender,
                            notify::CloseReason::Restart => Reason::Restart,
                            notify::CloseReason::Muted => Reason::Muted,
                            notify::CloseReason::Other => Reason::Other,
                        },
                    }),
                    // A web notification offers no buttons of its own.
                    notify::Event::Action { .. } => None,
                })
                .collect(),
            Err(_) => {
                // The shell's end is gone (it restarted): ask again.
                self.listener = None;
                Vec::new()
            }
        }
    }
}

/// `os.lazy.notify`, for web notifications: posts them, and listens to what becomes of them. Every
/// call is made on WebKit's main thread (`on_main`); this value is only the worker's handle.
pub(super) struct LazyShell;

impl Shell for LazyShell {
    fn post(&mut self, toast: &Toast) -> Result<u64, String> {
        let notice = notify::Notice {
            replaces: toast.replaces,
            ..notify::Notice::new(notify::Level::Info, &toast.title, &toast.body)
        };
        on_main("notify.post", move || {
            LISTENING.with(|state| state.borrow_mut().listen());
            notify::post(&notice)
        })
        .ok_or_else(|| String::from("the browser's main thread did not answer"))?
        .map_err(|code| format!("errno {code}"))
    }

    /// Waited for, on the worker's own thread: a closing window takes its toasts down, and the main
    /// thread's loop may end the moment this returns.
    fn close(&mut self, toast: u64) {
        let _ = on_main("notify.close", move || notify::close(toast));
    }

    /// What the last look at the shell's event channel found; the next look is made on the main
    /// thread without waiting for it (it is a Messenger call, and the main thread is WebKit's), so
    /// news arrives one poll interval late rather than holding this worker up.
    fn poll(&mut self) -> Vec<ShellEvent> {
        static NEWS: Mutex<Vec<ShellEvent>> = Mutex::new(Vec::new());
        static LOOKING: AtomicBool = AtomicBool::new(false);
        if !LOOKING.swap(true, Ordering::AcqRel) {
            on_main_detached(|| {
                let events = LISTENING.with(|state| state.borrow_mut().poll());
                if !events.is_empty() {
                    NEWS.lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .extend(events);
                }
                LOOKING.store(false, Ordering::Release);
            });
        }
        std::mem::take(&mut *NEWS.lock().unwrap_or_else(|e| e.into_inner()))
    }
}
