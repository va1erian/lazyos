//! Keyboard focus: delivery of focus changes, focus-stop membership, and the
//! Tab/PageUp/PageDown focus cycle.

use xui_core::backend::{Event, WidgetId, WindowId};
use xui_core::Modifiers;

use super::LazyOSBackend;

/// What a Tab press does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TabAction {
    /// Move the widget focus (forward unless Shift is held).
    CycleFocus,
    /// Deliver it to the focused widget like any key: the widget asked for
    /// Tab (`NodeSpec::wants_tab`, a code editor indenting with it).
    Deliver,
    /// Drop it: Alt/Ctrl+Tab are the compositor's chords, and one that slips
    /// through must neither move the focus nor reach a widget.
    Ignore,
}

/// What a Tab press with `modifiers` does when the focused widget does (or
/// does not) handle Tab itself.
pub(super) fn tab_action(modifiers: Modifiers, focused_wants_tab: bool) -> TabAction {
    if modifiers.alt || modifiers.ctrl {
        TabAction::Ignore
    } else if focused_wants_tab {
        TabAction::Deliver
    } else {
        TabAction::CycleFocus
    }
}

impl LazyOSBackend {
    /// Give `id` the keyboard focus, notifying the widget that lost it.
    pub(super) fn set_focus(&self, id: WidgetId) {
        let old = self.focused.replace(Some(id));
        if old == Some(id) {
            return;
        }
        let Some(window) = self.window_of(id) else {
            return;
        };
        if let Some(old) = old {
            if self.window_of(old) == Some(window) {
                self.deliver(window, old, &Event::KillFocus);
            }
        }
        self.deliver(window, id, &Event::SetFocus);
    }

    /// Whether `id` is a focus stop (a Tab-order control or a button-like one).
    pub(super) fn is_focus_stop(&self, id: WidgetId) -> bool {
        self.nodes
            .borrow()
            .iter()
            .find(|(node_id, _)| *node_id == id)
            .is_some_and(|(_, node)| node.visible && node.enabled && node.focus_stop)
    }

    /// Whether the focused widget handles Tab itself ([`TabAction::Deliver`]).
    pub(super) fn focused_wants_tab(&self) -> bool {
        let Some(focused) = self.focused.get() else {
            return false;
        };
        self.nodes
            .borrow()
            .iter()
            .find(|(id, _)| *id == focused)
            .is_some_and(|(_, node)| node.visible && node.enabled && node.wants_tab)
    }

    /// Move the keyboard focus to the next (or previous) focus stop.
    pub(super) fn cycle_focus(&self, window: WindowId, forward: bool) {
        let stops: Vec<WidgetId> = self
            .nodes
            .borrow()
            .iter()
            .filter(|(_, node)| {
                node.window == window && node.visible && node.enabled && node.focus_stop
            })
            .map(|(id, _)| *id)
            .collect();
        if stops.is_empty() {
            return;
        }
        let current = self
            .focused
            .get()
            .and_then(|id| stops.iter().position(|stop| *stop == id));
        let next = match current {
            Some(index) if forward => (index + 1) % stops.len(),
            Some(index) => (index + stops.len() - 1) % stops.len(),
            None if forward => 0,
            None => stops.len() - 1,
        };
        self.set_focus(stops[next]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHIFT: Modifiers = Modifiers {
        shift: true,
        ..Modifiers::NONE
    };
    const CTRL: Modifiers = Modifiers {
        ctrl: true,
        ..Modifiers::NONE
    };
    const ALT: Modifiers = Modifiers {
        alt: true,
        ..Modifiers::NONE
    };

    #[test]
    fn tab_moves_focus_unless_the_focused_widget_wants_it() {
        assert_eq!(tab_action(Modifiers::NONE, false), TabAction::CycleFocus);
        assert_eq!(tab_action(SHIFT, false), TabAction::CycleFocus);
        assert_eq!(tab_action(Modifiers::NONE, true), TabAction::Deliver);
        assert_eq!(
            tab_action(SHIFT, true),
            TabAction::Deliver,
            "Shift+Tab outdents"
        );
    }

    #[test]
    fn the_compositor_chords_never_reach_a_widget() {
        for wants in [false, true] {
            assert_eq!(tab_action(CTRL, wants), TabAction::Ignore);
            assert_eq!(tab_action(ALT, wants), TabAction::Ignore);
        }
    }
}
