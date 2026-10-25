//! The window's one engine, which is the front tab's (docs/lazyweb-plan.md 2.1).
//!
//! The page view and the window talk to a single [`Engine`]; each tab has an engine of its own (a
//! view of webd), and this one forwards every call to the front tab's. Switching tabs changes where
//! the calls go and nothing else, so the page view, built once, never knows.
//!
//! Three things a page numbers for itself, a dialog, a download and a notification, are numbered
//! per view, so two tabs could use the same number. The window gives those numbers to its lists
//! (dialogs, transfers, notifications) as a **wire id**, the tab in the upper 32 bits, and the
//! answer for a wire id goes to the tab that asked, whichever tab is in front.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use xui_webkit::engine::{Button, Engine, MouseKind, NotifyAnswer};

use crate::tabs::TabId;

/// The wire id of page number `id` of tab `tab`. A page's own number is cut to 32 bits.
pub fn wire_id(tab: TabId, id: u64) -> u64 {
    (tab << 32) | (id & 0xffff_ffff)
}

/// The tab and the page's own number in a wire id.
pub fn split_id(wire: u64) -> (TabId, u64) {
    (wire >> 32, wire & 0xffff_ffff)
}

/// The engines of the window's tabs, and which one is in front.
#[derive(Default)]
pub struct Switch {
    front: Cell<TabId>,
    engines: RefCell<BTreeMap<TabId, Rc<dyn Engine>>>,
}

impl Switch {
    pub fn new() -> Switch {
        Switch::default()
    }

    /// Gives tab `tab` its engine.
    pub fn add(&self, tab: TabId, engine: Rc<dyn Engine>) {
        self.engines.borrow_mut().insert(tab, engine);
    }

    /// Lets go of tab `tab`'s engine (its view ends when the last holder lets go).
    pub fn remove(&self, tab: TabId) -> Option<Rc<dyn Engine>> {
        self.engines.borrow_mut().remove(&tab)
    }

    /// From now on calls go to tab `tab`.
    pub fn set_front(&self, tab: TabId) {
        self.front.set(tab);
    }

    pub fn front(&self) -> TabId {
        self.front.get()
    }

    pub fn engine(&self, tab: TabId) -> Option<Rc<dyn Engine>> {
        self.engines.borrow().get(&tab).cloned()
    }

    fn current(&self) -> Option<Rc<dyn Engine>> {
        self.engine(self.front.get())
    }

    fn all(&self) -> Vec<Rc<dyn Engine>> {
        self.engines.borrow().values().cloned().collect()
    }

    /// The engine a wire id belongs to, and the page's own number.
    fn owner(&self, wire: u64) -> Option<(Rc<dyn Engine>, u64)> {
        let (tab, id) = split_id(wire);
        Some((self.engine(tab)?, id))
    }
}

impl Engine for Switch {
    fn load(&self, url: &str) {
        if let Some(engine) = self.current() {
            engine.load(url);
        }
    }

    fn reload(&self) {
        if let Some(engine) = self.current() {
            engine.reload();
        }
    }

    fn stop(&self) {
        if let Some(engine) = self.current() {
            engine.stop();
        }
    }

    fn recover(&self, reload: bool) {
        if let Some(engine) = self.current() {
            engine.recover(reload);
        }
    }

    fn back(&self) {
        if let Some(engine) = self.current() {
            engine.back();
        }
    }

    fn forward(&self) {
        if let Some(engine) = self.current() {
            engine.forward();
        }
    }

    fn resize(&self, width: i32, height: i32) {
        if let Some(engine) = self.current() {
            engine.resize(width, height);
        }
    }

    fn mouse(&self, kind: MouseKind, button: Button, x: i32, y: i32, mods: u32) {
        if let Some(engine) = self.current() {
            engine.mouse(kind, button, x, y, mods);
        }
    }

    fn wheel(&self, x: i32, y: i32, ticks_x: i32, ticks_y: i32, mods: u32, seq: u64) {
        if let Some(engine) = self.current() {
            engine.wheel(x, y, ticks_x, ticks_y, mods, seq);
        }
    }

    fn key(&self, press: bool, keysym: u32, text: &str, mods: u32) {
        if let Some(engine) = self.current() {
            engine.key(press, keysym, text, mods);
        }
    }

    fn focus(&self, focused: bool) {
        if let Some(engine) = self.current() {
            engine.focus(focused);
        }
    }

    fn download(&self, url: &str) {
        if let Some(engine) = self.current() {
            engine.download(url);
        }
    }

