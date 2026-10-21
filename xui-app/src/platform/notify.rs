//! A small client of the desktop notifications (`os.lazy.notify.v1`,
//! docs/notify-plan.md): an app tells the user something and LazyShell shows
//! it as a toast under the app's verified name.
//!
//! Fire and forget. Every call is bounded and a missing shell (no desktop, an
//! older image) is a quiet `Err`, so a notice never costs the app more than
//! one short call. The app's manifest needs `os.lazy.notify.v1`. An app that
//! wants to hear what became of its toasts (a click, a close) asks once with
//! [`listen`] and polls the [`Listener`]; a toast must never be the only way to
//! do something, so nothing here is required.

use libmessenger::Parcel;
use messenger_generated::errors::ERROR_FIELD;
use messenger_generated::os_lazy_notify_events_v1 as events;
use messenger_generated::os_lazy_notify_v1 as wire;

use super::messenger::Service;
use crate::sys::{self, errno};

/// The shell's registered name for the interface.
const NAME: &str = "os.lazy.notify";
/// How long a post may take (PIT ticks, 100 Hz): the UI thread calls it.
const POST_TICKS: u64 = 30;

/// How serious a notice is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Info,
    Success,
    Warn,
    Error,
}

impl Level {
    fn wire(self) -> u32 {
        match self {
            Level::Info => wire::LEVEL_INFO,
            Level::Success => wire::LEVEL_SUCCESS,
            Level::Warn => wire::LEVEL_WARN,
            Level::Error => wire::LEVEL_ERROR,
        }
    }
}

/// One notice. The limits are the shell's (`idl/notify.midl`): a longer text
/// is refused, so [`Notice::new`] shortens its title and body to fit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub level: Level,
    pub title: String,
    pub body: String,
    /// A Lucide outline by name (`download`); the level's icon when unknown.
    pub icon: Option<String>,
    /// Percent done, for a toast with a progress bar.
    pub progress: Option<u32>,
    /// Update this toast (an id [`post`] returned) instead of making one.
    pub replaces: Option<u64>,
    /// Milliseconds; `Some(0)` until dismissed, `None` the level's default.
    pub timeout_ms: Option<u32>,
}

impl Notice {
    /// A notice whose `title` is one line and whose `body` may hold line breaks
    /// (`\n`, one per paragraph: the shell wraps each).
    pub fn new(level: Level, title: &str, body: &str) -> Notice {
        Notice {
            level,
            title: fit(title, 64),
            body: fit_paragraphs(body, 256),
            icon: None,
            progress: None,
            replaces: None,
            timeout_ms: None,
        }
    }
}

/// `text` cut to `max` characters with an ellipsis.
fn fit(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let kept: String = flat.chars().take(max.saturating_sub(3)).collect();
    format!("{}...", kept.trim_end())
}

/// `text` with each line's white space collapsed, empty lines dropped, the lines kept apart by
/// one `\n`, and the whole cut to `max` characters with an ellipsis.
fn fit_paragraphs(text: &str, max: usize) -> String {
    let lines: Vec<String> = text
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect();
    let joined = lines.join("\n");
    if joined.chars().count() <= max {
        return joined;
    }
    let kept: String = joined.chars().take(max.saturating_sub(3)).collect();
    format!("{}...", kept.trim_end())
}

/// Close toast `id` (one [`post`] returned) at once. Not an error when it
/// is already gone.
pub fn close(id: u64) -> Result<(), i64> {
    let service = Service::try_connect(NAME).ok_or(-errno::ENOENT)?;
    let body = wire::encode_close_args(&wire::CloseArgs { id }).map_err(|_| -errno::EINVAL)?;
    service
        .call(
            wire::INTERFACE_ID,
            wire::METHOD_CLOSE,
            ERROR_FIELD,
            body,
            POST_TICKS,
        )
        .map(|_| ())
}

/// Show `notice`; the toast's id (for [`Notice::replaces`]) or the negative
/// errno (`-ENOENT` when there is no shell, `-EBUSY` when rate limited).
pub fn post(notice: &Notice) -> Result<u64, i64> {
    let service = Service::try_connect(NAME).ok_or(-errno::ENOENT)?;
    let body = wire::encode_notify_args(&wire::NotifyArgs {
        level: notice.level.wire(),
        title: notice.title.clone(),
        body: notice.body.clone(),
        icon: notice.icon.clone().map(|name| wire::Icon {
            lucide: Some(name),
            image: None,
        }),
        actions: Vec::new(),
        progress: notice.progress.map(|p| p.min(100)),
        replaces_id: notice.replaces,
        timeout: notice.timeout_ms,
    })
    .map_err(|_| -errno::EINVAL)?;
    let reply = service.call(
        wire::INTERFACE_ID,
        wire::METHOD_NOTIFY,
        ERROR_FIELD,
        body,
        POST_TICKS,
    )?;
    wire::decode_notify_reply(&reply.body)
        .map(|reply| reply.id)
        .map_err(|_| -errno::EINVAL)
}

