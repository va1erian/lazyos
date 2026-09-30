//! Request and reply layouts (virtio 1.2, 5.14.6). All fields are little
//! endian.

/// Control request codes.
pub mod code {
    pub const PCM_INFO: u32 = 0x0100;
    pub const PCM_SET_PARAMS: u32 = 0x0101;
    pub const PCM_PREPARE: u32 = 0x0102;
    pub const PCM_RELEASE: u32 = 0x0103;
    pub const PCM_START: u32 = 0x0104;
    pub const PCM_STOP: u32 = 0x0105;
}

/// Status words in a reply.
pub mod status {
    pub const OK: u32 = 0x8000;
    pub const BAD_MSG: u32 = 0x8001;
    pub const NOT_SUPP: u32 = 0x8002;
    pub const IO_ERR: u32 = 0x8003;
}

/// Stream direction.
pub mod direction {
    pub const OUTPUT: u8 = 0;
    pub const INPUT: u8 = 1;
}

/// Sample formats, by the spec's numbering (only the ones a driver maps).
pub mod format {
    pub const S16: u8 = 5;
    pub const S24: u8 = 15;
    pub const S32: u8 = 17;
    pub const FLOAT: u8 = 19;
}

/// Sample rates, by the spec's numbering.
pub mod rate {
    pub const R5512: u8 = 0;
    pub const R8000: u8 = 1;
    pub const R11025: u8 = 2;
    pub const R16000: u8 = 3;
    pub const R22050: u8 = 4;
    pub const R32000: u8 = 5;
    pub const R44100: u8 = 6;
    pub const R48000: u8 = 7;
    pub const R64000: u8 = 8;
    pub const R88200: u8 = 9;
    pub const R96000: u8 = 10;
    pub const R176400: u8 = 11;
    pub const R192000: u8 = 12;
    pub const R384000: u8 = 13;
}

/// Device configuration space: three little-endian u32 counts.
pub mod config {
    pub const JACKS: u32 = 0;
    pub const STREAMS: u32 = 4;
    pub const CHMAPS: u32 = 8;
    pub const LEN: u32 = 12;
}

/// Size of one `virtio_snd_pcm_info` record as this driver requests it.
pub const PCM_INFO_SIZE: usize = 32;

/// `PCM_INFO` request: header, `start_id`, `count`, `size`.
pub const PCM_INFO_REQUEST_LEN: usize = 16;

/// Reply header: one status word.
pub const STATUS_LEN: usize = 4;

/// A simple stream request (`PREPARE`, `RELEASE`, `START`, `STOP`).
pub const STREAM_REQUEST_LEN: usize = 8;

/// `PCM_SET_PARAMS` request length.
pub const SET_PARAMS_LEN: usize = 24;

/// I/O header of a TX/RX message: the stream id.
pub const XFER_HEADER_LEN: usize = 4;

/// I/O status: status word plus latency in bytes.
pub const XFER_STATUS_LEN: usize = 8;

