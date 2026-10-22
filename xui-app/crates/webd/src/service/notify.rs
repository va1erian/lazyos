//! A page's notifications (docs/webd-plan.md 3.5 and open question 6): webd keeps which sites may
//! notify per profile, asks the client when a page requests it (the bar is the client's), and posts
//! the toasts itself, under its own name, titled with the site's host and never with the page's own
//! words. A click runs the page's handler and tells the client (which may raise its window).
//!
//! WebKit keeps one permission record for the whole context, so a decision made in one profile also
//! reads as `Notification.permission` in another; webd therefore posts only what the view's own profile
//! allowed (a page of another profile that calls `new Notification` gets its `close` at once).

use std::collections::BTreeMap;

use lazywebnotify::{toast_body, toast_title, Admit, Bucket, Dropped, Origin, Registry};
use messenger_generated::os_lazy_webd_view_v1 as wire;

use super::calls::{live, MALFORMED};
use super::{Note, Service};
use crate::config::{c_string_safe, Refusal};
use crate::engine::Engine;
use crate::events::Once;
use crate::fabric::{errno, Fabric};
use crate::fullconfig::Profile;
use crate::ident::ClientKey;
use crate::limits::{
    CLIENT_TOAST_BURST, CLIENT_TOAST_REFILL_MS, ORIGIN_BYTES_MAX, TOASTS_PER_CLIENT,
    TOASTS_PER_VIEW,
};
use crate::outside::ToastNews;
use crate::policy::origin;

/// What one client has on screen and how fast its pages may post: webd is the shell's one sender for all
/// of them, so each client gets a share of the sender's allowance (`limits::TOASTS_PER_CLIENT`).
pub(super) struct ClientToasts {
    /// The client's live toasts, with the per-site limits and paces of LazyWeb+.
    registry: Registry,
    /// The client's own pace, over all its sites and views.
    pace: Bucket,
}

impl ClientToasts {
    fn new(now_ms: u64) -> Self {
        ClientToasts {
            registry: Registry::with_total(TOASTS_PER_CLIENT),
            pace: Bucket::new(now_ms, CLIENT_TOAST_BURST, CLIENT_TOAST_REFILL_MS),
        }
    }
}

/// Why a toast was not posted, as the trace says it.
fn why(dropped: Dropped) -> &'static str {
    match dropped {
        Dropped::Rate => "toast limit: site rate",
        Dropped::Site => "toast limit: site",
        Dropped::Busy => "toast limit: client",
    }
}

/// What a decision is kept under: the normalised origin (`scheme://host[:port]`, lower case, the
/// scheme's own port left out) when it is one, so a client's spelling and WebKit's agree.
fn site_key(text: &str) -> String {
    origin(text).unwrap_or_else(|| text.to_owned())
}

/// An origin a client names: not empty, bounded and safe as a C string.
fn check_origin(origin: &str) -> Result<(), Refusal> {
    if origin.is_empty() || origin.len() > ORIGIN_BYTES_MAX || !c_string_safe(origin) {
        return Err(MALFORMED);
    }
    Ok(())
}

impl<E: Engine, F: Fabric> Service<E, F> {
    /// What the profile decided about `origin`, if anything.
    pub(super) fn decision(&self, profile: &Profile, origin: &str) -> Option<bool> {
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
                page_url,
                title,
                body,
                tag,
            } => {
                if self.in_client(view) {
                    let texts = [origin, page_url, title, body, tag];
                    self.forward_show(view, id, texts);
                } else {
                    self.show(view, id, &origin, &title, &body);
                }
            }
            Once::NotifyCancel { view, id } if self.in_client(view) => {
                self.forward_end(view, id, wire::METHOD_NOTIFYCANCEL);
            }
            Once::NotifyGone { view, id } if self.in_client(view) => {
                self.forward_end(view, id, wire::METHOD_NOTIFYGONE);
            }
            Once::NotifyCancel { view, id } => {
                if let Some(registry) = self.registry_of(view) {
                    registry.cancel(id);
                }
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
        let owner = self.views.any_mut(view).map(|v| v.owner);
        // Only a web page has a site a person can allow (`data:`, `blob:` ... have none).
        let site = Origin::from_url(origin);
        let posted = match (allowed && room, owner, site) {
            (true, Some(owner), Some(site)) => self.post_toast(owner, id, &site, title, body),
            _ => Err("toast refused"),
        };
        match posted.and_then(|toast| {
            self.full_of(view)
                .map(|full| (toast, full))
                .ok_or("toast refused")
        }) {
            Ok((toast, full)) => {
                full.toasts.insert(toast, id);
                self.engine.notify_shown(id);
                self.note(Note::Full {
                    view,
                    what: "toast",
                });
            }
            Err(what) => {
                self.engine.notify_closed(id);
                self.note(Note::Full { view, what });
            }
        }
    }

