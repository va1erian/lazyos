//! `xui-pdf`: the PDF Viewer (`docs/pdf-reader-plan.md`).
//!
//! The window is the portable `xui-pdfview` crate over `lazypdf` (hayro);
//! this file supplies the LazyOS platform: the backend and theme, `$HOME`
//! for the Open dialog, a document named on the command line (how `mimed`
//! opens one), and files dropped on the window as `text/uri-list`.
//!
//! Serial evidence: `PDF:UP:PASS` after the first frame;
//! `PDF:BIND:FAIL:<code>`; the crate's `PDF:OPEN|PAGE|ZOOM|QUIT:*` lines.

use std::cell::RefCell;
use std::rc::Rc;

use xui_app::backend::{DropEvent, LazyOSBackend};
use xui_app::platform::{argv, dirs, urilist};
use xui_core::app::{run_app, Proxy};
use xui_core::backend::{Backend, PlatformSpec};
use xui_core::units::Dip;
use xui_pdfview::{Host, Msg, PdfApp, WINDOW};

fn main() -> std::process::ExitCode {
    let path = argv::file_arg(std::env::args_os());
    let backend = match LazyOSBackend::connect() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            println!("PDF:BIND:FAIL:{code}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let (width, height) = backend.window_size(WINDOW);
    // Below this the toolbar clips.
    backend.set_size_hints(420, 300, 0, 0);
    backend.on_first_frame(|| println!("PDF:UP:PASS"));

    // Dropped files reach the app through its proxy, once it exists.
    let outbox: Rc<RefCell<Option<Proxy<Msg>>>> = Rc::default();
    {
        let outbox = Rc::clone(&outbox);
        backend.on_drag_event(move |_, event| {
            if let DropEvent::Drop {
                mime,
                data: Ok(bytes),
                ..
            } = event
            {
                if mime == urilist::MIME {
                    if let Some(proxy) = outbox.borrow().as_ref() {
                        let _ = proxy.send(Msg::Dropped(urilist::decode(bytes)));
                    }
                }
            }
        });
    }

    let theme = backend.desktop_theme();
    let spec = PlatformSpec::new("PDF Viewer").size(Dip(width as f32), Dip(height as f32));
    let outcome = run_app(Rc::clone(&backend) as Rc<dyn Backend>, spec, move |ui| {
        if let Some(theme) = theme {
            ui.set_theme(theme);
        }
        *outbox.borrow_mut() = Some(ui.proxy());
        let app = PdfApp::build(ui, Host::std(dirs::default_dir()))
            .expect("the PDF Viewer's widgets built");
        if let Some(path) = path {
            ui.emit(Msg::OpenChosen(path));
        }
        app
    });
    backend.unbind();
    std::process::ExitCode::from(xui_app::launch::finish("PDF", outcome))
}
