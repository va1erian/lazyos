//! Round-trip tests for the generated `os.lazy.net.nic.v1` stubs
//! (docs/networking-plan.md N0).

use messenger_generated::os_lazy_net_nic_v1::*;

#[test]
fn info_roundtrips() {
    let info = NicInfo {
        mac: vec![0x52, 0x54, 0x00, 0x12, 0x34, 0x56],
        mtu: 1500,
        max_frame: 1514,
        link: true,
        features: 0,
        kind: NIC_KIND_WIRED,
    };
    let body = encode_nic_info(&info).unwrap();
    assert_eq!(decode_nic_info(&body).unwrap(), info);
}

#[test]
fn stats_roundtrip_at_the_extremes() {
    for v in [0u64, 1, u64::MAX] {
        let stats = NicStats {
            rx_frames: v,
            tx_frames: v,
            rx_bytes: v,
            tx_bytes: v,
            rx_dropped: v,
            tx_dropped: v,
            runts: v,
            oversize: v,
            ring_errors: v,
            interrupts: v,
            link_changes: v as u32,
        };
        assert_eq!(
            decode_nic_stats(&encode_nic_stats(&stats).unwrap()).unwrap(),
            stats
        );
    }
}

#[test]
fn call_arguments_roundtrip() {
    let attach = AttachRingArgs {
        slots: 256,
        rings: libmessenger::Buffer::whole(4, 2 * 8192),
        notify: 5,
    };
    let (body, objects) = encode_attach_ring_args(&attach).unwrap();
    assert_eq!(
        objects,
        vec![
            libmessenger::Object::Buffer(4),
            libmessenger::Object::Channel(5)
        ]
    );
    assert_eq!(decode_attach_ring_args(&body, &objects).unwrap(), attach);
    let reply = AttachRingReply { ring: 7 };
    assert_eq!(
        decode_attach_ring_reply(&encode_attach_ring_reply(&reply).unwrap()).unwrap(),
        reply
    );
    let notify = NotifyArgs {
        ring: 7,
        events: (1 << NOTIFY_BIT_RX_READY) | (1 << NOTIFY_BIT_LINK_CHANGE),
    };
    assert_eq!(
        decode_notify_args(&encode_notify_args(&notify).unwrap()).unwrap(),
        notify
    );
    let kick = KickArgs { ring: 7 };
    assert_eq!(
        decode_kick_args(&encode_kick_args(&kick).unwrap()).unwrap(),
        kick
    );
}

#[test]
fn wake_up_is_a_pair_of_one_way_messages_not_a_topic_string() {
    // The draft's `notify: String` is gone: AttachRing's body holds the slot
    // count and two object fields (an index each; the objects ride in the
    // parcel's list).
    let (body, _) = encode_attach_ring_args(&AttachRingArgs {
        slots: 16,
        rings: libmessenger::Buffer::whole(1, 8192),
        notify: 2,
    })
    .unwrap();
    assert!(
        body.len() < 64,
        "no topic name in the request body: {} bytes",
        body.len()
    );
    assert_ne!(METHOD_KICK, METHOD_NOTIFY);
}

#[test]
fn rx_mode_and_notify_bits_are_stable() {
    assert_eq!(
        (RX_MODE_OFF, RX_MODE_FILTERED, RX_MODE_PROMISCUOUS),
        (0, 1, 2)
    );
    assert_eq!(
        (
            NOTIFY_BIT_RX_READY,
            NOTIFY_BIT_TX_SPACE,
            NOTIFY_BIT_LINK_CHANGE
        ),
        (0, 1, 2)
    );
}

#[test]
fn link_topic_is_declared_retained() {
    assert_eq!(
        name_system_net_link("virtio-net0").unwrap(),
        "system/net/virtio-net0/link"
    );
    const { assert!(TOPIC_SYSTEM_NET_LINK_RETAINED) };
    assert!(
        name_system_net_link("a/b").is_err(),
        "a slash cannot smuggle in a segment"
    );
    assert!(
        name_system_net_link("+").is_err(),
        "no publishing to a wildcard"
    );
    let event = LinkEvent {
        up: true,
        changes: 3,
    };
    assert_eq!(
        decode_system_net_link(&encode_system_net_link(&event).unwrap()).unwrap(),
        event
    );
}

#[test]
fn truncated_bodies_are_rejected() {
    let body = encode_nic_info(&NicInfo {
        mac: vec![1, 2, 3, 4, 5, 6],
        mtu: 1500,
        max_frame: 1514,
        link: true,
        features: 0,
        kind: NIC_KIND_WIRED,
    })
    .unwrap();
    assert!(decode_nic_info(&body[..body.len() - 3]).is_err());
    let stats = encode_nic_stats(&NicStats {
        rx_frames: 1,
        tx_frames: 2,
        rx_bytes: 3,
        tx_bytes: 4,
        rx_dropped: 5,
        tx_dropped: 6,
        runts: 7,
        oversize: 8,
        ring_errors: 9,
        interrupts: 10,
        link_changes: 11,
    })
    .unwrap();
    assert!(decode_nic_stats(&stats[..stats.len() - 3]).is_err());
}

#[test]
fn attach_ring_declares_both_rings_and_the_notify_channel() {
    use libmessenger::ObjectKind;
    use messenger_generated::rings::{Layout, Side};
    assert_eq!(
        ATTACH_RING_OBJECTS,
        &[ObjectKind::Buffer, ObjectKind::Channel]
    );
    assert_eq!(ATTACH_RING_RINGS, [RING_RX, RING_TX]);
    // The driver produces received frames and rings `Notify` on the channel;
    // the client produces frames to send and rings `Kick`.
    assert_eq!(RING_RX.layout, Layout::Frames);
    assert_eq!(RING_RX.producer, Side::Server);
    assert_eq!(RING_RX.doorbell, Some(METHOD_NOTIFY));
    assert_eq!(RING_TX.producer, Side::Client);
    assert_eq!(RING_TX.doorbell, Some(METHOD_KICK));
}

#[test]
fn attach_ring_layout_is_back_to_back_and_checked() {
    let layout = attach_ring_rings(4096 + 16 * 2048).unwrap();
    assert_eq!((layout.rx, layout.tx, layout.total), (0, 36864, 73728));
    assert!(attach_ring_rings(u64::MAX).is_none());
}
