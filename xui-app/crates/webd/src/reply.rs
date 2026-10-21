//! Reply parcels: the same shape `xui_app::server` builds for the other services (this crate cannot
//! depend on xui-app), for `os.lazy.webd.light.v1`.

use libmessenger::{Encoder, Header, Parcel, VERSION};
use messenger_generated::errors::ERROR_FIELD;
use messenger_generated::os_lazy_webd_light_v1 as wire;

use crate::config::Refusal;

fn parcel(method: u32, body: Vec<u8>) -> Parcel {
    Parcel {
        header: Header {
            version: VERSION,
            flags: 0,
            interface_id: wire::INTERFACE_ID,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body,
        objects: Vec::new(),
    }
}

/// A successful reply carrying `body`.
pub fn ok(method: u32, body: Vec<u8>) -> Parcel {
    parcel(method, body)
}

/// A failed reply: the structured error field with the positive errno and a friendly text.
pub fn error(method: u32, refusal: Refusal) -> Parcel {
    let mut body = Encoder::new();
    // One bounded field into a fresh encoder cannot overflow.
    let _ = body.error(
        ERROR_FIELD,
        refusal.code.unsigned_abs() as u32,
        refusal.text,
    );
    parcel(method, body.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use libmessenger::{Decoder, Kind};

    #[test]
    fn an_error_reply_carries_the_positive_code_and_text() {
        let reply = error(3, Refusal::new(16, "slot is busy"));
        assert_eq!(
            (reply.header.interface_id, reply.header.method),
            (wire::INTERFACE_ID, 3)
        );
        let mut decoder = Decoder::new(&reply.body);
        let field = decoder.next().unwrap().unwrap();
        assert_eq!((field.kind, field.id), (Kind::Error, ERROR_FIELD));
        assert_eq!(field.error_parts().unwrap(), (16, "slot is busy"));
    }
}
