//! Codec verbs and parameters (HDA specification section 7.3), and the
//! [`Verbs`] transport the codec walk talks through.
//!
//! A command is one 32-bit word: codec address (4 bits), node id (8 bits),
//! then either a 12-bit verb with an 8-bit payload or a 4-bit verb with a
//! 16-bit payload. Everything a codec answers is untrusted input.

/// Get a parameter (payload: the parameter id).
pub const GET_PARAMETER: u16 = 0xF00;
/// Get connection list entries (payload: the first index).
pub const GET_CONN_LIST: u16 = 0xF02;
/// Select the active input of a selector, mixer-less pin or converter.
pub const SET_CONN_SELECT: u16 = 0x701;
/// Power state (payload 0: D0, fully on).
pub const SET_POWER_STATE: u16 = 0x705;
/// Converter stream tag (bits 7..4) and lowest channel (bits 3..0).
pub const SET_STREAM_CHANNEL: u16 = 0x706;
/// Pin widget control: output enable, headphone amp.
pub const SET_PIN_CONTROL: u16 = 0x707;
/// External amplifier power down (`EAPD`) and balanced I/O.
pub const SET_EAPD: u16 = 0x70C;
/// The pin's configuration default (what the board wired it to).
pub const GET_CONFIG_DEFAULT: u16 = 0xF1C;
/// 4-bit verbs (16-bit payload).
pub const SET_STREAM_FORMAT: u8 = 0x2;
pub const SET_AMP_GAIN_MUTE: u8 = 0x3;

/// Parameter ids.
pub mod param {
    pub const VENDOR_ID: u8 = 0x00;
    /// Start node (bits 23..16) and count (bits 7..0) of a node's children.
    pub const NODE_COUNT: u8 = 0x04;
    /// Function group type in bits 7..0 (1: audio).
    pub const FUNCTION_TYPE: u8 = 0x05;
    pub const WIDGET_CAPS: u8 = 0x09;
    /// Supported rates (bits 11..0) and sample sizes (bits 20..16).
    pub const PCM: u8 = 0x0A;
    pub const STREAM_FORMATS: u8 = 0x0B;
    pub const PIN_CAPS: u8 = 0x0C;
    pub const AMP_IN_CAPS: u8 = 0x0D;
    /// Connection list length (bits 6..0) and long form (bit 7).
    pub const CONN_LIST_LEN: u8 = 0x0E;
    pub const AMP_OUT_CAPS: u8 = 0x12;
}

/// Widget capability fields.
pub mod caps {
    /// Widget type: bits 23..20.
    pub const TYPE_SHIFT: u32 = 20;
    pub const OUTPUT: u32 = 0x0;
    pub const INPUT: u32 = 0x1;
    pub const MIXER: u32 = 0x2;
    pub const SELECTOR: u32 = 0x3;
    pub const PIN: u32 = 0x4;
    /// Stereo (two channels).
    pub const STEREO: u32 = 1 << 0;
    pub const IN_AMP: u32 = 1 << 1;
    pub const OUT_AMP: u32 = 1 << 2;
    /// The amp parameters are the widget's own, not the function group's.
    pub const AMP_OVERRIDE: u32 = 1 << 3;
    /// The PCM parameters are the widget's own.
    pub const FORMAT_OVERRIDE: u32 = 1 << 4;
    pub const CONN_LIST: u32 = 1 << 8;
    pub const POWER_CONTROL: u32 = 1 << 10;
}

/// Pin capability and control bits.
pub mod pin {
    pub const CAP_OUTPUT: u32 = 1 << 4;
    pub const CAP_HEADPHONE: u32 = 1 << 3;
    pub const CAP_EAPD: u32 = 1 << 16;
    pub const CTL_OUT_ENABLE: u8 = 1 << 6;
    pub const CTL_HP_ENABLE: u8 = 1 << 7;
    pub const EAPD_ON: u8 = 1 << 1;
}

/// Configuration default fields: port connectivity (bits 31..30: 1 means
/// nothing is attached) and the default device (bits 23..20).
pub mod config {
    pub const CONNECTIVITY_SHIFT: u32 = 30;
    pub const NO_CONNECTION: u32 = 1;
    pub const DEVICE_SHIFT: u32 = 20;
    pub const LINE_OUT: u32 = 0x0;
    pub const SPEAKER: u32 = 0x1;
    pub const HEADPHONE: u32 = 0x2;
}

/// `SET_AMP_GAIN_MUTE` payload bits.
pub mod amp {
    pub const OUTPUT: u16 = 1 << 15;
    pub const INPUT: u16 = 1 << 14;
    pub const LEFT: u16 = 1 << 13;
    pub const RIGHT: u16 = 1 << 12;
    pub const INDEX_SHIFT: u16 = 8;
    pub const MUTE: u16 = 1 << 7;
    /// The 0 dB step in an amp capability: bits 6..0.
    pub const OFFSET_MASK: u32 = 0x7F;
}

/// One command word for a 12-bit verb.
pub fn command(codec: u8, nid: u8, verb: u16, payload: u8) -> u32 {
    u32::from(codec & 0xF) << 28
        | u32::from(nid) << 20
        | u32::from(verb & 0xFFF) << 8
        | u32::from(payload)
}

/// One command word for a 4-bit verb with a 16-bit payload.
pub fn command16(codec: u8, nid: u8, verb: u8, payload: u16) -> u32 {
    u32::from(codec & 0xF) << 28
        | u32::from(nid) << 20
        | u32::from(verb & 0xF) << 16
        | u32::from(payload)
}

/// Why a verb got no answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerbError {
    /// The controller did not return a response in time.
    Timeout,
    /// The command rings could not be set up or are stopped.
    Ring,
}

/// Something that can send a command word and return the codec's response.
pub trait Verbs {
    fn send(&mut self, command: u32) -> Result<u32, VerbError>;

    /// `GET_PARAMETER` on `nid` of codec `codec`.
    fn param(&mut self, codec: u8, nid: u8, id: u8) -> Result<u32, VerbError> {
        self.send(command(codec, nid, GET_PARAMETER, id))
    }

    /// A 12-bit verb.
    fn verb(&mut self, codec: u8, nid: u8, verb: u16, payload: u8) -> Result<u32, VerbError> {
        self.send(command(codec, nid, verb, payload))
    }

    /// A 4-bit verb.
    fn verb16(&mut self, codec: u8, nid: u8, verb: u8, payload: u16) -> Result<u32, VerbError> {
        self.send(command16(codec, nid, verb, payload))
    }
}
