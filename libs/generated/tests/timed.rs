//! Round-trip tests for the generated `os.lazy.timed.v1` stubs (issue #369).

use messenger_generated::os_lazy_timed_v1::*;

#[test]
fn now_reply_roundtrips_signed_offsets() {
    for (unix_ms, offset, dst) in [
        (0i64, 0i32, false),
        (1_790_714_517_000, 7200, true),
        (1_767_225_600_123, -18_000, false),
        (i64::MAX, i32::MIN, true),
    ] {
        let reply = NowReply {
            unix_ms,
            tz_offset_s: offset,
            tz_name: String::from("America/New_York"),
            dst,
        };
        let body = encode_now_reply(&reply).unwrap();
        assert_eq!(decode_now_reply(&body).unwrap(), reply);
    }
}

#[test]
fn tick_roundtrips() {
    let tick = Tick {
        unix: 1_790_714_400,
        offset: 19_800,
        zone_name: String::from("Asia/Kolkata"),
    };
    let body = encode_time_tick(&tick).unwrap();
    assert_eq!(decode_time_tick(&body).unwrap(), tick);
    // The `time/tick` pattern has no wildcards, so its concrete name is the
    // declared pattern itself.
    assert_eq!(name_time_tick().unwrap(), "time/tick");
}

#[test]
fn set_args_roundtrip() {
    let zone = SetZoneArgs {
        name: String::from("Europe/Paris"),
    };
    assert_eq!(
        decode_set_zone_args(&encode_set_zone_args(&zone).unwrap()).unwrap(),
        zone
    );
    for unix_secs in [0i64, -1, 1_790_714_517, i64::MAX] {
        let time = SetTimeArgs { unix_secs };
        assert_eq!(
            decode_set_time_args(&encode_set_time_args(&time).unwrap()).unwrap(),
            time
        );
    }
}

#[test]
fn truncated_bodies_are_rejected() {
    let body = encode_now_reply(&NowReply {
        unix_ms: 5,
        tz_offset_s: 3600,
        tz_name: String::from("Europe/Paris"),
        dst: false,
    })
    .unwrap();
    assert!(decode_now_reply(&body[..body.len() - 3]).is_err());
}
