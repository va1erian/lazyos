//! What the terminal sends a program for the mouse: the X10 and SGR
//! encodings of presses, releases, motion and the wheel, for the reporting
//! level the program asked for (`?1000`, `?1002`, `?1003`).

/// How much of the mouse a program wants to hear about.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MouseMode {
    /// None: the terminal keeps the mouse (selection, scrolling).
    #[default]
    Off,
    /// `?1000`: presses, releases and the wheel.
    Click,
    /// `?1002`: also motion while a button is held.
    Drag,
    /// `?1003`: all motion.
    Motion,
}

impl MouseMode {
    pub(crate) fn from_code(code: u16) -> MouseMode {
        match code {
            1000 => MouseMode::Click,
            1002 => MouseMode::Drag,
            _ => MouseMode::Motion,
        }
    }
}

/// A mouse button, or a wheel notch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Button {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

/// What happened to it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Press,
    Release,
    /// The pointer moved; `button` is the one held, if any.
    Motion,
}

/// Modifier keys held.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

/// The largest column or row the X10 encoding can name (one byte, offset 32,
/// 1-based).
const X10_MAX: usize = 222;

/// The bytes to send for a mouse event at 0-based cell (`col`, `row`), or
/// `None` when this level or encoding does not report it.
pub fn mouse_report(
    mode: MouseMode,
    sgr: bool,
    button: Option<Button>,
    action: Action,
    mods: Mods,
    col: usize,
    row: usize,
) -> Option<Vec<u8>> {
    let wheel = matches!(button, Some(Button::WheelUp | Button::WheelDown));
    match (mode, action) {
        (MouseMode::Off, _) => return None,
        (_, Action::Release) if wheel => return None,
        (MouseMode::Click, Action::Motion) => return None,
        (MouseMode::Drag, Action::Motion) if button.is_none() => return None,
        _ => {}
    }
    let mut code = match button {
        Some(Button::Left) => 0,
        Some(Button::Middle) => 1,
        Some(Button::Right) => 2,
        Some(Button::WheelUp) => 64,
        Some(Button::WheelDown) => 65,
        None => 3,
    };
    // The X10 encoding has no release button: a release is "button 3".
    if action == Action::Release && !sgr {
        code = 3;
    }
    code += 4 * u32::from(mods.shift) + 8 * u32::from(mods.alt) + 16 * u32::from(mods.ctrl);
    if action == Action::Motion {
        code += 32;
    }
    if sgr {
        let end = if action == Action::Release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{};{}{end}", col + 1, row + 1).into_bytes());
    }
    let (col, row) = (col.min(X10_MAX), row.min(X10_MAX));
    Some(vec![0x1b, b'[', b'M', 32 + code as u8, 33 + col as u8, 33 + row as u8])
}
