//! One function per call the two interfaces share. Each decodes, checks that the view is the caller's
//! and of the interface the call came through, validates, then acts on the table and the engine. A
//! view of another client, or a light view named through the full interface (and the other way), is
//! `ENOENT`, exactly like a view that never existed.

use libmessenger::Object;
use messenger_generated::os_lazy_webd_light_v1 as wire;

use super::{claim, Note, Service};
use crate::config::{self, Refusal};
use crate::engine::Engine;
use crate::fabric::{errno, Fabric};
use crate::fullconfig::Profile;
use crate::ident::ClientKey;
use crate::limits::{
    BAND_ROWS_MAX, HTML_BYTES_MAX, SLOTS_PER_VIEW, SLOTS_WITH_BANDS, SLOT_BYTES_PER_CLIENT,
    URL_BYTES_MAX, VIEWS_PER_CLIENT,
};
use crate::view::{View, Views};

pub(super) const MALFORMED: Refusal = Refusal::new(errno::EINVAL, "malformed request");
const NO_VIEW: Refusal = Refusal::new(errno::ENOENT, "no such view");
const LOST: Refusal = Refusal::new(errno::ENODEV, "the view was lost");

const TOO_MUCH: Refusal = Refusal::new(errno::EDQUOT, "too much slot memory");

pub(super) type Reply = Result<Vec<u8>, Refusal>;

/// Whether `held` bytes of slot memory plus `charge` more pass the client's quota (a sum that does
/// not fit in a `u64` does).
fn over_quota(held: u64, charge: u64) -> bool {
    held.checked_add(charge)
        .is_none_or(|total| total > SLOT_BYTES_PER_CLIENT)
}

/// The caller's view of the kind the call came through (`full`), still alive: `ENOENT` for one that
/// is not the caller's or of the other kind, `ENODEV` for one the engine gave up.
pub(super) fn live(
    views: &mut Views,
    client: ClientKey,
    full: bool,
    id: u64,
) -> Result<&mut View, Refusal> {
    let view = views
        .get_mut(client, id)
        .filter(|view| view.full.is_some() == full)
        .ok_or(NO_VIEW)?;
    if view.lost.is_some() {
        return Err(LOST);
    }
    Ok(view)
}

/// What a slot must hold at least: a frame of the view (slots 0 to 2), or a band as wide as the view
/// (the band slots of a full view with deltas). `None`: no such slot for this view.
fn slot_bytes(view: &View, slot: u32) -> Option<u64> {
    let slot = usize::try_from(slot).ok()?;
    let bands = view.full.as_ref().is_some_and(|full| full.rules.deltas);
    if slot < SLOTS_PER_VIEW {
        Some(config::frame_bytes(view.width, view.height))
    } else if bands && slot < SLOTS_WITH_BANDS {
        // The engine paints a band of at most a view's height (and never more than 1024 rows): a
        // slot for more would be memory the client's shared-buffer quota pays for nothing.
        Some(config::frame_bytes(
            view.width,
            view.height.min(BAND_ROWS_MAX),
        ))
    } else {
        None
    }
}

impl<E: Engine, F: Fabric> Service<E, F> {
    pub(super) fn create_view(
        &mut self,
        client: ClientKey,
        body: &[u8],
        pending: &mut Vec<Object>,
    ) -> Reply {
        let args = wire::decode_create_view_args(body, pending).map_err(|_| MALFORMED)?;
        let mut config = config::engine_config(&args.config, args.width, args.height, 0)?;
        if self.views.count_of(client) >= VIEWS_PER_CLIENT {
            return Err(Refusal::new(errno::ENOSPC, "too many views"));
        }
        config.group = self
            .groups
            .group_of(client)
            .ok_or(Refusal::new(errno::ENOSPC, "no web process group left"))?;
        let view = View::new(client, args.events, args.width, args.height);
        let id = self
            .views
            .insert(view)
            .ok_or(Refusal::new(errno::ENOSPC, "no view id left"))?;
        // The endpoint is the view's from here on: closed when the view ends.
        claim(pending, Object::Channel(args.events));
        self.engine.view_create(id, &config);
        self.note(Note::ViewCreated {
            view: id,
            client,
            width: args.width,
            height: args.height,
            group: config.group,
        });
        wire::encode_create_view_reply(&wire::CreateViewReply { view: id }).map_err(|_| MALFORMED)
    }

