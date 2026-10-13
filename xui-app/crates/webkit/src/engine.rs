//! What the window asks of the engine.
//!
//! The engine is WebKit's UI-process half, in C++ (`webkit/port/app/`): it
//! runs on its own thread, so every [`Engine`] call only queues work and
//! returns. The other direction is the [`Mailbox`](crate::mailbox::Mailbox):
//! the engine posts painted frames and page state, the window collects them.

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

/// The engine, as the window sees it. Calls come from the UI thread and must
/// not block; positions are in the page view's pixels.
pub trait Engine {
    fn load(&self, url: &str);
    fn reload(&self);
    fn stop(&self);
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
}

/// One call an engine received, for tests.
#[derive(Clone, Debug, PartialEq)]
pub enum Call {
    Load(String),
    Reload,
    Stop,
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
}
