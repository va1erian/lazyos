//! `xui-archiver`: the Archiver, a 7-Zip-style archive manager
//! (`docs/archiver-plan.md`).
//!
//! The app (state, commands, dialogs, jobs) is the portable `xui-archiver`
//! crate over the `lazyarc` format library; this file supplies the LazyOS
//! platform through `AppContext`: the backend, `$HOME` for the pickers, a
//! private folder in `/tmp`, the `mimed` launcher for files opened from inside an archive, an
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
use std::path::Path;
use std::rc::Rc;

use xui_app::backend::{DragOffer, DropEvent};
use xui_app::platform::urilist;
use xui_app::trace::{self, Channel};
use xui_archiver::{drag, ArchiverApp, DragState, Msg, WINDOW};
use xui_core::app::Proxy;

/// The app's messages from outside `update` (the drag hooks).
type Outbox = Proxy<Msg>;

fn send(outbox: &Outbox, msg: Msg) {
    let _ = outbox.send(msg);
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
            trace::fail!(Channel::Archiver, "DROP", "{code}");
            send(outbox, Msg::DragLeave);
        }
        DropEvent::Started => {
            trace::pass!(Channel::Archiver, "DRAGSTART");
            send(outbox, Msg::DragStarted(carried.borrow().clone()));
        }
        DropEvent::Refused(code) => {
            trace::fail!(Channel::Archiver, "DRAGSTART", "{code}");
            send(
                outbox,
                Msg::DragFailed(format!("the drag was refused ({code})")),
            );
        }
        DropEvent::Ended { dropped } => trace::line!(Channel::Archiver, "DRAGEND:{dropped}"),
        _ => {}
    }
}

fn main() {
    xui_app::app(Channel::Archiver, "Archiver")
        .size(WINDOW.0, WINDOW.1)
        // The list re-flows to the window; below this the toolbar clips.
        .min_size(600, 360)
        .opens(Msg::OpenChosen)
        .run(move |ui, cx| {
            let backend = cx.backend();
            // The scratch folder for files opened from inside an archive and
            // for dragged-out entries: `/tmp/archiver-<pid>`.
            let host = cx.host();
            let temp = host.temp_dir();
            // Drops and drags reach the app through its proxy, which exists
            // now: the hooks run outside `update`.
            let bridge: Rc<RefCell<DragState>> = Rc::default();
            let outbox = ui.proxy();
            let carried: Rc<RefCell<Vec<usize>>> = Rc::default();
            {
                let (bridge, outbox, carried, temp) = (
                    Rc::clone(&bridge),
                    outbox.clone(),
                    Rc::clone(&carried),
                    temp.clone(),
                );
                backend.on_drag_gesture(move |_, widget, local| {
                    gesture(&bridge, &outbox, &carried, &temp, widget, local)
                });
            }
            backend.on_drag_event(move |_, event| drag_event(&outbox, &carried, event));
            ArchiverApp::build(ui, host, bridge)
        })
}
