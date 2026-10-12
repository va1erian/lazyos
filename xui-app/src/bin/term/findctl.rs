//! The find bar at work: opening it, searching as the text changes (a bounded
//! number of rows at a time, the rest on a timer while the scan has rows left),
//! stepping through matches and scrolling the view to the current one.

use lazyterm::search::DEFAULT_BUDGET;
use xui_app::probe;
use xui_core::app::Ui;

use super::findbar::{self, Find, Step};
use super::msg::Msg;
use super::timers::Timers;
use super::view::{first_row, max_offset};
use super::Terminal;

/// How often the scan continues while it has rows left (milliseconds).
const SCAN_MILLIS: u32 = 10;

impl Terminal {
    /// Open the find bar (Ctrl+Shift+F, the menu's Find...).
    pub(super) fn open_find(&mut self, ui: &mut Ui<Msg>) {
        if let Some(layout) = self.layout.get() {
            for (name, r) in findbar::probes(&layout) {
                probe::widget(&self.title, name, r.left, r.top, r.width(), r.height());
            }
        }
        let mut chrome = self.chrome.borrow_mut();
        if chrome.find.is_none() {
            chrome.find = Some(Find::default());
        }
        drop(chrome);
        self.found_reported = None;
        self.redraw(ui);
    }

    pub(super) fn close_find(&mut self, ui: &mut Ui<Msg>) {
        self.chrome.borrow_mut().find = None;
        Timers::stop(&self.timers.scan, ui);
        self.redraw(ui);
    }

    /// The first row the view shows (the anchor of a new search).
    fn view_top(&self) -> u64 {
        let term = self.term.borrow();
        let visible = self.page().clamp(1, term.rows());
        let first = first_row(term.cursor().0, visible, term.rows()) as u64;
        (term.screen_top() + first).saturating_sub(self.offset.get() as u64)
    }

    /// The text or the case changed: search again.
    pub(super) fn find_edited(&mut self, ui: &mut Ui<Msg>) {
        if let Some(find) = self.chrome.borrow_mut().find.as_mut() {
            find.restart();
        }
        self.found_reported = None;
        self.find_scan(ui);
    }

    /// Scan some more rows; keep going on a timer until the search is done.
    pub(super) fn find_scan(&mut self, ui: &mut Ui<Msg>) {
        let anchor = self.view_top();
        let (done, count, first_pick) = {
            let term = self.term.borrow();
            let mut chrome = self.chrome.borrow_mut();
            let Some(find) = chrome.find.as_mut() else {
                Timers::stop(&self.timers.scan, ui);
                return;
            };
            let had_current = find.search.current().is_some();
            let done = find.search.update(&term, DEFAULT_BUDGET, anchor);
            (done, find.search.count(), !had_current && find.search.current().is_some())
        };
        if done {
            Timers::stop(&self.timers.scan, ui);
            let text_empty = self.chrome.borrow().find.as_ref().is_none_or(|f| f.text.is_empty());
            if !text_empty && self.found_reported != Some(count) {
                self.found_reported = Some(count);
                println!("TERM:FIND:{count}");
            }
        } else {
            Timers::start(&self.timers.scan, ui, SCAN_MILLIS);
        }
        if first_pick {
            self.reveal_current();
        }
        self.redraw(ui);
    }

    /// A find bar key or click: step, close, toggle the case.
    pub(super) fn find_step(&mut self, ui: &mut Ui<Msg>, step: Step) {
        match step {
            Step::None => self.redraw(ui),
            Step::Close => self.close_find(ui),
            Step::Edited => self.find_edited(ui),
            Step::Case => {
                if let Some(find) = self.chrome.borrow_mut().find.as_mut() {
                    find.toggle_case();
                }
                self.find_edited(ui);
            }
            Step::Next | Step::Prev => {
                {
                    let mut chrome = self.chrome.borrow_mut();
                    if let Some(find) = chrome.find.as_mut() {
                        if step == Step::Next {
                            find.search.next();
                        } else {
                            find.search.prev();
                        }
                    }
                }
                self.reveal_current();
                self.redraw(ui);
            }
        }
    }

    /// Scroll the view so the current match is on screen (centred when it was
    /// not).
    fn reveal_current(&mut self) {
        let Some(current) = self.chrome.borrow().find.as_ref().and_then(|f| f.search.current()) else {
            return;
        };
        let (screen_top, visible, first, max) = {
            let term = self.term.borrow();
            let visible = self.page().clamp(1, term.rows());
            let first = first_row(term.cursor().0, visible, term.rows()) as u64;
            (term.screen_top(), visible as u64, first, max_offset(&term, visible))
        };
        let top = (screen_top + first).saturating_sub(self.offset.get() as u64);
        let line = current.start.line;
        if line >= top && line < top + visible {
            return;
        }
        // Put the line a third of the way down the view.
        let want_top = line.saturating_sub(visible / 3);
        let offset = (screen_top + first).saturating_sub(want_top) as usize;
        let offset = offset.min(max);
        self.offset.set(offset);
        println!("TERM:SCROLL:{offset}");
    }
}

impl Terminal {
    /// Output arrived while the find bar is open: its matches may have moved.
    pub(super) fn search_after_output(&mut self, ui: &mut Ui<Msg>) {
        if self.chrome.borrow().find.as_ref().is_some_and(|f| !f.text.is_empty()) {
            self.find_scan(ui);
        }
    }
}
