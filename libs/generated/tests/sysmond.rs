//! Round-trip tests for the generated `os.lazy.sysmond.v1` stubs (issue #301).

use messenger_generated::os_lazy_sysmond_v1::*;

#[test]
fn snapshot_reply_roundtrips_empty_and_large() {
    let snapshot = [0u8; 8 * 1024];
    for data in [Vec::new(), vec![0u8, 255, 7], snapshot.to_vec()] {
        let reply = SnapshotReply { data };
        let body = encode_snapshot_reply(&reply).unwrap();
        assert_eq!(decode_snapshot_reply(&body).unwrap(), reply);
    }
}

#[test]
fn truncated_body_is_rejected() {
    let reply = SnapshotReply {
        data: vec![0xabu8; 64],
    };
    let body = encode_snapshot_reply(&reply).unwrap();
    assert!(decode_snapshot_reply(&body[..body.len() - 3]).is_err());
}

#[test]
fn unknown_trailing_fields_are_ignored() {
    let mut body = libmessenger::Encoder::new();
    body.bytes(1, &[1, 2, 3]).unwrap();
    body.u64(99, 5).unwrap();
    assert_eq!(
        decode_snapshot_reply(&body.finish()).unwrap().data,
        vec![1, 2, 3]
    );
}

#[test]
fn error_field_is_ignored_by_decoders() {
    let mut body = libmessenger::Encoder::new();
    body.error(15, 5, "no stats").unwrap();
    assert_eq!(
        decode_snapshot_reply(&body.finish()).unwrap(),
        SnapshotReply::default()
    );
}
