//! The standard reply error (`docs/midl.md`, "Errors").

use libmessenger::{Decoder, Encoder};
use messenger_generated::errors::{self, ReplyError, ERROR_FIELD};

#[test]
fn the_error_field_is_fifteen_everywhere() {
    // Every service used 15 before the id was generated; changing it is a
    // wire break across every interface at once.
    assert_eq!(ERROR_FIELD, 15);
}

#[test]
fn a_plain_error_is_a_code_and_a_message() {
    let body = errors::encode(&ReplyError::new(2, "no such path")).unwrap();
    let mut legacy = Encoder::new();
    legacy.error(15, 2, "no such path").unwrap();
    assert_eq!(
        body,
        legacy.finish(),
        "byte-identical to the pre-standard form"
    );
    assert_eq!(errors::find_code(&body).unwrap(), Some((2, "no such path")));
    assert_eq!(
        errors::find(&body).unwrap(),
        Some(ReplyError::new(2, "no such path"))
    );
}

#[test]
fn domain_hint_and_docs_round_trip() {
    let error = ReplyError::new(1, "denied")
        .with_domain("os.lazy.messenger")
        .with_hint("approve it in Settings")
        .with_docs("err.messenger.denied");
    let body = errors::encode(&error).unwrap();
    assert_eq!(errors::find(&body).unwrap(), Some(error));
    // A reader that only knows code and message still gets them.
    assert_eq!(errors::find_code(&body).unwrap(), Some((1, "denied")));
}

#[test]
fn a_success_reply_has_no_error_and_other_ids_are_not_it() {
    let mut body = Encoder::new();
    body.string(1, "value").unwrap();
    body.error(14, 9, "not the standard field").unwrap();
    let bytes = body.finish();
    assert_eq!(errors::find(&bytes).unwrap(), None);
    let field = Decoder::new(&bytes).next().unwrap().unwrap();
    assert!(!errors::is_error(&field));
}

#[test]
fn write_code_appends_after_other_fields() {
    let mut body = Encoder::new();
    body.u32(1, 7).unwrap();
    errors::write_code(&mut body, 22, "bad").unwrap();
    assert_eq!(
        errors::find_code(&body.finish()).unwrap(),
        Some((22, "bad"))
    );
}
