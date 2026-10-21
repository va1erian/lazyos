//! The main window's side of the Topic monitor: opening it, sending it
//! requests, closing it with the main window
//! (`docs/messenger-playground-plan.md` section 3, "The Topic monitor window").
//!
//! One process owns both windows (`Ui::open_window`, the way Files opens one
//! window per folder): the monitor is a second [`xui_core::app::App`] with its
//! own message type, so it owns its `Subscriber` and nothing is handed over by
//! a wire. This side keeps the `WindowHandle<TMsg>` and never touches the
//! monitor's state; the monitor answers through the main window's `Proxy`.
//!
//! A client cannot raise its own window (`ActivateSurface` is the shell's), so
//! a request to an open monitor is delivered and the status bar says where it
//! is; only a new window comes up in front.

use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::backend::PlatformSpec;
use xui_core::Dip;
use xui_playground::listener::Registry;

use crate::app::Playground;
use crate::msg::Msg;
use crate::tmsg::{Request, TMsg};
use crate::topicswin::{Deps, TopicsTool, TITLE, WINDOW};

/// The window handle, once the monitor has been opened.
#[derive(Default)]
pub struct TopicsHost {
    handle: Option<xui_core::app::WindowHandle<TMsg>>,
    /// The subscriptions the monitor last reported.
    pub listening: usize,
}

impl TopicsHost {
    pub fn is_open(&self) -> bool {
        self.handle.as_ref().is_some_and(|h| h.is_open())
    }

    /// Deliver `msg`; dropped when no monitor is open.
    pub fn send(&self, msg: TMsg) {
        if let Some(handle) = self.handle.as_ref().filter(|h| h.is_open()) {
            handle.send(msg);
        }
    }

    /// Close the window now: its app is dropped, which ends every
    /// subscription (the main window is closing, so a message would be late).
    pub fn close(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.close();
        }
        self.listening = 0;
    }
}

/// What delivering one request takes.
#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    /// The window is not open: open it first.
    pub open_first: bool,
    /// What to tell the user (the window could not be raised).
    pub note: Option<&'static str>,
}

/// Decide how to deliver `request` to a monitor that is (not) `open`.
pub fn plan(open: bool, request: &Request) -> Plan {
    let note = match (open, request) {
        (true, Request::Show) => Some("The topic monitor is already open: find it in the taskbar."),
        (true, Request::Listen(_)) => Some("Listening in the topic monitor window."),
        (true, Request::FillPublish { .. }) => Some("Publish form filled in the topic monitor window."),
        (false, _) => None,
    };
    Plan {
        open_first: !open,
        note,
    }
}

/// The status bar's text about the monitor.
pub fn status_text(open: bool, listening: usize) -> String {
    match (open, listening) {
        (false, _) => "topic monitor: closed".to_string(),
        (true, 0) => "topic monitor: open".to_string(),
        (true, n) => format!("topic monitor: {n} listening"),
    }
}

impl Playground {
    /// Send `request` to the monitor, opening it first when it is closed.
    pub fn topics_request(&mut self, ui: &mut Ui<Msg>, request: Request) {
        let plan = plan(self.topics.is_open(), &request);
        if plan.open_first && !self.open_topics(ui) {
            return;
        }
        if let Some(note) = plan.note {
            self.note(note);
        }
        self.topics.send(TMsg::Request(request));
    }

    /// The monitor's registry snapshot (publisher names), from the Live read.
    pub fn registry_snapshot(&self) -> Registry {
        Registry::from_join(&self.ex.join, self.last_live_ms)
    }

    /// Open the monitor. Its build runs here, in this call (`Ui::open_window`),
    /// so it only enqueues on the proxy: nothing of `self` is touched.
    fn open_topics(&mut self, ui: &mut Ui<Msg>) -> bool {
        let deps = Deps {
            main: ui.proxy(),
            started: self.started,
            notice: Rc::clone(&self.notice),
            registry: self.registry_snapshot(),
        };
        let spec = PlatformSpec::new(xui_app::devtool::title(TITLE))
            .size(Dip(WINDOW.0 as f32), Dip(WINDOW.1 as f32));
        // The explorer does the same: a window whose widgets cannot be built is a bug.
        match ui.open_window(spec, move |ui| {
            TopicsTool::new(ui, deps).expect("the topic monitor's widgets built")
        }) {
            Ok(handle) => {
                self.topics.handle = Some(handle);
                self.probe_soon();
                true
            }
            Err(e) => {
                self.note(&format!("cannot open the topic monitor: {e}"));
                false
            }
        }
    }

    pub fn show_topics(&mut self, ui: &mut Ui<Msg>) {
        self.topics_request(ui, Request::Show);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closed_monitor_is_opened_for_every_request_and_an_open_one_is_not() {
        let listen = Request::Listen("time/tick".into());
        assert_eq!(plan(false, &listen), Plan { open_first: true, note: None });
        assert!(plan(true, &listen).note.is_some());
        assert!(!plan(true, &listen).open_first);
        let fill = Request::FillPublish { topic: "a".into(), payload: "b".into() };
        assert!(plan(false, &fill).open_first);
        assert!(!plan(true, &fill).open_first);
        assert!(plan(false, &Request::Show).open_first);
    }

    #[test]
    fn showing_an_open_monitor_says_where_it_is_because_it_cannot_be_raised() {
        let note = plan(true, &Request::Show).note.unwrap();
        assert!(note.contains("taskbar"), "{note}");
        assert!(plan(false, &Request::Show).note.is_none(), "a new window needs no note");
    }

    #[test]
    fn the_status_bar_tells_closed_open_and_how_many_listen() {
        assert_eq!(status_text(false, 3), "topic monitor: closed");
        assert_eq!(status_text(true, 0), "topic monitor: open");
        assert_eq!(status_text(true, 2), "topic monitor: 2 listening");
    }

    #[test]
    fn the_title_is_the_developer_tool_title() {
        assert_eq!(
            xui_app::devtool::title(TITLE),
            "Messenger Topics (developer tool)"
        );
    }
}
