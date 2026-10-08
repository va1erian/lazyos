//! Keyboard focus: delivery of focus changes, focus-stop membership, and the
//! Tab/PageUp/PageDown focus cycle.

use xui_core::backend::{Event, WidgetId, WindowId};
use xui_core::Modifiers;

use super::geometry::effectively_visible;
use super::{LazyOSBackend, Node};

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

/// Whether the node `id` takes Tab now: it asked for Tab, is enabled, and is
/// visible through its whole ancestry (an editor whose panel was hidden while it
/// kept the focus must let Tab move on to a visible focus stop).
pub(super) fn node_wants_tab(nodes: &[(WidgetId, Node)], id: WidgetId) -> bool {
    nodes
        .iter()
        .find(|(node_id, _)| *node_id == id)
        .is_some_and(|(_, node)| node.wants_tab && node.enabled)
        && effectively_visible(nodes, id)
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
        self.focused
            .get()
            .is_some_and(|focused| node_wants_tab(&self.nodes.borrow(), focused))
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
    use xui_core::backend::ParentRef;
    use xui_core::Rect;

    /// A node under `parent`, visible and enabled, asking for Tab or not.
    fn node(parent: ParentRef, wants_tab: bool) -> Node {
        Node {
            window: WindowId::from_raw(1),
            parent,
            bounds: Rect::new(0, 0, 10, 10),
            visible: true,
            enabled: true,
            focus_stop: true,
            wants_tab,
            text: String::new(),
            painter: None,
            clip: None,
        }
    }

    /// A panel (id 1) holding an editor (id 2) that asks for Tab.
    fn panel_with_editor() -> Vec<(WidgetId, Node)> {
        let panel = WidgetId::from_raw(1);
        vec![
            (panel, node(ParentRef::Window(WindowId::from_raw(1)), false)),
            (WidgetId::from_raw(2), node(ParentRef::Widget(panel), true)),
        ]
    }

    #[test]
    fn a_shown_enabled_editor_takes_tab() {
        let nodes = panel_with_editor();
        assert!(node_wants_tab(&nodes, WidgetId::from_raw(2)));
        assert!(
            !node_wants_tab(&nodes, WidgetId::from_raw(1)),
            "the panel did not ask"
        );
        assert!(
            !node_wants_tab(&nodes, WidgetId::from_raw(9)),
            "an unknown node"
        );
    }

    #[test]
    fn an_editor_hidden_by_an_ancestor_or_disabled_does_not_take_tab() {
        let mut nodes = panel_with_editor();
        nodes[0].1.visible = false;
        assert!(
            !node_wants_tab(&nodes, WidgetId::from_raw(2)),
            "its panel is hidden"
        );
        let mut nodes = panel_with_editor();
        nodes[1].1.enabled = false;
        assert!(
            !node_wants_tab(&nodes, WidgetId::from_raw(2)),
            "it is disabled"
        );
        let mut nodes = panel_with_editor();
        nodes[1].1.visible = false;
        assert!(
            !node_wants_tab(&nodes, WidgetId::from_raw(2)),
            "it is hidden itself"
        );
    }

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
