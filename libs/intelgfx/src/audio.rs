//! The display side of HDMI audio (`docs/kabylake-gpu-plan.md`, stage G2).
//!
//! The HDA codec only sees a sink once the display engine has been told about
//! it: the sink's ELD must be in the engine's per-pipe ELD buffer with its
//! valid bit set, the port's audio output enabled, and the clock
//! regeneration `N` programmed. This module is that sequence over [`Mmio`],
//! checked on a host by a register-file fake that enforces the ordering rules
//! (`tests_audio`); it is the first code in this crate that **writes**.
//!
//! Sequence (i915's `hsw_audio_codec_enable`, from memory, so every offset
//! and bit used here is **to confirm** on the box before the first write; the
//! two offsets the GOP capture did answer are noted in [`regs`]):
//!
//! 1. refuse unless the transcoder is enabled and in HDMI mode: this never
//!    rewrites a live link (a DVI-mode link is the case plan section 4 calls
//!    a partial mode set, and it is out of this module's reach on purpose);
//! 2. output enable on, ELD valid off;
//! 3. reset the ELD write address, write the ELD one dword at a time;
//! 4. `AUD_CONFIG`: the pixel-clock index (or an explicit `N`), read back;
//! 5. ELD valid on, last, so a sink never looks ready with the clock unset.
//!
//! (i915 sets valid before the clock; the order here is deliberate.) Any
//! failure after step 2 runs [`disable`]. Disabling is valid off, then output
//! enable off.

use alloc::vec::Vec;

use crate::adopt::{Adopted, Display};
use crate::eld::{ConnType, Eld};
use crate::hdmi_audio::{self, Rate};
use crate::regs::{
    self, aud_config, DdiFunc, DdiMode, AUD_CONFIG_A, AUD_DIP_ELD_CTRL_ST_A, AUD_HDMIW_HDMIEDID_A,
    AUD_PIN_ELD_CP_VLD, AUD_PIPE_STRIDE,
};
use crate::Mmio;

/// The hardware ELD buffer holds 84 bytes (21 dwords); a longer ELD cannot
/// be uploaded.
pub const ELD_BUFFER_BYTES: usize = 84;

/// `AUD_PIN_ELD_CP_VLD` holds one nibble per transcoder (A at bit 0).
const NIBBLE_BITS: u32 = 4;
const ELD_VALID: u32 = 1 << 0;
const OUTPUT_ENABLE: u32 = 1 << 2;

/// `AUD_DIP_ELD_CTRL_ST`: the ELD write address (bits 9:5).
const ELD_ADDRESS_MASK: u32 = 0x1F << 5;

/// Whether `gpud` may write the register at `offset` while its audio grant
/// is open: the audio block of transcoders A to C and nothing else (no
/// display, plane, clock or pipe register, which is what keeps G2 from being
/// a mode set). The ELD data port is write-only here; reads of it stay out of
/// the probe allowlist.
pub fn may_write(offset: u32) -> bool {
    let base = |reg: u32| (0..3).map(move |t| reg + t * AUD_PIPE_STRIDE);
    offset.is_multiple_of(4)
        && (base(AUD_CONFIG_A)
            .chain(base(AUD_DIP_ELD_CTRL_ST_A))
            .chain(base(AUD_HDMIW_HDMIEDID_A))
            .any(|reg| reg == offset)
            || offset == AUD_PIN_ELD_CP_VLD)
}

/// What a request asks of the display engine.
#[derive(Clone, Copy, Debug)]
pub struct Request<'a> {
    /// The transcoder (0 = A) feeding the HDMI port; it is also the pipe
    /// whose audio register block is used.
    pub transcoder: u8,
    /// The ELD as `Eld::to_bytes` builds it.
    pub eld: &'a [u8],
    /// The TMDS character clock (`hdmi_audio::tmds_clock_khz`).
    pub tmds_khz: u32,
    /// The `N` to program when the clock has no index in the engine's table.
    pub n: Option<u32>,
}

/// Why a request was refused before anything was written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Only transcoders A to C have an audio block.
    TransOutOfRange(u8),
    /// The ELD is empty, not a whole number of dwords, or longer than the
    /// hardware buffer.
    BadEld(usize),
    /// The transcoder is off (register value attached).
    TranscoderOff(u32),
    /// The link is not in HDMI mode: audio would need a mode set.
    NotHdmi(DdiMode),
    /// The clock has no table index and no explicit `N` was given.
    NoClockIndex(u32),
    /// A register did not read back what was written (offset, wrote, read).
    Readback(u32, u32, u32),
}

