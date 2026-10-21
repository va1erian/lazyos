//! `playground`: the Messenger Playground, a developer tool
//! (`docs/messenger-playground-plan.md`, `docs/devtool-packages-plan.md`).
//!
//! Browse every Messenger interface with its documentation, listen to topics
//! decoded with the type the IDL declares, and write Rhai against them: the
//! buttons on the doc pane write script text, and Run starts a child `rhai`
//! under this app's `tool:` label. The portable core is the `xui-playground`
//! crate; this file and `playground/` are the window and the LazyOS glue.
//!
//! Two windows, one process: the main window (Explorer, doc pane, editor,
//! Output) and the Topic monitor, "Messenger Topics (developer tool)"
//! (`playground/topicswin.rs`), a second `App` opened with `Ui::open_window`
//! that owns the subscriptions, the events list and the Publish form. The main
//! window's Listen, Publish and View > Topic monitor (Ctrl+T) send it requests
//! (`topicshost.rs`); it answers through the main window's `Proxy`.
//!
//! Serial evidence (channel `PLAYGROUND`): `UP:PASS` after the first frame,
//! `DEVTOOL:UP`, `OPEN:<interface>`, `RUN:<id>:START:<hash>` and
//! `RUN:<id>:EXIT:<status>:<hash>`, `LISTEN:<filter>`, `EVENT:<topic>:<seq>`,
//! `CALL:<interface>.<method>:OK|ERR:<errno>`, `TOPICS:UP:PASS`, `TOPICS:QUIT:PASS`,
//! `NOTICE:HIDDEN|SHOWN`, `LAYOUT:BOTTOM:SHOWN|HIDDEN` (Ctrl+J), `LAYOUT:SAVED:<explorer>:<share>`
//! (a dragged divider, written once it settles), `LAYOUT:LOADED:...` (a stored layout at start)
//! and `QUIT:PASS`. Never a
//! script, an argument, a payload or a reply. With the UI probe, `UI:WIDGET`
//! lines name the controls (`tree`, `editor`, `run_button`, `btn:listen`, `toggle_bottom`, and the
//! dividers `split_main` and `split_doc`, ...).

#[path = "playground/app.rs"]
mod app;
#[path = "playground/callform.rs"]
mod callform;
#[path = "playground/docbody.rs"]
mod docbody;
#[path = "playground/docview.rs"]
mod docview;
#[path = "playground/explorer.rs"]
mod explorer;
#[path = "playground/files.rs"]
mod files;
#[path = "playground/formctl.rs"]
mod formctl;
#[path = "playground/help.rs"]
mod help;
#[path = "playground/lifecycle.rs"]
mod lifecycle;
#[path = "playground/listen.rs"]
mod listen;
#[path = "playground/menus.rs"]
mod menus;
#[path = "playground/msg.rs"]
mod msg;
#[path = "playground/notice.rs"]
mod notice;
#[path = "playground/panes.rs"]
mod panes;
#[path = "playground/panesapp.rs"]
mod panesapp;
#[path = "playground/paste.rs"]
mod paste;
#[path = "playground/proc.rs"]
mod proc;
#[path = "playground/publish.rs"]
mod publish;
#[path = "playground/runs.rs"]
mod runs;
#[path = "playground/selection.rs"]
mod selection;
#[path = "playground/startup.rs"]
mod startup;
#[path = "playground/tmsg.rs"]
mod tmsg;
#[path = "playground/topics.rs"]
mod topics;
#[path = "playground/topicshost.rs"]
mod topicshost;
#[path = "playground/topicsui.rs"]
mod topicsui;
#[path = "playground/topicsview.rs"]
mod topicsview;
#[path = "playground/topicswin.rs"]
mod topicswin;
#[path = "playground/ui.rs"]
mod ui;

#[cfg(test)]
#[path = "playground/snapshot_tests.rs"]
mod snapshot_tests;
#[cfg(test)]
#[path = "playground/layout_tests.rs"]
mod layout_tests;
#[cfg(test)]
#[path = "playground/scroll_tests.rs"]
mod scroll_tests;
#[cfg(test)]
#[path = "playground/topics_snapshot_tests.rs"]
mod topics_snapshot_tests;

use xui_app::devtool;
use xui_app::trace::Channel;

/// The window size when a compositor lays the app out; as the display owner
/// it fills the screen instead.
const WINDOW: (i32, i32) = (1180, 660);

fn main() {
    // The code editor and the doc pane's signatures are a monospace grid.
    xui_app::font::register_mono();
    // A panic is reported on the console (and `init` restarts the app).
    xui_app::trace::install_panic_hook(Channel::Playground);
    devtool::run(Channel::Playground, "Messenger Playground", WINDOW, startup::build)
}
