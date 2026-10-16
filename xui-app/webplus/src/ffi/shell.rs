//! The window's line to the shell for web notifications: every call is made on WebKit's main thread.
//!
//! The shell's event channel is a handle of the main thread (a handle is its thread's own), but its
//! readiness is a descriptor any thread may wait on (`notify::Watch`): the worker waits on that, and
//! only goes to the main thread when the channel has news or hung up (issue #893), never on a timer.

use std::sync::Condvar;

use xui_app::platform::notify::Readiness;

use super::*;
use crate::notify::shell::Posted;

/// The shell's event channel for web notifications. Messenger handles belong to the thread that
/// made them: this lives on WebKit's main thread (`on_main`), which is also the thread the shell
/// accepts the app's calls from.
struct Listening {
    listener: Option<notify::Listener>,
    /// The number of the current listener (each new one has a higher one): a toast is heard on it.
    channel: u64,
    retry_at: Option<Instant>,
}

thread_local! {
    static LISTENING: RefCell<Listening> =
        const { RefCell::new(Listening { listener: None, channel: 0, retry_at: None }) };
}

/// The current channel's readiness, for the worker: set on the main thread with the listener,
/// cleared with it.
static WATCH: Mutex<Option<Arc<notify::Watch>>> = Mutex::new(None);

fn set_watch(watch: Option<Arc<notify::Watch>>) {
    *WATCH.lock().unwrap_or_else(|e| e.into_inner()) = watch;
}

/// How long the shell is not asked again after it refused an event channel.
const LISTEN_RETRY: Duration = Duration::from_secs(2);

impl Listening {
    /// Asks for the event channel when there is none (before the first toast, so no event is
    /// missed; again, a little later, when the shell restarted). The channel's number when there
    /// is one.
    fn listen(&mut self) -> Option<u64> {
        if self.listener.is_some() {
            return Some(self.channel);
        }
        if self.retry_at.is_some_and(|at| Instant::now() < at) {
            return None;
        }
        // Without a descriptor to wait on, the channel is no use to the worker: a toast it cannot
        // hear of is not waited for.
        let Some((listener, watch)) = notify::listen()
            .ok()
            .and_then(|listener| listener.watch().ok().map(|watch| (listener, watch)))
        else {
            self.retry_at = Some(Instant::now() + LISTEN_RETRY);
            return None;
        };
        self.channel += 1;
        set_watch(Some(Arc::new(watch)));
        self.listener = Some(listener);
        self.retry_at = None;
        Some(self.channel)
    }

    /// What the channel holds; with `hangup` (the descriptor said the shell's end is closed), the
    /// channel is dropped after its last events and reported lost.
    fn poll(&mut self, hangup: bool) -> Vec<ShellEvent> {
        let Some(listener) = self.listener.as_mut() else {
            return Vec::new();
        };
        let (mut events, failed) = match listener.poll() {
            Ok(events) => (events.into_iter().filter_map(event).collect(), false),
            Err(_) => (Vec::new(), true),
        };
        if hangup || failed {
            // The shell is gone, and its toasts with it: the next toast asks again, and the worker
            // forgets the ones it was waiting for on this channel.
            self.listener = None;
            set_watch(None);
            events.push(ShellEvent::Lost {
                channel: self.channel,
            });
        }
        events
    }
}

fn event(event: notify::Event) -> Option<ShellEvent> {
    match event {
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
    }
}

/// A look at the event channel on the main thread: whether one is under way, what the last ones
/// found that the worker has not taken, and whether a look the descriptor asked for found nothing.
struct Look {
    busy: bool,
    news: Vec<ShellEvent>,
    empty: bool,
}

static LOOK: Mutex<Look> = Mutex::new(Look {
    busy: false,
    news: Vec::new(),
    empty: false,
});
static LOOKED: Condvar = Condvar::new();

fn look() -> std::sync::MutexGuard<'static, Look> {
    LOOK.lock().unwrap_or_else(|e| e.into_inner())
}

/// `os.lazy.notify`, for web notifications: posts them, and listens to what becomes of them. Every
/// call is made on WebKit's main thread (`on_main`); this value is only the worker's handle.
pub(super) struct LazyShell {
    /// What the last wait saw: the next poll looks at the channel unless it is `Idle`.
    seen: Readiness,
}

impl Default for LazyShell {
    fn default() -> Self {
        LazyShell {
            seen: Readiness::Idle,
        }
    }
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
    /// is done (the descriptor stays readable until the main thread has drained the channel). After
    /// a look that found nothing the descriptor said was there, it sits the timeout out: a channel
    /// that reads ready and holds nothing must not keep the main thread busy.
    fn wait(&mut self, timeout: Duration) {
        self.seen = Readiness::Idle;
        {
            let mut state = look();
            if state.busy || !state.news.is_empty() {
                if state.news.is_empty() {
                    let _ = LOOKED.wait_timeout(state, timeout);
                }
                return;
            }
            if std::mem::take(&mut state.empty) {
                drop(state);
                std::thread::sleep(timeout);
                return;
            }
        }
        let watch = WATCH.lock().unwrap_or_else(|e| e.into_inner()).clone();
        match watch {
            Some(watch) => self.seen = watch.wait(timeout),
            // No channel: nothing will be heard, and the worker has been or will be told.
            None => std::thread::sleep(timeout),
        }
    }

    /// What the last look found; starts a look on the main thread when the wait saw news or a
    /// hangup. The look is not waited for (it is a Messenger call on WebKit's busy thread): its
    /// news arrives with the next wait, which it ends.
    fn poll(&mut self) -> Vec<ShellEvent> {
        let seen = std::mem::replace(&mut self.seen, Readiness::Idle);
        let (start, news) = {
            let mut state = look();
            let start = seen != Readiness::Idle && !state.busy;
            state.busy |= start;
            (start, std::mem::take(&mut state.news))
        };
        if start {
            let hangup = seen == Readiness::Hangup;
            on_main_detached(move || {
                let events = LISTENING.with(|listening| listening.borrow_mut().poll(hangup));
                let mut state = look();
                state.empty = events.is_empty();
                state.news.extend(events);
                state.busy = false;
                LOOKED.notify_all();
            });
        }
        news
    }
}
