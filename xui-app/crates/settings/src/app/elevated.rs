//! Machine-wide changes that wait on an administrator (or a slow service):
//! run on a worker thread so the window keeps painting, resizing and closing
//! while the prompt is up. The answer returns as [`Msg::Done`].

use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::HasText;

use super::{say, user_theme, ConfigStore, Msg, SettingsApp, UserTheme};
use crate::menu_ops;
use crate::menu_page::MenuMsg;
use crate::store::Detached;
use crate::system::Detached as DetachedSystem;
use crate::time_page::TimeMsg;

/// What a change that waited on an administrator (or a slow service) reports
/// back from its worker thread, ready to show.
#[derive(Clone, Debug, PartialEq)]
pub enum Done {
    /// The user's theme was published as the machine default.
    Theme(std::result::Result<user_theme::Published, String>),
    /// The status line after the keyboard layout was made the default.
    Keyboard(String),
    /// The Time & Date page: a machine change a worker performed, or why it
    /// did not.
    Time(std::result::Result<String, String>),
    /// The Menu page: how many entries the saved menu holds, or why the
    /// save was refused.
    Menu(std::result::Result<usize, String>),
    /// The worker ended without an answer (it panicked).
    Lost,
}

/// Tells the window the worker is gone if it never answered, so the window
/// does not wait on it for ever.
struct Answer(Option<xui_core::app::Proxy<Msg>>);

impl Drop for Answer {
    fn drop(&mut self) {
        if let Some(proxy) = self.0.take() {
            let _ = proxy.send(Msg::Done(Done::Lost));
        }
    }
}

/// The status line while a worker waits for an administrator.
const WAITING: &str = "Waiting for an administrator to approve the change...";

impl SettingsApp {
    /// Publish the user's theme as the machine default.
    pub(super) fn make_default(&mut self, ui: &Ui<Msg>) {
        let Some(uid) = self.store.uid() else {
            return self.status.set_text("Your account is unknown.");
        };
        self.elevated(ui, move |_store, machine| {
            Done::Theme(user_theme::make_default(machine, uid))
        });
    }

    /// Run `job` over the user's and the machine's store, where it may wait
    /// for an administrator's approval (up to minutes): on a worker thread
    /// when the store can be rebuilt there ([`ConfigStore::detached`]), so
    /// the window keeps painting, resizing and closing while the prompt is
    /// up; the answer returns as [`Msg::Done`]. A store that cannot (the
    /// in-memory one) runs it here and now.
    pub(super) fn elevated<F>(&mut self, ui: &Ui<Msg>, job: F)
    where
        F: FnOnce(&dyn ConfigStore, &dyn ConfigStore) -> Done + Send + 'static,
    {
        if self.busy {
            return self
                .status
                .set_text("Still waiting for the previous change to be approved.");
        }
        let Some(detached) = self.machine.detached() else {
            let done = job(self.store.as_ref(), self.machine.as_ref());
            return self.finish(done, ui);
        };
        let proxy = ui.proxy();
        let spawned = std::thread::Builder::new()
            .name("settings-elevated".into())
            .spawn(move || {
                // The worker's own copy of the stores: the UI's are `Rc`s,
                // and a thread resolves its own service endpoints anyway.
                let mut answer = Answer(Some(proxy));
                let machine: Rc<dyn ConfigStore> = Rc::from(detached());
                let store = UserTheme::scoped(Rc::clone(&machine));
                let done = job(store.as_ref(), machine.as_ref());
                if let Some(proxy) = answer.0.take() {
                    let _ = proxy.send(Msg::Done(done));
                }
            });
        match spawned {
            Ok(_) => {
                self.busy = true;
                self.status.set_text(WAITING);
            }
            Err(error) => self
                .status
                .set_text(&format!("Could not start the change: {error}")),
        }
    }