    pub(super) fn destroy_view(&mut self, client: ClientKey, full: bool, body: &[u8]) -> Reply {
        let args = wire::decode_destroy_view_args(body).map_err(|_| MALFORMED)?;
        // Gone, never made and someone else's all succeed: the answer must not tell them apart.
        let mine = self
            .views
            .get(client, args.view)
            .is_some_and(|view| view.full.is_some() == full);
        if mine {
            self.drop_view(args.view, "destroyed");
        }
        Ok(Vec::new())
    }

    pub(super) fn attach_slot(
        &mut self,
        client: ClientKey,
        full: bool,
        body: &[u8],
        pending: &mut Vec<Object>,
    ) -> Reply {
        let args = wire::decode_attach_slot_args(body, pending).map_err(|_| MALFORMED)?;
        let used = self.views.slot_bytes_of(client);
        let view = live(&mut self.views, client, full, args.view)?;
        let needed =
            slot_bytes(view, args.slot).ok_or(Refusal::new(errno::EINVAL, "no such slot"))?;
        let slot = args.slot as usize;
        let buffer = args.pixels;
        if buffer.end().is_none() || buffer.len < needed {
            return Err(Refusal::new(errno::EINVAL, "slot is smaller than the view"));
        }
        if view.slots.is_busy(slot) {
            return Err(Refusal::new(errno::EBUSY, "slot is busy"));
        }
        // What the client holds besides the slot this one replaces. A declared range never exceeds
        // its buffer, so a client over its quota on that alone is refused before anything is mapped.
        let held = used.saturating_sub(view.slots.bytes_of(slot));
        if over_quota(held, buffer.len) {
            return Err(TOO_MUCH);
        }
        // `map` takes the handle over, also when it fails.
        claim(pending, Object::Buffer(buffer.handle));
        let mapping = self
            .fabric
            .map(buffer)
            .map_err(|_| Refusal::new(errno::EINVAL, "slot buffer cannot be mapped"))?;
        if (mapping.capacity() as u64) < needed {
            return Err(Refusal::new(errno::EINVAL, "slot is smaller than the view"));
        }
        // The whole buffer is mapped whatever range the client declared, so that is the charge;
        // refused here, the mapping drops and the handle is closed.
        let charge = mapping.mapped_bytes();
        if over_quota(held, charge) {
            return Err(TOO_MUCH);
        }
        // Not busy and in range: the attach cannot fail.
        let _ = view.slots.attach(slot, mapping, charge);
        let wanted = view.should_refresh();
        if wanted {
            self.engine.refresh(args.view);
        }
        Ok(Vec::new())
    }

    pub(super) fn release_slot(&mut self, client: ClientKey, full: bool, body: &[u8]) -> Reply {
        let args = wire::decode_release_slot_args(body).map_err(|_| MALFORMED)?;
        let view = live(&mut self.views, client, full, args.view)?;
        if let Ok(slot) = usize::try_from(args.slot) {
            view.slots.release(slot);
        }
        // A frame dropped while every slot was busy is asked for again now that one is free.
        if view.should_refresh() {
            self.engine.refresh(args.view);
        }
        Ok(Vec::new())
    }

    pub(super) fn resize(&mut self, client: ClientKey, full: bool, body: &[u8]) -> Reply {
        let args = wire::decode_resize_args(body).map_err(|_| MALFORMED)?;
        let (width, height) = config::check_size(args.width, args.height)?;
        let view = live(&mut self.views, client, full, args.view)?;
        view.width = args.width;
        view.height = args.height;
        // Every slot was sized for the old picture; the client attaches new ones.
        view.slots.detach_all();
        view.want_frame();
        self.engine.resize(args.view, width, height);
        Ok(Vec::new())
    }

    pub(super) fn set_visible(&mut self, client: ClientKey, full: bool, body: &[u8]) -> Reply {
        let args = wire::decode_set_visible_args(body).map_err(|_| MALFORMED)?;
        let view = live(&mut self.views, client, full, args.view)?;
        view.visible = args.visible;
        self.engine.set_visible(args.view, args.visible);
        Ok(Vec::new())
    }

    pub(super) fn refresh(&mut self, client: ClientKey, full: bool, body: &[u8]) -> Reply {
        let args = wire::decode_refresh_args(body).map_err(|_| MALFORMED)?;
        let view = live(&mut self.views, client, full, args.view)?;
        // The client wants a picture it can show: if it is waiting for one already, this is it.
        view.want_frame();
        if view.should_refresh() {
            self.engine.refresh(args.view);
        }
        Ok(Vec::new())
    }

