//! One notification as the shell keeps it, and the validation that turns a
//! decoded `os.lazy.notify.v1` `Notify` into it (docs/notify-plan.md).
//!
//! Everything here comes from an untrusted app, so every field is checked
//! before it is kept: lengths, the level, action ids and labels, the
//! progress, image size against data. A broken *icon* is not an error: it is
//! dropped and the toast shows its level's icon. Anything else out of bounds
//! refuses the whole call with [`Invalid`]. Text is plain: control
//! characters become spaces, there is no markup.

use messenger_generated::os_lazy_notify_v1 as wire;

use crate::tray::item::Image;

/// Longest title, in characters.
pub const TITLE_MAX: usize = 64;
/// Longest body, in characters.
pub const BODY_MAX: usize = 256;
/// Most actions on one toast.
pub const ACTIONS_MAX: usize = 3;
/// Longest action label, in characters.
pub const ACTION_LABEL_MAX: usize = 24;
/// Largest image side, in pixels (the tray's limit).
pub const IMAGE_MAX: u32 = 64;
/// Longest Lucide name accepted before it is looked up.
pub const LUCIDE_NAME_MAX: usize = 64;
/// A progress at this value is done.
pub const PROGRESS_DONE: u8 = 100;
/// The default life of an Info or Success toast, milliseconds.
pub const DEFAULT_TIMEOUT_MS: u32 = 5_000;
/// The range a sender's `timeout` is clamped to, milliseconds.
pub const TIMEOUT_MIN_MS: u32 = 1_000;
pub const TIMEOUT_MAX_MS: u32 = 60_000;

/// What a field broke, for the `EINVAL` reply and the log line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invalid {
    Level,
    Title,
    Body,
    ActionCount,
    ActionId,
    ActionLabel,
    Progress,
}

impl Invalid {
    /// A short word for logs and error texts.
    pub fn as_str(self) -> &'static str {
        match self {
            Invalid::Level => "level",
            Invalid::Title => "title",
            Invalid::Body => "body",
            Invalid::ActionCount => "action-count",
            Invalid::ActionId => "action-id",
            Invalid::ActionLabel => "action-label",
            Invalid::Progress => "progress",
        }
    }
}

/// How serious a notification is (`Level` on the wire). The order is the
/// order of seriousness.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    #[default]
    Info,
    Success,
    Warn,
    Error,
}

impl Level {
    pub fn from_wire(value: u32) -> Option<Level> {
        match value {
            wire::LEVEL_INFO => Some(Level::Info),
            wire::LEVEL_SUCCESS => Some(Level::Success),
            wire::LEVEL_WARN => Some(Level::Warn),
            wire::LEVEL_ERROR => Some(Level::Error),
            _ => None,
        }
    }

    /// A word for logs and the UI probe.
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Info => "info",
            Level::Success => "success",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }

    /// The Lucide outline a toast of this level shows when its sender gave
    /// none (or an unusable one).
    pub fn lucide(self) -> &'static str {
        match self {
            Level::Info => "info",
            Level::Success => "check",
            Level::Warn => "triangle-alert",
            Level::Error => "circle-x",
        }
    }

    /// Whether a toast of this level stays until the user dismisses it: a
    /// missed one is gone (there is no history), so the serious ones wait.
    pub fn persists(self) -> bool {
        self >= Level::Warn
    }
}

/// Why a toast went away (`CloseReason` on the wire).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseReason {
    /// The user clicked it, dismissed it or pressed an action.
    User,
    /// Its time ran out.
    Timeout,
    /// Evicted, or merged into a summary.
    Replaced,
    /// Its sender closed it.
    Sender,
    /// The shell restarted.
    Restart,
    /// Never shown: do-not-disturb or the app is muted.
    Muted,
}

impl CloseReason {
    /// The wire value.
    pub fn wire(self) -> u32 {
        match self {
            CloseReason::User => wire::CLOSE_REASON_USER,
            CloseReason::Timeout => wire::CLOSE_REASON_TIMEOUT,
            CloseReason::Replaced => wire::CLOSE_REASON_REPLACED,
            CloseReason::Sender => wire::CLOSE_REASON_SENDER,
            CloseReason::Restart => wire::CLOSE_REASON_RESTART,
            CloseReason::Muted => wire::CLOSE_REASON_MUTED,
        }
    }

    /// A word for logs.
    pub fn as_str(self) -> &'static str {
        match self {
            CloseReason::User => "user",
            CloseReason::Timeout => "timeout",
            CloseReason::Replaced => "replaced",
            CloseReason::Sender => "sender",
            CloseReason::Restart => "restart",
            CloseReason::Muted => "muted",
        }
    }
}

/// A validated icon source. Whether the named-icon library knows the Lucide
/// name is the painter's question; an unknown one falls back to the level's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Lucide(String),
    Image(Image),
}

/// A button on the toast.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action {
    pub id: u32,
    pub label: String,
}

