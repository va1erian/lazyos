#![forbid(unsafe_code)]

//! Everything a host chooses when it builds the app.

use std::cell::RefCell;
use std::rc::Rc;

use xui_core::widget::FileSystem;

use crate::model::text::TextRasterizer;
use crate::storage::Storage;
use crate::view::Observer;

/// The host's choices for [`PaintApp::build_with`](super::PaintApp::build_with).
pub struct Setup {
    /// Where Save and Open go without a dialog.
    pub storage: Rc<dyn Storage>,
    /// The file system behind the Open and Save As dialogs; they are wired
    /// when this is set and the storage supports paths.
    pub fs: Option<Rc<dyn FileSystem>>,
    /// Where the app reports its state.
    pub observer: Rc<RefCell<Observer>>,
    /// The Text tool's rasterizer, with the fonts the host registered on it.
    /// Without one the Text tool tells the user there are no fonts.
    pub text: Option<Rc<dyn TextRasterizer>>,
}

impl Setup {
    /// A setup with only a storage.
    pub fn new(storage: Rc<dyn Storage>) -> Setup {
        Setup {
            storage,
            fs: None,
            observer: Rc::new(RefCell::new(Observer::default())),
            text: None,
        }
    }

    /// Adds the Open and Save As dialogs over `fs`.
    pub fn with_files(mut self, fs: Rc<dyn FileSystem>) -> Setup {
        self.fs = Some(fs);
        self
    }

    /// Reports to `observer`.
    pub fn with_observer(mut self, observer: Rc<RefCell<Observer>>) -> Setup {
        self.observer = observer;
        self
    }

    /// Gives the Text tool a rasterizer.
    pub fn with_text(mut self, text: Rc<dyn TextRasterizer>) -> Setup {
        self.text = Some(text);
        self
    }
}
