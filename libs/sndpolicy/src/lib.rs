//! The access-control rules of the virtio-sound driver (`sndd`,
//! `docs/architecture/audio.md`), as data.
//!
//! Like `libs/netpolicy` and `libs/usbpolicy`, the kernel installs these
//! device-class rules at boot (`dev::policy`), so `_snd` may claim an audio
//! device and nothing else, and no other driver uid gets the audio class.

#![no_std]

/// The `_snd` system user `sndd` runs as: only `CAP_DEV_CLAIM`.
pub const SND_UID: u32 = 901;

/// The `_audio` system user the mixer `audiod` runs as: no capabilities at
/// all. It holds no device, no DMA and no authority over other tasks; it only
/// maps the rings its clients hand it and owns the card's one stream.
pub const AUDIO_UID: u32 = 905;

/// The topics the audio services publish their stream events under
/// (`system/audio/{card}/event`, issue #453).
pub const AUDIO_TOPIC_PREFIX: &str = "system/audio/";

/// Whether `uid` may publish `topic` in the broker's `system/` namespace,
/// which only root may otherwise write: the driver (`_snd`) and the mixer
/// (`_audio`) may publish their stream events under [`AUDIO_TOPIC_PREFIX`],
/// and nothing else there.
pub fn may_publish_audio_event(uid: u32, topic: &str) -> bool {
    (uid == SND_UID || uid == AUDIO_UID)
        && topic
            .strip_prefix(AUDIO_TOPIC_PREFIX)
            .is_some_and(|rest| !rest.is_empty())
}

/// The ACL device class of an audio function (`dev::class::AUDIO`, PCI
/// `04/01` and `04/03`; virtio-sound is `04/01`).
pub const AUDIO_CLASS: &str = "os.kernel.dev.audio";

/// One rule. `actor` is a uid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuleSpec {
    pub actor: u32,
    pub interface: &'static str,
    pub method: &'static str,
    pub allow: bool,
}

const fn allow(actor: u32, interface: &'static str, method: &'static str) -> RuleSpec {
    RuleSpec {
        actor,
        interface,
        method,
        allow: true,
    }
}

/// What `_snd` may do to a device: claim an audio function and take the MMIO
/// and DMA rights a virtio driver needs. Nothing names another class.
pub const SND_DRIVER_CLASS_RULES: &[RuleSpec] = &[
    allow(SND_UID, AUDIO_CLASS, "claim"),
    allow(SND_UID, AUDIO_CLASS, "map"),
    allow(SND_UID, AUDIO_CLASS, "dma"),
];

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    #[test]
    fn the_driver_gets_exactly_claim_map_and_dma_on_the_audio_class() {
        let methods: std::vec::Vec<_> = SND_DRIVER_CLASS_RULES.iter().map(|r| r.method).collect();
        assert_eq!(methods, ["claim", "map", "dma"]);
        for rule in SND_DRIVER_CLASS_RULES {
            assert!(rule.allow);
            assert_eq!(rule.actor, SND_UID);
            assert_eq!(
                rule.interface, AUDIO_CLASS,
                "no other device class is named"
            );
            assert_ne!(rule.method, "*", "no wildcard grants");
        }
    }

    #[test]
    fn the_mixer_has_its_own_uid_and_no_device_rules() {
        const { assert!(AUDIO_UID < 1000) };
        // `_snd` 901, `_net` 902, `_netd` 903, `_usb` 904.
        const { assert!(AUDIO_UID > 904) };
        assert!(SND_DRIVER_CLASS_RULES
            .iter()
            .all(|rule| rule.actor != AUDIO_UID));
    }

    #[test]
    fn only_the_audio_services_publish_audio_events_and_nothing_else() {
        for uid in [SND_UID, AUDIO_UID] {
            assert!(may_publish_audio_event(uid, "system/audio/mixer/event"));
            assert!(may_publish_audio_event(
                uid,
                "system/audio/virtio-snd0/event"
            ));
            assert!(!may_publish_audio_event(uid, "system/audio/"));
            assert!(!may_publish_audio_event(uid, "system/events/service/x"));
            assert!(!may_publish_audio_event(uid, "system/audiox/y"));
        }
        for uid in [0, 902, 1000] {
            assert!(!may_publish_audio_event(uid, "system/audio/mixer/event"));
        }
    }

    #[test]
    fn the_snd_uid_is_its_own_system_uid() {
        const { assert!(SND_UID < 1000) };
        // `_net` is 902, `_netd` 903, `_usb` 904.
        const { assert!(SND_UID != 902 && SND_UID != 903 && SND_UID != 904) };
    }
}
