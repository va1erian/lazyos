//! What the window asks of the engine.
//!
//! The engine is WebKit, which runs in another process (`webd`, docs/webd.md): a window drives it
//! through an implementation of [`Engine`] (`xui-webkit-remote`'s `RemoteEngine` over webd's full
//! views) and every call returns without waiting for the page. The other direction is the
//! [`Mailbox`](crate::mailbox::Mailbox): the engine posts painted frames and page state, the window
//! collects them.

/// Which mouse button an event is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    None,
    Left,
    Middle,
    Right,
}

impl Button {
    /// The ABI value (`WKEventMouseButton`): -1 none, 0 left, 1 middle, 2 right.
    pub const fn code(self) -> i32 {
        match self {
            Button::None => -1,
            Button::Left => 0,
            Button::Middle => 1,
            Button::Right => 2,
        }
    }
}

/// What a mouse event did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseKind {
    Down,
    Up,
    Move,
}

impl MouseKind {
    /// The ABI value: 0 down, 1 up, 2 move.
    pub const fn code(self) -> i32 {
        match self {
            MouseKind::Down => 0,
            MouseKind::Up => 1,
            MouseKind::Move => 2,
        }
    }
}

/// Modifier bits, `WKEventModifiers`.
pub mod mods {
    pub const SHIFT: u32 = 1 << 0;
    pub const CTRL: u32 = 1 << 1;
    pub const ALT: u32 = 1 << 2;
    pub const META: u32 = 1 << 3;
}

/// Which JavaScript dialog a page opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogKind {
    Alert,
    Confirm,
    Prompt,
}

impl DialogKind {
    /// The ABI value: 0 alert, 1 confirm, 2 prompt.
    pub const fn code(self) -> i32 {
        match self {
            DialogKind::Alert => 0,
            DialogKind::Confirm => 1,
            DialogKind::Prompt => 2,
        }
    }

    /// The kind an ABI value names; anything unknown is an alert (the one with no answer to give).
    pub const fn from_code(code: i32) -> DialogKind {
        match code {
            1 => DialogKind::Confirm,
            2 => DialogKind::Prompt,
            _ => DialogKind::Alert,
        }
    }

    /// The word markers and tests use.
    pub const fn name(self) -> &'static str {
        match self {
            DialogKind::Alert => "alert",
            DialogKind::Confirm => "confirm",
            DialogKind::Prompt => "prompt",
        }
    }
}

/// What the window answers a page that asked for the permission to notify.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotifyAnswer {
    /// The person allowed the site: remembered by the engine from now on.
    Allow,
    /// The person blocked it: remembered too.
    Block,
    /// No decision (the bar was closed, the page had no gesture, the page is gone): the page's
    /// request is refused and the site may ask again.
    Dismiss,
}

impl NotifyAnswer {
    /// The ABI value: 1 allow, 0 block, -1 dismiss.
    pub const fn code(self) -> i32 {
        match self {
            NotifyAnswer::Allow => 1,
            NotifyAnswer::Block => 0,
            NotifyAnswer::Dismiss => -1,
        }
    }
}

