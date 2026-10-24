//! What ties a page view to the window that holds it: the window's own keys, and what it does with
//! a click on the page.

use std::rc::Rc;

use xui_core::{Key, Modifiers, MouseButton};

/// Whether a key is one of the window's own.
pub type IsShortcut = Rc<dyn Fn(Key, Modifiers) -> bool>;

/// What ties a view to the window that holds it.
pub struct Hooks<M> {
    /// The window's own shortcuts (Ctrl+L, F5, ...): the page never sees them.
    pub is_shortcut: IsShortcut,
    /// The message raised when the page is clicked.
    pub clicked: M,
    /// The message raised for a right click at a point of the view, when the window has a
    /// context menu of its own (the page gets the click as well).
    pub right_click: Option<Rc<dyn Fn(i32, i32) -> M>>,
    /// Asked at a right press: true when the window's menu takes the click, so the page does not
    /// get it (a link menu replaces the page's own). `None` never swallows.
    pub swallow_right: Option<Rc<dyn Fn() -> bool>>,
    /// Asked at a press of another button: the message to raise when the window takes the click
    /// itself (a middle click or Ctrl+click on a link opens it in a tab), in which case the page
    /// gets neither the press nor the release. `None` never takes one.
    pub open_link: Option<OpenLink<M>>,
}

/// What the window does with a press of a button with these modifiers on the view, if anything.
pub type OpenLink<M> = Rc<dyn Fn(MouseButton, Modifiers) -> Option<M>>;
