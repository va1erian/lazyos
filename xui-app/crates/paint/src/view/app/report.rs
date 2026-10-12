#![forbid(unsafe_code)]

//! What the host may want to log: a picture was opened or saved.

use std::path::PathBuf;
use std::rc::Rc;

use super::PaintApp;
use crate::model::Format;

/// The host hook [`PaintApp::set_reporter`] installs.
pub(super) type Reporter = Rc<dyn Fn(&Report)>;

/// Something the host may want to log: a picture was opened or saved. The
/// LazyOS binary turns these into its `PAINT:OPEN:PASS` and `PAINT:SAVE:PASS`
/// evidence lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Report {
    /// A picture was opened.
    Opened {
        /// Where it came from, if the store has a path.
        path: Option<PathBuf>,
        /// Its size in pixels.
        size: (u32, u32),
        /// How many layers it has.
        layers: usize,
    },
    /// A picture was saved.
    Saved {
        /// Where it went, if the store has a path.
        path: Option<PathBuf>,
        /// The format written.
        format: Format,
    },
}

impl PaintApp {
    /// Installs the hook that hears about each picture opened or saved.
    pub fn set_reporter(&mut self, reporter: impl Fn(&Report) + 'static) {
        self.reporter = Some(Rc::new(reporter));
    }

    /// Tells the reporter, if any.
    pub(super) fn announce(&self, report: Report) {
        if let Some(reporter) = &self.reporter {
            reporter(&report);
        }
    }
}
