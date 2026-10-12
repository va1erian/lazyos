#![forbid(unsafe_code)]

//! The status bar and the observer mirror, rewritten after every message.

use super::super::shortcuts::tool_hint;
use super::PaintApp;

impl PaintApp {
    /// Rewrites the status bar parts.
    pub(super) fn refresh_status(&mut self) {
        let position = match self.cursor {
            Some((x, y)) => format!("{x}, {y}"),
            None => "--".to_string(),
        };
        let (width, height) = self.model.document().size();
        let selection = self.model.selection().map(|s| s.bounds());
        let size = match selection {
            Some(b) => format!("{width} x {height}  sel {}x{}", b.width(), b.height()),
            None => format!("{width} x {height}"),
        };
        let tool = self.model.tool().label().to_string();
        // A hovered tool names itself over the last message while the pointer is
        // on the grid.
        let message = match self.hint {
            Some(tool) => tool_hint(tool),
            None => self.message.clone().unwrap_or_default(),
        };
        self.status.set_text(0, &position);
        self.status.set_text(1, &size);
        self.status.set_text(2, &tool);
        self.status.set_text(3, &message);
        let zoom_percent = self.canvas.zoom_percent();
        let zoom = format!("{zoom_percent}%");
        self.status.set_text(4, &zoom);

        let mut observer = self.observer.borrow_mut();
        observer.tool = self.model.tool();
        observer.size = self.model.size();
        observer.primary = self.model.primary();
        observer.secondary = self.model.secondary();
        observer.can_undo = self.model.history().can_undo();
        observer.can_redo = self.model.history().can_redo();
        observer.cursor = self.cursor;
        observer.dragging = self.model.is_dragging();
        observer.selection = selection.map(|b| (b.x0, b.y0, b.width(), b.height()));
        observer.floating = self.model.is_floating();
        observer.text_editing = self.model.is_editing_text();
        observer.zoom_percent = zoom_percent;
        observer.editing = self.editing.map(|(side, _)| side);
        observer.layers = self
            .model
            .document()
            .layers()
            .iter()
            .map(|l| (l.name.clone(), l.visible, l.opacity))
            .collect();
        observer.active_layer = self.model.document().active();
        let history = self.model.history();
        observer.history = (history.undo_len(), history.undo_len() + history.redo_len());
        observer.props_open = self.editing_layer.is_some();
        observer.status = [position, size, tool, message, zoom];
    }
}
