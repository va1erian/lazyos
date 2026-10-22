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
use xui_webd_client::attack;

use crate::Msg;

/// How long after the first paint the probes run (ms): enough for the window to be up and webd
/// resolvable. The probes themselves use bounded calls, so a slow webd does not hang the UI.
const DELAY_MS: u32 = 400;

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
        {
            let done = Rc::clone(&done);
            // A one-shot: the first fire runs the probes and returns Quit; later fires do nothing.
            control.set_timer(DELAY_MS, move || {
                let mut ran = done.try_borrow_mut().ok()?;
                if *ran {
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

/// Run every probe against the running webd and print the outcomes.
fn run_probes() {
    let probes = attack::run(WebdTransport::full);
    for probe in &probes {
        trace::line!(Channel::Webdattack, "PROBE:{}:{}", probe.name, probe.outcome);
    }
    trace::pass!(Channel::Webdattack, "DONE");
}