/// Why a toast went away (`CloseReason` in `idl/notify.midl`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseReason {
    /// The person dismissed it.
    User,
    /// Its time ran out.
    Timeout,
    /// Another toast took its place.
    Replaced,
    /// The app closed it.
    Sender,
    /// The shell restarted.
    Restart,
    /// Do-not-disturb or the app's mute hid it.
    Muted,
    /// A reason a newer shell sent.
    Other,
}

impl CloseReason {
    fn from_wire(value: u32) -> CloseReason {
        match value {
            wire::CLOSE_REASON_USER => CloseReason::User,
            wire::CLOSE_REASON_TIMEOUT => CloseReason::Timeout,
            wire::CLOSE_REASON_REPLACED => CloseReason::Replaced,
            wire::CLOSE_REASON_SENDER => CloseReason::Sender,
            wire::CLOSE_REASON_RESTART => CloseReason::Restart,
            wire::CLOSE_REASON_MUTED => CloseReason::Muted,
            _ => CloseReason::Other,
        }
    }
}

/// What happened to one of this app's toasts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The person clicked the toast's body.
    Activated { id: u64 },
    /// The person pressed action `action`.
    Action { id: u64, action: u32 },
    /// The toast is gone.
    Closed { id: u64, reason: CloseReason },
}

/// Ticks (100 Hz) the `Listen` call may take.
const LISTEN_TICKS: u64 = 30;
/// Receive buffer for one event (a few dozen bytes).
const EVENT_BYTES: usize = 512;
/// Most events read per poll.
const PER_POLL: usize = 32;

/// This app's event channel from the shell. One per app: the shell keeps the newest, so a second
/// [`listen`] ends the first.
pub struct Listener {
    channel: u64,
    buf: Vec<u8>,
}

/// Ask the shell for this app's toast events. `Err` is a negative errno (`-ENOENT` without a
/// shell, `-EACCES` when the manifest lacks `os.lazy.notify.v1`).
pub fn listen() -> Result<Listener, i64> {
    let service = Service::try_connect(NAME).ok_or(-errno::ENOENT)?;
    let (sent, kept) = sys::msg_create_pair()?;
    let result = wire::encode_listen_args(&wire::ListenArgs { events: sent })
        .map_err(|_| -errno::EINVAL)
        .and_then(|(body, objects)| {
            service.call_moving_within(
                wire::INTERFACE_ID,
                wire::METHOD_LISTEN,
                ERROR_FIELD,
                body,
                objects,
                LISTEN_TICKS,
            )
        });
    match result {
        Ok(_) => Ok(Listener {
            channel: kept,
            buf: vec![0; EVENT_BYTES],
        }),
        Err(code) => {
            // The request moved `sent` only when it was delivered; a refused or failed call may
            // still hold it, and closing a handle that moved is harmless.
            let _ = sys::msg_close(sent);
            let _ = sys::msg_close(kept);
            Err(code)
        }
    }
}

impl Listener {
    /// The events queued since the last poll; never parks. `Err` means the shell's end is gone
    /// (it restarted): [`listen`] again.
    pub fn poll(&mut self) -> Result<Vec<Event>, i64> {
        let mut found = Vec::new();
        for _ in 0..PER_POLL {
            match sys::msg_queued(self.channel) {
                Ok(0) => break,
                Ok(_) => {}
                Err(code) => {
                    return if found.is_empty() {
                        Err(code)
                    } else {
                        Ok(found)
                    }
                }
            }
            let Ok(result) = sys::msg_recv(self.channel, &mut self.buf, sys::EXPIRED_DEADLINE)
            else {
                break;
            };
            let Some(bytes) = self.buf.get(..result.bytes as usize) else {
                continue;
            };
            if let Some(event) = Parcel::decode(bytes)
                .ok()
                .and_then(|parcel| decode(&parcel))
            {
                found.push(event);
            }
        }
        Ok(found)
    }
}