    /// The limits first (the client's pace, then the site's live toasts and pace, then the client's
    /// live toasts), then the toast itself: titled with the site, the words cleaned. `Err` is the trace
    /// text; whatever the limits admitted is forgotten again when the shell refuses.
    fn post_toast(
        &mut self,
        owner: ClientKey,
        id: u64,
        site: &Origin,
        title: &str,
        body: &str,
    ) -> Result<u64, &'static str> {
        let now = self.now_ms;
        let state = self
            .client_toasts
            .entry(owner)
            .or_insert_with(|| ClientToasts::new(now));
        if !state.pace.take(now) {
            return Err("toast limit: client rate");
        }
        match state.registry.admit(now, id, &site.key(), "", "") {
            Admit::Drop(dropped) => return Err(why(dropped)),
            // No tag reaches webd, so nothing is replaced in place.
            Admit::Post { .. } => {}
        }
        match self
            .outside
            .toast(&toast_title(site), &toast_body(title, body))
        {
            Some(toast) => {
                state.registry.posted(id, toast);
                Ok(toast)
            }
            None => {
                state.registry.failed(id);
                Err("toast refused")
            }
        }
    }

    /// The client's registry, if it has one.
    fn registry_of(&mut self, view: u64) -> Option<&mut Registry> {
        let owner = self.views.any_mut(view)?.owner;
        Some(&mut self.client_toasts.get_mut(&owner)?.registry)
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
        check_origin(&args.origin)?;
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
            self.publish_decisions();
        }
        self.engine
            .notify_permission(args.view, &args.origin, answer);
        Ok(())
    }

    /// `NotifyForget` (P4): the profile has no decision about `origin` any more, here and in the
    /// engine (whose record is the context's), so the page's next request is asked again.
    pub(super) fn notify_forget_call(
        &mut self,
        client: ClientKey,
        body: &[u8],
    ) -> Result<(), Refusal> {
        let args = wire::decode_notify_forget_args(body).map_err(|_| MALFORMED)?;
        check_origin(&args.origin)?;
        let view = live(&mut self.views, client, true, args.view)?;
        let rules = &view.full.as_ref().ok_or(MALFORMED)?.rules;
        if !rules.notifications {
            return Err(Refusal::new(
                errno::EACCES,
                "the view was not made with that",
            ));
        }
        if let Some(decisions) = self.permissions.get_mut(&rules.profile) {
            decisions.remove(&site_key(&args.origin));
        }
        self.publish_decisions();
        self.engine.notify_forget(args.view, &args.origin);
        Ok(())
    }

    /// Hands the engine the decisions a page process starts with. WebKit keeps one record for the
    /// whole context, so a site any profile allowed is allowed in it (webd still posts only what the
    /// view's own profile allowed, `show`); one only blocked is blocked.
    fn publish_decisions(&mut self) {
        let mut merged: BTreeMap<String, bool> = BTreeMap::new();
        for decisions in self.permissions.values() {
            for (site, allowed) in decisions {
                let entry = merged.entry(site.clone()).or_insert(false);
                *entry |= *allowed;
            }
        }
        self.queue
            .rules()
            .set_notify_decisions(merged.into_iter().collect());
    }

    /// What the shell said about webd's toasts: a click runs the page's handler and tells the client;
    /// a toast that went away is the page's `close`.
    pub(super) fn toast_news(&mut self) {
        self.expire_toasts();
        for news in self.outside.toast_news() {
            let toast = match news {
                ToastNews::Clicked { toast } | ToastNews::Closed { toast } => toast,
                ToastNews::Lost => {
                    self.forget_toasts();
                    continue;
                }
            };
            // The toast is spent, whoever's it was and whether or not its view still lives.
            for state in self.client_toasts.values_mut() {
                state.registry.toast_closed(toast);
            }
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
        for state in self.client_toasts.values_mut() {
            state.registry.forget_all();
        }
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

    /// Forget a toast the shell never reported closed (a toast lives 60 s at most), so a lost report
    /// cannot hold a client's allowance for good; and a client that has no views, nothing on screen and
    /// a full pace needs no entry.
    fn expire_toasts(&mut self) {
        let now = self.now_ms;
        let mut gone = Vec::new();
        for state in self.client_toasts.values_mut() {
            gone.extend(
                state
                    .registry
                    .expire(now)
                    .into_iter()
                    .filter_map(|n| n.toast),
            );
        }
        for toast in gone {
            for view in self.views.ids() {
                if let Some(full) = self.full_of(view) {
                    full.toasts.remove(&toast);
                }
            }
        }
        let views = &self.views;
        self.client_toasts.retain(|client, state| {
            views.count_of(*client) > 0 || !state.registry.is_empty() || !state.pace.is_full(now)
        });
    }
}
