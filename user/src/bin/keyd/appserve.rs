//! The `os.lazy.keyd.appsecret.v1` service half of `keyd`: who is calling, and
//! the four methods (`appsecrets.rs` is their table and disk).

use alloc::string::String;
use alloc::vec::Vec;
use user::messenger::keyd::appsecret;
use user::messenger::{self, errno, keyd as wire, Error, Message, Parcel};
use user::sys;

use crate::appsecrets::AppSecrets;
use crate::{bounded_bytes, bounded_text};

/// Serve `os.lazy.keyd.appsecret.v1`: every call is for the caller's own
/// `(uid, installed-app label)`, resolved from its kernel-stamped credentials
/// ([`app_caller`]); no name in a request can reach another app's secrets.
pub(crate) fn dispatch_app(
    entropy: &mut lazyos_crypto::rng::Entropy,
    secrets: &mut AppSecrets,
    message: &Message,
) -> messenger::Result<Parcel> {
    use appsecret::wire as app;
    let (uid, label) = app_caller(message)?;
    let app_id = secretstore::app_of_label(&label).ok_or(Error::Errno(-errno::EPERM))?;
    let body = &message.parcel.body;
    let parse = Error::Parcel;
    let frame =
        |method: u32, body: Result<Vec<u8>, libmessenger::Error>| -> messenger::Result<Parcel> {
            Ok(wire::parcel_on(
                appsecret::INTERFACE,
                method,
                body.map_err(Error::Parcel)?,
            ))
        };
    match message.method() {
        app::METHOD_PUT => {
            let args = app::decode_put_args(body).map_err(parse)?;
            let name = bounded_text(args.name)?;
            let secret = bounded_bytes(args.secret, secretstore::APP_MAX_SECRET)?;
            secrets.put(entropy, uid, app_id, &name, &secret)?;
            Ok(wire::ok_reply_on(appsecret::INTERFACE, app::METHOD_PUT))
        }
        app::METHOD_GET => {
            let args = app::decode_get_args(body).map_err(parse)?;
            let secret = secrets.get(uid, app_id, &bounded_text(args.name)?)?;
            frame(
                app::METHOD_GET,
                app::encode_get_reply(&app::GetReply { secret }),
            )
        }
        app::METHOD_DELETE => {
            let args = app::decode_delete_args(body).map_err(parse)?;
            secrets.delete(entropy, uid, app_id, &bounded_text(args.name)?)?;
            Ok(wire::ok_reply_on(appsecret::INTERFACE, app::METHOD_DELETE))
        }
        app::METHOD_LIST => frame(
            app::METHOD_LIST,
            app::encode_list_reply(&app::ListReply {
                names: secrets.names(uid, app_id),
            }),
        ),
        _ => Err(Error::Errno(-errno::EINVAL)),
    }
}

/// The caller's uid and installed-app label, from its kernel-stamped
/// credentials. An unlabelled caller (a system service, the Terminal) is
/// refused: it has no app, so nothing of its could be scoped. A label the
/// kernel cannot name is refused too, never guessed.
fn app_caller(message: &Message) -> messenger::Result<(u32, String)> {
    let caller = message.caller();
    if caller.label_id == 0 {
        return Err(Error::Errno(-errno::EPERM));
    }
    let mut name = [0u8; sys::MAX_LABEL_BYTES];
    let len =
        sys::label_name(caller.label_id, &mut name).map_err(|_| Error::Errno(-errno::EPERM))?;
    let label = core::str::from_utf8(&name[..len]).map_err(|_| Error::Errno(-errno::EPERM))?;
    Ok((caller.uid, String::from(label)))
}
