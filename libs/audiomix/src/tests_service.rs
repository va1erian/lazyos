//! The wire layer: requests encoded with the generated codecs, the outcomes
//! decoded back, the errno mapping and ring adoption.

use std::cell::Cell;
use std::rc::Rc;
use std::vec::Vec;

use messenger_generated::os_lazy_audio_mixer_v1 as control;
use messenger_generated::os_lazy_audio_v1 as audio;

use crate::service::{self, errno, Outcome, Request};
use crate::tests::{OWNER, PERIOD};
use crate::{Mixer, Ring};

/// A ring that counts how many times it was dropped (closed by `audiod`).
struct CountedRing {
    bytes: usize,
    drops: Rc<Cell<u32>>,
}

impl Ring for CountedRing {
    fn len(&self) -> usize {
        self.bytes
    }

    fn read(&self, _offset: usize, dst: &mut [u8]) {
        dst.fill(0);
    }
}

impl Drop for CountedRing {
    fn drop(&mut self) {
        self.drops.set(self.drops.get() + 1);
    }
}

struct Harness {
    mixer: Mixer<CountedRing>,
    drops: Rc<Cell<u32>>,
}

impl Harness {
    fn new() -> Harness {
        Harness {
            mixer: Mixer::new(crate::Config::new(48000, PERIOD)),
            drops: Rc::new(Cell::new(0)),
        }
    }

    fn ring(&self, bytes: usize) -> CountedRing {
        CountedRing {
            bytes,
            drops: self.drops.clone(),
        }
    }

    fn call(
        &mut self,
        interface: u64,
        method: u32,
        body: Vec<u8>,
        ring: Option<CountedRing>,
    ) -> Outcome {
        self.call_as(OWNER, interface, method, body, ring)
    }

    fn call_as(
        &mut self,
        sender: u64,
        interface: u64,
        method: u32,
        body: Vec<u8>,
        ring: Option<CountedRing>,
    ) -> Outcome {
        service::handle(
            Some(&mut self.mixer),
            Request {
                interface,
                method,
                body: &body,
                sender,
                ring,
                now: 0,
            },
        )
    }

    fn open(&mut self) -> audio::StreamGrant {
        let body = audio::encode_open_stream_args(&audio::OpenStreamArgs {
            dir: audio::DIRECTION_PLAYBACK,
            format: audio::FORMAT_S16_LE,
            rate: 48000,
            channels: 2,
            period_bytes: 4096,
        })
        .unwrap();
        match self.call(audio::INTERFACE_ID, audio::METHOD_OPENSTREAM, body, None) {
            Outcome::Reply(reply) => audio::decode_open_stream_reply(&reply).unwrap().grant,
            other => panic!("open: {other:?}"),
        }
    }
}

fn reply(outcome: Outcome) -> Vec<u8> {
    match outcome {
        Outcome::Reply(body) => body,
        other => panic!("expected a reply, got {other:?}"),
    }
}

#[test]
fn info_describes_the_mixer() {
    let mut h = Harness::new();
    let body = reply(h.call(audio::INTERFACE_ID, audio::METHOD_INFO, Vec::new(), None));
    let info = audio::decode_info_reply(&body).unwrap().info;
    assert_eq!(info.streams, 16);
    assert_eq!(info.formats, 1);
    assert_eq!(info.rates, 0x7ff);
    assert_eq!(info.channels, 2);
}

#[test]
fn a_whole_stream_over_the_wire() {
    let mut h = Harness::new();
    let grant = h.open();
    assert_ne!(grant.stream, 0);
    let ring_bytes = (grant.period_bytes * grant.periods) as usize;
    let (attach, _) = audio::encode_attach_ring_args(&audio::AttachRingArgs {
        stream: grant.stream,
        ring: libmessenger::Buffer::whole(1, ring_bytes as u64),
    })
    .unwrap();
    let ring = h.ring(ring_bytes);
    reply(h.call(
        audio::INTERFACE_ID,
        audio::METHOD_ATTACHRING,
        attach,
        Some(ring),
    ));
    assert_eq!(h.drops.get(), 0, "the ring was adopted");

    let commit = audio::encode_commit_args(&audio::CommitArgs {
        stream: grant.stream,
        written_frames: PERIOD as u64,
    })
    .unwrap();
    let body = reply(h.call(audio::INTERFACE_ID, audio::METHOD_COMMIT, commit, None));
    assert_eq!(audio::decode_commit_reply(&body).unwrap().consumed, 0);

    let start = audio::encode_start_args(&audio::StartArgs {
        stream: grant.stream,
    })
    .unwrap();
    reply(h.call(audio::INTERFACE_ID, audio::METHOD_START, start, None));
    let drain = audio::encode_drain_args(&audio::DrainArgs {
        stream: grant.stream,
    })
    .unwrap();
    assert_eq!(
        h.call(audio::INTERFACE_ID, audio::METHOD_DRAIN, drain, None),
        Outcome::DrainPending(grant.stream)
    );
    let mut out = std::vec![0i16; 2 * PERIOD];
    h.mixer.mix(&mut out, PERIOD as u64);
    let mut done = Vec::new();
    h.mixer.played(PERIOD as u64, |id| done.push(id));
    assert_eq!(done, [grant.stream]);
    assert!(
        audio::decode_drain_reply(&service::drained_reply())
            .unwrap()
            .ok
    );

    let close = audio::encode_close_stream_args(&audio::CloseStreamArgs {
        stream: grant.stream,
    })
    .unwrap();
    reply(h.call(audio::INTERFACE_ID, audio::METHOD_CLOSESTREAM, close, None));
    assert_eq!(h.drops.get(), 1, "closing the stream drops its ring");
}

