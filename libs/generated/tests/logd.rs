//! Round-trip tests for the generated `os.lazy.logd.v1` stubs (issue #301).

use messenger_generated::os_lazy_logd_v1::*;

fn record(i: u64) -> LogRecord {
    LogRecord {
        seq: i + 1,
        tick: i * 7,
        topic: format!("system/events/service/svc{i}"),
        detail: format!("state=running pid={i}"),
        hash: 0xdead_beef_0000_0000 | u64::from(i as u32),
    }
}

#[test]
fn tail_count_option_present_and_absent() {
    for count in [None, Some(0u64), Some(10), Some(u64::MAX)] {
        let args = TailArgs { count };
        assert_eq!(
            decode_tail_args(&encode_tail_args(&args).unwrap()).unwrap(),
            args
        );
    }
}

#[test]
fn tail_reply_roundtrips_empty_and_many() {
    for count in [0usize, 1, 80] {
        let reply = TailReply {
            records: (0..count as u64).map(record).collect(),
        };
        let body = encode_tail_reply(&reply).unwrap();
        assert_eq!(decode_tail_reply(&body).unwrap(), reply);
    }
}

#[test]
fn count_and_verify_replies_roundtrip() {
    for count in [0u64, 1, u64::MAX] {
        let reply = CountReply { count };
        assert_eq!(
            decode_count_reply(&encode_count_reply(&reply).unwrap()).unwrap(),
            reply
        );
    }
    for (ok, index) in [(true, 0u64), (false, 3), (true, u64::MAX)] {
        let reply = VerifyReply { ok, index };
        assert_eq!(
            decode_verify_reply(&encode_verify_reply(&reply).unwrap()).unwrap(),
            reply
        );
    }
}

#[test]
fn truncated_bodies_are_rejected() {
    let reply = TailReply {
        records: vec![record(0), record(1)],
    };
    let body = encode_tail_reply(&reply).unwrap();
    assert!(decode_tail_reply(&body[..body.len() - 3]).is_err());
}

#[test]
fn unknown_trailing_fields_are_ignored() {
    let mut body = libmessenger::Encoder::new();
    body.option(1, None).unwrap();
    body.u64(99, 5).unwrap();
    let args = decode_tail_args(&body.finish()).unwrap();
    assert_eq!(args, TailArgs::default());

    let mut body = libmessenger::Encoder::new();
    body.u64(1, 12).unwrap();
    body.u64(99, 5).unwrap();
    assert_eq!(decode_count_reply(&body.finish()).unwrap().count, 12);
}

#[test]
fn error_field_is_ignored_by_decoders() {
    let mut body = libmessenger::Encoder::new();
    body.error(messenger_generated::errors::ERROR_FIELD, 2, "no store")
        .unwrap();
    assert_eq!(
        decode_tail_reply(&body.finish()).unwrap(),
        TailReply::default()
    );
}

#[test]
fn sources_and_tail_file_roundtrip() {
    for count in [0usize, 1, 40] {
        let reply = SourcesReply {
            sources: (0..count).map(|i| format!("svc{i}")).collect(),
        };
        let body = encode_sources_reply(&reply).unwrap();
        assert_eq!(decode_sources_reply(&body).unwrap(), reply);
    }
    for (source, count) in [("system", 0u64), ("kernel", 20), ("", u64::MAX)] {
        let args = TailFileArgs {
            source: source.into(),
            count,
        };
        let body = encode_tail_file_args(&args).unwrap();
        assert_eq!(decode_tail_file_args(&body).unwrap(), args);
    }
    let reply = TailFileReply {
        lines: vec!["1\t5\tsystem/health/summary\tstatus=ok\t00ff".into(); 3],
    };
    let body = encode_tail_file_reply(&reply).unwrap();
    assert_eq!(decode_tail_file_reply(&body).unwrap(), reply);
    assert_ne!(METHOD_SOURCES, METHOD_TAILFILE);
}