/// What [`enable`] did, for the `GPU:AUDIO` marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Enabled {
    pub eld_dwords: u8,
    pub config: u32,
}

fn pipe_reg(base: u32, transcoder: u8) -> u32 {
    base + u32::from(transcoder) * AUD_PIPE_STRIDE
}

fn shift(transcoder: u8) -> u32 {
    u32::from(transcoder) * NIBBLE_BITS
}

/// Check a request against the link without touching anything: the
/// transcoder must be running an HDMI link.
pub fn check(m: &mut impl Mmio, req: &Request) -> Result<(), Error> {
    if req.transcoder > 2 {
        return Err(Error::TransOutOfRange(req.transcoder));
    }
    let len = req.eld.len();
    if len == 0 || !len.is_multiple_of(4) || len > ELD_BUFFER_BYTES {
        return Err(Error::BadEld(len));
    }
    let raw = m.read32(regs::TRANS_DDI_FUNC_CTL_A + u32::from(req.transcoder) * regs::TRANS_STRIDE);
    let func = DdiFunc::decode(raw);
    if !func.enabled {
        return Err(Error::TranscoderOff(raw));
    }
    if func.mode != DdiMode::Hdmi {
        return Err(Error::NotHdmi(func.mode));
    }
    if crate::hdmi_audio::aud_config_clock_index(req.tmds_khz).is_none() && req.n.is_none() {
        return Err(Error::NoClockIndex(req.tmds_khz));
    }
    Ok(())
}

/// The `AUD_CONFIG` value for a clock: the table index, or the explicit `N`.
pub fn config_value(old: u32, tmds_khz: u32, n: Option<u32>) -> Option<u32> {
    let mut v = old
        & !(aud_config::CLOCK_MASK
            | aud_config::N_VALUE_INDEX
            | aud_config::N_PROG_ENABLE
            | aud_config::UPPER_N_MASK
            | aud_config::LOWER_N_MASK);
    match crate::hdmi_audio::aud_config_clock_index(tmds_khz) {
        Some(index) => v |= index << aud_config::CLOCK_SHIFT,
        None => v |= aud_config::n_fields(n?),
    }
    Some(v)
}

/// The `AUD_CONFIG` bits this module programs: a readback is judged on these
/// alone, since the register's other bits (reserved, read-only) may read back
/// differently from what the read-modify-write carried over.
const CONFIG_PROGRAMMED: u32 = aud_config::CLOCK_MASK
    | aud_config::N_VALUE_INDEX
    | aud_config::N_PROG_ENABLE
    | aud_config::UPPER_N_MASK
    | aud_config::LOWER_N_MASK;

/// Run the enable sequence. Nothing is written when [`check`] refuses; a
/// failure after the first write undoes it ([`disable`]), so an error never
/// leaves the output on or an ELD valid.
pub fn enable(m: &mut impl Mmio, req: &Request) -> Result<Enabled, Error> {
    check(m, req)?;
    program(m, req).inspect_err(|_| {
        let _ = disable(m, req.transcoder);
    })
}

fn program(m: &mut impl Mmio, req: &Request) -> Result<Enabled, Error> {
    let t = req.transcoder;

    // Output enable on, ELD invalid, so the codec sees "sink present, no
    // ELD yet" while the buffer is filled.
    let v = m.read32(AUD_PIN_ELD_CP_VLD);
    m.write32(
        AUD_PIN_ELD_CP_VLD,
        (v | OUTPUT_ENABLE << shift(t)) & !(ELD_VALID << shift(t)),
    );

    // Reset the write address, then fill the buffer.
    let ctrl = pipe_reg(AUD_DIP_ELD_CTRL_ST_A, t);
    let c = m.read32(ctrl);
    m.write32(ctrl, c & !ELD_ADDRESS_MASK);
    let data = pipe_reg(AUD_HDMIW_HDMIEDID_A, t);
    for dword in req.eld.as_chunks::<4>().0 {
        m.write32(data, u32::from_le_bytes(*dword));
    }

    // Clock regeneration, checked before the ELD is declared valid: the
    // codec starts streaming once it sees a valid ELD, and must not find an
    // unprogrammed clock behind it.
    let cfg = pipe_reg(AUD_CONFIG_A, t);
    let value = config_value(m.read32(cfg), req.tmds_khz, req.n)
        .ok_or(Error::NoClockIndex(req.tmds_khz))?;
    m.write32(cfg, value);
    let back = m.read32(cfg);
    if back & CONFIG_PROGRAMMED != value & CONFIG_PROGRAMMED {
        return Err(Error::Readback(cfg, value, back));
    }

    // ELD valid.
    let v = m.read32(AUD_PIN_ELD_CP_VLD);
    m.write32(AUD_PIN_ELD_CP_VLD, v | ELD_VALID << shift(t));
    Ok(Enabled {
        eld_dwords: (req.eld.len() / 4) as u8,
        config: value,
    })
}

