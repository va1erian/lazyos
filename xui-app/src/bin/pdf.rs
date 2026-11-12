//! `xui-pdf`: the PDF Viewer (`docs/pdf-reader-plan.md`).
//!
//! The window is the portable `xui-pdfview` crate over `lazypdf` (hayro);
//! this file supplies the LazyOS platform through `AppContext`: `$HOME` for
//! the Open dialog, a document named on the command line (how `mimed` opens
//! one), and files dropped on the window as `text/uri-list`.
//!
//! Serial evidence: `PDF:UP:PASS` after the first frame;
//! `PDF:BIND:FAIL:<code>`; the crate's `PDF:OPEN|PAGE|ZOOM:*` lines and
//! `PDF:QUIT:PASS`.

use xui_app::trace::Channel;
use xui_pdfview::{Msg, PdfApp, WINDOW};

fn main() {
    xui_app::app(Channel::Pdf, "PDF Viewer")
        .size(WINDOW.0, WINDOW.1)
        // Below this the toolbar clips.
        .min_size(420, 300)
        .opens(Msg::OpenChosen)
        .drops(Msg::Dropped)
        .run(|ui, cx| PdfApp::build(ui, cx.host()))
}