fn le32(value: u32, out: &mut [u8], at: usize) {
    out[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// Build a `PCM_INFO` request for `count` streams from `start`.
pub fn pcm_info_request(start: u32, count: u32) -> [u8; PCM_INFO_REQUEST_LEN] {
    let mut bytes = [0u8; PCM_INFO_REQUEST_LEN];
    le32(code::PCM_INFO, &mut bytes, 0);
    le32(start, &mut bytes, 4);
    le32(count, &mut bytes, 8);
    le32(PCM_INFO_SIZE as u32, &mut bytes, 12);
    bytes
}

/// Build a `PCM_SET_PARAMS` request. `features` is always 0: this driver uses
/// neither shared memory nor explicit notifications.
pub fn set_params_request(
    stream: u32,
    buffer_bytes: u32,
    period_bytes: u32,
    channels: u8,
    format: u8,
    rate: u8,
) -> [u8; SET_PARAMS_LEN] {
    let mut bytes = [0u8; SET_PARAMS_LEN];
    le32(code::PCM_SET_PARAMS, &mut bytes, 0);
    le32(stream, &mut bytes, 4);
    le32(buffer_bytes, &mut bytes, 8);
    le32(period_bytes, &mut bytes, 12);
    le32(0, &mut bytes, 16);
    bytes[20] = channels;
    bytes[21] = format;
    bytes[22] = rate;
    bytes
}

/// Build a `PREPARE`/`RELEASE`/`START`/`STOP` request.
pub fn stream_request(code: u32, stream: u32) -> [u8; STREAM_REQUEST_LEN] {
    let mut bytes = [0u8; STREAM_REQUEST_LEN];
    le32(code, &mut bytes, 0);
    le32(stream, &mut bytes, 4);
    bytes
}

/// The header that precedes the samples of a TX (or RX) message.
pub fn xfer_header(stream: u32) -> [u8; XFER_HEADER_LEN] {
    stream.to_le_bytes()
}

/// The status word at the start of a reply, or `None` for a short reply.
pub fn parse_status(reply: &[u8]) -> Option<u32> {
    Some(u32::from_le_bytes(reply.get(..4)?.try_into().ok()?))
}

/// The status and latency of a TX/RX completion.
pub fn parse_xfer_status(reply: &[u8]) -> Option<(u32, u32)> {
    let status = u32::from_le_bytes(reply.get(..4)?.try_into().ok()?);
    let latency = u32::from_le_bytes(reply.get(4..8)?.try_into().ok()?);
    Some((status, latency))
}

/// One stream's capabilities from a `PCM_INFO` reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PcmInfo {
    /// Bitmap over the [`format`] numbering.
    pub formats: u64,
    /// Bitmap over the [`rate`] numbering.
    pub rates: u64,
    pub direction: u8,
    pub channels_min: u8,
    pub channels_max: u8,
}

impl PcmInfo {
    /// Parse the record for `index` from the bytes after a reply's status
    /// word. Records are [`PCM_INFO_SIZE`] bytes: `hda_fn_nid`, `features`,
    /// `formats`, `rates`, `direction`, `channels_min`, `channels_max`, padding.
    pub fn parse(records: &[u8], index: usize) -> Option<PcmInfo> {
        let start = index.checked_mul(PCM_INFO_SIZE)?;
        let record = records.get(start..start.checked_add(PCM_INFO_SIZE)?)?;
        Some(PcmInfo {
            formats: u64::from_le_bytes(record[8..16].try_into().ok()?),
            rates: u64::from_le_bytes(record[16..24].try_into().ok()?),
            direction: record[24],
            channels_min: record[25],
            channels_max: record[26],
        })
    }

    pub fn supports_format(&self, format: u8) -> bool {
        format < 64 && self.formats >> format & 1 == 1
    }

    pub fn supports_rate(&self, rate: u8) -> bool {
        rate < 64 && self.rates >> rate & 1 == 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_info_request_layout() {
        let bytes = pcm_info_request(0, 2);
        assert_eq!(&bytes[..4], &[0x00, 0x01, 0, 0]);
        assert_eq!(&bytes[4..8], &0u32.to_le_bytes());
        assert_eq!(&bytes[8..12], &2u32.to_le_bytes());
        assert_eq!(&bytes[12..], &32u32.to_le_bytes());
    }

    #[test]
    fn set_params_layout_matches_the_spec() {
        let bytes = set_params_request(1, 32768, 8192, 2, format::S16, rate::R48000);
        assert_eq!(bytes.len(), 24);
        assert_eq!(u32::from_le_bytes(bytes[0..4].try_into().unwrap()), 0x0101);
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()), 32768);
        assert_eq!(u32::from_le_bytes(bytes[12..16].try_into().unwrap()), 8192);
        assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 0);
        assert_eq!(&bytes[20..], &[2, 5, 7, 0]);
    }

    #[test]
    fn stream_requests_carry_code_and_id() {
        let bytes = stream_request(code::PCM_START, 3);
        assert_eq!(bytes, [0x04, 0x01, 0, 0, 3, 0, 0, 0]);
        assert_eq!(xfer_header(3), [3, 0, 0, 0]);
    }

    fn record(formats: u64, rates: u64, direction: u8, min: u8, max: u8) -> [u8; 32] {
        let mut record = [0u8; 32];
        record[8..16].copy_from_slice(&formats.to_le_bytes());
        record[16..24].copy_from_slice(&rates.to_le_bytes());
        record[24] = direction;
        record[25] = min;
        record[26] = max;
        record
    }

    #[test]
    fn pcm_info_parses_and_answers_support_queries() {
        let mut bytes = [0u8; 64];
        bytes[..32].copy_from_slice(&record(1 << format::S16, 1 << rate::R48000, 0, 1, 2));
        bytes[32..].copy_from_slice(&record(0, 0, direction::INPUT, 2, 2));
        let info = PcmInfo::parse(&bytes, 0).expect("stream 0");
        assert!(info.supports_format(format::S16));
        assert!(!info.supports_format(format::S32));
        assert!(info.supports_rate(rate::R48000));
        assert!(!info.supports_rate(rate::R44100));
        assert_eq!(
            (info.direction, info.channels_min, info.channels_max),
            (0, 1, 2)
        );
        assert_eq!(
            PcmInfo::parse(&bytes, 1).expect("stream 1").direction,
            direction::INPUT
        );
    }

    #[test]
    fn short_or_out_of_range_replies_are_rejected() {
        let bytes = [0u8; 40];
        assert!(PcmInfo::parse(&bytes, 0).is_some());
        assert!(PcmInfo::parse(&bytes, 1).is_none()); // second record truncated
        assert!(PcmInfo::parse(&bytes, usize::MAX).is_none());
        assert_eq!(parse_status(&[0x00, 0x80]), None);
        assert_eq!(parse_status(&[0x00, 0x80, 0, 0]), Some(status::OK));
        assert_eq!(parse_xfer_status(&[0; 7]), None);
        assert_eq!(
            parse_xfer_status(&[1, 0x80, 0, 0, 9, 0, 0, 0]),
            Some((0x8001, 9))
        );
    }

    #[test]
    fn format_and_rate_queries_do_not_overflow_the_shift() {
        let info = PcmInfo {
            formats: u64::MAX,
            rates: u64::MAX,
            direction: 0,
            channels_min: 1,
            channels_max: 2,
        };
        assert!(info.supports_format(63));
        assert!(!info.supports_format(64));
        assert!(!info.supports_rate(200));
    }
}