/// Change only `N`/the clock index (the stream's rate changed).
pub fn set_clock(
    m: &mut impl Mmio,
    transcoder: u8,
    tmds_khz: u32,
    n: Option<u32>,
) -> Result<u32, Error> {
    if transcoder > 2 {
        return Err(Error::TransOutOfRange(transcoder));
    }
    let cfg = pipe_reg(AUD_CONFIG_A, transcoder);
    let value = config_value(m.read32(cfg), tmds_khz, n).ok_or(Error::NoClockIndex(tmds_khz))?;
    m.write32(cfg, value);
    Ok(value)
}

/// ELD invalid, then output off. Idempotent.
pub fn disable(m: &mut impl Mmio, transcoder: u8) -> Result<(), Error> {
    if transcoder > 2 {
        return Err(Error::TransOutOfRange(transcoder));
    }
    let v = m.read32(AUD_PIN_ELD_CP_VLD);
    let v = v & !(ELD_VALID << shift(transcoder));
    m.write32(AUD_PIN_ELD_CP_VLD, v);
    m.write32(
        AUD_PIN_ELD_CP_VLD,
        v & !(OUTPUT_ENABLE << shift(transcoder)),
    );
    Ok(())
}

/// An owned request: what [`prepare`] derives from the adopted state and the
/// sink's EDID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    pub transcoder: u8,
    pub eld: Vec<u8>,
    pub tmds_khz: u32,
    pub n: Option<u32>,
    /// The `N` is for this sample rate (it follows the stream's rate).
    pub rate: Rate,
}

impl Prepared {
    pub fn request(&self) -> Request<'_> {
        Request {
            transcoder: self.transcoder,
            eld: &self.eld,
            tmds_khz: self.tmds_khz,
            n: self.n,
        }
    }
}

/// Why nothing could be prepared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrepareError {
    /// Only the Gen9 audio block is known; display 13's is not confirmed.
    Unsupported(Display),
    /// No running pipe.
    NoPipe,
    /// The running pipe's link is not HDMI.
    NotHdmi,
    /// No pixel clock could be decoded from the pipe.
    NoClock,
    /// The EDID did not parse.
    BadEdid(crate::edid::EdidError),
    /// The EDID has no CEA extension, so the sink says nothing about audio.
    NoAudio,
    /// The sink lists no LPCM format (an ELD with none would make the codec
    /// refuse every stream).
    NoLpcm,
}

/// Derive the request for the adopted pipe and the sink's EDID, with `N` for
/// `rate`. Pure: nothing is read or written.
pub fn prepare(adopted: &Adopted, edid: &[u8], rate: Rate) -> Result<Prepared, PrepareError> {
    if adopted.display != Display::Gen9 {
        return Err(PrepareError::Unsupported(adopted.display));
    }
    let pipe = adopted.active.as_ref().ok_or(PrepareError::NoPipe)?;
    if !pipe.hdmi_link() {
        return Err(PrepareError::NotHdmi);
    }
    let pixel_khz = pipe.pixel_clock_khz.ok_or(PrepareError::NoClock)?;
    let tmds_khz = hdmi_audio::tmds_clock_khz(
        pixel_khz,
        u32::from(pipe.func.bits_per_component.unwrap_or(8)),
    );
    let edid = crate::edid::Edid::parse(edid).map_err(PrepareError::BadEdid)?;
    let eld = Eld::from_edid(&edid, ConnType::Hdmi, [0; 8]).ok_or(PrepareError::NoAudio)?;
    if eld.lpcm_rates() == 0 {
        return Err(PrepareError::NoLpcm);
    }
    let n = match hdmi_audio::aud_config_clock_index(tmds_khz) {
        Some(_) => None,
        None => hdmi_audio::select(tmds_khz, rate).map(|pair| pair.n),
    };
    Ok(Prepared {
        transcoder: pipe.pipe,
        eld: eld.to_bytes(),
        tmds_khz,
        n,
        rate,
    })
}
