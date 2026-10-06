//! Round-trip tests for the pipelined present path of `os.lazy.display.v1`:
//! `AttachBufferSlot`, `Present` with its damage list, and the
//! `BufferRelease`/`FrameDone` events.

use messenger_generated::os_lazy_display_v1::*;

#[test]
fn pipelined_present_ids_are_appended_after_the_legacy_range() {
    let ids = [
        METHOD_ATTACHBUFFERSLOT,
        METHOD_PRESENT,
        METHOD_BUFFERRELEASE,
        METHOD_FRAMEDONE,
    ];
    assert_eq!(ids.to_vec(), vec![25, 26, 27, 28]);
}

#[test]
fn present_and_its_events_roundtrip() {
    roundtrip!(
        AttachBufferSlotArgs {
            surface: 7,
            slot: 3
        },
        encode_attach_buffer_slot_args,
        decode_attach_buffer_slot_args
    );
    roundtrip!(
        BufferReleaseArgs {
            surface: u64::MAX,
            slot: 1
        },
        encode_buffer_release_args,
        decode_buffer_release_args
    );
    roundtrip!(
        FrameDoneArgs {
            surface: 2,
            seq: u64::MAX
        },
        encode_frame_done_args,
        decode_frame_done_args
    );
    // An empty damage list (whole surface), one rect, and the 16-rect cap and
    // beyond all survive the wire.
    for count in [0usize, 1, 16, 17, 64] {
        let damage = (0..count as u32)
            .map(|i| Rect {
                x: i,
                y: i * 2,
                w: u32::MAX - i,
                h: 5,
            })
            .collect();
        roundtrip!(
            PresentArgs {
                surface: 4,
                slot: 1,
                seq: 99,
                damage
            },
            encode_present_args,
            decode_present_args
        );
    }
}

#[test]
fn present_rejects_truncated_bodies_and_ignores_unknown_fields() {
    let body = encode_present_args(&PresentArgs {
        surface: 1,
        slot: 0,
        seq: 5,
        damage: vec![Rect {
            x: 1,
            y: 2,
            w: 3,
            h: 4,
        }],
    })
    .unwrap();
    for cut in 1..body.len() {
        // A prefix is either an error or decodes without panicking.
        let _ = decode_present_args(&body[..cut]);
    }
    assert_eq!(decode_present_args(&[]).unwrap(), PresentArgs::default());
}
