//! The Terminal's timers. Each exists only while it has work (the drain's
//! 1 s safety net aside), so an idle Terminal costs no frames: the autoscroll
//! while a drag is past the edge, the input retry while the pty is full, the
//! cursor blink while someone is there, the size badge for a second after a
//! resize, the synchronized-update hold, the bell's flash and the find scan
//! while it has rows left.

use std::cell::Cell;
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::backend::TimerId;

use super::msg::Msg;

#[derive(Clone, Default)]
pub struct Timers {
    pub auto: Rc<Cell<Option<TimerId>>>,
    pub flush: Rc<Cell<Option<TimerId>>>,
    pub blink: Rc<Cell<Option<TimerId>>>,
    pub badge: Rc<Cell<Option<TimerId>>>,
    pub sync: Rc<Cell<Option<TimerId>>>,
    pub flash: Rc<Cell<Option<TimerId>>>,
    pub scan: Rc<Cell<Option<TimerId>>>,
}

impl Timers {
    /// Which message a timer firing is: its own, or the poll tick.
    pub fn message(&self, id: TimerId) -> Msg {
        let is = |cell: &Rc<Cell<Option<TimerId>>>| cell.get() == Some(id);
        if is(&self.auto) {
            Msg::Autoscroll
        } else if is(&self.flush) {
            Msg::Flush
        } else if is(&self.blink) {
            Msg::Blink
        } else if is(&self.sync) {
            Msg::SyncExpired
        } else if is(&self.badge) {
            Msg::BadgeOff
        } else if is(&self.flash) {
            Msg::FlashOff
        } else if is(&self.scan) {
            Msg::FindScan
        } else {
            Msg::Tick
        }
    }

    /// Start `cell`'s timer for `millis` unless it runs; `true` if it started.
    pub fn start(cell: &Rc<Cell<Option<TimerId>>>, ui: &mut Ui<Msg>, millis: u32) -> bool {
        if cell.get().is_some() {
            return false;
        }
        cell.set(Some(ui.set_timer(millis)));
        true
    }

    /// Stop `cell`'s timer if it runs.
    pub fn stop(cell: &Rc<Cell<Option<TimerId>>>, ui: &mut Ui<Msg>) {
        if let Some(id) = cell.take() {
            ui.kill_timer(id);
        }
    }
}
