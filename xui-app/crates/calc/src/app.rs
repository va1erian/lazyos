//! The Calculator window: the display over a 4x5 keypad, and the keyboard.

use xui_core::arrange::{build as create, button, column, grid, Handle, LayoutExt, Track};
use xui_core::backend::Result;
use xui_core::widget::{Button, HasText};
use xui_core::{App, Lucide, Ui};

use crate::display::{self, Display};
use crate::engine::{Engine, Op, Press};
use crate::keys;

/// The window's design size when a compositor lays the app out.
pub const WINDOW: (i32, i32) = (260, 360);

const PADDING: f32 = 12.0;
const GAP: f32 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Msg {
    Press(Press),
    Quit,
}

/// What the app tells its host (the binary prints it as serial evidence).
#[derive(Debug)]
pub enum Report<'a> {
    /// Every message, as `update` receives it.
    Msg(&'a Msg),
    /// The display after `=`.
    Result(&'a str),
}

/// One keypad key: its label (or icon) and what it presses.
struct Key {
    label: &'static str,
    press: Press,
}

const fn key(label: &'static str, press: Press) -> Key {
    Key { label, press }
}

const fn digit(label: &'static str, digit: u8) -> Key {
    key(label, Press::Digit(digit))
}

/// The keypad, row by row. The clear key's label follows the engine
/// (`C` or `AC`); the backspace key is an icon.
const KEYPAD: [Key; 20] = [
    key("AC", Press::Clear),
    key("", Press::Backspace),
    key("%", Press::Percent),
    key("\u{f7}", Press::Op(Op::Div)),
    digit("7", 7),
    digit("8", 8),
    digit("9", 9),
    key("\u{d7}", Press::Op(Op::Mul)),
    digit("4", 4),
    digit("5", 5),
    digit("6", 6),
    key("\u{2212}", Press::Op(Op::Sub)),
    digit("1", 1),
    digit("2", 2),
    digit("3", 3),
    key("+", Press::Op(Op::Add)),
    key("\u{b1}", Press::Negate),
    digit("0", 0),
    key(".", Press::Point),
    key("=", Press::Equals),
];

/// The app: the engine, the widgets it updates, and the host's report.
pub struct CalcApp {
    engine: Engine,
    display: Handle<Display>,
    clear: Handle<Button<Msg>>,
    report: Box<dyn Fn(Report<'_>)>,
}

impl CalcApp {
    /// Builds the window in `ui`; `report` hears every message and result.
    pub fn build(ui: &mut Ui<Msg>, report: impl Fn(Report<'_>) + 'static) -> Result<CalcApp> {
        let app = CalcApp {
            engine: Engine::new(),
            display: Handle::new(),
            clear: Handle::new(),
            report: Box::new(report),
        };
        ui.root(
            column().padding(PADDING).gap(GAP * 2.0).children((
                create(Display::new)
                    .bind(&app.display)
                    .fixed(display::HEIGHT),
                keypad(&app.clear).fill(1),
            )),
        )?;
        ui.on_key(keys::shortcut);
        ui.on_close(|| Some(Msg::Quit));
        app.display.get().focus();
        Ok(app)
    }

    /// The engine, for a host or a test to read.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Shows the engine's state.
    fn refresh(&self) {
        let engine = &self.engine;
        let display = self.display.get();
        display.show(engine.display(), engine.expression(), engine.is_error());
        let clear = if engine.clears_entry() { "C" } else { "AC" };
        self.clear.get().set_text(clear);
        // A clicked key took the focus; give it back so Enter means `=`.
        display.focus();
    }
}

/// The keys in a grid of equal cells that share the height.
fn keypad(clear: &Handle<Button<Msg>>) -> xui_core::arrange::Layout<Msg> {
    let keys = KEYPAD.iter().map(|key| {
        let mut entry = button(key.label).on_click(Msg::Press(key.press));
        match key.press {
            Press::Clear => entry = entry.bind(clear),
            Press::Backspace => entry = entry.then(|button| button.icon(Lucide::ChevronLeft)),
            Press::Equals => entry = entry.then(Button::primary),
            _ => {}
        }
        entry.fill(1)
    });
    grid(vec![Track::Fill(1); 4])
        .gap(GAP)
        .children(keys.collect::<Vec<_>>())
}

impl App for CalcApp {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        (self.report)(Report::Msg(&msg));
        match msg {
            Msg::Press(press) => {
                self.engine.press(press);
                self.refresh();
                if press == Press::Equals {
                    (self.report)(Report::Result(self.engine.display()));
                }
            }
            Msg::Quit => ui.quit(),
        }
    }
}
