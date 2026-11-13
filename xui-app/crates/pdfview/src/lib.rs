//! The PDF Viewer (`os.lazy.pdf`, docs/pdf-reader-plan.md): pages laid out
//! in one scrolling column, rendered in tiles by `lazypdf` on worker
//! threads and painted as they arrive over a small preview of each page.
//!
//! [`PdfApp`] is the window; the LazyOS binary (`xui-app/src/bin/pdf.rs`)
//! hands it an [`AppHost`] and a file from the command line. Evidence lines:
//! `PDF:OPEN:PASS:<path>:<pages>`, `PDF:OPEN:FAIL:<path>:<reason>`,
//! `PDF:OPEN:PASSWORD:<path>`, `PDF:PAGE:DRAWN:<page>:<ms>:<render ms>`,
//! `PDF:PAGE:FAIL:<page>`, and `PDF:ZOOM:<percent>`; `PDF:QUIT:PASS` is
//! the launcher's (`xui_app::app`).

#![forbid(unsafe_code)]

mod app;
mod cache;
pub mod layout;
mod ui;
mod view;
mod viewer;
mod worker;

pub use app::{shortcut, Msg, PdfApp, WINDOW};
pub use cache::Key;
pub use viewer::Viewer;
pub use xui_host::{AppHost, SharedHost};
