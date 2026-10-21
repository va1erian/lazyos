//! A double-click on a power row of the real start menu must not power off.
//!
//! The menu panel is a self-painted node, so the backend hands the second
//! press of a quick pair to it as a lone `MouseDoubleClick`, which the model
//! reads as a repeat and never lets confirm (`Menu::choose`). This drives two
//! quick presses through the backend, the menu's real event mapping
//! ([`crate::shell::menu::event_msg`]) and its model, as `MenuApp` does.

use std::cell::RefCell;
use std::rc::Rc;

use lazyshell::menu::{Action, Choice, Menu, Power, Shipped};
use xui_core::backend::{Event, NodeKind, ParentRef, WidgetId, WindowId};
use xui_core::router::WidgetHost;
use xui_core::{MouseButton, Rect};

use super::test_support::client_backend;
use super::{LazyOSBackend, Node, Window};
use crate::shell::menu::{event_msg, MenuMsg};

const W: WindowId = WindowId::from_raw(1);
const SCREEN_H: i32 = 768;

/// The menu panel as `MenuApp` hosts it: every event goes through
/// `event_msg`, a press through `Menu::row_at` and `Menu::choose`.
struct Panel {
    menu: RefCell<Menu>,
    choices: RefCell<Vec<Choice>>,
}

impl WidgetHost for Panel {
    fn deliver(&self, _target: WidgetId, event: &Event) -> bool {
        if let Some(MenuMsg::Press(x, y, repeat)) = event_msg(event) {
            let mut menu = self.menu.borrow_mut();
            if let Some(row) = menu.row_at(x, y) {
                let choice = menu.choose(row, repeat);
                self.choices.borrow_mut().push(choice);
            }
        }
        true
    }
}

/// A backend whose window holds the menu panel as one `Custom` node, and the
/// panel, with the center of the row `action` is on.
fn rig(action: Action) -> (LazyOSBackend, Rc<Panel>, (i32, i32)) {
    let menu = Menu::build(&[], &[], Shipped::Unknown, SCREEN_H);
    let row = menu.find_action(action).expect("the row");
    let rect = menu.row_rect(row).expect("its rectangle");
    let at = (rect.x + rect.w / 2, rect.y + rect.h / 2);
    let panel = Rc::new(Panel {
        menu: RefCell::new(menu),
        choices: RefCell::new(Vec::new()),
    });
    let backend = client_backend();
    backend
        .windows
        .borrow_mut()
        .insert(W.raw(), Window::for_tests(panel.clone()));
    backend.nodes.borrow_mut().push((
        WidgetId::from_raw(1),
        Node {
            window: W,
            parent: ParentRef::Window(W),
            bounds: Rect::new(0, 0, 2000, 2000),
            visible: true,
            enabled: true,
            focus_stop: false,
            wants_tab: false,
            popup: false,
            text: String::new(),
            painter: None,
            clip: None,
            kind: NodeKind::Custom,
        },
    ));
    (backend, panel, at)
}

#[test]
fn a_double_click_on_a_power_row_never_requests_the_action() {
    for power in [Power::Logout, Power::Reboot, Power::PowerOff] {
        let (backend, panel, (x, y)) = rig(Action::Ask(power));
        backend.pointer_down_at(W, x, y, MouseButton::Left, 100);
        backend.pointer_down_at(W, x, y, MouseButton::Left, 110);
        assert_eq!(
            *panel.choices.borrow(),
            [Choice::Confirming(power), Choice::Nothing],
            "{power:?}: the row that replaced the one clicked ignores the repeat"
        );
        assert!(panel.menu.borrow().confirming(), "still asking");
    }
}

#[test]
fn a_second_click_after_the_interval_confirms() {
    let (backend, panel, (x, y)) = rig(Action::Ask(Power::Reboot));
    backend.pointer_down_at(W, x, y, MouseButton::Left, 100);
    // Slower than the double-click interval: a deliberate second click.
    backend.pointer_down_at(W, x, y, MouseButton::Left, 100 + 500);
    assert_eq!(
        *panel.choices.borrow(),
        [
            Choice::Confirming(Power::Reboot),
            Choice::Request(Power::Reboot)
        ]
    );
}
