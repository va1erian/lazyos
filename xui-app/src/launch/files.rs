//! Files an app is asked to open: a path on its command line (how `mimed`
//! and `init`'s "open with" start it) and `text/uri-list` payloads dropped on
//! its window. Both become one of the app's own messages, built by the
//! functions the app gave [`AppBuilder::opens`] and [`AppBuilder::drops`]; the
//! drop handler is registered while the window is built, so it holds the
//! window's proxy (no "outbox" filled in later).
//!
//! The file types an app opens are the `mime` list of its package manifest
//! (`docs/packages.md`), where `mimed` reads them; the process only sees the
//! result.

use std::path::PathBuf;

use xui_core::app::Ui;

use super::AppBuilder;
use crate::backend::{DropEvent, LazyOSBackend};
use crate::platform::{argv, urilist};

/// What an app does with the files it is asked to open.
pub trait Files<M> {
    /// Registers the drop handler on the window being built.
    fn attach(&self, backend: &LazyOSBackend, ui: &Ui<M>);
    /// Raises the message for the file named on the command line, if any.
    fn open_argv(&self, ui: &Ui<M>);
}

/// An app that opens nothing.
pub struct NoFiles;

impl<M> Files<M> for NoFiles {
    fn attach(&self, _backend: &LazyOSBackend, _ui: &Ui<M>) {}
    fn open_argv(&self, _ui: &Ui<M>) {}
}

/// An app that opens a file from its command line and/or takes drops.
pub struct WithFiles<M> {
    open: Option<fn(PathBuf) -> M>,
    drop: Option<fn(Vec<PathBuf>) -> M>,
}

impl<M: Send + 'static> Files<M> for WithFiles<M> {
    fn attach(&self, backend: &LazyOSBackend, ui: &Ui<M>) {
        let Some(drop) = self.drop else {
            return;
        };
        let proxy = ui.proxy();
        backend.on_drag_event(move |_, event| {
            if let DropEvent::Drop {
                mime,
                data: Ok(bytes),
                ..
            } = event
            {
                if let Some(msg) = dropped(drop, mime, bytes) {
                    let _ = proxy.send(msg);
                }
            }
        });
    }

    fn open_argv(&self, ui: &Ui<M>) {
        if let (Some(open), Some(path)) = (self.open, argv::file_arg(std::env::args_os())) {
            ui.emit(open(path));
        }
    }
}

/// The message for a drop of `mime` carrying `bytes`: only a `text/uri-list`
/// that names at least one file is one (`drop` is never called with an empty
/// list).
fn dropped<M>(drop: fn(Vec<PathBuf>) -> M, mime: &str, bytes: &[u8]) -> Option<M> {
    if mime != urilist::MIME {
        return None;
    }
    let paths = urilist::decode(bytes);
    (!paths.is_empty()).then(|| drop(paths))
}

impl AppBuilder<NoFiles> {
    /// The message for a file named on the command line.
    pub fn opens<M>(self, open: fn(PathBuf) -> M) -> AppBuilder<WithFiles<M>> {
        self.with_files(WithFiles {
            open: Some(open),
            drop: None,
        })
    }

    /// The message for `text/uri-list` files dropped on the window.
    pub fn drops<M>(self, drop: fn(Vec<PathBuf>) -> M) -> AppBuilder<WithFiles<M>> {
        self.with_files(WithFiles {
            open: None,
            drop: Some(drop),
        })
    }

    fn with_files<M>(self, files: WithFiles<M>) -> AppBuilder<WithFiles<M>> {
        AppBuilder {
            setup: self.setup,
            files,
            at_exit: self.at_exit,
        }
    }
}

impl<M> AppBuilder<WithFiles<M>> {
    /// The message for a file named on the command line.
    pub fn opens(mut self, open: fn(PathBuf) -> M) -> Self {
        self.files.open = Some(open);
        self
    }

    /// The message for `text/uri-list` files dropped on the window.
    pub fn drops(mut self, drop: fn(Vec<PathBuf>) -> M) -> Self {
        self.files.drop = Some(drop);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(paths: Vec<PathBuf>) -> usize {
        paths.len()
    }

    #[test]
    fn a_uri_list_drop_names_its_files() {
        let list = urilist::encode(&[PathBuf::from("/a.pdf"), PathBuf::from("/b.pdf")]);
        assert_eq!(dropped(count, urilist::MIME, list.as_bytes()), Some(2));
    }

    #[test]
    fn a_drop_of_another_type_or_with_no_file_is_not_one() {
        let list = urilist::encode(&[PathBuf::from("/a.pdf")]);
        assert_eq!(dropped(count, "text/plain", list.as_bytes()), None);
        assert_eq!(dropped(count, urilist::MIME, b""), None);
        assert_eq!(dropped(count, urilist::MIME, b"# no files\r\n"), None);
    }
}
