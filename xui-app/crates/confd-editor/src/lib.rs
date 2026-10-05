#![forbid(unsafe_code)]

//! The Config app core: a generic, registry-style editor for the `confd`
//! hierarchical key/value store.
//!
//! Unlike the fixed-schema Settings app, this app knows nothing about the keys
//! it edits: it lists the tree, reads a leaf lazily when selected, and parses
//! and formats a value by its [`value_edit::Kind`]. Everything OS-specific sits
//! behind [`store::ConfStore`] (confd on LazyOS, a `BTreeMap` in tests), so the
//! tree, the value grammar and the editor state machine all run and are tested
//! on the host.

pub mod app;
pub mod sections;
pub mod store;
pub mod tree;
pub mod value_edit;
mod view;

pub use app::{ConfdEditorApp, Msg, WINDOW};
pub use sections::{CreateOutcome, KeyEditor, NewKeyEditor};
pub use store::{ConfStore, MemStore, StoreError, StoreInfo};
pub use tree::{Row, Tree};
pub use value_edit::Kind;
