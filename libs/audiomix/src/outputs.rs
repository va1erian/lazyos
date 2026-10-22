//! Which output the mixer plays through (docs/usb-audio-plan.md, "Several
//! outputs"): every card a driver registers under `os.lazy.audio.card/<id>`
//! is an output; the mixer holds one at a time.
//!
//! The rule: the output someone selected (`SelectOutput`, remembered per
//! user by the Volume applet) whenever it is present; otherwise the best
//! kind present (USB, then HDMI, then a built-in codec, then a virtual
//! card), the one that appeared last among outputs of that kind. So a
//! headset plugged in takes over and unplugging it falls back, and the
//! choice at boot does not depend on which driver happened to register
//! first. This module is the bookkeeping and the
//! `ListOutputs`/`SelectOutput` wire handling, without syscalls, so it is
//! host-tested; `audiod` scans the registry and opens the cards.

use alloc::string::String;
use alloc::vec::Vec;

use messenger_generated::os_lazy_audio_mixer_v1 as control;

use crate::service::{errno, Outcome};

/// The registry prefix of a card: `os.lazy.audio.card/<id>`.
pub const CARD_PREFIX: &str = "os.lazy.audio.card/";
/// Longest id kept (a hostile name cannot grow the table's strings).
const MAX_ID: usize = 32;
/// Outputs tracked at once.
const MAX_OUTPUTS: usize = 16;
/// Scans an output that failed to open is passed over (about 30 s), unless
/// it goes away and comes back first.
const FAILED_SCANS: u64 = 30;

/// The `OutputKind` of a card, from its id's prefix.
pub fn kind_of(id: &str) -> u32 {
    if id.starts_with("usb-audio") {
        control::OUTPUT_KIND_USB
    } else if id.contains("hdmi") {
        control::OUTPUT_KIND_HDMI
    } else if id.starts_with("intel-hda") {
        control::OUTPUT_KIND_HDA
    } else if id.starts_with("virtio-snd") {
        control::OUTPUT_KIND_VIRTIO
    } else {
        control::OUTPUT_KIND_OTHER
    }
}

/// Rank of an output's kind for the automatic choice: higher wins.
fn rank(id: &str) -> u8 {
    match kind_of(id) {
        control::OUTPUT_KIND_USB => 4,
        control::OUTPUT_KIND_HDMI => 3,
        control::OUTPUT_KIND_HDA => 2,
        control::OUTPUT_KIND_VIRTIO => 1,
        _ => 0,
    }
}

/// The id of a registered card name, if it is one.
pub fn card_id(name: &str) -> Option<&str> {
    card_id_of(name.strip_prefix(CARD_PREFIX)?)
}

/// What to show for a card before its driver said (`AudioInfo.name`).
pub fn default_name(id: &str) -> String {
    String::from(match kind_of(id) {
        control::OUTPUT_KIND_USB => "USB audio",
        control::OUTPUT_KIND_HDMI => "HDMI audio",
        control::OUTPUT_KIND_HDA => "Built-in audio",
        control::OUTPUT_KIND_VIRTIO => "Virtual sound card",
        _ => "Sound card",
    })
}

struct Present {
    id: String,
    name: String,
    /// The scan it was first seen in.
    seen: u64,
    /// The scan it last failed to open in.
    failed: Option<u64>,
}

/// The outputs present, the one playing and the one preferred.
#[derive(Default)]
pub struct Outputs {
    present: Vec<Present>,
    active: Option<String>,
    preferred: Option<String>,
    scans: u64,
}

impl Outputs {
    pub fn new() -> Outputs {
        Outputs::default()
    }

