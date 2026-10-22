//! Output choice (`outputs.rs`): hot-plug takes over and falls back, a
//! selection sticks while present, and the wire calls answer as declared.

use alloc::vec;
use alloc::vec::Vec;

use messenger_generated::os_lazy_audio_mixer_v1 as control;

use crate::outputs::{card_id, kind_of, Outputs};
use crate::service::{errno, Outcome};
use crate::{Config, Mixer};

#[test]
fn ids_and_kinds() {
    assert_eq!(card_id("os.lazy.audio.card/usb-audio0"), Some("usb-audio0"));
    assert_eq!(card_id("os.lazy.audio.card/"), None);
    assert_eq!(card_id("os.lazy.audio.card/../x"), None);
    assert_eq!(card_id("os.lazy.audio"), None);
    assert_eq!(kind_of("usb-audio0"), control::OUTPUT_KIND_USB);
    assert_eq!(kind_of("intel-hda0-hdmi"), control::OUTPUT_KIND_HDMI);
    assert_eq!(kind_of("intel-hda0"), control::OUTPUT_KIND_HDA);
    assert_eq!(kind_of("virtio-snd0"), control::OUTPUT_KIND_VIRTIO);
}

#[test]
fn a_headset_takes_over_and_falls_back() {
    let mut outputs = Outputs::new();
    assert_eq!(outputs.target(), None);
    assert!(outputs.update(["intel-hda0"]));
    assert_eq!(outputs.target(), Some("intel-hda0"));
    assert!(!outputs.update(["intel-hda0"]));
    outputs.update(["intel-hda0", "usb-audio0"]);
    assert_eq!(outputs.target(), Some("usb-audio0"));
    outputs.update(["intel-hda0"]);
    assert_eq!(outputs.target(), Some("intel-hda0"));
}

#[test]
fn the_kind_decides_not_the_order_drivers_registered_in() {
    let mut outputs = Outputs::new();
    outputs.update(["usb-audio0"]);
    outputs.update(["usb-audio0", "intel-hda0"]);
    assert_eq!(outputs.target(), Some("usb-audio0"), "the built-in codec came later");
    let mut outputs = Outputs::new();
    outputs.update(["intel-hda0"]);
    outputs.update(["intel-hda0", "virtio-snd0"]);
    assert_eq!(outputs.target(), Some("intel-hda0"));
}

#[test]
fn outputs_found_together_rank_usb_first() {
    let mut outputs = Outputs::new();
    outputs.update(["intel-hda0", "intel-hda0-hdmi", "usb-audio0"]);
    assert_eq!(outputs.target(), Some("usb-audio0"));
    let mut outputs = Outputs::new();
    outputs.update(["intel-hda0", "intel-hda0-hdmi"]);
    assert_eq!(outputs.target(), Some("intel-hda0-hdmi"));
}

#[test]
fn a_selection_sticks_while_present() {
    let mut outputs = Outputs::new();
    outputs.update(["intel-hda0", "intel-hda0-hdmi"]);
    assert_eq!(outputs.select("intel-hda0"), Ok(()));
    outputs.update(["intel-hda0", "intel-hda0-hdmi", "usb-audio0"]);
    assert_eq!(outputs.target(), Some("intel-hda0"), "the choice beats a new device");
    outputs.update(["intel-hda0-hdmi", "usb-audio0"]);
    assert_eq!(outputs.target(), Some("usb-audio0"), "absent: the newest");
    outputs.update(["intel-hda0", "intel-hda0-hdmi", "usb-audio0"]);
    assert_eq!(outputs.target(), Some("intel-hda0"), "back: chosen again");
    assert_eq!(outputs.select("nope0"), Err(errno::ENOENT));
}

