//! A page's notifications (docs/webd-plan.md 3.5 and open question 6): webd keeps which sites may
//! notify per profile, asks the client when a page requests it (the bar is the client's), and posts
//! the toasts itself, under its own name, titled with the site's host and never with the page's own
//! words. A click runs the page's handler and tells the client (which may raise its window).
//!
//! WebKit keeps one permission record for the whole context, so a decision made in one profile also
//! reads as `Notification.permission` in another; webd therefore posts only what the view's own profile
//! allowed (a page of another profile that calls `new Notification` gets its `close` at once).

use messenger_generated::os_lazy_webd_view_v1 as wire;

use super::calls::{live, MALFORMED};
use super::{Note, Service};
use crate::config::{c_string_safe, Refusal};
use crate::engine::Engine;
use crate::events::Once;
use crate::fabric::{errno, Fabric};
use crate::fullconfig::Profile;
use crate::ident::ClientKey;
use crate::limits::{ORIGIN_BYTES_MAX, TOASTS_PER_VIEW};
use crate::outside::ToastNews;
use crate::policy::{host, origin};

/// What a decision is kept under: the normalised origin (`scheme://host[:port]`, lower case, the
/// scheme's own port left out) when it is one, so a client's spelling and WebKit's agree.
fn site_key(text: &str) -> String {
    origin(text).unwrap_or_else(|| text.to_owned())
}

impl<E: Engine, F: Fabric> Service<E, F> {
    /// What the profile decided about `origin`, if anything.
    fn decision(&self, profile: &Profile, origin: &str) -> Option<bool> {
        self.permissions
            .get(profile)?
            .get(&site_key(origin))
            .copied()
    }

    /// The notification news of the engine. Returns anything else back.
    pub(super) fn notify_once(&mut self, event: Once) -> Option<Once> {
        match event {
            Once::NotifyShow {
                view,
                id,
                origin,
                title,
                body,
            } => self.show(view, id, &origin, &title, &body),
            Once::NotifyCancel { view, id } => {
                let toast = self.full_of(view).and_then(|full| {
                    let toast = full
                        .toasts
                        .iter()
                        .find(|(_, n)| **n == id)
                        .map(|(t, _)| *t)?;
                    full.toasts.remove(&toast);
                    Some(toast)
                });
                if let Some(toast) = toast {
                    self.outside.close_toast(toast);
                }
            }
            // The page's object is gone: the toast stays, a click only tells the client.
            Once::NotifyGone { view, id } => {
                if let Some(full) = self.full_of(view) {
                    full.toasts
                        .values_mut()
                        .filter(|n| **n == id)
                        .for_each(|n| *n = 0);
                }
            }
            Once::NotifyAsk { view, origin } => self.ask(view, origin),
            Once::NotifyAskGone { .. } => {}
            other => return Some(other),
        }
        None
    }

    fn show(&mut self, view: u64, id: u64, origin: &str, title: &str, body: &str) {
        let profile = self.full_of(view).map(|full| full.rules.profile.clone());
        let allowed = profile.is_some_and(|p| self.decision(&p, origin) == Some(true));
        let room = self
            .full_of(view)
            .is_some_and(|full| full.toasts.len() < TOASTS_PER_VIEW);
        let site = host(origin).unwrap_or_else(|| origin.to_owned());
        let toast = if allowed && room {
            let text = [title, body]
                .iter()
                .filter(|part| !part.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join("\n");
            self.outside.toast(&site, &text)
        } else {
            None
        };
        match toast.and_then(|toast| Some((toast, self.full_of(view)?))) {
            Some((toast, full)) => {
                full.toasts.insert(toast, id);
                self.engine.notify_shown(id);
                self.note(Note::Full {
                    view,
                    what: "toast",
                });
            }
            None => {
                self.engine.notify_closed(id);
                self.note(Note::Full {
                    view,
                    what: "toast refused",
                });
            }
        }
    }

    /// A page asked to notify: a decision the profile has is answered at once, else the client asks.
    fn ask(&mut self, view: u64, origin: String) {
        let Some(profile) = self.full_of(view).map(|full| full.rules.profile.clone()) else {
            return;
        };
        if let Some(allowed) = self.decision(&profile, &origin) {
            self.engine
                .notify_permission(view, &origin, i32::from(allowed));
            return;
        }
        let body =
            wire::encode_notify_permission_request_args(&wire::NotifyPermissionRequestArgs {
                view,
                origin,
            });
        self.event_to_live(view, wire::METHOD_NOTIFYPERMISSIONREQUEST, body);
    }

    /// `NotifyPermission`.
    pub(super) fn notify_permission_call(
        &mut self,
        client: ClientKey,
        body: &[u8],
    ) -> Result<(), Refusal> {
        let args = wire::decode_notify_permission_args(body).map_err(|_| MALFORMED)?;
        if args.origin.is_empty()
            || args.origin.len() > ORIGIN_BYTES_MAX
            || !c_string_safe(&args.origin)
        {
            return Err(MALFORMED);
        }
        let answer = match args.answer {
            wire::PERMISSION_ANSWER_ALLOW => 1,
            wire::PERMISSION_ANSWER_BLOCK => 0,
            wire::PERMISSION_ANSWER_DISMISS => -1,
            _ => return Err(MALFORMED),
        };
        let view = live(&mut self.views, client, true, args.view)?;
        let rules = &view.full.as_ref().ok_or(MALFORMED)?.rules;
        if !rules.notifications {
            return Err(Refusal::new(
                errno::EACCES,
                "the view was not made with that",
            ));
        }
        let profile = rules.profile.clone();
        if answer >= 0 {
            self.permissions
                .entry(profile)
                .or_default()
                .insert(site_key(&args.origin), answer > 0);
        }
        self.engine
            .notify_permission(args.view, &args.origin, answer);
        Ok(())
    }

    /// What the shell said about webd's toasts: a click runs the page's handler and tells the client;
    /// a toast that went away is the page's `close`.
    pub(super) fn toast_news(&mut self) {
        for news in self.outside.toast_news() {
            let toast = match news {
                ToastNews::Clicked { toast } | ToastNews::Closed { toast } => toast,
                ToastNews::Lost => {
                    self.forget_toasts();
                    continue;
                }
            };
            let found = self.views.ids().into_iter().find_map(|view| {
                let full = self.full_of(view)?;
                let id = full.toasts.remove(&toast)?;
                Some((view, id))
            });
            let Some((view, id)) = found else {
                continue;
            };
            match news {
                ToastNews::Clicked { .. } => {
                    if id != 0 {
                        self.engine.notify_clicked(id);
                    }
                    let body = wire::encode_notify_clicked_args(&wire::NotifyClickedArgs {
                        view,
                        notification: id,
                    });
                    self.event_to_live(view, wire::METHOD_NOTIFYCLICKED, body);
                }
                ToastNews::Closed { .. } => {
                    if id != 0 {
                        self.engine.notify_closed(id);
                    }
                }
                ToastNews::Lost => {}
            }
        }
    }

    /// Every toast is gone with the shell's line: each page gets its `close`, and no view's count
    /// stays full of toasts nobody will report.
    fn forget_toasts(&mut self) {
        for view in self.views.ids() {
            let ids: Vec<u64> = match self.full_of(view) {
                Some(full) => std::mem::take(&mut full.toasts).into_values().collect(),
                None => continue,
            };
            for id in ids.into_iter().filter(|id| *id != 0) {
                self.engine.notify_closed(id);
            }
        }
    }
}
