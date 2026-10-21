//! HDMI output pins (docs/kabylake-gpu-plan.md, G2, codec side).
//!
//! The generic walk in [`crate::codec`] takes the first wired output pin, which
//! on an Intel display codec is a pin with no monitor behind it: every pin of
//! the codec is "wired" (its configuration default says jack), and only the
//! one whose port has a sink says so through its presence-detect bit. So an
//! HDMI codec is driven differently: pick the pin that senses a monitor with a
//! valid ELD, bind the stream to the codec's converters, turn on the digital
//! output, tell the sink the stream's layout with an Audio InfoFrame.
//!
//! Verbs are HDA 1.0a section 7.3.3 and Intel's HDMI codec as `patch_hdmi.c`
//! drives it; the bit layouts here are from that, and **to confirm** on the
//! Kaby Lake box (the codec there answers differently from the survey: see
//! the `SNDD:PROBE` lines).

use crate::codec::{Graph, Hop, Path, Widget};
use crate::verbs::{
    caps, VerbError, Verbs, SET_PIN_CONTROL, SET_POWER_STATE, SET_STREAM_CHANNEL, SET_STREAM_FORMAT,
};

pub const GET_PIN_SENSE: u16 = 0xF09;
pub const GET_DIGI_CONVERT: u16 = 0xF0D;
pub const SET_DIGI_CONVERT_1: u16 = 0x70D;
pub const SET_CHAN_COUNT: u16 = 0x72D;
pub const SET_DIP_INDEX: u16 = 0x730;
pub const SET_DIP_DATA: u16 = 0x731;
pub const SET_DIP_XMIT: u16 = 0x732;
pub const SET_CHAN_SLOT: u16 = 0x734;

/// Pin capability: HDMI (bit 24).
pub const PIN_CAP_HDMI: u32 = 1 << 24;
/// `GET_PIN_SENSE`: a sink is present (bit 31) and its ELD is valid (bit 30).
pub const SENSE_PRESENT: u32 = 1 << 31;
pub const SENSE_ELD_VALID: u32 = 1 << 30;
/// Pin control: output enable.
const PIN_OUT: u8 = 1 << 6;
/// Digital converter control 1: digital output enabled.
const DIGI_ENABLE: u8 = 1;
/// `SET_DIP_XMIT`: stop, and send every frame ("best effort": each vsync).
const XMIT_STOP: u8 = 0x00;
const XMIT_BEST: u8 = 0xC0;
/// Slot value of an unused channel position in `SET_CHAN_SLOT`.
const SLOT_UNUSED: u8 = 0xF;
/// Converters driven at once.
pub const MAX_CONVERTERS: usize = 8;
/// Bytes of an Audio InfoFrame as sent: 4 of header, 10 of payload.
pub const INFOFRAME_BYTES: usize = 14;

/// The HDMI output chosen: the pin and the converters its stream is bound to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hdmi {
    pub pin: u8,
    pub converters: [u8; MAX_CONVERTERS],
    pub count: usize,
    /// The pin's ELD was valid when it was chosen.
    pub eld_valid: bool,
}

impl Hdmi {
    pub fn converters(&self) -> &[u8] {
        &self.converters[..self.count]
    }
}

fn is_hdmi_pin(widget: &Widget) -> bool {
    widget.kind() == caps::PIN && widget.pin_caps & PIN_CAP_HDMI != 0
}

/// Put the function group in D0. An HDMI pin reports no sink while the group
/// is asleep, so this comes before [`find`]; the codec then needs a moment
/// (the caller retries `find`).
pub fn power_up(verbs: &mut impl Verbs, codec: u8, graph: &Graph) -> Result<(), VerbError> {
    verbs.verb(codec, graph.afg, SET_POWER_STATE, 0).map(drop)
}

