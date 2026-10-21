//! One function per client call. Each decodes, checks that the view is the caller's, validates, then
//! acts on the table and the engine. A view of another client is `ENOENT`, exactly like a view
//! that never existed.

use libmessenger::Object;
use messenger_generated::os_lazy_webd_light_v1 as wire;

use super::{claim, Note, Service};
use crate::config::{self, Refusal};
use crate::engine::Engine;
use crate::fabric::{errno, Fabric};
use crate::ident::ClientKey;
use crate::limits::{HTML_BYTES_MAX, SLOTS_PER_VIEW, SLOT_BYTES_PER_CLIENT, VIEWS_PER_CLIENT};
use crate::view::{View, Views};

const MALFORMED: Refusal = Refusal::new(errno::EINVAL, "malformed request");
const NO_VIEW: Refusal = Refusal::new(errno::ENOENT, "no such view");
const LOST: Refusal = Refusal::new(errno::ENODEV, "the view was lost");

type Reply = Result<Vec<u8>, Refusal>;

/// The caller's view that is still alive: `ENOENT` for one that is not the caller's, `ENODEV`
/// for one the engine gave up.
fn live(views: &mut Views, client: ClientKey, id: u64) -> Result<&mut View, Refusal> {
    let view = views.get_mut(client, id).ok_or(NO_VIEW)?;
    if view.lost.is_some() {
        return Err(LOST);
    }
    Ok(view)
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

    pub(super) fn destroy_view(&mut self, client: ClientKey, body: &[u8]) -> Reply {
        let args = wire::decode_destroy_view_args(body).map_err(|_| MALFORMED)?;
        // Gone, never made and someone else's all succeed: the answer must not tell them apart.
        if self.views.get(client, args.view).is_some() {
            self.drop_view(args.view, "destroyed");
        }
        Ok(Vec::new())
    }

    pub(super) fn attach_slot(
        &mut self,
        client: ClientKey,
        body: &[u8],
        pending: &mut Vec<Object>,
    ) -> Reply {
        let args = wire::decode_attach_slot_args(body, pending).map_err(|_| MALFORMED)?;
        let slot = usize::try_from(args.slot)
            .ok()
            .filter(|slot| *slot < SLOTS_PER_VIEW)
            .ok_or(Refusal::new(errno::EINVAL, "no such slot"))?;
        let used = self.views.slot_bytes_of(client);
        let view = live(&mut self.views, client, args.view)?;
        let buffer = args.pixels;
        let needed = config::frame_bytes(view.width, view.height);
        if buffer.end().is_none() || buffer.len < needed {
            return Err(Refusal::new(errno::EINVAL, "slot is smaller than the view"));
        }
        if view.slots.is_busy(slot) {
            return Err(Refusal::new(errno::EBUSY, "slot is busy"));
        }
        let replaced = view.slots.bytes_of(slot);
        if used - replaced + buffer.len > SLOT_BYTES_PER_CLIENT {
            return Err(Refusal::new(errno::EDQUOT, "too much slot memory"));
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
        // Not busy and in range: the attach cannot fail.
        let _ = view.slots.attach(slot, mapping, buffer.len);
        let wanted = view.should_refresh();
        if wanted {
            self.engine.refresh(args.view);
        }
        Ok(Vec::new())
    }

    pub(super) fn release_slot(&mut self, client: ClientKey, body: &[u8]) -> Reply {
        let args = wire::decode_release_slot_args(body).map_err(|_| MALFORMED)?;
        let view = live(&mut self.views, client, args.view)?;
        if let Ok(slot) = usize::try_from(args.slot) {
            view.slots.release(slot);
        }
        // A frame dropped while every slot was busy is asked for again now that one is free.
        if view.should_refresh() {
            self.engine.refresh(args.view);
        }
        Ok(Vec::new())
    }

    pub(super) fn resize(&mut self, client: ClientKey, body: &[u8]) -> Reply {
        let args = wire::decode_resize_args(body).map_err(|_| MALFORMED)?;
        let (width, height) = config::check_size(args.width, args.height)?;
        let view = live(&mut self.views, client, args.view)?;
        view.width = args.width;
        view.height = args.height;
        // Every slot was sized for the old picture; the client attaches new ones.
        view.slots.detach_all();
        view.want_frame();
        self.engine.resize(args.view, width, height);
        Ok(Vec::new())
    }

    pub(super) fn set_visible(&mut self, client: ClientKey, body: &[u8]) -> Reply {
        let args = wire::decode_set_visible_args(body).map_err(|_| MALFORMED)?;
        let view = live(&mut self.views, client, args.view)?;
        view.visible = args.visible;
        self.engine.set_visible(args.view, args.visible);
        Ok(Vec::new())
    }

    pub(super) fn refresh(&mut self, client: ClientKey, body: &[u8]) -> Reply {
        let args = wire::decode_refresh_args(body).map_err(|_| MALFORMED)?;
        let view = live(&mut self.views, client, args.view)?;
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
        body: &[u8],
        pending: &mut Vec<Object>,
    ) -> Reply {
        let args = wire::decode_load_html_args(body, pending).map_err(|_| MALFORMED)?;
        live(&mut self.views, client, args.view)?;
        if args.html.len > HTML_BYTES_MAX {
            return Err(Refusal::new(errno::E2BIG, "page is too large"));
        }
        let base_url = config::check_base_url(&args.base_url)?;
        // `read` takes the handle over, also when it fails.
        claim(pending, Object::Buffer(args.html.handle));
        let bytes = self
            .fabric
            .read(args.html)
            .map_err(|_| Refusal::new(errno::EINVAL, "page buffer cannot be read"))?;
        // The engine takes UTF-8; bytes that are not become U+FFFD instead of a refused page.
        let html = String::from_utf8_lossy(&bytes);
        self.engine.load_html(args.view, html.as_bytes(), base_url);
        Ok(Vec::new())
    }

    pub(super) fn mouse(&mut self, client: ClientKey, body: &[u8]) -> Reply {
        let args = wire::decode_mouse_args(body).map_err(|_| MALFORMED)?;
        let kind = i32::try_from(args.kind)
            .ok()
            .filter(|kind| (0..=2).contains(kind))
            .ok_or(MALFORMED)?;
        if !(0..=2).contains(&args.button) {
            return Err(MALFORMED);
        }
        live(&mut self.views, client, args.view)?;
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

    pub(super) fn wheel(&mut self, client: ClientKey, body: &[u8]) -> Reply {
        let args = wire::decode_wheel_args(body).map_err(|_| MALFORMED)?;
        live(&mut self.views, client, args.view)?;
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

    pub(super) fn key(&mut self, client: ClientKey, body: &[u8]) -> Reply {
        let args = wire::decode_key_args(body).map_err(|_| MALFORMED)?;
        // The text travels as a C string.
        if !config::c_string_safe(&args.text) {
            return Err(MALFORMED);
        }
        live(&mut self.views, client, args.view)?;
        self.engine.key(
            args.view,
            args.press,
            args.keysym,
            &args.text,
            args.mods & MODS,
        );
        Ok(Vec::new())
    }

    pub(super) fn focus(&mut self, client: ClientKey, body: &[u8]) -> Reply {
        let args = wire::decode_focus_args(body).map_err(|_| MALFORMED)?;
        live(&mut self.views, client, args.view)?;
        self.engine.focus(args.view, args.focused);
        Ok(Vec::new())
    }
}

/// shift, control, alt, meta: the bits the engine knows.
const MODS: u32 = 0xF;