#[test]
fn refusals_carry_the_driver_errnos() {
    let mut h = Harness::new();
    let grant = h.open();
    let start = |stream| audio::encode_start_args(&audio::StartArgs { stream }).unwrap();
    assert_eq!(
        h.call(
            audio::INTERFACE_ID,
            audio::METHOD_START,
            start(grant.stream),
            None
        ),
        Outcome::Refuse(errno::EINVAL),
        "no ring yet"
    );
    assert_eq!(
        h.call(audio::INTERFACE_ID, audio::METHOD_START, start(999), None),
        Outcome::Refuse(errno::EINVAL),
        "no such stream"
    );
    assert_eq!(
        h.call_as(
            99,
            audio::INTERFACE_ID,
            audio::METHOD_START,
            start(grant.stream),
            None
        ),
        Outcome::Refuse(errno::EACCES)
    );
    let capture = audio::encode_open_stream_args(&audio::OpenStreamArgs {
        dir: audio::DIRECTION_CAPTURE,
        format: 0,
        rate: 48000,
        channels: 2,
        period_bytes: 4096,
    })
    .unwrap();
    assert_eq!(
        h.call(audio::INTERFACE_ID, audio::METHOD_OPENSTREAM, capture, None),
        Outcome::Refuse(errno::ENOTSUP)
    );
    assert_eq!(
        h.call(audio::INTERFACE_ID, 0xdead, Vec::new(), None),
        Outcome::Refuse(errno::EINVAL)
    );
    assert_eq!(
        h.call(0x1234, audio::METHOD_INFO, Vec::new(), None),
        Outcome::Refuse(errno::EINVAL)
    );
    assert_eq!(
        h.call(
            audio::INTERFACE_ID,
            audio::METHOD_COMMIT,
            std::vec![0xff; 3],
            None
        ),
        Outcome::Refuse(errno::EINVAL),
        "garbage body"
    );
    let volume = audio::encode_set_volume_args(&audio::SetVolumeArgs {
        stream: grant.stream,
        gain_q16: u32::MAX,
    })
    .unwrap();
    assert_eq!(
        h.call(audio::INTERFACE_ID, audio::METHOD_SETVOLUME, volume, None),
        Outcome::Refuse(errno::EINVAL)
    );
}

#[test]
fn rings_on_the_wrong_path_are_dropped_not_kept() {
    let mut h = Harness::new();
    let grant = h.open();
    // A ring attached to another method.
    let position = audio::encode_position_args(&audio::PositionArgs {
        stream: grant.stream,
    })
    .unwrap();
    let ring = h.ring(1 << 16);
    reply(h.call(
        audio::INTERFACE_ID,
        audio::METHOD_POSITION,
        position,
        Some(ring),
    ));
    // A malformed AttachRing, a short ring and a second ring.
    let ring = h.ring(1 << 16);
    h.call(
        audio::INTERFACE_ID,
        audio::METHOD_ATTACHRING,
        Vec::new(),
        Some(ring),
    );
    let (attach, _) = audio::encode_attach_ring_args(&audio::AttachRingArgs {
        stream: grant.stream,
        ring: libmessenger::Buffer::whole(1, 1 << 16),
    })
    .unwrap();
    let ring = h.ring(16);
    assert_eq!(
        h.call(
            audio::INTERFACE_ID,
            audio::METHOD_ATTACHRING,
            attach.clone(),
            Some(ring)
        ),
        Outcome::Refuse(errno::EINVAL)
    );
    let ring = h.ring(1 << 16);
    reply(h.call(
        audio::INTERFACE_ID,
        audio::METHOD_ATTACHRING,
        attach.clone(),
        Some(ring),
    ));
    let ring = h.ring(1 << 16);
    assert_eq!(
        h.call(
            audio::INTERFACE_ID,
            audio::METHOD_ATTACHRING,
            attach,
            Some(ring)
        ),
        Outcome::Refuse(errno::EBUSY)
    );
    // Five offered, one adopted.
    assert_eq!(h.drops.get(), 4);
    // Hundreds of hostile attaches leak nothing.
    for _ in 0..300 {
        let ring = h.ring(1 << 16);
        h.call(
            audio::INTERFACE_ID,
            audio::METHOD_ATTACHRING,
            Vec::new(),
            Some(ring),
        );
    }
    assert_eq!(h.drops.get(), 304);
}