    /// One registry scan found `ids`: forget the outputs gone, note the new
    /// ones. Whether anything changed.
    pub fn update<'a>(&mut self, ids: impl IntoIterator<Item = &'a str>) -> bool {
        self.scans += 1;
        let ids: Vec<&str> = ids.into_iter().filter_map(|id| card_id_of(id)).collect();
        let before = self.present.len();
        self.present
            .retain(|output| ids.contains(&output.id.as_str()));
        let mut changed = self.present.len() != before;
        for id in ids {
            if self.present.len() < MAX_OUTPUTS && !self.present.iter().any(|o| o.id == id) {
                self.present.push(Present {
                    id: String::from(id),
                    name: default_name(id),
                    seen: self.scans,
                    failed: None,
                });
                changed = true;
            }
        }
        changed
    }

    /// The output the mixer should play through now: never one that just
    /// failed to open while another is present.
    pub fn target(&self) -> Option<&str> {
        let usable = |o: &&Present| o.failed.is_none_or(|at| self.scans >= at + FAILED_SCANS);
        if let Some(preferred) = self.preferred.as_deref() {
            if self
                .present
                .iter()
                .filter(usable)
                .any(|o| o.id == preferred)
            {
                return Some(preferred);
            }
        }
        self.present
            .iter()
            .filter(usable)
            .max_by_key(|o| (rank(&o.id), o.seen))
            .or_else(|| self.present.iter().min_by_key(|o| o.failed))
            .map(|o| o.id.as_str())
    }

    /// Output `id` could not be opened: pass it over for a while, so the
    /// next one plays instead of the mixer retrying a broken device forever.
    pub fn failed(&mut self, id: &str) {
        let scans = self.scans;
        if let Some(output) = self.present.iter_mut().find(|o| o.id == id) {
            output.failed = Some(scans);
        }
    }

    /// The output now playing (`None`: none).
    pub fn set_active(&mut self, id: Option<&str>) {
        self.active = id.map(String::from);
    }

    pub fn active(&self) -> Option<&str> {
        self.active.as_deref()
    }

    /// The name the driver gave an output.
    pub fn set_name(&mut self, id: &str, name: &str) {
        if let Some(output) = self.present.iter_mut().find(|o| o.id == id) {
            if !name.is_empty() {
                output.name = String::from(name);
            }
        }
    }

    /// `SelectOutput`: prefer `id` from now on (`ENOENT` when absent).
    pub fn select(&mut self, id: &str) -> Result<(), i64> {
        if !self.present.iter().any(|o| o.id == id) {
            return Err(errno::ENOENT);
        }
        self.preferred = Some(String::from(id));
        Ok(())
    }

    /// `ListOutputs`: the playing one first, then in the order they came.
    pub fn list(&self) -> Vec<control::Output> {
        let mut outputs: Vec<control::Output> = self
            .present
            .iter()
            .map(|o| control::Output {
                id: o.id.clone(),
                name: o.name.clone(),
                kind: kind_of(&o.id),
                active: self.active.as_deref() == Some(&o.id),
                preferred: self.preferred.as_deref() == Some(&o.id),
            })
            .collect();
        outputs.sort_by_key(|o| !o.active);
        outputs
    }

    /// Answer `ListOutputs` and `SelectOutput` of the control interface;
    /// `None` for any other method (the mixer's own).
    pub fn handle(&mut self, method: u32, body: &[u8]) -> Option<Outcome> {
        let answer = match method {
            control::METHOD_LISTOUTPUTS => {
                control::encode_list_outputs_reply(&control::ListOutputsReply {
                    outputs: self.list(),
                })
                .map(Outcome::Reply)
                .unwrap_or(Outcome::Refuse(errno::EINVAL))
            }
            control::METHOD_SELECTOUTPUT => match control::decode_select_output_args(body) {
                Ok(args) => match self.select(&args.id) {
                    Ok(()) => Outcome::Reply(Vec::new()),
                    Err(code) => Outcome::Refuse(code),
                },
                Err(_) => Outcome::Refuse(errno::EINVAL),
            },
            _ => return None,
        };
        Some(answer)
    }
}

/// An id as `update` takes it: already a bare id, checked like a name's.
fn card_id_of(id: &str) -> Option<&str> {
    let ok = !id.is_empty()
        && id.len() <= MAX_ID
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    ok.then_some(id)
}
