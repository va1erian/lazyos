//! Round-trip tests for the generated `os.lazy.healthd.v1` stubs (issue #301).

use messenger_generated::os_lazy_healthd_v1::*;

fn record(i: u64) -> HealthRecord {
    HealthRecord {
        name: format!("svc{i}"),
        status: if i.is_multiple_of(3) {
            "ok"
        } else {
            "degraded"
        }
        .into(),
        detail: format!("pid={i} restarts=0"),
        tick: i * 7,
    }
}

#[test]
fn report_args_roundtrip() {
    let args = ReportArgs {
        name: "flaky".into(),
        status: "ok".into(),
        detail: "recovered".into(),
    };
    assert_eq!(
        decode_report_args(&encode_report_args(&args).unwrap()).unwrap(),
        args
    );
}

#[test]
fn report_and_status_replies_roundtrip_empty_and_many() {
    for count in [0usize, 1, 80] {
        let records = (0..count as u64).map(record).collect::<Vec<_>>();
        let summary = HealthRecord {
            name: "summary".into(),
            status: "ok".into(),
            detail: format!("{count}/{count} services ok"),
            tick: 1234,
        };
        let report = ReportReply {
            summary: summary.clone(),
            records: records.clone(),
        };
        let body = encode_report_reply(&report).unwrap();
        assert_eq!(decode_report_reply(&body).unwrap(), report);

        let status = StatusReply { summary, records };
        let body = encode_status_reply(&status).unwrap();
        assert_eq!(decode_status_reply(&body).unwrap(), status);
    }
}

#[test]
fn truncated_bodies_are_rejected() {
    let reply = StatusReply {
        summary: record(0),
        records: vec![record(1), record(2)],
    };
    let body = encode_status_reply(&reply).unwrap();
    assert!(decode_status_reply(&body[..body.len() - 3]).is_err());
}

#[test]
fn unknown_trailing_fields_are_ignored() {
    let mut body = libmessenger::Encoder::new();
    body.string(1, "svc").unwrap();
    body.string(2, "ok").unwrap();
    body.string(3, "detail").unwrap();
    body.u64(4, 9).unwrap();
    body.u64(99, 7).unwrap();
    let decoded = decode_health_record(&body.finish()).unwrap();
    assert_eq!(decoded, record_with("svc", "ok", "detail", 9));
}

fn record_with(name: &str, status: &str, detail: &str, tick: u64) -> HealthRecord {
    HealthRecord {
        name: name.into(),
        status: status.into(),
        detail: detail.into(),
        tick,
    }
}

#[test]
fn error_field_is_ignored_by_decoders() {
    let mut body = libmessenger::Encoder::new();
    body.error(15, 1, "down").unwrap();
    assert_eq!(
        decode_status_reply(&body.finish()).unwrap(),
        StatusReply::default()
    );
}

#[test]
fn health_topic_payloads_roundtrip_and_reject_malformed() {
    let row = record(2);
    let body = encode_system_health(&row).unwrap();
    assert_eq!(decode_system_health(&body).unwrap(), row);
    assert!(decode_system_health(&body[..body.len() - 2]).is_err());
    assert!(decode_system_health(&[0xff, 0xff, 0xff, 0xff]).is_err());
    assert!(decode_system_health(&[0x01]).is_err());

    let body = encode_system_health_summary(&row).unwrap();
    assert_eq!(decode_system_health_summary(&body).unwrap(), row);
    assert!(decode_system_health_summary(&[0x01]).is_err());
}

#[test]
fn health_topic_patterns_and_qos_are_declared() {
    assert_eq!(TOPIC_SYSTEM_HEALTH, "system/health/+");
    assert_eq!(TOPIC_SYSTEM_HEALTH_SUMMARY, "system/health/summary");
    assert_eq!(
        TOPIC_SYSTEM_HEALTH_QOS,
        messenger_generated::topics::QOS_LATEST
    );
    assert_eq!(
        TOPIC_SYSTEM_HEALTH_SUMMARY_QOS,
        messenger_generated::topics::QOS_LATEST
    );
}
