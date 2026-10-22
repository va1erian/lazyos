//! Every number webd enforces on a client, in one place (docs/webd-plan.md sections 3.6 and 5). The
//! first six are the plan's; the rest are not caps on what a client may do but the sizes a
//! message must fit in or the cadence of a probe. Nothing else limits a client: a number added here
//! needs a reason that is not "to be safe".

/// Live views one client may hold (a view the engine lost still counts until `DestroyView`).
pub const VIEWS_PER_CLIENT: usize = 8;

/// A view's width or height in pixels, at most (the engine clamps to the same).
pub const SIZE_MAX: u32 = 8192;

/// A view's area in pixels, at most: 4096 x 4096, so 64 MiB per slot.
pub const PIXELS_MAX: u64 = 16 << 20;

/// Slots a view may have attached: the client keeps one on screen, one being painted, one spare.
pub const SLOTS_PER_VIEW: usize = 3;

/// Bytes of attached slot memory one client may hold across all its views: the whole size of each
/// attached buffer as the kernel maps it, whatever range `AttachSlot` declares of it. Eight 1080p views with three slots each are 199 MB; a 4K view with three is 100 MB.
pub const SLOT_BYTES_PER_CLIENT: u64 = 512 << 20;

/// The `html` of a `LoadHtml`, at most (the range of the buffer).
pub const HTML_BYTES_MAX: u64 = 8 << 20;

/// A view's `user_style`, at most (`webd_view_config` says the same).
pub const USER_STYLE_BYTES_MAX: usize = 64 << 10;

/// The largest text (title, error, URL, copied text) the service puts in one event. Longer text is
/// cut at a character boundary, when the engine's news is queued (`events`) and again when it is
/// sent. The cut is what lets a client's receive buffer be a fixed size ([`EVENT_BYTES_MAX`]).
pub const EVENT_TEXT_BYTES: usize = 48 << 10;

/// The largest event parcel the service sends, and so the receive buffer a client must offer (the
/// kernel drops a message larger than the buffer with `EMSGSIZE`): a `State` carries two texts of
/// [`EVENT_TEXT_BYTES`] (title and error), plus the parcel header and the TLV framing. The client
/// crate's `EVENT_BYTES` is this number; a test keeps the two together.
pub const EVENT_BYTES_MAX: usize = 2 * EVENT_TEXT_BYTES + (4 << 10);

/// The receive buffer the program that hosts the service must offer: a `CreateView` with the
/// largest `user_style` is a little over 64 KiB, and a message larger than the buffer is dropped by
/// the kernel (`EMSGSIZE`).
pub const REQUEST_BYTES: usize = 128 << 10;

/// Milliseconds between the `Ping` events that tell the service a client is gone (`EPIPE`).
pub const PING_INTERVAL_MS: u64 = 1000;

/// Milliseconds after a send met a client's full queue before webd asks the engine for another
/// picture of that view (or copies one): a client that stopped reading would otherwise cost a whole
/// frame, requested and copied, at the paint rate. A send to the client that gets through ends the
/// wait early.
pub const FULL_BACKOFF_MS: u64 = 250;

/// Things that happened once (a link click, a copy, a lost view, an engine marker) and service
/// notes kept for the service thread between two drains, so a page that clicks links in a loop
/// cannot grow memory; the oldest go first. Frames, states and scroll positions are kept as the
/// latest per view instead (`events`).
pub const QUEUE_KEPT: usize = 1024;

// ---- full views (os.lazy.webd.view.v1, P2 slice 3) ----

/// Web processes one user's full views may hold at once: a view of a new (app, profile) pair past
/// this is `EDQUOT` (docs/webd-plan.md 3.3 and open question 7; a process costs about 107 MiB on this
/// system). Light views are not counted: each app's light views share one process and the per-client
/// view count bounds them.
pub const WEB_PROCESSES_PER_USER: usize = 4;

/// Memory a new web process is assumed to need, used as a safety margin when webd gates a
/// `CreateView` that would start one against the user's memory charge (docs/webd-plan.md 11 item 7):
/// a full view's web process is about 107 MiB measured, and the first full view also starts the
/// shared network process. webd refuses a view whose new process would push the user's charge past
/// its quota by this margin rather than letting the child's `exec` fail with `ENOMEM` (which the old
/// in-process browser hit, `engine.cpp`). The fixed [`WEB_PROCESSES_PER_USER`] cap still applies; this
/// is the finer gate when the quota is lower than four processes' worth. 160 MiB leaves headroom for
/// the network process and the page's first heap over the 107 MiB resident of the process image.
pub const WEB_PROCESS_RESERVE_BYTES: u64 = 160 << 20;