    /// Show what a finished machine-wide change did.
    pub(super) fn finish(&mut self, done: Done, ui: &Ui<Msg>) {
        self.busy = false;
        match done {
            Done::Theme(result) => {
                let text = match result {
                    Ok(done) if done.written == 0 => {
                        String::from("Your theme already is the default for everyone.")
                    }
                    Ok(done) => {
                        println!("SETTINGS:THEME:DEFAULT:PASS keys={}", done.written);
                        let note = if done.kept_picture {
                            " Your own picture stays yours."
                        } else {
                            ""
                        };
                        format!("Your theme is now the default for everyone.{note}")
                    }
                    Err(error) => format!("The default theme was not changed: {error}"),
                };
                self.status.set_text(&text);
                self.load_state();
                self.retheme(ui);
            }
            Done::Lost => self
                .status
                .set_text("The change stopped unexpectedly; nothing was confirmed."),
            Done::Keyboard(text) => {
                self.pages
                    .keyboard
                    .load(self.store.as_ref(), self.machine.as_ref());
                say(&self.status, text);
            }
            // Show what is in effect, not what was refused, then the answer.
            Done::Time(outcome) => {
                self.pages
                    .time
                    .load(self.store.as_ref(), self.system.as_ref());
                self.status.set_text(&match outcome {
                    Ok(text) | Err(text) => text,
                });
            }
            Done::Menu(outcome) => {
                self.pages.menu.load(self.store.as_ref());
                self.status.set_text(&match outcome {
                    Ok(entries) => {
                        println!("SETTINGS:MENU:SAVED:{entries}");
                        String::from("Menu saved.")
                    }
                    Err(error) => format!("Menu not saved: {error}"),
                });
            }
        }
    }

    /// The Time page's message: a machine write waits on an administrator
    /// at the prompt, so it runs on the worker thread and returns as
    /// `Msg::Done` (issue #720); the page answers everything else itself.
    pub(super) fn time_update(
        &mut self,
        ui: &Ui<Msg>,
        msg: TimeMsg,
        store: &dyn ConfigStore,
    ) -> String {
        let detached = self.system.detached();
        match self
            .pages
            .time
            .change(&msg, store, self.system.as_ref())
            .filter(|_| detached.is_some())
        {
            Some(change) => {
                self.with_system(ui, detached, move |machine, system| {
                    Done::Time(crate::time_page::run(change, machine, system))
                });
                String::new()
            }
            None => self.pages.time.update(msg, store, self.system.as_ref()),
        }
    }

    /// The Menu page's message: **Save** is a machine write (it waits on an
    /// administrator), so it runs on a worker thread and comes back as
    /// `Msg::Done`; every other message only edits the draft.
    pub(super) fn menu_update(
        &mut self,
        ui: &Ui<Msg>,
        msg: MenuMsg,
        store: &dyn ConfigStore,
    ) -> String {
        let detached = self.system.detached();
        if matches!(msg, MenuMsg::Save) && self.pages.menu.unsaved() && detached.is_some() {
            let draft = self.pages.menu.draft().to_vec();
            let entries = draft.len();
            self.with_system(ui, detached, move |machine, _system| {
                Done::Menu(crate::menu_ops::save(machine, &draft).map(|()| entries))
            });
            String::new()
        } else {
            self.pages.menu.update(msg, store)
        }
    }

    /// Run a machine change that needs its own `System` on the worker (the
    /// Time and Menu pages: the clock and zone go to `timed` and can wait
    /// for an administrator, story #720): like [`SettingsApp::elevated`],
    /// but `job` also gets the page's system connection. When either the
    /// store or the system cannot be rebuilt on a worker thread (host
    /// tests), the job runs on the calling thread as before.
    pub(super) fn with_system<F>(&mut self, ui: &Ui<Msg>, system: Option<DetachedSystem>, job: F)
    where
        F: FnOnce(&dyn ConfigStore, &dyn crate::system::System) -> Done + Send + 'static,
    {
        if self.busy {
            return self
                .status
                .set_text("Still waiting for the previous change to be approved.");
        }
        let detached: Option<Detached> = self.machine.detached();
        let (Some(detached), Some(system)) = (detached, system) else {
            let done = job(self.machine.as_ref(), self.system.as_ref());
            return self.finish(done, ui);
        };
        let proxy = ui.proxy();
        let spawned = std::thread::Builder::new()
            .name("settings-elevated".into())
            .spawn(move || {
                let mut answer = Answer(Some(proxy));
                // The worker's own copies: the UI's are `Rc`s, and a thread
                // resolves its service endpoints anyway.
                let machine: Rc<dyn ConfigStore> = Rc::from(detached());
                let system = system();
                let done = job(machine.as_ref(), system.as_ref());
                if let Some(proxy) = answer.0.take() {
                    let _ = proxy.send(Msg::Done(done));
                }
            });
        match spawned {
            Ok(_) => {
                self.busy = true;
                self.status.set_text(WAITING);
            }
            Err(error) => self
                .status
                .set_text(&format!("Could not start the change: {error}")),
        }
    }
}