#[test]
fn the_control_panel_lists_and_sets_volumes() {
    let mut h = Harness::new();
    let grant = h.open();
    let body = reply(h.call(
        control::INTERFACE_ID,
        control::METHOD_LISTSTREAMS,
        Vec::new(),
        None,
    ));
    let list = control::decode_list_streams_reply(&body).unwrap().streams;
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].stream, grant.stream);
    assert_eq!(list[0].owner, OWNER);
    assert_eq!(list[0].state, control::STREAM_STATE_IDLE);
    assert_eq!(list[0].gain_q16, 65536);

    // Anyone may set a stream's volume through the panel.
    let set = control::encode_set_stream_volume_args(&control::SetStreamVolumeArgs {
        stream: grant.stream,
        gain_q16: 32768,
        mute: true,
    })
    .unwrap();
    reply(h.call_as(
        99,
        control::INTERFACE_ID,
        control::METHOD_SETSTREAMVOLUME,
        set,
        None,
    ));
    let status = h.mixer.statuses().next().unwrap();
    assert_eq!((status.gain.q16(), status.muted), (32768, true));

    // A bad gain changes nothing at all.
    let bad = control::encode_set_stream_volume_args(&control::SetStreamVolumeArgs {
        stream: grant.stream,
        gain_q16: u32::MAX,
        mute: false,
    })
    .unwrap();
    assert_eq!(
        h.call(
            control::INTERFACE_ID,
            control::METHOD_SETSTREAMVOLUME,
            bad,
            None
        ),
        Outcome::Refuse(errno::EINVAL)
    );
    assert!(h.mixer.statuses().next().unwrap().muted);

    let unknown = control::encode_set_stream_volume_args(&control::SetStreamVolumeArgs {
        stream: 777,
        gain_q16: 1,
        mute: false,
    })
    .unwrap();
    assert_eq!(
        h.call(
            control::INTERFACE_ID,
            control::METHOD_SETSTREAMVOLUME,
            unknown,
            None
        ),
        Outcome::Refuse(errno::ENOENT)
    );

    let master = control::encode_set_master_args(&control::SetMasterArgs {
        gain_q16: 49152,
        mute: false,
    })
    .unwrap();
    reply(h.call(
        control::INTERFACE_ID,
        control::METHOD_SETMASTER,
        master,
        None,
    ));
    let body = reply(h.call(
        control::INTERFACE_ID,
        control::METHOD_GETMASTER,
        Vec::new(),
        None,
    ));
    let master = control::decode_get_master_reply(&body).unwrap().master;
    assert_eq!(master.gain_q16, 49152);
    assert!(master.card);
    assert_eq!((master.rate, master.channels), (48000, 2));
    assert_eq!((master.streams, master.max_streams), (1, 16));
}

#[test]
fn without_a_card_streams_fail_and_the_panel_says_so() {
    let call = |interface, method| {
        service::handle::<CountedRing>(
            None,
            Request {
                interface,
                method,
                body: &[],
                sender: 1,
                ring: None,
                now: 0,
            },
        )
    };
    assert_eq!(
        call(audio::INTERFACE_ID, audio::METHOD_INFO),
        Outcome::Refuse(errno::ENODEV)
    );
    // Setting volumes needs a card too: `ENODEV`, never `ENOENT`.
    let set = |interface, method, body: Vec<u8>| {
        service::handle::<CountedRing>(
            None,
            Request {
                interface,
                method,
                body: &body,
                sender: 1,
                ring: None,
                now: 0,
            },
        )
    };
    let stream = control::encode_set_stream_volume_args(&control::SetStreamVolumeArgs {
        stream: 1,
        gain_q16: 65536,
        mute: false,
    })
    .unwrap();
    assert_eq!(
        set(
            control::INTERFACE_ID,
            control::METHOD_SETSTREAMVOLUME,
            stream
        ),
        Outcome::Refuse(errno::ENODEV)
    );
    let master = control::encode_set_master_args(&control::SetMasterArgs {
        gain_q16: 65536,
        mute: false,
    })
    .unwrap();
    assert_eq!(
        set(control::INTERFACE_ID, control::METHOD_SETMASTER, master),
        Outcome::Refuse(errno::ENODEV)
    );
    let body = reply(call(control::INTERFACE_ID, control::METHOD_GETMASTER));
    assert!(!control::decode_get_master_reply(&body).unwrap().master.card);
    let body = reply(call(control::INTERFACE_ID, control::METHOD_LISTSTREAMS));
    assert!(control::decode_list_streams_reply(&body)
        .unwrap()
        .streams
        .is_empty());
}
