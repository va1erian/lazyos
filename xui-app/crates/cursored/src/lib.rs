//! Cursor Studio: draw the mouse pointer's shapes as vector polygons and
//! apply them (docs/cursor-editor-plan.md). The model, the theme store seam
//! and the window are host testable; `src/bin/cursored.rs` supplies the OS.

pub mod app;
mod canvas;
pub mod model;
mod preview;
pub mod store;

use model::Tool;

/// What the window can be asked to do.
#[derive(Clone, Debug)]
pub enum Msg {
    Pick(usize),
    Tool(Tool),
    /// The canvas changed the model.
    Edited,
    Customize,
    ResetShape,
    Undo,
    Redo,
    Delete,
    Finish,
    AddLayer,
    Thin,
    Fill(String),
    Outline(String),
    Name(String),
    Apply,
    Revert,
    Quit,
}