/// How long a toast lives, as the sender asked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Timeout {
    /// The level's default.
    #[default]
    Default,
    /// Until dismissed.
    Sticky,
    /// This many milliseconds (clamped when read).
    Millis(u32),
}

/// A validated notification.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Note {
    pub level: Level,
    pub title: String,
    pub body: String,
    /// `None`: the level's icon.
    pub icon: Option<Source>,
    pub actions: Vec<Action>,
    /// Percent done, for a toast with a progress bar.
    pub progress: Option<u8>,
    pub timeout: Timeout,
}

impl Note {
    /// A plain notice with no icon, actions or progress.
    pub fn new(level: Level, title: &str, body: &str) -> Note {
        Note {
            level,
            title: title.to_owned(),
            body: body.to_owned(),
            ..Note::default()
        }
    }

    /// The same notice with `actions`.
    pub fn with_actions(mut self, actions: &[(u32, &str)]) -> Note {
        self.actions = actions
            .iter()
            .map(|(id, label)| Action {
                id: *id,
                label: (*label).to_owned(),
            })
            .collect();
        self
    }

    /// How long it stays on screen, in milliseconds; `None` until the user
    /// dismisses it. Warn and Error never time out, and neither does a
    /// toast whose progress has not finished.
    pub fn lifetime_ms(&self) -> Option<u32> {
        if self.level.persists() {
            return None;
        }
        if self.progress.is_some_and(|done| done < PROGRESS_DONE) {
            return None;
        }
        match self.timeout {
            Timeout::Default => Some(DEFAULT_TIMEOUT_MS),
            Timeout::Sticky => None,
            Timeout::Millis(ms) => Some(ms.clamp(TIMEOUT_MIN_MS, TIMEOUT_MAX_MS)),
        }
    }
}

/// A validated `Notify`: the note and the toast it asks to replace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub note: Note,
    pub replaces: Option<u64>,
}

impl Request {
    /// Validate a decoded `Notify`.
    pub fn from_wire(args: wire::NotifyArgs) -> Result<Request, Invalid> {
        let level = Level::from_wire(args.level).ok_or(Invalid::Level)?;
        let title = text(&args.title, TITLE_MAX).ok_or(Invalid::Title)?;
        if title.trim().is_empty() {
            return Err(Invalid::Title);
        }
        let body = text(&args.body, BODY_MAX).ok_or(Invalid::Body)?;
        let progress = match args.progress {
            Some(value) if value > u32::from(PROGRESS_DONE) => return Err(Invalid::Progress),
            Some(value) => Some(value as u8),
            None => None,
        };
        Ok(Request {
            note: Note {
                level,
                title,
                body,
                icon: args.icon.and_then(icon),
                actions: actions(args.actions)?,
                progress,
                timeout: match args.timeout {
                    None => Timeout::Default,
                    Some(0) => Timeout::Sticky,
                    Some(ms) => Timeout::Millis(ms),
                },
            },
            replaces: args.replaces_id.filter(|id| *id != 0),
        })
    }
}

/// `value` with control characters as spaces, when it has at most `max`
/// characters.
fn text(value: &str, max: usize) -> Option<String> {
    if value.chars().count() > max {
        return None;
    }
    Some(
        value
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect(),
    )
}

/// At most [`ACTIONS_MAX`] actions with unique non-zero ids and non-empty
/// labels.
fn actions(rows: Vec<wire::Action>) -> Result<Vec<Action>, Invalid> {
    if rows.len() > ACTIONS_MAX {
        return Err(Invalid::ActionCount);
    }
    let mut out: Vec<Action> = Vec::with_capacity(rows.len());
    for row in rows {
        if row.id == 0 || out.iter().any(|seen| seen.id == row.id) {
            return Err(Invalid::ActionId);
        }
        let label = text(&row.label, ACTION_LABEL_MAX).ok_or(Invalid::ActionLabel)?;
        if label.trim().is_empty() {
            return Err(Invalid::ActionLabel);
        }
        out.push(Action { id: row.id, label });
    }
    Ok(out)
}

/// The icon's single usable source, or `None` (the level's icon) for no
/// source, two, or one that fails its checks.
fn icon(icon: wire::Icon) -> Option<Source> {
    match (icon.lucide, icon.image) {
        (Some(name), None) => lucide_name(&name).then_some(Source::Lucide(name)),
        (None, Some(image)) => self::image(image).map(Source::Image),
        _ => None,
    }
}

/// A kebab-case name of sane length (`lazyicons` decides if it is known).
fn lucide_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= LUCIDE_NAME_MAX
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// An image within [`IMAGE_MAX`] whose data is exactly `width * height * 4`.
fn image(image: wire::Image) -> Option<Image> {
    let side_ok = |side: u32| (1..=IMAGE_MAX).contains(&side);
    if !side_ok(image.width) || !side_ok(image.height) {
        return None;
    }
    let len = (image.width as usize) * (image.height as usize) * 4;
    (image.data.len() == len).then_some(Image {
        width: image.width,
        height: image.height,
        data: image.data,
    })
}
