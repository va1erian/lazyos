//! Round-trip tests for the generated `os.lazy.audio.v1` and
//! `os.lazy.audio.mixer.v1` stubs (docs/audio-plan.md stage A2).

use messenger_generated::os_lazy_audio_mixer_v1 as mixer;
use messenger_generated::os_lazy_audio_v1 as audio;

#[test]
fn volume_and_mute_args_roundtrip() {
    for gain_q16 in [0, 1, 32768, 65536, 262_144, u32::MAX] {
        let args = audio::SetVolumeArgs {
            stream: 7,
            gain_q16,
        };
        let body = audio::encode_set_volume_args(&args).unwrap();
        assert_eq!(audio::decode_set_volume_args(&body).unwrap(), args);
    }
    for mute in [false, true] {
        let args = audio::SetMuteArgs { stream: 3, mute };
        let body = audio::encode_set_mute_args(&args).unwrap();
        assert_eq!(audio::decode_set_mute_args(&body).unwrap(), args);
    }
}

#[test]
fn stream_list_roundtrips_including_empty() {
    let status = |stream| mixer::StreamStatus {
        stream,
        owner: u64::from(stream) << 32 | 9,
        state: mixer::STREAM_STATE_RUNNING,
        rate: 44100,
        channels: 1,
        gain_q16: 49152,
        mute: stream % 2 == 0,
        frames: u64::MAX - u64::from(stream),
        underruns: stream * 3,
    };
    for count in [0u32, 1, 16] {
        let reply = mixer::ListStreamsReply {
            streams: (1..=count).map(status).collect(),
        };
        let body = mixer::encode_list_streams_reply(&reply).unwrap();
        assert_eq!(mixer::decode_list_streams_reply(&body).unwrap(), reply);
    }
}

#[test]
fn master_roundtrips() {
    let reply = mixer::GetMasterReply {
        master: mixer::Master {
            gain_q16: 65536,
            mute: true,
            card: true,
            rate: 48000,
            channels: 2,
            streams: 3,
            max_streams: 16,
        },
    };
    let body = mixer::encode_get_master_reply(&reply).unwrap();
    assert_eq!(mixer::decode_get_master_reply(&body).unwrap(), reply);
}

#[test]
fn attach_ring_declares_a_client_produced_stream() {
    use messenger_generated::rings::{Layout, Side};
    assert_eq!(audio::ATTACH_RING_RINGS, [audio::RING_SAMPLES]);
    assert_eq!(audio::RING_SAMPLES.layout, Layout::Stream);
    assert_eq!(audio::RING_SAMPLES.producer, Side::Client);
    assert_eq!(audio::RING_SAMPLES.advance, Some(audio::METHOD_COMMIT));
    assert_eq!(audio::request_transfers(audio::METHOD_ATTACHRING), audio::ATTACH_RING_TRANSFERS);
}