#[cfg(target_os = "linux")]
impl Listener {
    /// A descriptor that becomes readable when an event is queued on this channel (or it hangs up),
    /// for another thread to wait on: a handle is its thread's own, a descriptor is the process's
    /// (docs/architecture/endpoint-fd.md). The thread that listened still does the [`poll`]ing.
    ///
    /// [`poll`]: Listener::poll
    pub fn watch(&self) -> Result<Watch, i64> {
        use std::os::fd::FromRawFd;
        let raw = lazyos_sys::msg::endpoint_fd(self.channel, true)?;
        // SAFETY: the kernel just opened `raw` in this process's table for this value alone.
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
        Ok(Watch { fd })
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = sys::msg_close(self.channel);
    }
}

/// A [`Listener`]'s readiness, which any thread may wait on. Once the shell's end is closed (it
/// restarted) or the listener is dropped, it reads [`Readiness::Hangup`]. Linux only: a host
/// build (`lzpack`) has no descriptors to wait on.
#[cfg(target_os = "linux")]
pub struct Watch {
    fd: std::os::fd::OwnedFd,
}

/// What a [`Watch::wait`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Readiness {
    /// An event is queued: poll the listener.
    Ready,
    /// The shell's end is closed (it restarted) or the channel is gone: the listener is dead, and
    /// a poll would find nothing for ever (the descriptor stays readable).
    Hangup,
    /// Nothing within the time given, or the wait failed (treated as nothing, never as news, so a
    /// broken descriptor cannot make the caller spin).
    Idle,
}

#[cfg(target_os = "linux")]
impl Watch {
    /// Parks the calling thread until an event is waiting, the channel hangs up, or `timeout`
    /// passes. Level-triggered: a queued event stays [`Readiness::Ready`] until it is received.
    pub fn wait(&self, timeout: std::time::Duration) -> Readiness {
        use std::os::fd::AsRawFd;
        let mut entry = libc::pollfd {
            fd: self.fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = libc::c_int::try_from(timeout.as_millis()).unwrap_or(libc::c_int::MAX);
        // SAFETY: one `pollfd` that lives for the call; the kernel writes only its `revents`.
        let found = unsafe { libc::poll(&mut entry, 1, millis) };
        if found <= 0 {
            return Readiness::Idle;
        }
        if entry.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            // Events the shell queued before it went may still be there (POLLIN | POLLHUP): the
            // caller takes them with a last poll before it drops the listener.
            return Readiness::Hangup;
        }
        if entry.revents & libc::POLLIN != 0 {
            Readiness::Ready
        } else {
            Readiness::Idle
        }
    }
}

/// One event of the shell's channel, or `None` for a ping or anything else.
fn decode(parcel: &Parcel) -> Option<Event> {
    if parcel.header.interface_id != events::INTERFACE_ID {
        return None;
    }
    match parcel.header.method {
        events::METHOD_ACTIVATED => events::decode_activated_args(&parcel.body)
            .ok()
            .map(|args| Event::Activated { id: args.id }),
        events::METHOD_ACTIONINVOKED => {
            events::decode_action_invoked_args(&parcel.body)
                .ok()
                .map(|args| Event::Action {
                    id: args.id,
                    action: args.action,
                })
        }
        events::METHOD_CLOSED => {
            events::decode_closed_args(&parcel.body)
                .ok()
                .map(|args| Event::Closed {
                    id: args.id,
                    reason: CloseReason::from_wire(args.reason),
                })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_body_keeps_its_paragraphs_and_the_title_is_one_line() {
        let notice = Notice::new(Level::Info, "a  b\nc", "first   line\n\n  second\tline  ");
        assert_eq!(notice.title, "a b c");
        assert_eq!(notice.body, "first line\nsecond line");
    }

    #[test]
    fn a_long_body_is_cut_to_the_shells_limit() {
        let notice = Notice::new(
            Level::Info,
            &"t".repeat(100),
            &format!("{}\n{}", "a".repeat(200), "b".repeat(200)),
        );
        assert_eq!(notice.title.chars().count(), 64);
        assert!(notice.title.ends_with("..."));
        assert_eq!(notice.body.chars().count(), 256);
        assert!(notice.body.ends_with("..."));
    }

    #[test]
    fn close_reasons_follow_the_wire_values() {
        assert_eq!(CloseReason::from_wire(0), CloseReason::User);
        assert_eq!(CloseReason::from_wire(1), CloseReason::Timeout);
        assert_eq!(CloseReason::from_wire(2), CloseReason::Replaced);
        assert_eq!(CloseReason::from_wire(3), CloseReason::Sender);
        assert_eq!(CloseReason::from_wire(4), CloseReason::Restart);
        assert_eq!(CloseReason::from_wire(5), CloseReason::Muted);
        assert_eq!(CloseReason::from_wire(99), CloseReason::Other);
    }
}
