//! A client's side of `os.lazy.webd.light.v1` over the in-memory fabric, for the end-to-end and
//! fuzz tests: build a request with the generated encoders, hand it to the service with a chosen
//! kernel-stamped origin, decode the reply, and read the events the service sent.

#![allow(dead_code)]

use std::sync::Arc;

use libmessenger::{Buffer, Decoder, Header, Kind, Parcel, VERSION};
use messenger_generated::errors::ERROR_FIELD;
use messenger_generated::os_lazy_webd_light_v1 as wire;
use xui_webd::events::EventQueue;
use xui_webd::ident::Origin;
use xui_webd::mem::{MemFabric, Shared};
use xui_webd::recording::RecordingEngine;
use xui_webd::{Call, Service};

pub const SESSION: u64 = 1;

/// A caller of the package `label` (0: unlabelled), user 1000, in webd's session.
pub fn origin(label: u32) -> Origin {
    Origin {
        uid: 1000,
        label,
        session: SESSION,
        sender: 100 + u64::from(label),
    }
}

/// What a request came back with: the reply body, or the errno of its error field.
pub type Outcome = Result<Vec<u8>, i64>;

/// An event the service sent a client.
#[derive(Clone, Debug, PartialEq)]
pub enum Ev {
    Frame(wire::FrameArgs),
    State(wire::StateArgs),
    Scroll(wire::ScrollArgs),
    Link(wire::LinkClickedArgs),
    Clipboard(wire::ClipboardWriteArgs),
    Lost(wire::ViewLostArgs),
    Ping,
}

pub struct Rig {
    pub service: Service<RecordingEngine, MemFabric>,
    pub engine: RecordingEngine,
    pub fabric: MemFabric,
    pub queue: Arc<EventQueue>,
}

pub fn rig() -> Rig {
    let engine = RecordingEngine::new();
    let fabric = MemFabric::new();
    let queue = Arc::new(EventQueue::new());
    let service = Service::new(engine.clone(), fabric.clone(), queue.clone(), SESSION);
    Rig {
        service,
        engine,
        fabric,
        queue,
    }
}

