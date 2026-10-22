//! The seam's [`Engine`] over the view: each call is one request or oneway message to webd (or
//! waits for the view when it has none yet).

use messenger_generated::os_lazy_webd_view_v1 as wire;
use xui_webd_client::transport::ScopedTransport;
use xui_webd_client::MouseKind as WireKind;
use xui_webkit::engine::{Button, Engine, MouseKind, NotifyAnswer};
use xui_webkit::gesture;

use crate::input::Move;
use crate::life::answer_code;
use crate::{Health, RemoteEngine};

/// Calls a deferred job may make before the view exists (the blocker's); older ones are dropped.
const DEFERRED_MAX: usize = 256;

impl<T: ScopedTransport + 'static> RemoteEngine<T> {
    /// A call on the live view whose failure only matters when it lost the view.
    fn call(
        &self,
        call: impl FnOnce(&mut xui_webd_client::FullWebView<T>) -> Result<(), xui_webd_client::Error>,
    ) {
        let result = {
            let mut inner = self.inner.borrow_mut();
            if !inner.view.is_live() {
                return;
            }
            call(&mut inner.view)
        };
        self.answer(result, None);
    }

    /// The page on show (its original address), for a reload without a view.
    fn page(&self) -> String {
        let inner = self.inner.borrow();
        let page = if inner.state.committed_url.is_empty() {
            &inner.state.url
        } else {
            &inner.state.committed_url
        };
        inner.aliases.original(page)
    }

    /// A blocker call: now, or when the view is made.
    fn deferred(
        &self,
        job: impl FnOnce(&mut xui_webd_client::FullWebView<T>) -> Result<(), xui_webd_client::Error>
            + 'static,
    ) {
        {
            let mut inner = self.inner.borrow_mut();
            if inner.deferred.len() >= DEFERRED_MAX {
                drop(inner.deferred.remove(0));
            }
        }
        self.on_view(move |view| {
            let _ = job(view);
        });
    }
}

impl<T: ScopedTransport + 'static> Engine for RemoteEngine<T> {
    fn load(&self, url: &str) {
        self.load_now(url);
    }

    fn reload(&self) {
        let (live, loaded) = {
            let inner = self.inner.borrow();
            (inner.view.is_live(), inner.loaded)
        };
        if live && loaded {
            self.call(|view| view.reload(false));
        } else {
            let page = self.page();
            if !page.is_empty() {
                self.load_now(&page);
            }
        }
    }

    fn stop(&self) {
        self.call(|view| view.stop());
    }

    fn recover(&self, reload: bool) {
        self.call(|view| view.recover(reload));
    }

    fn back(&self) {
        self.call(|view| view.back());
    }

    fn forward(&self) {
        self.call(|view| view.forward());
    }

    fn resize(&self, width: i32, height: i32) {
        let size = (width.max(1) as u32, height.max(1) as u32);
        let (live, ended) = {
            let mut inner = self.inner.borrow_mut();
            inner.size = Some(size);
            if inner.health == Health::Waiting {
                inner.health = Health::NotRunning;
            }
            (inner.view.is_live(), inner.health == Health::Ended)
        };
        if live {
            self.call(|view| view.resize(size.0, size.1));
        } else if !ended {
            // A page whose process ended stays blank until the next load makes its view.
            self.ensure_view();
        }
    }

    fn mouse(&self, kind: MouseKind, button: Button, x: i32, y: i32, mods: u32) {
        if kind == MouseKind::Move {
            self.offer_move(Move { button, x, y, mods });
            return;
        }
        // A click is the person's: a hand-off shortly after it is theirs (`gesture`).
        gesture::stamp();
        self.flush_move(self.now_ms(), true);
        let kind = if kind == MouseKind::Down {
            WireKind::Press
        } else {
            WireKind::Release
        };
        let button = button.code().clamp(0, 2);
        self.inner.borrow_mut().view.mouse(kind, button, x, y, mods);
    }

    fn wheel(&self, x: i32, y: i32, ticks_x: i32, ticks_y: i32, mods: u32) {
        self.flush_move(self.now_ms(), true);
        self.inner
            .borrow_mut()
            .view
            .wheel(x, y, ticks_x, ticks_y, mods);
    }

    fn key(&self, press: bool, keysym: u32, text: &str, mods: u32) {
        if press {
            gesture::stamp();
        }
        self.flush_move(self.now_ms(), true);
        self.inner.borrow_mut().view.key(press, keysym, text, mods);
    }

    fn focus(&self, focused: bool) {
        self.inner.borrow_mut().view.focus(focused);
    }

    fn download(&self, url: &str) {
        let url = url.to_owned();
        self.call(move |view| view.download(&url));
    }

    fn download_cancel(&self, id: u64) {
        self.call(|view| view.download_cancel(id));
    }

    fn dialog_reply(&self, id: u64, accepted: bool, value: &str) {
        let value = value.to_owned();
        self.call(move |view| view.dialog_reply(id, accepted, &value));
    }

    fn notify_shown(&self, id: u64) {
        self.call(|view| view.notify_state(id, wire::NOTIFY_STATE_KIND_SHOWN));
    }

    fn notify_clicked(&self, id: u64) {
        self.call(|view| view.notify_state(id, wire::NOTIFY_STATE_KIND_CLICKED));
    }

    fn notify_closed(&self, id: u64) {
        self.call(|view| view.notify_state(id, wire::NOTIFY_STATE_KIND_CLOSED));
    }

    fn notify_permission(&self, origin: &str, answer: NotifyAnswer) {
        if answer != NotifyAnswer::Dismiss {
            let mut inner = self.inner.borrow_mut();
            inner.permissions.insert(origin.to_owned(), answer);
            inner.forgotten.remove(origin);
        }
        let origin = origin.to_owned();
        self.call(move |view| view.notify_permission(&origin, answer_code(answer)));
    }

    fn notify_forget(&self, origin: &str) {
        let live = {
            let mut inner = self.inner.borrow_mut();
            inner.permissions.remove(origin);
            if !inner.view.is_live() {
                inner.forgotten.insert(origin.to_owned());
            }
            inner.view.is_live()
        };
        if live {
            let origin = origin.to_owned();
            self.call(move |view| view.notify_forget(&origin));
        }
    }

    fn blocking_load(&self, id: &str) {
        let id = id.to_owned();
        self.deferred(move |view| view.blocking_load(&id));
    }

    fn blocking_compile(&self, id: &str, json: &str) {
        let (id, json) = (id.to_owned(), json.to_owned());
        self.deferred(move |view| view.blocking_compile(&id, json.as_bytes()));
    }

    fn blocking_forget(&self, id: &str) {
        let id = id.to_owned();
        self.deferred(move |view| view.blocking_forget(&id));
    }

    fn set_blocking(&self, enabled: bool, exceptions: &[String]) {
        self.inner.borrow_mut().blocking = Some((enabled, exceptions.to_vec()));
        let exceptions = exceptions.to_vec();
        self.call(move |view| view.set_blocking(enabled, &exceptions));
    }
}
