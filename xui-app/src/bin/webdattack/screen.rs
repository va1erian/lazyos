//! The control that runs the malicious-client probes once, a short time after the window is up (so
//! webd is resolvable), prints the outcomes and asks the app to quit.

use std::cell::RefCell;
use std::rc::Rc;

use xui_app::platform::webd::WebdTransport;
use xui_app::trace::{self, Channel};
use xui_core::app::Ui;
use xui_core::backend::{NodeKind, NodeSpec, WidgetId};
use xui_core::geometry::Size;
use xui_core::layout::Constraints;
use xui_core::widget::{Control, Placeable};
use xui_core::{Canvas, Color, Dip, Rect, TextStyle};
use xui_webd_client::full::{FullWebView, Scope};
use xui_webd_client::{attack, ViewConfig};

use crate::Msg;

/// How often the timer checks whether webd is up (ms). webd starts its WebKit context and compiles the
/// network-lock rule list at login, which takes a few seconds, so an app that opens at the same login
/// (this one) must wait for it before the probes mean anything.
const TICK_MS: u32 = 500;
/// Ticks to wait for webd before giving up and running the probes anyway (so a truly absent webd still
/// produces a verdict): 60 x 500 ms = 30 s.
const MAX_TICKS: u32 = 60;

pub struct Screen {
    control: Control<Msg>,
    _done: Rc<RefCell<bool>>,
}

impl Placeable<Msg> for Screen {
    fn id(&self) -> WidgetId {
        self.control.id()
    }

    fn measure(&self, _ui: &Ui<Msg>, _constraints: Constraints) -> Size {
        Size::new(0, 0)
    }
}

impl Screen {
    pub fn new(ui: &Ui<Msg>) -> Screen {
        let control = Control::new(ui, &NodeSpec::new(NodeKind::Custom, Rect::default()))
            .expect("the attack control");
        control.set_painter(Rc::new(move |canvas: &mut dyn Canvas| {
            let bounds = canvas.bounds();
            canvas.clear(Color::hex(0x20_20_28));
            let style = TextStyle::new(Color::hex(0xDD_DD_DD), Dip(14.0))
                .centered()
                .middle();
            canvas.draw_text("Web Attack Probe", bounds, &style);
        }));
        let done = Rc::new(RefCell::new(false));
        let ticks = Rc::new(RefCell::new(0u32));
        {
            let done = Rc::clone(&done);
            let ticks = Rc::clone(&ticks);
            // Fires until webd is up (a view-scope create no longer answers "not running") or the
            // deadline; then runs the probes once and quits. Later fires after that do nothing.
            control.set_timer(TICK_MS, move || {
                let mut ran = done.try_borrow_mut().ok()?;
                if *ran {
                    return None;
                }
                let mut n = ticks.try_borrow_mut().ok()?;
                *n += 1;
                if !webd_up() && *n < MAX_TICKS {
                    return None;
                }
                *ran = true;
                run_probes();
                Some(Msg::Quit)
            });
        }
        Screen {
            control,
            _done: done,
        }
    }
}

/// Whether webd makes a valid view through the view scope (a `DataOnly` page, which that scope
/// allows): then its service is registered and its engine ready (a full view without the network
/// waits for the light views' lock, which webd compiles at start). Only a made view counts: a refusal
/// would show the name resolves, not that the engine is up. The trial view is destroyed at once; the
/// real probes make their own.
fn webd_up() -> bool {
    let config = ViewConfig {
        scheme_policy: messenger_generated::os_lazy_webd_view_v1::SCHEME_POLICY_DATA_ONLY,
        ..ViewConfig::default()
    };
    let mut trial = FullWebView::new(WebdTransport::full(), Scope::View, config);
    let up = trial.create(8, 8).is_ok();
    trial.destroy();
    up
}

/// Run every probe against the running webd and print the outcomes.
fn run_probes() {
    let probes = attack::run(WebdTransport::full);
    for probe in &probes {
        trace::line!(
            Channel::Webdattack,
            "PROBE:{}:{}",
            probe.name,
            probe.outcome
        );
    }
    trace::pass!(Channel::Webdattack, "DONE");
}
