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
    body.error(messenger_generated::errors::ERROR_FIELD, 5, "no stats")
        .unwrap();
    assert_eq!(
        decode_snapshot_reply(&body.finish()).unwrap(),
        SnapshotReply::default()
    );
}

fn memory() -> MemoryStats {
    MemoryStats {
        ticks: 1_000,
        frames_total: 65_536,
        frames_live: 512,
        frames_free: 65_024,
        slab_live: 4_096,
        slab_peak: 8_192,
        heap_used: 1_024,
        heap_total: 262_144,
    }
}

fn task_row(pid: u64) -> TaskRow {
    TaskRow {
        pid,
        ppid: 0,
        state: String::from("run"),
        wait: String::new(),
        class: String::from("norm"),
        cpu: pid * 3,
        name: String::from("init"),
    }
}

#[test]
fn memory_stats_roundtrips() {
    let decoded = decode_memory_stats(&encode_memory_stats(&memory()).unwrap()).unwrap();
    assert_eq!(decoded, memory());
}

#[test]
fn tasks_stats_roundtrips_empty_and_many_rows() {
    for tasks in [
        Vec::new(),
        vec![task_row(1)],
        (1..=64).map(task_row).collect::<Vec<_>>(),
    ] {
        let stats = TasksStats {
            live: tasks.len() as u64,
            tasks,
        };
        let decoded = decode_tasks_stats(&encode_tasks_stats(&stats).unwrap()).unwrap();
        assert_eq!(decoded, stats);
    }
}

#[test]
fn malformed_stats_payloads_are_rejected_not_panicked() {
    let memory_body = encode_memory_stats(&memory()).unwrap();
    for body in [
        &memory_body[..2],
        &[0xffu8, 0xff, 0xff, 0xff][..],
        &[0x01u8][..],
    ] {
        assert!(decode_memory_stats(body).is_err());
    }
    let tasks = encode_tasks_stats(&TasksStats {
        live: 1,
        tasks: vec![task_row(1)],
    })
    .unwrap();
    assert!(decode_tasks_stats(&tasks[..tasks.len() - 2]).is_err());
    assert!(decode_tasks_stats(&[0xff, 0xff, 0xff, 0xff]).is_err());
    assert!(decode_tasks_stats(&[0x01]).is_err());
}
