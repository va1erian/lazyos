//! The logic of the demo client (`webddemo`, `os.lazy.webddemo`), apart from its window so the host
//! tests can run it against the real service half: it keeps one light view alive, loads
//! [`crate::page::demo_page`] into it, and says what happened as [`Marker`]s the app prints through
//! `xui_app::trace` and the QEMU harness (`tools/webd`) judges.
//!
//! The story it tells: wait for `webd` to be there, make a view, load the page, see the first frame
//! that is not blank (and how long that took), report link clicks and titles, and when the view is
//! lost make a new one once and show the page again.

use crate::event::{Error, ViewEvent};
use crate::page;
use crate::picture::{Colour, Picture};
use crate::transport::Transport;
use crate::view::{LightConfig, LightWebView};

/// Milliseconds between tries to create the view while `webd` is not there (the app may start
/// before the service has registered its name).
pub const RETRY_MS: u64 = 250;
/// Milliseconds after a loss before the new view is made.
pub const RECREATE_AFTER_MS: u64 = 500;

/// One line of evidence. [`Marker::text`] is what follows `WEBDDEMO:`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Marker {
    /// A view exists: `VIEW:PASS id=<n>`.
    View { id: u64 },
    /// The first frame of the page: `FRAME:PASS colour=<c> w=<w> h=<h> seq=<n> ms=<ms>`.
    Frame {
        colour: Colour,
        width: u32,
        height: u32,
        seq: u64,
        ms: u64,
    },
    /// `LATENCY:<ms>`, from `set_html` to that frame.
    Latency { ms: u64 },
    /// `LINK:<url>`.
    Link { url: String },
    /// `STATE:<title>`, once per finished load.
    State { title: String },
    /// `LOST:<reason>`.
    Lost { reason: &'static str },
    /// The new view painted the page again: `RECREATE:PASS`.
    Recreated,
    /// A call was refused: `FAIL:<what>:<why>` (never the page's own words).
    Failed { what: &'static str, why: String },
}

impl Marker {
    /// The line after the `WEBDDEMO:` prefix.
    pub fn text(&self) -> String {
        match self {
            Marker::View { id } => format!("VIEW:PASS id={id}"),
            Marker::Frame {
                colour,
                width,
                height,
                seq,
                ms,
            } => format!(
                "FRAME:PASS colour={} w={width} h={height} seq={seq} ms={ms}",
                colour.name()
            ),
            Marker::Latency { ms } => format!("LATENCY:{ms}"),
            Marker::Link { url } => format!("LINK:{url}"),
            Marker::State { title } => format!("STATE:{title}"),
            Marker::Lost { reason } => format!("LOST:{reason}"),
            Marker::Recreated => "RECREATE:PASS".to_owned(),
            Marker::Failed { what, why } => format!("{what}:FAIL:{why}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// No view yet: create one at `at` (milliseconds) or later.
    Creating {
        at: u64,
    },
    Running,
    /// The view was lost at `at`.
    Lost {
        at: u64,
    },
    /// Lost again after the one automatic recreate: nothing more happens by itself.
    Gone,
}

/// What the next non-blank frame is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Awaiting {
    Nothing,
    /// The first page of the run.
    First {
        loaded_at: u64,
    },
    /// The page again, in the view made after a loss.
    Again,
}

/// The demo's state machine, driven by [`Demo::tick`] from the app's timer.
pub struct Demo<T: Transport> {
    view: LightWebView<T>,
    phase: Phase,
    awaiting: Awaiting,
    /// The one automatic recreate was used.
    recreated: bool,
    /// The last error told, so a retry loop reports a failure once, not every tick.
    last_failure: Option<String>,
    last_title: Option<String>,
}

impl<T: Transport> Demo<T> {
    pub fn new(transport: T) -> Self {
        Demo {
            view: LightWebView::new(transport, LightConfig::default()),
            phase: Phase::Creating { at: 0 },
            awaiting: Awaiting::Nothing,
            recreated: false,
            last_failure: None,
            last_title: None,
        }
    }

    /// The view, for input.
    pub fn view(&mut self) -> &mut LightWebView<T> {
        &mut self.view
    }

    /// The picture on show, for drawing.
    pub fn picture(&self) -> &Picture {
        self.view.picture()
    }

    /// Ask for a new view now (the `R` key), whatever state the old one is in.
    pub fn recreate_now(&mut self) {
        self.phase = Phase::Lost { at: 0 };
        self.recreated = false;
    }

    /// One turn: `size` is the area the picture fills in pixels, `now_ms` any monotonic clock.
    pub fn tick(&mut self, size: (u32, u32), now_ms: u64) -> Vec<Marker> {
        let mut out = Vec::new();
        if size.0 == 0 || size.1 == 0 {
            return out;
        }
        match self.phase {
            Phase::Creating { at } if now_ms >= at => self.create(size, now_ms, &mut out),
            Phase::Lost { at } if now_ms >= at.saturating_add(RECREATE_AFTER_MS) => {
                self.recreate(now_ms, &mut out);
            }
            Phase::Running => {
                if size != self.view.size() && self.view.is_live() {
                    if let Err(error) = self.view.resize(size.0, size.1) {
                        self.fail("RESIZE", error, &mut out);
                    }
                }
                self.running(now_ms, &mut out);
            }
            _ => {}
        }
        out
    }

    fn create(&mut self, size: (u32, u32), now_ms: u64, out: &mut Vec<Marker>) {
        let made = self.view.create(size.0, size.1).and_then(|()| self.load());
        match made {
            Ok(()) => {
                self.last_failure = None;
                self.phase = Phase::Running;
                self.awaiting = Awaiting::First { loaded_at: now_ms };
                out.push(Marker::View {
                    id: self.view.view_id().unwrap_or(0),
                });
            }
            Err(error) => {
                // The service may not be up yet: try again, and say so once.
                self.phase = Phase::Creating {
                    at: now_ms + RETRY_MS,
                };
                if error != Error::NotRunning {
                    self.fail("CREATE", error, out);
                }
            }
        }
    }

    fn recreate(&mut self, now_ms: u64, out: &mut Vec<Marker>) {
        match self.view.recreate() {
            Ok(()) => {
                self.last_failure = None;
                self.recreated = true;
                self.phase = Phase::Running;
                self.awaiting = Awaiting::Again;
                out.push(Marker::View {
                    id: self.view.view_id().unwrap_or(0),
                });
            }
            Err(error) => {
                // Still down (webd restarting): keep trying.
                self.phase = Phase::Lost { at: now_ms };
                if error != Error::NotRunning {
                    self.fail("RECREATE", error, out);
                }
            }
        }
    }

    fn load(&mut self) -> Result<(), Error> {
        self.view
            .set_html(&page::demo_page(page::BLOCKED_IMAGE_URL), "about:blank")
    }

    fn running(&mut self, now_ms: u64, out: &mut Vec<Marker>) {
        for event in self.view.poll() {
            match event {
                ViewEvent::Frame {
                    seq, width, height, ..
                } => self.frame(seq, (width, height), now_ms, out),
                ViewEvent::LinkClicked(url) => out.push(Marker::Link { url }),
                ViewEvent::State(state) => {
                    if state.finished > 0
                        && !state.loading
                        && self.last_title.as_deref() != Some(state.title.as_str())
                    {
                        self.last_title = Some(state.title.clone());
                        out.push(Marker::State { title: state.title });
                    }
                }
                ViewEvent::Lost(why) => {
                    out.push(Marker::Lost { reason: why.name() });
                    self.awaiting = Awaiting::Nothing;
                    self.last_title = None;
                    self.phase = if self.recreated {
                        Phase::Gone
                    } else {
                        Phase::Lost { at: now_ms }
                    };
                }
                ViewEvent::Scroll { .. } | ViewEvent::ClipboardWrite(_) => {}
            }
        }
    }

    /// The first frame that is not the blank white page is the page.
    fn frame(&mut self, seq: u64, size: (u32, u32), now_ms: u64, out: &mut Vec<Marker>) {
        if self.view.picture().dominant() == Colour::White {
            return;
        }
        match std::mem::replace(&mut self.awaiting, Awaiting::Nothing) {
            Awaiting::First { loaded_at } => {
                let ms = now_ms.saturating_sub(loaded_at);
                out.push(Marker::Frame {
                    colour: self.view.picture().dominant(),
                    width: size.0,
                    height: size.1,
                    seq,
                    ms,
                });
                out.push(Marker::Latency { ms });
            }
            Awaiting::Again => out.push(Marker::Recreated),
            Awaiting::Nothing => {}
        }
    }

    fn fail(&mut self, what: &'static str, error: Error, out: &mut Vec<Marker>) {
        let why = error.to_string();
        if self.last_failure.as_deref() != Some(why.as_str()) {
            self.last_failure = Some(why.clone());
            out.push(Marker::Failed { what, why });
        }
    }
}
