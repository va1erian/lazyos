#![forbid(unsafe_code)]

//! What each open window shows, for the platform's drag and drop and the
//! shell's refreshes: the folder (a drop target copies into it), the view on
//! screen (the drag source) and the selection as paths.
//!
//! xui's views select on press, so pressing a tile of a multi-selection
//! collapses it before the drag is recognised. [`ViewState::drag_paths`]
//! carries the earlier selection when the current one is a single tile of
//! it, and [`Msg::RestoreSelection`] puts it back on screen
//! (va1erian/xui#289 asks the views to keep it instead).

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

use xui_core::app::Proxy;
use xui_core::backend::WidgetId;

use crate::window::Msg;

/// One window's state as the platform sees it.
#[derive(Clone)]
pub struct ViewState {
    /// The window's raw id.
    pub window: u64,
    /// The folder the window shows.
    pub dir: PathBuf,
    /// The view on screen: the icon view or the details list.
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
    pub fn publish(&self, state: ViewState) {
        self.0.borrow_mut().insert(state.window, state);
    }

    pub fn forget(&self, window: u64) {
        self.0.borrow_mut().remove(&window);
    }

    pub fn get(&self, window: u64) -> Option<ViewState> {
        self.0.borrow().get(&window).cloned()
    }

    /// Every open window's state.
    pub fn all(&self) -> Vec<ViewState> {
        self.0.borrow().values().cloned().collect()
    }
}
