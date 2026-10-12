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
    /// scrolling up or left.
    fn wheel(&self, x: i32, y: i32, ticks_x: i32, ticks_y: i32, mods: u32);
    /// A key. `text` is what it types (empty for a key that types nothing).
    fn key(&self, press: bool, keysym: u32, text: &str, mods: u32);
    /// The page view gained or lost the keyboard focus.
    fn focus(&self, focused: bool);
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
}
