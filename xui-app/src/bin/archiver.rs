//! `xui-archiver`: the Archiver, a 7-Zip-style archive manager
//! (`docs/archiver-plan.md`).
//!
//! The app (state, commands, dialogs, jobs) is the portable `xui-archiver`
//! crate over the `lazyarc` format library; this file supplies the LazyOS
//! platform: the backend, `$HOME` for the pickers, a private folder in
//! `/tmp`, the `mimed` launcher for files opened from inside an archive, an
//! archive named on the command line, and drag and drop: dropped
//! `text/uri-list` payloads become [`Msg::Dropped`], and a press-and-drag on
//! the list extracts the selection to the private folder and offers it as a
//! `text/uri-list`.
//!
//! Serial evidence: `ARCHIVER:UP:PASS` after the first frame;
//! `ARCHIVER:BIND:FAIL:<code>` and `ARCHIVER:RUN:FAIL:<err>`; the crate's
//! `ARCHIVER:<OP>:PASS|FAIL` lines; `ARCHIVER:DRAGSTART:PASS|FAIL:<code>`,
//! `ARCHIVER:DRAGEND:<dropped>` and `ARCHIVER:DROP:FAIL:<code>` here.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use xui_app::backend::{DragOffer, DropEvent, LazyOSBackend};
use xui_app::platform::launcher::LazyLauncher;
use xui_app::platform::{argv, dirs, urilist};
use xui_archiver::{drag, ArchiverApp, DragState, Host, Msg, WINDOW};
use xui_core::app::{run_app, Proxy};
use xui_core::backend::{Backend, PlatformSpec};
use xui_core::units::Dip;
use xui_explorer::platform::Launcher;

/// The app's messages from outside `update` (the drag hooks).
type Outbox = Rc<RefCell<Option<Proxy<Msg>>>>;

fn send(outbox: &Outbox, msg: Msg) {
    if let Some(proxy) = outbox.borrow().as_ref() {
        let _ = proxy.send(msg);
    }
}

/// LazyOS's side of the app.
fn host(temp_dir: PathBuf) -> Host {
    let mut host = Host::std(dirs::default_dir());
    host.temp_dir = temp_dir;
    host.launch = Rc::new(|path: &Path| {
        LazyLauncher::new()
            .open(path)
            .map_err(|error| error.to_string())
    });
    host.log = Rc::new(|line| println!("{line}"));
    host.read_only_roots = vec![PathBuf::from(fhs::system::SYSTEM)];
    host
}

/// A press-and-drag on the list: extract what it carries and offer it.
fn gesture(
    bridge: &Rc<RefCell<DragState>>,
    outbox: &Outbox,
    carried: &Rc<RefCell<Vec<usize>>>,
    temp: &Path,
    widget: xui_core::backend::WidgetId,
    local: (i32, i32),
) -> Option<DragOffer> {
    let state = bridge.borrow();
    if !state.can_drag_from(widget, local.1) {
        return None;
    }
    match drag::prepare(&state, temp) {
        Ok(paths) => {
            let bytes = urilist::encode(&paths).into_bytes();
            *carried.borrow_mut() = state.drag_rows();
            Some(DragOffer {
                mime: urilist::MIME.to_owned(),
                bytes,
            })
        }
        Err(reason) => {
            drop(state);
            send(outbox, Msg::DragFailed(reason));
            None
        }
    }
}

/// The window's drag-and-drop events as messages.
fn drag_event(outbox: &Outbox, carried: &Rc<RefCell<Vec<usize>>>, event: &DropEvent) {
    match event {
        DropEvent::Enter { mime, .. } if mime == urilist::MIME => send(outbox, Msg::DragEnter),
        DropEvent::Leave => send(outbox, Msg::DragLeave),
        DropEvent::Drop {
            mime,
            data: Ok(bytes),
            ..
        } if mime == urilist::MIME => {
            send(outbox, Msg::Dropped(urilist::decode(bytes)));
        }
        DropEvent::Drop {
            data: Err(code), ..
        } => {
            println!("ARCHIVER:DROP:FAIL:{code}");
            send(outbox, Msg::DragLeave);
        }
        DropEvent::Started => {
            println!("ARCHIVER:DRAGSTART:PASS");
            send(outbox, Msg::DragStarted(carried.borrow().clone()));
        }
        DropEvent::Refused(code) => {
            println!("ARCHIVER:DRAGSTART:FAIL:{code}");
            send(
                outbox,
                Msg::DragFailed(format!("the drag was refused ({code})")),
            );
        }
        DropEvent::Ended { dropped } => println!("ARCHIVER:DRAGEND:{dropped}"),
        _ => {}
    }
}

fn main() -> std::process::ExitCode {
    let path = argv::file_arg(std::env::args_os());
    let backend = match LazyOSBackend::connect() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            println!("ARCHIVER:BIND:FAIL:{code}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let (width, height) = backend.window_size(WINDOW);
    // The list re-flows to the window; below this the toolbar clips.
    backend.set_size_hints(600, 360, 0, 0);
    backend.on_first_frame(|| println!("ARCHIVER:UP:PASS"));

    let temp = PathBuf::from(fhs::mount::TMP).join(format!("archiver-{}", std::process::id()));
    let bridge: Rc<RefCell<DragState>> = Rc::default();
    let outbox: Outbox = Rc::default();
    let carried: Rc<RefCell<Vec<usize>>> = Rc::default();
    {
        let (bridge, outbox, carried, temp) = (
            Rc::clone(&bridge),
            Rc::clone(&outbox),
            Rc::clone(&carried),
            temp.clone(),
        );
        backend.on_drag_gesture(move |_, widget, local| {
            gesture(&bridge, &outbox, &carried, &temp, widget, local)
        });
    }
    {
        let (outbox, carried) = (Rc::clone(&outbox), Rc::clone(&carried));
        backend.on_drag_event(move |_, event| drag_event(&outbox, &carried, event));
    }

    let theme = backend.desktop_theme();
    let spec = PlatformSpec::new("Archiver").size(Dip(width as f32), Dip(height as f32));
    let outcome = run_app(Rc::clone(&backend) as Rc<dyn Backend>, spec, move |ui| {
        if let Some(theme) = theme {
            ui.set_theme(theme);
        }
        *outbox.borrow_mut() = Some(ui.proxy());
        let app = ArchiverApp::build(ui, host(temp), bridge).expect("the Archiver's widgets built");
        if let Some(path) = path {
            ui.emit(Msg::OpenChosen(path));
        }
        app
    });
    backend.unbind();
    std::process::ExitCode::from(xui_app::launch::finish("ARCHIVER", outcome))
}
