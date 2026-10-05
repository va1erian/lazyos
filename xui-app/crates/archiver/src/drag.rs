//! The drag bridge: what the platform's drag-and-drop layer needs to know
//! about the window, and the extraction a drag out of the archive runs.
//!
//! xui has no drag-and-drop vocabulary, so the LazyOS binary's backend hook
//! asks this shared state, outside `App::update`, whether a press-and-drag
//! on a widget starts a drag and what it carries. The app keeps it current
//! after every update.
//!
//! xui's list view selects on press, so pressing a row of a multi-selection
//! collapses it to that row before the drag is recognised. When the
//! selection just went from several rows to one of them, the drag carries
//! the earlier selection (and the app restores it on screen).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use lazyarc::extract::{self, Options};
use lazyarc::{rewrite, Archive, Progress};
use xui_core::backend::WidgetId;

use crate::folder::{selected_paths, Row};

/// What a drag may carry, mirrored from the app.
#[derive(Default)]
pub struct DragState {
    pub archive: Option<Arc<Archive>>,
    pub folder: String,
    pub rows: Vec<Row>,
    pub selection: Vec<usize>,
    pub previous: Vec<usize>,
    /// The list view, the only drag source.
    pub list: Option<WidgetId>,
    /// The list's header height in pixels: a press there resizes or sorts.
    pub list_header: i32,
    /// A job is running: no drag (its archive may be about to change).
    pub busy: bool,
}

impl DragState {
    /// Whether a drag may start on widget `id`, pressed `y` pixels below
    /// its top.
    pub fn can_drag_from(&self, id: WidgetId, y: i32) -> bool {
        y >= self.list_header
            && !self.busy
            && self.archive.is_some()
            && self.list == Some(id)
            && !self.drag_rows().is_empty()
    }

    /// The rows a drag starting now carries.
    pub fn drag_rows(&self) -> Vec<usize> {
        let collapsed = self.selection.len() == 1
            && self.previous.len() > 1
            && self.previous.contains(&self.selection[0]);
        let rows = if collapsed {
            &self.previous
        } else {
            &self.selection
        };
        rows.iter()
            .copied()
            .filter(|&i| !selected_paths(&self.rows, &[i]).is_empty())
            .collect()
    }
}

/// Extract the rows a drag carries into a fresh folder below `scratch`;
/// returns the top-level paths created there (what the drop target gets).
pub fn prepare(state: &DragState, scratch: &Path) -> Result<Vec<PathBuf>, String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let archive = state.archive.as_ref().ok_or("no archive is open")?;
    let paths = selected_paths(&state.rows, &state.drag_rows());
    if paths.is_empty() {
        return Err("nothing selected".to_owned());
    }
    let dest = scratch.join(format!("drag-{}", NEXT.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&dest);
    let options = Options {
        strip: state.folder.clone(),
        ..Options::default()
    };
    let wanted = |entry: &lazyarc::Entry| rewrite::under_any(entry, &paths);
    let report = extract::extract(
        archive,
        &wanted,
        &dest,
        &options,
        &Arc::new(Progress::new()),
    )
    .map_err(|error| error.to_string())?;
    if report.top_level.is_empty() {
        return Err(report
            .skipped
            .first()
            .map(|(path, why)| format!("{path}: {why}"))
            .unwrap_or_else(|| "nothing could be extracted".to_owned()));
    }
    Ok(report.top_level)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::folder::parent_row;

    fn row(name: &str) -> Row {
        let mut row = parent_row();
        row.name = name.to_owned();
        row.path = name.to_owned();
        row.kind = crate::folder::RowKind::File;
        row
    }

    #[test]
    fn a_collapsed_multi_selection_drags_whole() {
        let state = DragState {
            rows: vec![parent_row(), row("a"), row("b"), row("c")],
            selection: vec![2],
            previous: vec![1, 2, 3],
            ..DragState::default()
        };
        assert_eq!(state.drag_rows(), [1, 2, 3]);
    }

    #[test]
    fn a_press_on_another_row_drags_only_it() {
        let state = DragState {
            rows: vec![row("a"), row("b"), row("c")],
            selection: vec![0],
            previous: vec![1, 2],
            ..DragState::default()
        };
        assert_eq!(state.drag_rows(), [0]);
    }

    #[test]
    fn the_parent_row_never_drags() {
        let state = DragState {
            rows: vec![parent_row(), row("a")],
            selection: vec![0],
            ..DragState::default()
        };
        assert!(state.drag_rows().is_empty());
    }
}
