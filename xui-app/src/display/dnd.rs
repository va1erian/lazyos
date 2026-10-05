//! Compositor-mediated drag and drop (issue #145, `os.lazy.display.v1`
//! methods 11-17): the `DragStart`/`DragCancel` calls a source makes and the
//! enter/over/leave/drop/ended events both ends receive.
//!
//! The payload never crosses the compositor: a source offers it to
//! `clipboardd` and hands `DragStart` the offer's token; the target pastes
//! the token it receives in `Drop` with its own credentials.

use libmessenger::Parcel;
use messenger_generated::os_lazy_display_v1 as wire;

use super::{request, Client};
use crate::sys::errno;

/// A drag-and-drop event on a surface's event endpoint.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DragEvent {
    /// A drag carrying `mime` entered the surface at `(x, y)`.
    Enter { x: i32, y: i32, mime: String },
    /// The drag moved inside the surface.
    Over { x: i32, y: i32 },
    /// The drag left the surface.
    Leave,
    /// The drag was released over the surface: paste `token` as `mime`.
    Drop {
        x: i32,
        y: i32,
        token: u64,
        mime: String,
    },
    /// To the source: its drag ended, `dropped` on a target or cancelled.
    Ended { dropped: bool },
}

/// Decode a drag event, or `None` when `parcel` is not a well-formed one.
pub fn decode_drag_event(parcel: &Parcel) -> Option<DragEvent> {
    let body = &parcel.body;
    Some(match parcel.header.method {
        wire::METHOD_DRAGENTER => {
            let args = wire::decode_drag_enter_args(body).ok()?;
            DragEvent::Enter {
                x: args.x,
                y: args.y,
                mime: args.mime,
            }
        }
        wire::METHOD_DRAGOVER => {
            let args = wire::decode_drag_over_args(body).ok()?;
            DragEvent::Over {
                x: args.x,
                y: args.y,
            }
        }
        wire::METHOD_DRAGLEAVE => DragEvent::Leave,
        wire::METHOD_DROP => {
            let args = wire::decode_drop_args(body).ok()?;
            DragEvent::Drop {
                x: args.x,
                y: args.y,
                token: args.token,
                mime: args.mime,
            }
        }
        wire::METHOD_DRAGENDED => DragEvent::Ended {
            dropped: wire::decode_drag_ended_args(body).ok()?.dropped,
        },
        _ => return None,
    })
}

/// The `DragStart` request parcel.
pub(super) fn drag_start_parcel(surface: u64, token: u64, mime: &str) -> Result<Parcel, i64> {
    let body = wire::encode_drag_start_args(&wire::DragStartArgs {
        surface,
        token,
        mime: mime.into(),
    })
    .map_err(|_| -errno::EINVAL)?;
    Ok(request(
        wire::METHOD_DRAGSTART,
        body,
        Vec::new(),
        Vec::new(),
    ))
}

impl Client {
    /// `DragStart`: hand the compositor the pointer for a drag of clipboard
    /// offer `token` (`mime`) from `surface`. Refused unless a pointer button
    /// is still held and this task created the surface.
    pub fn drag_start(&self, surface: u64, token: u64, mime: &str) -> Result<(), i64> {
        let parcel = drag_start_parcel(surface, token, mime)?;
        self.call(&parcel).map(|_| ())
    }

    /// `DragCancel`: abandon the drag that started at `surface`.
    pub fn drag_cancel(&self, surface: u64) -> Result<(), i64> {
        let body = wire::encode_drag_cancel_args(&wire::DragCancelArgs { surface })
            .map_err(|_| -errno::EINVAL)?;
        let parcel = request(wire::METHOD_DRAGCANCEL, body, Vec::new(), Vec::new());
        self.call(&parcel).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(method: u32, body: Vec<u8>) -> Parcel {
        request(method, body, Vec::new(), Vec::new())
    }

    #[test]
    fn drag_events_decode() {
        let enter = wire::encode_drag_enter_args(&wire::DragEnterArgs {
            x: 3,
            y: -4,
            mime: "text/uri-list".into(),
        })
        .unwrap();
        assert_eq!(
            decode_drag_event(&event(wire::METHOD_DRAGENTER, enter)),
            Some(DragEvent::Enter {
                x: 3,
                y: -4,
                mime: "text/uri-list".into()
            })
        );
        let drop = wire::encode_drop_args(&wire::DropArgs {
            x: 10,
            y: 20,
            token: 99,
            mime: "text/uri-list".into(),
        })
        .unwrap();
        assert_eq!(
            decode_drag_event(&event(wire::METHOD_DROP, drop)),
            Some(DragEvent::Drop {
                x: 10,
                y: 20,
                token: 99,
                mime: "text/uri-list".into()
            })
        );
        let ended = wire::encode_drag_ended_args(&wire::DragEndedArgs { dropped: true }).unwrap();
        assert_eq!(
            decode_drag_event(&event(wire::METHOD_DRAGENDED, ended)),
            Some(DragEvent::Ended { dropped: true })
        );
        assert_eq!(
            decode_drag_event(&event(wire::METHOD_DRAGLEAVE, Vec::new())),
            Some(DragEvent::Leave)
        );
    }

    #[test]
    fn other_methods_are_not_drag_events() {
        assert_eq!(
            decode_drag_event(&event(wire::METHOD_POINTERMOVE, Vec::new())),
            None
        );
    }

    #[test]
    fn the_start_parcel_carries_method_11() {
        let parcel = drag_start_parcel(5, 77, "text/uri-list").unwrap();
        assert_eq!(parcel.header.method, 11);
        let args = wire::decode_drag_start_args(&parcel.body).unwrap();
        assert_eq!(
            (args.surface, args.token, args.mime.as_str()),
            (5, 77, "text/uri-list")
        );
    }
}
