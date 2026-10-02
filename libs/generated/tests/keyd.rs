//! Round-trip tests for the generated `os.lazy.keyd.v1` stubs (issue #285).

use messenger_generated::os_lazy_keyd_v1::*;

#[test]
fn verify_and_provision_roundtrip() {
    let args = VerifyArgs {
        user: "lazyos".into(),
        secret: "hunter2".into(),
    };
    assert_eq!(
        decode_verify_args(&encode_verify_args(&args).unwrap()).unwrap(),
        args
    );
    for ok in [true, false] {
        let reply = VerifyReply { ok };
        assert_eq!(
            decode_verify_reply(&encode_verify_reply(&reply).unwrap()).unwrap(),
            reply
        );
    }
    let provision = ProvisionArgs {
        user: "user".into(),
        secret: "pw".into(),
    };
    assert_eq!(
        decode_provision_args(&encode_provision_args(&provision).unwrap()).unwrap(),
        provision
    );
}

#[test]
fn bytes_payloads_roundtrip_including_empty_and_large() {
    for payload in [vec![], vec![0u8, 255, 7], vec![0xa5u8; 8 * 1024]] {
        let sign = SignArgs {
            key: 7,
            digest: payload.clone(),
        };
        assert_eq!(
            decode_sign_args(&encode_sign_args(&sign).unwrap()).unwrap(),
            sign
        );
        let wrap = WrapArgs {
            key: u64::MAX,
            plaintext: payload.clone(),
        };
        assert_eq!(
            decode_wrap_args(&encode_wrap_args(&wrap).unwrap()).unwrap(),
            wrap
        );
        let unwrap = UnwrapArgs {
            key: 1,
            blob: payload.clone(),
        };
        assert_eq!(
            decode_unwrap_args(&encode_unwrap_args(&unwrap).unwrap()).unwrap(),
            unwrap
        );
        let tag = SignReply {
            tag: payload.clone(),
        };
        assert_eq!(
            decode_sign_reply(&encode_sign_reply(&tag).unwrap()).unwrap(),
            tag
        );
        let blob = WrapReply {
            blob: payload.clone(),
        };
        assert_eq!(
            decode_wrap_reply(&encode_wrap_reply(&blob).unwrap()).unwrap(),
            blob
        );
        let plain = UnwrapReply {
            plaintext: payload.clone(),
        };
        assert_eq!(
            decode_unwrap_reply(&encode_unwrap_reply(&plain).unwrap()).unwrap(),
            plain
        );
        let random = RandomReply { bytes: payload };
        assert_eq!(
            decode_random_reply(&encode_random_reply(&random).unwrap()).unwrap(),
            random
        );
    }
}

#[test]
fn random_and_generate_roundtrip() {
    let args = RandomArgs { len: 32 };
    assert_eq!(
        decode_random_args(&encode_random_args(&args).unwrap()).unwrap(),
        args
    );
    let generate = GenerateArgs {
        kind: "hmac".into(),
    };
    assert_eq!(
        decode_generate_args(&encode_generate_args(&generate).unwrap()).unwrap(),
        generate
    );
    let reply = GenerateReply { id: 42 };
    assert_eq!(
        decode_generate_reply(&encode_generate_reply(&reply).unwrap()).unwrap(),
        reply
    );
}

#[test]
fn key_list_is_an_array_of_structs() {
    for count in [0usize, 1, 5] {
        let keys = (0..count as u64)
            .map(|i| KeyInfo {
                id: i + 1,
                kind: if i % 2 == 0 {
                    "hmac".into()
                } else {
                    "wrap".into()
                },
                uses: i * 3,
                last_use: i * 100,
            })
            .collect();
        let reply = ListReply { keys };
        assert_eq!(
            decode_list_reply(&encode_list_reply(&reply).unwrap()).unwrap(),
            reply
        );
    }
}

#[test]
fn decoders_ignore_unknown_fields_such_as_the_error_field() {
    let mut body = libmessenger::Encoder::new();
    body.error(15, 1, "denied").unwrap();
    assert_eq!(
        decode_sign_reply(&body.finish()).unwrap(),
        SignReply::default()
    );
}
