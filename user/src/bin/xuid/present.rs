//! Buffer slots and the pipelined present (issue #361): the compositor side
//! of `AttachBufferSlot`, `AttachBuffer` (slot 0, legacy) and the one-way
//! `Present`, with the `BufferRelease`/`FrameDone` events it answers with.
//!
//! The rules live in `surfbuf::SlotTable` (host- and kernel-tested); this
//! module maps them onto Messenger parcels, shared-buffer mappings and
//! repaints. `Surface::pixels`/`bytes` always mirror the *current* slot, so
//! the renderer never has to know slots exist.

use surfbuf::{clip_damage, Area};
use user::messenger::display::{self, wire, Rect};
use user::messenger::{self, Endpoint, Message};
use user::sys;

use super::compositor::Compositor;
use super::surface::Surface;

/// One shared buffer the compositor has mapped for a slot.
#[derive(Clone, Copy, Debug)]
pub(super) struct Mapping {
    /// Address in the compositor's address space.
    pub(super) va: u64,
    /// Bytes the compositor may read (the surface's `width * height * 4`).
    pub(super) bytes: u64,
    /// The compositor's handle for the buffer, closed to unmap it.
    pub(super) handle: u64,
}

impl Mapping {
    /// Unmap and drop the compositor's reference; the client keeps its own.
    fn unmap(self) {
        let _ = sys::display_close_buffer(self.handle);
    }
}

impl Surface {
    /// Point `pixels`/`bytes` at the current slot (or clear them).
    pub(super) fn sync_pixels(&mut self) {
        let (va, bytes) = self
            .slots
            .current()
            .map_or((0, 0), |mapping| (mapping.va, mapping.bytes));
        self.pixels = va;
        self.bytes = bytes;
    }

    /// Unmap every slot; the surface is going away.
    pub(super) fn release_buffers(&mut self) {
        for mapping in self.slots.take_all().into_iter().flatten() {
            mapping.unmap();
        }
        self.sync_pixels();
    }
}

/// Serve `AttachBuffer` (`slot == None`, the legacy slot-0 replace) or
/// `AttachBufferSlot`. On any refusal the buffer handle that arrived with the
/// request is closed, so a bad request cannot leak the compositor's handles.
pub(super) fn attach(
    message: &Message,
    surfaces: &mut [Surface],
    id: u64,
    slot: Option<u32>,
) -> Result<(), i64> {
    let result = try_attach(message, surfaces, id, slot);
    if result.is_err() && message.buffers != 0 {
        let _ = sys::display_close_buffer(message.first_buffer);
    }
    result
}

/// [`attach`] without the cleanup; errors are positive errno values.
fn try_attach(
    message: &Message,
    surfaces: &mut [Surface],
    id: u64,
    slot: Option<u32>,
) -> Result<(), i64> {
    let Some(surface) = surfaces.iter_mut().find(|surface| surface.id == id) else {
        return Err(messenger::errno::EINVAL);
    };
    if surface.owner != message.sender {
        // Only the surface's own client may attach its pixels (issue #176:
        // any caller that guessed the id could spoof another app's window).
        return Err(messenger::errno::EACCES);
    }
    // The descriptor's length is the sender's claim about how many bytes the
    // surface needs; never trust it to cover the geometry the compositor
    // paints. Checked `u64` arithmetic avoids the wrap a pathological
    // width/height could otherwise cause in the `i32` product (issue #176);
    // `CreateSurface` also bounds both to the screen size.
    let expected = (surface.w.max(0) as u64)
        .checked_mul(surface.h.max(0) as u64)
        .and_then(|area| area.checked_mul(4))
        .ok_or(messenger::errno::EINVAL)?;
    let claimed = message
        .parcel
        .buffers
        .first()
        .map_or(0, |buffer| buffer.len);
    if message.buffers == 0 || claimed < expected {
        return Err(messenger::errno::EINVAL);
    }
    let va = sys::display_map_buffer(message.first_buffer).map_err(|code| -code)?;
    let mapping = Mapping {
        va,
        bytes: expected,
        handle: message.first_buffer,
    };
    let replaced = match slot {
        Some(slot) => surface
            .slots
            .attach(slot, mapping)
            .map_err(|error| match error {
                surfbuf::AttachError::BadSlot => messenger::errno::EINVAL,
                surfbuf::AttachError::Busy => messenger::errno::EBUSY,
            })?,
        None => surface.slots.attach_legacy(mapping),
    };
    if let Some(old) = replaced {
        old.unmap();
    }
    surface.sync_pixels();
    Ok(())
}

impl Compositor {
    /// Serve a one-way `Present`: swap in the slot, composite the damage,
    /// then answer with `BufferRelease` (if the current slot changed) and
    /// `FrameDone`.
    ///
    /// There is no reply path, so a request the sender does not own or that
    /// does not decode is dropped silently; a refused slot changes nothing
    /// but still gets its `FrameDone`, so a client pacing on it cannot
    /// deadlock.
    pub(super) fn present(&mut self, message: &Message) {
        let Ok(args) = wire::decode_present_args(&message.parcel.body) else {
            return;
        };
        let Some(surface) = self
            .surfaces
            .iter_mut()
            .find(|surface| surface.id == args.surface)
        else {
            return;
        };
        if surface.owner != message.sender {
            return;
        }
        let events = Endpoint::from_raw(surface.events);
        let outcome = surface.slots.present(args.slot);
        if outcome.is_ok() {
            surface.sync_pixels();
        }
        // A minimized surface is not painted; the swap above is all it needs.
        let (rects, count) = if outcome.is_ok() && !surface.minimized {
            clip_damage(
                surface.w.max(0) as u32,
                surface.h.max(0) as u32,
                args.damage.iter().map(|rect| Area {
                    x: rect.x,
                    y: rect.y,
                    w: rect.w,
                    h: rect.h,
                }),
            )
        } else {
            ([Area::default(); surfbuf::MAX_DAMAGE], 0)
        };
        // A window's damage is relative to its content origin; the desktop
        // has no chrome, so its origin is the surface origin.
        let origin = if surface.desktop {
            (surface.x, surface.y)
        } else {
            let content = surface.content();
            (content.x, content.y)
        };
        for area in &rects[..count] {
            // Clipped to the surface, which is bounded by the screen, so
            // these fit `i32`.
            self.repaint(Rect::new(
                origin.0 + area.x as i32,
                origin.1 + area.y as i32,
                area.w as i32,
                area.h as i32,
            ));
        }
        if let Ok(Some(old)) = outcome {
            let _ = display::send_event(
                &events,
                &mut self.scratch,
                wire::METHOD_BUFFERRELEASE,
                wire::encode_buffer_release_args(&wire::BufferReleaseArgs {
                    surface: args.surface,
                    slot: old,
                }),
            );
        }
        let _ = display::send_event(
            &events,
            &mut self.scratch,
            wire::METHOD_FRAMEDONE,
            wire::encode_frame_done_args(&wire::FrameDoneArgs {
                surface: args.surface,
                seq: args.seq,
            }),
        );
    }
}
