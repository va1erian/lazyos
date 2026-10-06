//! The PDF Viewer (`os.lazy.pdf`, docs/pdf-reader-plan.md): pages laid out
//! in one scrolling column, rendered in tiles by `lazypdf` on worker
//! threads and painted as they arrive over a small preview of each page.
//!
//! [`PdfApp`] is the window; the LazyOS binary (`xui-app/src/bin/pdf.rs`)
//! hands it a [`Host`] and a file from the command line. Evidence lines:
//! `PDF:OPEN:PASS:<path>:<pages>`, `PDF:OPEN:FAIL:<path>:<reason>`,
//! `PDF:OPEN:PASSWORD:<path>`, `PDF:PAGE:DRAWN:<page>:<ms>:<render ms>`,
//! `PDF:PAGE:FAIL:<page>`, `PDF:ZOOM:<percent>` and `PDF:QUIT:PASS`.

#![forbid(unsafe_code)]

mod app;
mod cache;
mod host;
pub mod layout;
mod ui;
mod view;
mod viewer;
mod worker;

pub use app::{shortcut, Msg, PdfApp, WINDOW};
pub use cache::Key;
pub use host::{Host, LogFn};
pub use viewer::Viewer;