/// Frame slots are 0..SLOTS_PER_VIEW; a view with deltas also has band slots up to this.
pub const SLOTS_WITH_BANDS: usize = SLOTS_PER_VIEW + 2;

/// Rows of the page a band carries at most (WebKit patch 0030 paints at most 1024 at a time): a band
/// slot holds `width * BAND_ROWS_MAX * 4` bytes.
pub const BAND_ROWS_MAX: u32 = 1024;

/// Bands kept per view between two turns of the service: they are speculative, the oldest go first.
pub const BANDS_KEPT: usize = 4;

/// Deltas a view may be behind by between two turns (as the window's mailbox): past this, or past a
/// frame's worth of pixels, the engine is told to paint a whole frame instead.
pub const DELTAS_KEPT: usize = 8;

/// A URL a client sends (`Load`, `Download`, a base URL), at most.
pub const URL_BYTES_MAX: usize = 8 << 10;

/// `ViewConfig.origins`: how many, and how long each.
pub const ORIGINS_MAX: usize = 64;
pub const ORIGIN_BYTES_MAX: usize = 256;

/// `SetBlocking`'s exceptions: how many, and how long each (a host name).
pub const EXCEPTIONS_MAX: usize = 64;
pub const EXCEPTION_BYTES_MAX: usize = 255;

/// A content rule list (`BlockingCompile`), at most: a converted EasyList is a few MiB.
pub const RULES_BYTES_MAX: u64 = 32 << 20;

/// A rule list's name, at most.
pub const LIST_NAME_BYTES_MAX: usize = 128;

/// `ViewConfig.user_agent_suffix`, at most (printable ASCII).
pub const USER_AGENT_SUFFIX_MAX: usize = 64;

/// A held navigation (`Handoff.Ask`) not answered within this is refused.
pub const HOLD_MS: u64 = 10_000;

/// JavaScript dialogs a view may open in [`DIALOG_WINDOW_MS`]; more are dismissed without asking
/// the app (a page that loops `alert()` must not keep a window busy).
pub const DIALOGS_PER_WINDOW: usize = 3;
pub const DIALOG_WINDOW_MS: u64 = 10_000;

/// A page may hand an address to another program (`Handoff.Browser`) only with a user gesture and
/// within this long of real input forwarded to its view (xui_webkit::gesture's lock).
pub const INPUT_LOCK_MS: u64 = 3000;

/// Navigations held at once per view, and notifications on screen at once per view.
pub const HELD_PER_VIEW: usize = 8;
pub const TOASTS_PER_VIEW: usize = 4;

/// Notifications a view made with `notify_in_client` may have open in its app at once (given with
/// `NotifyShow`, not yet `Closed` or cancelled). Past this the page's notification is closed by webd
/// and nothing is sent: a page cannot grow webd's table, nor the app's.
pub const CLIENT_NOTIFICATIONS_PER_VIEW: usize = 64;

/// Each of a `NotifyShow`'s five texts (origin, page address, title, body, tag) is cut at this:
/// five of them and the framing stay well inside [`EVENT_BYTES_MAX`].
pub const NOTIFY_TEXT_BYTES: usize = 16 << 10;
/// webd is the one sender of every client's toasts, so the shell's allowance for a sender (16 on
/// screen, a burst of 6 and one more each half second) is shared by all clients. One client's pages
/// therefore get a share of it: [`TOASTS_PER_CLIENT`] on screen at once (three clients fit in the 16)
/// and a pace of [`CLIENT_TOAST_BURST`] at once, then one every [`CLIENT_TOAST_REFILL_MS`]; no one
/// looping `new Notification(); close()` can use up what the others need. Beyond these a site has the
/// per-site limits of LazyWeb+ (`lazywebnotify`).
pub const TOASTS_PER_CLIENT: usize = 5;
pub const CLIENT_TOAST_BURST: u64 = 3;
pub const CLIENT_TOAST_REFILL_MS: u64 = 2000;
