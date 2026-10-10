//! A small client of the desktop notifications (`os.lazy.notify.v1`,
//! docs/notify-plan.md): an app tells the user something and LazyShell shows
//! it as a toast under the app's verified name.
//!
//! Fire and forget. Every call is bounded and a missing shell (no desktop, an
//! older image) is a quiet `Err`, so a notice never costs the app more than
//! one short call. The app's manifest needs `os.lazy.notify.v1`. There is no
//! `Listen` here: an app that wants clicks reports them itself, and a toast
//! must never be the only way to do something.

use messenger_generated::errors::ERROR_FIELD;
use messenger_generated::os_lazy_notify_v1 as wire;

use super::messenger::Service;
use crate::sys::errno;

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
    pub fn new(level: Level, title: &str, body: &str) -> Notice {
        Notice {
            level,
            title: fit(title, 64),
            body: fit(body, 256),
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

/// Close toast `id` (one [`post`] returned) at once. Not an error when it
/// is already gone.
pub fn close(id: u64) -> Result<(), i64> {
    let service = Service::try_connect(NAME).ok_or(-errno::ENOENT)?;
    let body = wire::encode_close_args(&wire::CloseArgs { id }).map_err(|_| -errno::EINVAL)?;
    service
        .call_within(
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
    let reply = service.call_within(
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