/// The engine, as the window sees it. Calls come from the UI thread and must
/// not block; positions are in the page view's pixels.
pub trait Engine {
    fn load(&self, url: &str);
    fn reload(&self);
    fn stop(&self);
    /// The page's process does not answer ([`Event::PageResponsive`](crate::events::Event::PageResponsive)
    /// said so) and the person chose a way out: its process is ended, and with `reload` the page is
    /// loaded again in a fresh one; without, it stays stopped until the next load.
    fn recover(&self, reload: bool);
    fn back(&self);
    fn forward(&self);
    /// The page view's size changed.
    fn resize(&self, width: i32, height: i32);
    fn mouse(&self, kind: MouseKind, button: Button, x: i32, y: i32, mods: u32);
    /// A wheel step at (x, y): `ticks_x` and `ticks_y` count notches, positive
    /// scrolling up or right (xui's convention, Windows'; WebKit's own x is
    /// positive to the left, which the C++ side flips).
    fn wheel(&self, x: i32, y: i32, ticks_x: i32, ticks_y: i32, mods: u32);
    /// A key. `text` is what it types (empty for a key that types nothing).
    fn key(&self, press: bool, keysym: u32, text: &str, mods: u32);
    /// The page view gained or lost the keyboard focus.
    fn focus(&self, focused: bool);
    /// Saves `url` as a download (the page, or a picture), as if the server had sent it as an
    /// attachment.
    fn download(&self, url: &str);
    /// Ends download `id` (the file is the window's to remove).
    fn download_cancel(&self, id: u64);
    /// Answers the dialog `id` that [`Event::Dialog`](crate::events::Event::Dialog) announced:
    /// `accepted` is OK, `value` the text of a prompt.
    fn dialog_reply(&self, id: u64, accepted: bool, value: &str);
    /// The notification `id` that [`Event::NotifyShow`](crate::events::Event::NotifyShow)
    /// announced is on screen: the page gets its `show` event.
    fn notify_shown(&self, id: u64);
    /// The person clicked notification `id`: the page's `click` handler runs (when its document
    /// is still there).
    fn notify_clicked(&self, id: u64);
    /// Notification `id` went away without the page asking (the person dismissed it, it timed
    /// out): the page gets its `close` event.
    fn notify_closed(&self, id: u64);
    /// The answer to a [`Event::NotifyPermission`](crate::events::Event::NotifyPermission) for
    /// `origin`: an allow or a block is remembered by the engine for the pages of that origin
    /// (`Notification.permission` reads `granted` or `denied` from then on), a dismissal only
    /// refuses the pages' pending requests.
    fn notify_permission(&self, origin: &str, answer: NotifyAnswer);
    /// Forgets what the engine knows about `origin`'s decision (`Notification.permission` is
    /// `default` again): a decision the person revoked.
    fn notify_forget(&self, origin: &str);
    /// Attaches the rule list `id` the store already holds (compiled by an earlier run); the
    /// result is an [`Event::BlockingList`](crate::events::Event::BlockingList).
    fn blocking_load(&self, id: &str);
    /// Compiles `json` (a WebKit content-rule list) as rule list `id`, stores and attaches it;
    /// compiling takes a while, so the result comes later as an `Event::BlockingList`.
    fn blocking_compile(&self, id: &str, json: &str);
    /// Detaches rule list `id` and removes it from the store.
    fn blocking_forget(&self, id: &str);
    /// Content blocking on or off for the browsing profile, and the sites it is off for (the
    /// user's switch and exceptions; webd decides each navigation from them).
    fn set_blocking(&self, enabled: bool, exceptions: &[String]);
    /// The user agent the page's requests and `navigator.userAgent` show (`""`: the engine's own),
    /// from the next load on: the page on show is loaded again. An engine without the choice
    /// keeps its own.
    fn set_user_agent(&self, _agent: &str) {}
}

/// One call an engine received, for tests.
#[derive(Clone, Debug, PartialEq)]
pub enum Call {
    Load(String),
    Reload,
    Stop,
    Recover(bool),
    Back,
    Forward,
    Resize(i32, i32),
    Mouse(MouseKind, Button, i32, i32, u32),
    Wheel(i32, i32, i32, i32, u32),
    Key(bool, u32, String, u32),
    Focus(bool),
    Download(String),
    DownloadCancel(u64),
    DialogReply(u64, bool, String),
    NotifyShown(u64),
    NotifyClicked(u64),
    NotifyClosed(u64),
    NotifyPermission(String, NotifyAnswer),
    NotifyForget(String),
    BlockingLoad(String),
    /// The id and the JSON's length: a recording keeps no megabytes.
    BlockingCompile(String, usize),
    BlockingForget(String),
    SetBlocking(bool, Vec<String>),
    SetUserAgent(String),
}

/// An engine that records its calls.
#[derive(Default)]
pub struct Recorder {
    calls: std::cell::RefCell<Vec<Call>>,
}

impl Recorder {
    /// The calls so far.
    pub fn calls(&self) -> Vec<Call> {
        self.calls.borrow().clone()
    }