/// The pin that senses a sink (preferring one with a valid ELD), or `None`
/// when the codec has no HDMI pin or none has a sink.
pub fn find(verbs: &mut impl Verbs, codec: u8, graph: &Graph) -> Result<Option<Hdmi>, VerbError> {
    let mut best: Option<(u8, bool)> = None;
    for widget in graph.widgets().filter(|w| is_hdmi_pin(w)) {
        let sense = verbs.verb(codec, widget.nid, GET_PIN_SENSE, 0)?;
        if sense & SENSE_PRESENT == 0 {
            continue;
        }
        let valid = sense & SENSE_ELD_VALID != 0;
        if best.is_none_or(|(_, had)| valid && !had) {
            best = Some((widget.nid, valid));
        }
    }
    let Some((pin, eld_valid)) = best else {
        return Ok(None);
    };
    let mut hdmi = Hdmi {
        pin,
        converters: [0; MAX_CONVERTERS],
        count: 0,
        eld_valid,
    };
    // The pin's own connections that are converters come first, then every
    // other converter of the group: the connection lists of this codec were
    // seen to disagree with the survey, so a stream is bound to all of them.
    let own = graph.widget(pin).map(|w| w.connections()).unwrap_or(&[]);
    let converters = own
        .iter()
        .filter_map(|&nid| graph.widget(nid))
        .chain(graph.widgets())
        .filter(|w| w.kind() == caps::OUTPUT);
    for widget in converters {
        if hdmi.count < MAX_CONVERTERS && !hdmi.converters().contains(&widget.nid) {
            hdmi.converters[hdmi.count] = widget.nid;
            hdmi.count += 1;
        }
    }
    Ok((hdmi.count > 0).then_some(hdmi))
}

/// The path [`crate::program::Output`] drives: the pin and its first converter.
pub fn path(hdmi: &Hdmi) -> Path {
    let mut path = Path {
        hops: [Hop {
            nid: 0,
            input: None,
        }; crate::codec::MAX_PATH],
        len: 2,
    };
    path.hops[0] = Hop {
        nid: hdmi.pin,
        input: Some(0),
    };
    path.hops[1] = Hop {
        nid: hdmi.converters[0],
        input: None,
    };
    path
}

/// The Audio InfoFrame for `channels` channels of linear PCM (CTA-861 section
/// 6.6): header, checksum, then ten payload bytes of which only the channel
/// count is set (the rest say "refer to the stream header").
pub fn audio_infoframe(channels: u8) -> [u8; INFOFRAME_BYTES] {
    let mut frame = [0u8; INFOFRAME_BYTES];
    frame[0] = 0x84;
    frame[1] = 0x01;
    frame[2] = 0x0A;
    frame[4] = channels.clamp(1, 8) - 1;
    let sum = frame.iter().fold(0u8, |sum, &byte| sum.wrapping_add(byte));
    frame[3] = 0u8.wrapping_sub(sum);
    frame
}

/// Send the Audio InfoFrame through the pin's data-island buffer 0.
pub fn send_infoframe(
    verbs: &mut impl Verbs,
    codec: u8,
    pin: u8,
    channels: u8,
) -> Result<(), VerbError> {
    verbs.verb(codec, pin, SET_DIP_XMIT, XMIT_STOP)?;
    verbs.verb(codec, pin, SET_DIP_INDEX, 0)?;
    for byte in audio_infoframe(channels) {
        verbs.verb(codec, pin, SET_DIP_DATA, byte)?;
    }
    verbs.verb(codec, pin, SET_DIP_XMIT, XMIT_BEST)?;
    Ok(())
}

/// Power the pin, enable its output and the converters' power.
pub fn enable(verbs: &mut impl Verbs, codec: u8, hdmi: &Hdmi) -> Result<(), VerbError> {
    verbs.verb(codec, hdmi.pin, SET_POWER_STATE, 0)?;
    verbs.verb(codec, hdmi.pin, SET_PIN_CONTROL, PIN_OUT)?;
    for &nid in hdmi.converters() {
        verbs.verb(codec, nid, SET_POWER_STATE, 0)?;
    }
    Ok(())
}

/// Bind every converter to stream `tag` (0 unbinds them), turn on its
/// digital output and set its channel count and slot map; with a stream, send
/// the Audio InfoFrame.
pub fn bind(
    verbs: &mut impl Verbs,
    codec: u8,
    hdmi: &Hdmi,
    tag: u8,
    format: u16,
    channels: u8,
) -> Result<(), VerbError> {
    let channels = channels.clamp(1, 8);
    for &nid in hdmi.converters() {
        verbs.verb16(codec, nid, SET_STREAM_FORMAT, format)?;
        verbs.verb(codec, nid, SET_STREAM_CHANNEL, (tag & 0xF) << 4)?;
        let digital = verbs.verb(codec, nid, GET_DIGI_CONVERT, 0)? as u8;
        verbs.verb(codec, nid, SET_DIGI_CONVERT_1, digital | DIGI_ENABLE)?;
        verbs.verb(codec, nid, SET_CHAN_COUNT, channels - 1)?;
        for slot in 0..8u8 {
            let channel = if slot < channels { slot } else { SLOT_UNUSED };
            verbs.verb(codec, nid, SET_CHAN_SLOT, slot << 4 | channel)?;
        }
    }
    if tag != 0 {
        send_infoframe(verbs, codec, hdmi.pin, channels)?;
    }
    Ok(())
}