pub fn parcel(interface: u64, method: u32, body: Vec<u8>) -> Parcel {
    Parcel {
        header: Header {
            version: VERSION,
            flags: 0,
            interface_id: interface,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body,
        objects: Vec::new(),
    }
}

/// The errno of a reply that is an error, or its body.
pub fn outcome(reply: &Parcel) -> Outcome {
    let mut decoder = Decoder::new(&reply.body);
    if let Ok(Some(field)) = decoder.next() {
        if field.kind == Kind::Error && field.id == ERROR_FIELD {
            return Err(i64::from(field.error_parts().unwrap().0));
        }
    }
    Ok(reply.body.clone())
}

pub fn decode_event(method: u32, body: &[u8]) -> Ev {
    match method {
        wire::METHOD_FRAME => Ev::Frame(wire::decode_frame_args(body).unwrap()),
        wire::METHOD_STATE => Ev::State(wire::decode_state_args(body).unwrap()),
        wire::METHOD_SCROLL => Ev::Scroll(wire::decode_scroll_args(body).unwrap()),
        wire::METHOD_LINKCLICKED => Ev::Link(wire::decode_link_clicked_args(body).unwrap()),
        wire::METHOD_CLIPBOARDWRITE => {
            Ev::Clipboard(wire::decode_clipboard_write_args(body).unwrap())
        }
        wire::METHOD_VIEWLOST => Ev::Lost(wire::decode_view_lost_args(body).unwrap()),
        wire::METHOD_PING => Ev::Ping,
        other => panic!("not an event: {other}"),
    }
}

impl Rig {
    /// Send a raw request.
    pub fn raw(
        &mut self,
        from: Origin,
        method: u32,
        body: Vec<u8>,
        objects: Vec<libmessenger::Object>,
    ) -> Outcome {
        let call = Call {
            origin: from,
            parcel: parcel(wire::INTERFACE_ID, method, body),
            objects,
        };
        outcome(&self.service.handle(call))
    }

    pub fn create(
        &mut self,
        from: Origin,
        config: wire::LightConfig,
        w: u32,
        h: u32,
    ) -> Result<(u64, u64), i64> {
        let channel = self.fabric.new_channel();
        let args = wire::CreateViewArgs {
            config,
            width: w,
            height: h,
            events: channel,
        };
        let (body, objects) = wire::encode_create_view_args(&args).unwrap();
        let body = self.raw(from, wire::METHOD_CREATEVIEW, body, objects)?;
        Ok((wire::decode_create_view_reply(&body).unwrap().view, channel))
    }

    pub fn create_plain(&mut self, from: Origin, w: u32, h: u32) -> (u64, u64) {
        self.create(from, wire::LightConfig::default(), w, h)
            .unwrap()
    }

    pub fn destroy(&mut self, from: Origin, view: u64) -> Outcome {
        let body = wire::encode_destroy_view_args(&wire::DestroyViewArgs { view }).unwrap();
        self.raw(from, wire::METHOD_DESTROYVIEW, body, vec![])
    }

    /// Attach a fresh buffer of `bytes` bytes as `slot`; the client's handle on its bytes comes back.
    pub fn attach_bytes(
        &mut self,
        from: Origin,
        view: u64,
        slot: u32,
        bytes: usize,
    ) -> (Outcome, Shared) {
        let (handle, shared) = self.fabric.new_buffer(bytes);
        (
            self.attach_handle(from, view, slot, handle, bytes as u64),
            shared,
        )
    }

    pub fn attach_handle(
        &mut self,
        from: Origin,
        view: u64,
        slot: u32,
        handle: u64,
        len: u64,
    ) -> Outcome {
        let args = wire::AttachSlotArgs {
            view,
            slot,
            pixels: Buffer::whole(handle, len),
        };
        let (body, objects) = wire::encode_attach_slot_args(&args).unwrap();
        self.raw(from, wire::METHOD_ATTACHSLOT, body, objects)
    }

    /// Attach a slot of exactly the view's size.
    pub fn attach(&mut self, from: Origin, view: u64, slot: u32, w: u32, h: u32) -> Shared {
        let (outcome, shared) = self.attach_bytes(from, view, slot, (w * h * 4) as usize);
        outcome.unwrap();
        shared
    }

    pub fn release(&mut self, from: Origin, view: u64, slot: u32) -> Outcome {
        let body = wire::encode_release_slot_args(&wire::ReleaseSlotArgs { view, slot }).unwrap();
        self.raw(from, wire::METHOD_RELEASESLOT, body, vec![])
    }

    pub fn resize(&mut self, from: Origin, view: u64, w: u32, h: u32) -> Outcome {
        let body = wire::encode_resize_args(&wire::ResizeArgs {
            view,
            width: w,
            height: h,
        })
        .unwrap();
        self.raw(from, wire::METHOD_RESIZE, body, vec![])
    }

    pub fn load_html(&mut self, from: Origin, view: u64, html: &[u8], base: &str) -> Outcome {
        let (handle, _) = self.fabric.new_buffer_with(html.to_vec());
        self.load_buffer(from, view, Buffer::whole(handle, html.len() as u64), base)
    }

    pub fn load_buffer(&mut self, from: Origin, view: u64, html: Buffer, base: &str) -> Outcome {
        let args = wire::LoadHtmlArgs {
            view,
            html,
            base_url: base.into(),
        };
        let (body, objects) = wire::encode_load_html_args(&args).unwrap();
        self.raw(from, wire::METHOD_LOADHTML, body, objects)
    }

    /// The events the service sent on `channel` since the last call.
    pub fn events(&mut self, channel: u64) -> Vec<Ev> {
        self.fabric
            .take_events(channel)
            .into_iter()
            .map(|(method, body)| decode_event(method, &body))
            .collect()
    }

    /// The engine paints a whole frame of one colour.
    pub fn paint(&mut self, view: u64, w: i32, h: i32, shade: u8) {
        use xui_webd::engine::EngineEvents;
        self.queue
            .frame(view, &vec![shade; (w * h * 4) as usize], w, h);
    }
}
