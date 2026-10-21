//! Every number webd enforces on a client, in one place (docs/webd-plan.md sections 3.6 and 5). The
//! first six are the plan's; the last four are not caps on what a client may do but the sizes a
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

/// The largest text (title, error, URL, copied text) the service puts in one event: a client's
/// receive buffer is 64 KiB (xui's `Server` precedent) and the parcel header and the other fields
/// share it. Longer text is cut at a character boundary.
pub const EVENT_TEXT_BYTES: usize = 48 << 10;

/// The receive buffer the program that hosts the service must offer: a `CreateView` with the
/// largest `user_style` is a little over 64 KiB, and a message larger than the buffer is dropped by
/// the kernel (`EMSGSIZE`).
pub const REQUEST_BYTES: usize = 128 << 10;

/// Milliseconds between the `Ping` events that tell the service a client is gone (`EPIPE`).
pub const PING_INTERVAL_MS: u64 = 1000;

/// Things that happened once (a link click, a copy, a lost view, an engine marker) and service
/// notes kept for the service thread between two drains, so a page that clicks links in a loop
/// cannot grow memory; the oldest go first. Frames, states and scroll positions are kept as the
/// latest per view instead (`events`).
pub const QUEUE_KEPT: usize = 1024;
