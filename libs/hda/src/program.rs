//! Switching an output path on: power, connection selects, pin control,
//! external amplifier, amps unmuted at 0 dB, and the converter bound to a
//! stream tag and format.

use crate::codec::{Graph, Path, Widget};
use crate::verbs::{amp, caps, config, param, pin, VerbError, Verbs};
use crate::verbs::{SET_AMP_GAIN_MUTE, SET_CONN_SELECT, SET_EAPD, SET_PIN_CONTROL};
use crate::verbs::{SET_POWER_STATE, SET_STREAM_CHANNEL, SET_STREAM_FORMAT};

/// The codec and its graph, as the path programming needs them.
pub struct Output<'a> {
    pub codec: u8,
    pub graph: &'a Graph,
    pub path: Path,
}

impl Output<'_> {
    fn widget(&self, nid: u8) -> Option<&Widget> {
        self.graph.widget(nid)
    }

    /// The amp capability `id` that applies to `widget`: its own when it
    /// overrides, the function group's otherwise.
    fn amp_caps(&self, verbs: &mut impl Verbs, widget: &Widget, id: u8) -> Result<u32, VerbError> {
        let nid = if widget.caps & caps::AMP_OVERRIDE != 0 {
            widget.nid
        } else {
            self.graph.afg
        };
        verbs.param(self.codec, nid, id)
    }

    /// Unmute an amp at its 0 dB step (the capability's offset).
    fn unmute(
        &self,
        verbs: &mut impl Verbs,
        widget: &Widget,
        output: bool,
        index: u8,
    ) -> Result<(), VerbError> {
        let (present, id, side) = if output {
            (caps::OUT_AMP, param::AMP_OUT_CAPS, amp::OUTPUT)
        } else {
            (caps::IN_AMP, param::AMP_IN_CAPS, amp::INPUT)
        };
        if widget.caps & present == 0 {
            return Ok(());
        }
        let gain = (self.amp_caps(verbs, widget, id)? & amp::OFFSET_MASK) as u16;
        let payload =
            side | amp::LEFT | amp::RIGHT | u16::from(index & 0xF) << amp::INDEX_SHIFT | gain;
        verbs.verb16(self.codec, widget.nid, SET_AMP_GAIN_MUTE, payload)?;
        Ok(())
    }

    /// Power the path, route it and open every amp on it. The converter is
    /// bound to a stream by [`Output::bind`].
    pub fn enable(&self, verbs: &mut impl Verbs) -> Result<(), VerbError> {
        verbs.verb(self.codec, self.graph.afg, SET_POWER_STATE, 0)?;
        for hop in self.path.hops() {
            let Some(widget) = self.widget(hop.nid) else {
                continue;
            };
            if widget.caps & caps::POWER_CONTROL != 0 {
                verbs.verb(self.codec, widget.nid, SET_POWER_STATE, 0)?;
            }
            if let Some(input) = hop.input {
                match widget.kind() {
                    caps::MIXER => self.unmute(verbs, widget, false, input)?,
                    _ if widget.conn_count > 1 => {
                        verbs.verb(self.codec, widget.nid, SET_CONN_SELECT, input)?;
                    }
                    _ => {}
                }
            }
            self.unmute(verbs, widget, true, 0)?;
            if widget.kind() == caps::PIN {
                self.enable_pin(verbs, widget)?;
            }
        }
        Ok(())
    }

    fn enable_pin(&self, verbs: &mut impl Verbs, widget: &Widget) -> Result<(), VerbError> {
        let headphone = (widget.config >> config::DEVICE_SHIFT) & 0xF == config::HEADPHONE
            && widget.pin_caps & pin::CAP_HEADPHONE != 0;
        let control = pin::CTL_OUT_ENABLE | if headphone { pin::CTL_HP_ENABLE } else { 0 };
        verbs.verb(self.codec, widget.nid, SET_PIN_CONTROL, control)?;
        if widget.pin_caps & pin::CAP_EAPD != 0 {
            verbs.verb(self.codec, widget.nid, SET_EAPD, pin::EAPD_ON)?;
        }
        Ok(())
    }

    /// Bind the converter to stream `tag` at stream format `format`; tag 0
    /// unbinds it (the converter then ignores the link).
    pub fn bind(&self, verbs: &mut impl Verbs, tag: u8, format: u16) -> Result<(), VerbError> {
        let dac = self.path.dac();
        verbs.verb16(self.codec, dac, SET_STREAM_FORMAT, format)?;
        verbs.verb(self.codec, dac, SET_STREAM_CHANNEL, (tag & 0xF) << 4)?;
        Ok(())
    }

    /// The converter's supported rates and sample sizes (`param::PCM`): its
    /// own when it overrides the function group's.
    pub fn pcm(&self, verbs: &mut impl Verbs) -> Result<u32, VerbError> {
        let dac = self.path.dac();
        let own = self
            .widget(dac)
            .is_some_and(|widget| widget.caps & caps::FORMAT_OVERRIDE != 0);
        verbs.param(
            self.codec,
            if own { dac } else { self.graph.afg },
            param::PCM,
        )
    }

    /// Channels the converter carries: two for a stereo widget.
    pub fn channels(&self) -> u8 {
        match self.widget(self.path.dac()) {
            Some(widget) if widget.caps & caps::STEREO != 0 => 2,
            _ => 1,
        }
    }
}
