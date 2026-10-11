//! Pointer shapes over a client window (docs/cursor-plan.md).
//!
//! Widgets ask for a shape with [`Backend::set_cursor`] (a hyperlink wants the
//! hand, an edit the I-beam, a splitter a resize arrow). The compositor draws
//! the pointer, so after every pointer move the backend resolves the shape of
//! the node under it (its own request, else its nearest ancestor's) and tells
//! `xuid` with `SetCursor`, but only when it differs from what the surface
//! last got. A compositor or label policy that refuses the call once is not
//! asked again: the pointer simply stays an arrow.
//!
//! [`Backend::set_cursor`]: xui_core::backend::Backend::set_cursor

use std::collections::{HashMap, HashSet};

use messenger_generated::os_lazy_display_cursor_v1 as wire;
use xui_core::backend::{Cursor, ParentRef, WidgetId, WindowId};

use crate::sys::errno;

use super::{LazyOSBackend, Mode, Node};

/// The shapes widgets asked for and what each surface was last told.
#[derive(Default)]
pub(super) struct Cursors {
    /// The shape each node asked for (absent: no opinion).
    asked: HashMap<u64, Cursor>,
    /// The `CursorShape` each surface was last sent.
    sent: HashMap<u64, u32>,
    /// Windows an app marked busy: the busy pointer wins over any node.
    busy: HashSet<u64>,
    /// The compositor refused `SetCursor`: stop asking.
    refused: bool,
}

impl Cursors {
    pub(super) fn ask(&mut self, id: WidgetId, cursor: Cursor) {
        if cursor == Cursor::Default {
            self.asked.remove(&id.raw());
        } else {
            self.asked.insert(id.raw(), cursor);
        }
    }

    pub(super) fn forget(&mut self, id: WidgetId) {
        self.asked.remove(&id.raw());
    }

    pub(super) fn forget_surface(&mut self, surface: u64) {
        self.sent.remove(&surface);
    }

    /// The `CursorShape` for a pointer over `target` of `window`.
    fn shape(&self, nodes: &[(WidgetId, Node)], window: WindowId, target: Option<WidgetId>) -> u32 {
        if self.busy.contains(&window.raw()) {
            return wire::CURSOR_SHAPE_BUSY;
        }
        let mut at = target;
        // The ancestry is acyclic; the bound only guards a corrupt table.
        for _ in 0..=nodes.len() {
            let Some(id) = at else { break };
            if let Some(cursor) = self.asked.get(&id.raw()) {
                return wire_shape(*cursor);
            }
            at = nodes
                .iter()
                .find(|(node_id, _)| *node_id == id)
                .and_then(|(_, node)| match node.parent {
                    ParentRef::Widget(parent) => Some(parent),
                    ParentRef::Window(_) => None,
                });
        }
        wire::CURSOR_SHAPE_DEFAULT
    }

    /// Record `shape` as sent to `surface`; `false` when it already was (or
    /// sending is off), so nothing needs to go out.
    fn should_send(&mut self, surface: u64, shape: u32) -> bool {
        if self.refused {
            return false;
        }
        self.sent.insert(surface, shape) != Some(shape)
    }

    /// Take note of a failed call.
    fn failed(&mut self, surface: u64, code: i64) {
        // Not allowed, or not understood: never ask again. Anything else
        // (a surface that just went away) is retried at the next move.
        if [-errno::EACCES, -errno::EPERM, -errno::EINVAL].contains(&code) {
            self.refused = true;
        } else {
            self.sent.remove(&surface);
        }
    }
}

/// The wire `CursorShape` of a widget's request.
fn wire_shape(cursor: Cursor) -> u32 {
    match cursor {
        Cursor::Default => wire::CURSOR_SHAPE_DEFAULT,
        Cursor::Hand => wire::CURSOR_SHAPE_HAND,
        Cursor::Text => wire::CURSOR_SHAPE_TEXT,
        Cursor::SizeHorizontal => wire::CURSOR_SHAPE_RESIZE_H,
        Cursor::SizeVertical => wire::CURSOR_SHAPE_RESIZE_V,
        Cursor::SizeNwSe => wire::CURSOR_SHAPE_RESIZE_NWSE,
        Cursor::SizeNeSw => wire::CURSOR_SHAPE_RESIZE_NESW,
        Cursor::Busy => wire::CURSOR_SHAPE_BUSY,
    }
}

impl LazyOSBackend {
    /// After a pointer move that reached `target` (or nothing): tell the
    /// compositor the shape the pointer should have over `window`, when that
    /// changed. Client mode only; owner mode draws no software pointer here.
    pub(super) fn update_cursor(&self, window: WindowId, target: Option<WidgetId>) {
        let Mode::Client(state) = &self.mode else {
            return;
        };
        // A widget may ask for a shape from inside a delivery or a paint that
        // holds one of these tables: skip rather than panic, the next move
        // resolves it.
        let (Ok(windows), Ok(nodes), Ok(mut cursors), Ok(state)) = (
            self.windows.try_borrow(),
            self.nodes.try_borrow(),
            self.cursors.try_borrow_mut(),
            state.try_borrow(),
        ) else {
            return;
        };
        let Some(surface) = windows
            .get(&window.raw())
            .and_then(|entry| entry.client.as_ref())
            .map(|client| client.surface)
        else {
            return;
        };
        let shape = cursors.shape(&nodes, window, target);
        if !cursors.should_send(surface, shape) {
            return;
        }
        let client = state.client;
        // The call may take a while: nothing stays borrowed across it.
        drop((windows, nodes, state));
        drop(cursors);
        if let Err(code) = client.set_cursor(surface, shape) {
            if let Ok(mut cursors) = self.cursors.try_borrow_mut() {
                cursors.failed(surface, code);
            }
        }
    }

    /// A widget changed what it asks for: when the pointer is over it, the
    /// compositor hears at once and not at the next move (NetSurf learns what
    /// is under the pointer a moment after the move that put it there).
    pub(super) fn refresh_cursor(&self) {
        if let Some((window, hovered)) = self.hovered.get() {
            self.update_cursor(window, Some(hovered));
        }
    }

    /// Show the busy pointer over every part of `window` (an app doing long
    /// work), or go back to the per-widget shapes. Takes effect at once, with
    /// the pointer where it last was.
    pub fn set_busy(&self, window: WindowId, busy: bool) {
        {
            let mut cursors = self.cursors.borrow_mut();
            let changed = if busy {
                cursors.busy.insert(window.raw())
            } else {
                cursors.busy.remove(&window.raw())
            };
            if !changed {
                return;
            }
        }
        let hovered = self
            .hovered
            .get()
            .filter(|(owner, _)| *owner == window)
            .map(|(_, id)| id);
        self.update_cursor(window, hovered);
    }
}

#[cfg(test)]
mod tests;
