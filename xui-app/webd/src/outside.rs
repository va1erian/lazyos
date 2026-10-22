//! [`xui_webd::outside::Outside`] over the system's services: `mimed` (through `init`) opens an address
//! a page handed over (`Handoff.Browser`), and the shell's notification service shows a page's
//! notifications as webd's toasts and tells webd when one is clicked or goes away.
//!
//! Both are called on the service thread, the process's first: the notification service refuses a
//! call from any other thread of an app (docs/lazywebplus.md).

use std::cell::RefCell;

use xui_app::platform::launcher;
use xui_app::platform::notify::{self, Event, Level, Listener, Notice};
use xui_webd::outside::{Outside, ToastNews};

/// The system's services, with the toast listener made at the first toast.
#[derive(Default)]
pub struct System {
    listener: RefCell<Option<Listener>>,
}

impl Outside for System {
    fn open_url(&self, url: &str) -> bool {
        launcher::open_url(url).is_ok()
    }

    fn toast(&self, title: &str, body: &str) -> Option<u64> {
        if self.listener.borrow().is_none() {
            *self.listener.borrow_mut() = notify::listen().ok();
        }
        // Without the listener webd would never hear the toast go: not shown at all.
        self.listener.borrow().as_ref()?;
        notify::post(&Notice::new(Level::Info, title, body)).ok()
    }

    fn close_toast(&self, toast: u64) {
        let _ = notify::close(toast);
    }

    fn toast_news(&self) -> Vec<ToastNews> {
        let mut slot = self.listener.borrow_mut();
        let Some(listener) = slot.as_mut() else {
            return Vec::new();
        };
        match listener.poll() {
            Ok(events) => events
                .into_iter()
                .filter_map(|event| match event {
                    Event::Activated { id } | Event::Action { id, .. } => {
                        Some(ToastNews::Clicked { toast: id })
                    }
                    Event::Closed { id, .. } => Some(ToastNews::Closed { toast: id }),
                })
                .collect(),
            // The shell restarted: listen again at the next toast; the toasts of before are gone.
            Err(_) => {
                *slot = None;
                vec![ToastNews::Lost]
            }
        }
    }
}