#[test]
fn a_broken_output_is_passed_over() {
    let mut outputs = Outputs::new();
    outputs.update(["intel-hda0", "usb-audio0"]);
    outputs.failed("usb-audio0");
    assert_eq!(outputs.target(), Some("intel-hda0"));
    // Even when chosen.
    outputs.select("usb-audio0").unwrap();
    assert_eq!(outputs.target(), Some("intel-hda0"));
    // Tried again later.
    for _ in 0..30 {
        outputs.update(["intel-hda0", "usb-audio0"]);
    }
    assert_eq!(outputs.target(), Some("usb-audio0"));
    // With nothing else, the one that failed longest ago.
    let mut alone = Outputs::new();
    alone.update(["usb-audio0"]);
    alone.failed("usb-audio0");
    assert_eq!(alone.target(), Some("usb-audio0"));
    // Unplugged and plugged in again: a fresh start.
    outputs.failed("usb-audio0");
    outputs.update(["intel-hda0"]);
    outputs.update(["intel-hda0", "usb-audio0"]);
    assert_eq!(outputs.target(), Some("usb-audio0"));
}

#[test]
fn hostile_ids_are_dropped() {
    let mut outputs = Outputs::new();
    let long = "a".repeat(64);
    outputs.update(["", long.as_str(), "x/y", "ok0"]);
    assert_eq!(outputs.list().len(), 1);
}

#[test]
fn wire_calls() {
    let mut outputs = Outputs::new();
    outputs.update(["intel-hda0", "usb-audio0"]);
    outputs.set_active(Some("usb-audio0"));
    outputs.set_name("usb-audio0", "USB: Headset");
    let Some(Outcome::Reply(body)) = outputs.handle(control::METHOD_LISTOUTPUTS, &[]) else {
        panic!("ListOutputs answers");
    };
    let listed = control::decode_list_outputs_reply(&body).unwrap().outputs;
    assert_eq!(listed.len(), 2);
    assert_eq!((listed[0].id.as_str(), listed[0].active), ("usb-audio0", true));
    assert_eq!(listed[0].name, "USB: Headset");
    assert_eq!(listed[1].name, "Built-in audio");
    let body = control::encode_select_output_args(&control::SelectOutputArgs {
        id: "intel-hda0".into(),
    })
    .unwrap();
    assert_eq!(
        outputs.handle(control::METHOD_SELECTOUTPUT, &body),
        Some(Outcome::Reply(vec![]))
    );
    assert_eq!(outputs.target(), Some("intel-hda0"));
    let missing = control::encode_select_output_args(&control::SelectOutputArgs {
        id: "gone0".into(),
    })
    .unwrap();
    assert_eq!(
        outputs.handle(control::METHOD_SELECTOUTPUT, &missing),
        Some(Outcome::Refuse(errno::ENOENT))
    );
    assert_eq!(
        outputs.handle(control::METHOD_SELECTOUTPUT, &[0xFF]),
        Some(Outcome::Refuse(errno::EINVAL))
    );
    assert_eq!(outputs.handle(control::METHOD_GETMASTER, &[]), None);
}

#[test]
fn retarget_keeps_streams() {
    use crate::tests::{open_attached, OWNER};
    let mut mixer: Mixer<crate::tests::VecRing> = Mixer::new(Config::new(48_000, 1024));
    let (id, ring, _) = open_attached(&mut mixer, OWNER, 48_000, 2);
    let tone: Vec<i16> = (0..2 * 4096).map(|i| if i % 4 < 2 { 8000 } else { -8000 }).collect();
    ring.write(0, 2, &tone);
    mixer.commit(id, OWNER, 4096, 0).unwrap();
    mixer.start(id, OWNER, 0).unwrap();
    let mut out = vec![0i16; 2048];
    mixer.mix(&mut out, 1024);
    mixer.forget_card(|_| {});
    mixer.retarget(44_100, 512);
    assert_eq!(mixer.config().rate, 44_100);
    assert!(mixer.statuses().any(|s| s.id == id), "the stream survives");
    let mut out = vec![0i16; 1024];
    mixer.mix(&mut out, 512);
    assert!(out.iter().any(|&s| s != 0), "still audible after the switch");
}