    pub(super) fn load_html(
        &mut self,
        client: ClientKey,
        full: bool,
        body: &[u8],
        pending: &mut Vec<Object>,
    ) -> Reply {
        let args = wire::decode_load_html_args(body, pending).map_err(|_| MALFORMED)?;
        let view = live(&mut self.views, client, full, args.view)?;
        if args.html.len > HTML_BYTES_MAX {
            return Err(Refusal::new(errno::E2BIG, "page is too large"));
        }
        let base_url = match &view.full {
            // A full view's page may sit at an address its policy lets it load (relative addresses
            // resolve against it); never at a file.
            Some(full_view) => {
                let base = if args.base_url.is_empty() {
                    "about:blank"
                } else {
                    args.base_url.as_str()
                };
                // The client's own markup at a site's address in the person's browsing profile would run
                // as that site with its cookies and storage: only the app's own profile may do it.
                let shared = matches!(full_view.rules.profile, Profile::Browser { .. });
                let network = crate::policy::kind(base) == crate::policy::Kind::Network;
                if base.len() > URL_BYTES_MAX
                    || !config::c_string_safe(base)
                    || !crate::policy::may_be_page_base(base)
                    || !full_view.rules.policy.client_may_load(base)
                    || (shared && network)
                {
                    return Err(Refusal::new(
                        errno::EACCES,
                        "the view may not load that base URL",
                    ));
                }
                base.to_owned()
            }
            None => config::check_base_url(&args.base_url)?.to_owned(),
        };
        // `read` takes the handle over, also when it fails.
        claim(pending, Object::Buffer(args.html.handle));
        let bytes = self
            .fabric
            .read(args.html)
            .map_err(|_| Refusal::new(errno::EINVAL, "page buffer cannot be read"))?;
        // The engine takes UTF-8; bytes that are not become U+FFFD instead of a refused page.
        let html = String::from_utf8_lossy(&bytes);
        self.engine.load_html(args.view, html.as_bytes(), &base_url);
        Ok(Vec::new())
    }

    pub(super) fn mouse(&mut self, client: ClientKey, full: bool, body: &[u8]) -> Reply {
        let args = wire::decode_mouse_args(body).map_err(|_| MALFORMED)?;
        let kind = i32::try_from(args.kind)
            .ok()
            .filter(|kind| (0..=2).contains(kind))
            .ok_or(MALFORMED)?;
        if !(-1..=2).contains(&args.button) {
            return Err(MALFORMED);
        }
        live(&mut self.views, client, full, args.view)?;
        // A press or a release is real input: the page may hand an address over shortly after it.
        if full && kind != wire::MOUSE_KIND_MOVE as i32 {
            self.stamp_input(args.view);
        }
        self.engine.mouse(
            args.view,
            kind,
            args.button,
            args.x,
            args.y,
            args.mods & MODS,
        );
        Ok(Vec::new())
    }

    pub(super) fn wheel(&mut self, client: ClientKey, full: bool, body: &[u8]) -> Reply {
        let args = wire::decode_wheel_args(body).map_err(|_| MALFORMED)?;
        live(&mut self.views, client, full, args.view)?;
        self.engine.wheel(
            args.view,
            args.x,
            args.y,
            args.ticks_x,
            args.ticks_y,
            args.mods & MODS,
        );
        Ok(Vec::new())
    }

    pub(super) fn key(&mut self, client: ClientKey, full: bool, body: &[u8]) -> Reply {
        let args = wire::decode_key_args(body).map_err(|_| MALFORMED)?;
        // The text travels as a C string.
        if !config::c_string_safe(&args.text) {
            return Err(MALFORMED);
        }
        live(&mut self.views, client, full, args.view)?;
        if full && args.press {
            self.stamp_input(args.view);
        }
        self.engine.key(
            args.view,
            args.press,
            args.keysym,
            &args.text,
            args.mods & MODS,
        );
        Ok(Vec::new())
    }

    pub(super) fn focus(&mut self, client: ClientKey, full: bool, body: &[u8]) -> Reply {
        let args = wire::decode_focus_args(body).map_err(|_| MALFORMED)?;
        live(&mut self.views, client, full, args.view)?;
        self.engine.focus(args.view, args.focused);
        Ok(Vec::new())
    }

    /// Real input reached a full view now (the queue's clock, the one its decisions read).
    fn stamp_input(&mut self, view: u64) {
        let now = self.queue.now_ms();
        self.queue.rules().input(view, now);
    }
}

/// shift, control, alt, meta: the bits the engine knows.
const MODS: u32 = 0xF;
