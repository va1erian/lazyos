//! Reply error decoding and the errno values/help text shared by the fabric
//! panels.

use libmessenger::{Decoder, Kind, Parcel};

/// The first structured `ERROR` field of a reply, when it carries one.
pub(super) fn error_code(parcel: &Parcel) -> Option<i64> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Ok(Some(field)) = decoder.next() {
        if field.kind == Kind::Error {
            return field.error_parts().ok().map(|(code, _)| code as i64);
        }
    }
    None
}

/// No such handle or name.
pub(super) const ENOENT: i64 = 2;
/// The peer endpoint is gone.
pub(super) const EPIPE: i64 = 32;
/// Invalid argument.
pub(super) const EINVAL: i64 = 22;

/// A friendly one-line explanation of a negative errno returned by syscall 5.
pub fn errno_text(code: i64) -> String {
    let text = match -code {
        1 => "operation not permitted",
        2 => "not found",
        3 => "no such process",
        7 => "buffer too large",
        11 => "try again",
        12 => "out of memory",
        13 => "permission denied",
        14 => "bad address",
        22 => "invalid argument",
        32 => "peer closed",
        35 => "deadlock",
        110 => "timed out",
        125 => "canceled",
        _ => "unknown error",
    };
    format!("{} ({code})", text)
}