    /// The calls so far, forgotten.
    pub fn take(&self) -> Vec<Call> {
        std::mem::take(&mut *self.calls.borrow_mut())
    }

    fn push(&self, call: Call) {
        self.calls.borrow_mut().push(call);
    }
}

impl Engine for Recorder {
    fn load(&self, url: &str) {
        self.push(Call::Load(url.to_string()));
    }
    fn reload(&self) {
        self.push(Call::Reload);
    }
    fn stop(&self) {
        self.push(Call::Stop);
    }
    fn recover(&self, reload: bool) {
        self.push(Call::Recover(reload));
    }
    fn back(&self) {
        self.push(Call::Back);
    }
    fn forward(&self) {
        self.push(Call::Forward);
    }
    fn resize(&self, width: i32, height: i32) {
        self.push(Call::Resize(width, height));
    }
    fn mouse(&self, kind: MouseKind, button: Button, x: i32, y: i32, mods: u32) {
        self.push(Call::Mouse(kind, button, x, y, mods));
    }
    fn wheel(&self, x: i32, y: i32, ticks_x: i32, ticks_y: i32, mods: u32) {
        self.push(Call::Wheel(x, y, ticks_x, ticks_y, mods));
    }
    fn key(&self, press: bool, keysym: u32, text: &str, mods: u32) {
        self.push(Call::Key(press, keysym, text.to_string(), mods));
    }
    fn focus(&self, focused: bool) {
        self.push(Call::Focus(focused));
    }
    fn download(&self, url: &str) {
        self.push(Call::Download(url.to_string()));
    }
    fn download_cancel(&self, id: u64) {
        self.push(Call::DownloadCancel(id));
    }
    fn dialog_reply(&self, id: u64, accepted: bool, value: &str) {
        self.push(Call::DialogReply(id, accepted, value.to_string()));
    }
    fn notify_shown(&self, id: u64) {
        self.push(Call::NotifyShown(id));
    }
    fn notify_clicked(&self, id: u64) {
        self.push(Call::NotifyClicked(id));
    }
    fn notify_closed(&self, id: u64) {
        self.push(Call::NotifyClosed(id));
    }
    fn notify_permission(&self, origin: &str, answer: NotifyAnswer) {
        self.push(Call::NotifyPermission(origin.to_string(), answer));
    }
    fn notify_forget(&self, origin: &str) {
        self.push(Call::NotifyForget(origin.to_string()));
    }
    fn blocking_load(&self, id: &str) {
        self.push(Call::BlockingLoad(id.to_string()));
    }
    fn blocking_compile(&self, id: &str, json: &str) {
        self.push(Call::BlockingCompile(id.to_string(), json.len()));
    }
    fn blocking_forget(&self, id: &str) {
        self.push(Call::BlockingForget(id.to_string()));
    }
    fn set_blocking(&self, enabled: bool, exceptions: &[String]) {
        self.push(Call::SetBlocking(enabled, exceptions.to_vec()));
    }

    fn set_user_agent(&self, agent: &str) {
        self.push(Call::SetUserAgent(agent.to_owned()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_permission_answer_has_the_documented_codes() {
        assert_eq!(NotifyAnswer::Allow.code(), 1);
        assert_eq!(NotifyAnswer::Block.code(), 0);
        assert_eq!(NotifyAnswer::Dismiss.code(), -1);
    }

    #[test]
    fn the_recorder_keeps_every_notification_call_in_order() {
        let engine = Recorder::default();
        engine.notify_shown(1);
        engine.notify_clicked(1);
        engine.notify_closed(1);
        engine.notify_permission("https://a.test", NotifyAnswer::Allow);
        engine.notify_forget("https://a.test");
        assert_eq!(
            engine.calls(),
            vec![
                Call::NotifyShown(1),
                Call::NotifyClicked(1),
                Call::NotifyClosed(1),
                Call::NotifyPermission("https://a.test".into(), NotifyAnswer::Allow),
                Call::NotifyForget("https://a.test".into()),
            ]
        );
    }
}
