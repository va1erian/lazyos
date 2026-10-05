#![forbid(unsafe_code)]

//! What each open folder window shows, for the platform's drag and drop:
//! the folder (a drop target copies into it), the icon view's widget (the
//! only drag source) and the selection as paths.
//!
//! xui's icon view selects on press, so pressing a tile of a multi-selection
//! collapses it before the drag is recognised. [`ViewState::drag_paths`]
//! carries the earlier selection when the current one is a single tile of
//! it, and [`Msg::RestoreSelection`] puts it back on screen.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

use xui_core::app::Proxy;
use xui_core::backend::WidgetId;

use crate::window::Msg;

/// One window's state as the platform sees it.
#[derive(Clone)]
pub struct ViewState {
    /// The folder the window shows.
    pub dir: PathBuf,
    /// The icon view.
    pub view: WidgetId,
    /// The selected entries, as paths.
    pub selected: Vec<PathBuf>,
    /// The selection before the latest change.
    pub previous: Vec<PathBuf>,
    /// Sends the window a message from outside its `update`.
    pub proxy: Proxy<Msg>,
}

impl ViewState {
    /// What a drag starting now carries.
    pub fn drag_paths(&self) -> Vec<PathBuf> {
        let collapsed = self.selected.len() == 1
            && self.previous.len() > 1
            && self.previous.contains(&self.selected[0]);
        if collapsed {
            self.previous.clone()
        } else {
            self.selected.clone()
        }
    }

    /// Whether the drag carries an earlier selection the screen no longer
    /// shows.
    pub fn collapsed(&self) -> bool {
        self.drag_paths() != self.selected
    }
}

/// Every open window's state, by raw window id.
#[derive(Default)]
pub struct Views(RefCell<HashMap<u64, ViewState>>);

impl Views {
    pub fn publish(&self, window: u64, state: ViewState) {
        self.0.borrow_mut().insert(window, state);
    }

    pub fn forget(&self, window: u64) {
        self.0.borrow_mut().remove(&window);
    }

    pub fn get(&self, window: u64) -> Option<ViewState> {
        self.0.borrow().get(&window).cloned()
    }
}