    fn download_cancel(&self, id: u64) {
        if let Some((engine, id)) = self.owner(id) {
            engine.download_cancel(id);
        }
    }

    fn dialog_reply(&self, id: u64, accepted: bool, value: &str) {
        if let Some((engine, id)) = self.owner(id) {
            engine.dialog_reply(id, accepted, value);
        }
    }

    fn notify_shown(&self, id: u64) {
        if let Some((engine, id)) = self.owner(id) {
            engine.notify_shown(id);
        }
    }

    fn notify_clicked(&self, id: u64) {
        if let Some((engine, id)) = self.owner(id) {
            engine.notify_clicked(id);
        }
    }

    fn notify_closed(&self, id: u64) {
        if let Some((engine, id)) = self.owner(id) {
            engine.notify_closed(id);
        }
    }

    /// The profile keeps the decision (webd), so one view is enough; the others learn it when they
    /// are made (the window's list is read then).
    fn notify_permission(&self, origin: &str, answer: NotifyAnswer) {
        if let Some(engine) = self.current() {
            engine.notify_permission(origin, answer);
        }
    }

    fn notify_forget(&self, origin: &str) {
        for engine in self.all() {
            engine.notify_forget(origin);
        }
    }

    /// Rule lists are the `browser` profile's, shared by its views: the front tab's call is enough.
    fn blocking_load(&self, id: &str) {
        if let Some(engine) = self.current() {
            engine.blocking_load(id);
        }
    }

    fn blocking_compile(&self, id: &str, json: &str) {
        if let Some(engine) = self.current() {
            engine.blocking_compile(id, json);
        }
    }

    fn blocking_forget(&self, id: &str) {
        if let Some(engine) = self.current() {
            engine.blocking_forget(id);
        }
    }

    /// Every engine keeps the switch, so a view made again (or a tab revived) is told.
    fn set_blocking(&self, enabled: bool, exceptions: &[String]) {
        for engine in self.all() {
            engine.set_blocking(enabled, exceptions);
        }
    }

    fn set_user_agent(&self, agent: &str) {
        if let Some(engine) = self.current() {
            engine.set_user_agent(agent);
        }
    }

    fn set_visible(&self, visible: bool) {
        if let Some(engine) = self.current() {
            engine.set_visible(visible);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xui_webkit::engine::{Call, Recorder};

    fn two() -> (Switch, Rc<Recorder>, Rc<Recorder>) {
        let switch = Switch::new();
        let (a, b) = (Rc::new(Recorder::default()), Rc::new(Recorder::default()));
        switch.add(1, a.clone());
        switch.add(2, b.clone());
        switch.set_front(1);
        (switch, a, b)
    }

    #[test]
    fn calls_go_to_the_front_tab_and_follow_a_switch() {
        let (switch, a, b) = two();
        switch.load("https://a.example/");
        switch.set_front(2);
        switch.load("https://b.example/");
        switch.reload();
        assert_eq!(a.calls(), [Call::Load("https://a.example/".into())]);
        assert_eq!(
            b.calls(),
            [Call::Load("https://b.example/".into()), Call::Reload]
        );
    }

    #[test]
    fn an_answer_goes_to_the_tab_that_asked_whichever_is_in_front() {
        let (switch, a, b) = two();
        switch.set_front(2);
        switch.dialog_reply(wire_id(1, 7), true, "x");
        switch.download_cancel(wire_id(2, 3));
        switch.notify_clicked(wire_id(1, 9));
        assert_eq!(
            a.calls(),
            [
                Call::DialogReply(7, true, "x".into()),
                Call::NotifyClicked(9)
            ]
        );
        assert_eq!(b.calls(), [Call::DownloadCancel(3)]);
        // A tab that is gone has nobody to answer.
        switch.remove(1);
        switch.dialog_reply(wire_id(1, 8), false, "");
    }

    #[test]
    fn the_blockers_switch_reaches_every_tab_and_its_lists_the_front_one() {
        let (switch, a, b) = two();
        switch.set_blocking(true, &["x.example".to_owned()]);
        switch.blocking_load("easylist");
        let on = Call::SetBlocking(true, vec!["x.example".to_owned()]);
        assert_eq!(
            a.calls(),
            [on.clone(), Call::BlockingLoad("easylist".into())]
        );
        assert_eq!(b.calls(), [on]);
    }

    #[test]
    fn wire_ids_keep_the_tab_and_the_pages_number() {
        assert_eq!(split_id(wire_id(5, 42)), (5, 42));
        assert_ne!(wire_id(1, 1), wire_id(2, 1));
    }
}
