//! The window's line to the shell for web notifications: every call is made on WebKit's main thread.
//!
//! The shell's event channel is a handle of the main thread (a handle is its thread's own), but its
//! readiness is a descriptor any thread may wait on (`notify::Watch`): the worker waits on that, and
//! only goes to the main thread when the channel has news (issue #893), never on a timer.

use std::sync::Condvar;

use super::*;
use crate::notify::shell::Posted;

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

/// The current channel's readiness, for the worker: set on the main thread with the listener,
/// cleared with it.
static WATCH: Mutex<Option<Arc<notify::Watch>>> = Mutex::new(None);

fn set_watch(watch: Option<Arc<notify::Watch>>) {
    *WATCH.lock().unwrap_or_else(|e| e.into_inner()) = watch;
}

impl Listening {
    /// Asks for the event channel when there is none (before the first toast, so no event is
    /// missed; again, a little later, when the shell restarted). True when there is one.
    fn listen(&mut self) -> bool {
        if self.listener.is_some() {
            return true;
        }
        if self.retry_at.is_some_and(|at| Instant::now() < at) {
            return false;
        }
        match notify::listen() {
            Ok(listener) => {
                // Without a descriptor to wait on, the channel is no use to the worker: a toast it
                // cannot hear of is not waited for.
                let Ok(watch) = listener.watch() else {
                    self.retry_at = Some(Instant::now() + Duration::from_secs(2));
                    return false;
                };
                set_watch(Some(Arc::new(watch)));
                self.listener = Some(listener);
                self.retry_at = None;
                true
            }
            Err(_) => {
                self.retry_at = Some(Instant::now() + Duration::from_secs(2));
                false
            }
        }
    }

    fn poll(&mut self) -> Vec<ShellEvent> {
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
                // The shell's end is gone (it restarted, and its toasts with it): the next toast asks
                // again, and the worker forgets the ones it was waiting for.
                self.listener = None;
                set_watch(None);
                vec![ShellEvent::Lost]
            }
        }
    }
}

/// A look at the event channel on the main thread: whether one is under way, and what the last
/// ones found that the worker has not taken.
struct Look {
    busy: bool,
    news: Vec<ShellEvent>,
}

static LOOK: Mutex<Look> = Mutex::new(Look {
    busy: false,
    news: Vec::new(),
});
static LOOKED: Condvar = Condvar::new();

fn look() -> std::sync::MutexGuard<'static, Look> {
    LOOK.lock().unwrap_or_else(|e| e.into_inner())
}

/// `os.lazy.notify`, for web notifications: posts them, and listens to what becomes of them. Every
/// call is made on WebKit's main thread (`on_main`); this value is only the worker's handle.
#[derive(Default)]
pub(super) struct LazyShell {
    /// The last wait saw the channel ready: the next poll looks at it.
    ready: bool,
}

impl Shell for LazyShell {
    fn post(&mut self, toast: &Toast) -> Result<Posted, String> {
        let notice = notify::Notice {
            replaces: toast.replaces,
            ..notify::Notice::new(notify::Level::Info, &toast.title, &toast.body)
        };
        let (heard, posted) = on_main("notify.post", move || {
            let heard = LISTENING.with(|state| state.borrow_mut().listen());
            (heard, notify::post(&notice))
        })
        .ok_or_else(|| String::from("the browser's main thread did not answer"))?;
        let toast = posted.map_err(|code| format!("errno {code}"))?;
        Ok(Posted { toast, heard })
    }

    /// Waited for, on the worker's own thread: a closing window takes its toasts down, and the main
    /// thread's loop may end the moment this returns.
    fn close(&mut self, toast: u64) {
        let _ = on_main("notify.close", move || notify::close(toast));
    }

    /// Parks on the event channel's descriptor, or, while a look runs on the main thread, until it
    /// is done: the descriptor stays readable until the main thread has drained the channel.
    fn wait(&mut self, timeout: Duration) {
        {
            let state = look();
            if state.busy || !state.news.is_empty() {
                if state.news.is_empty() {
                    let _ = LOOKED.wait_timeout(state, timeout);
                }
                return;
            }
        }
        let watch = WATCH.lock().unwrap_or_else(|e| e.into_inner()).clone();
        self.ready = match watch {
            Some(watch) => watch.wait(timeout),
            // No channel: the main thread's look reports it lost, and the worker stops waiting.
            None => true,
        };
    }

    /// What the last look found; starts a look on the main thread when the wait saw news. The look
    /// is not waited for (it is a Messenger call on WebKit's busy thread): its news arrives with
    /// the next wait, which it ends.
    fn poll(&mut self) -> Vec<ShellEvent> {
        let (start, news) = {
            let mut state = look();
            let start = std::mem::take(&mut self.ready) && !state.busy;
            state.busy |= start;
            (start, std::mem::take(&mut state.news))
        };
        if start {
            on_main_detached(|| {
                let events = LISTENING.with(|listening| listening.borrow_mut().poll());
                let mut state = look();
                state.news.extend(events);
                state.busy = false;
                LOOKED.notify_all();
            });
        }
        news
    }
}
