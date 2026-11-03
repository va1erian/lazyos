//! The Intel `.sfi` firmware container, far enough to download it.
//!
//! A file is a signed header followed by a stream of HCI commands the
//! bootloader executes as they arrive:
//!
//! ```text
//! RSA   0..128 CSS header    128..384 public key    388..644 signature
//! ECDSA 644 + 0..128 CSS header, +128..224 key, +224..320 signature (newer parts)
//! command stream: opcode:u16, length:u8, parameters, repeated
//! ```
//!
//! The parts that carry both signature schemes pick one by the `sbe_type` of
//! the version answer. The offsets, the fragment types and the grouping of the
//! command stream are the documented behaviour of Linux's `btintel_download_*`
//! functions (`drivers/bluetooth/btintel.c`) and were checked against the
//! files in linux-firmware; the parser below is independent.
//!
//! A file is untrusted input, so [`Sfi::parse`] walks the whole command stream
//! once, before anything is sent, and refuses a command that runs past the end
//! or a stream that cannot be cut into 4-octet-aligned groups. After that the
//! lazy [`Sfi::next_group`] cannot fail.

use alloc::vec::Vec;
use core::ops::Range;

/// Size of the RSA header block; the first command, or the ECDSA block, follows.
pub(crate) const RSA_LEN: usize = 644;
const ECDSA_LEN: usize = 320;
const CSS_VERSION_AT: usize = 8;
const CSS_RSA: u32 = 0x0001_0000;
const CSS_ECDSA: u32 = 0x0002_0000;
/// The first octet of an ECDSA block (its CSS module type).
const ECDSA_MAGIC: u8 = 0x06;
/// `Intel_Write_Boot_Params`: boot address and the build the file carries.
const WRITE_BOOT_PARAMS: u16 = 0xfc0e;

/// Secure-send fragment types.
pub(crate) const FRAG_INIT: u8 = 0x00;
pub(crate) const FRAG_DATA: u8 = 0x01;
pub(crate) const FRAG_SIGN: u8 = 0x02;
pub(crate) const FRAG_PKEY: u8 = 0x03;

/// Which of the file's signature blocks the controller verifies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Layout {
    /// Legacy parts and TLV parts up to hardware variant 0x14: RSA only.
    Rsa,
    /// Hardware variant 0x17 and later: RSA and ECDSA blocks, `sbe_type`
    /// (0 RSA, 1 ECDSA) picks the one to send.
    Both { sbe_type: u8 },
}

pub(crate) struct Sfi {
    data: Vec<u8>,
    headers: [(u8, Range<usize>); 3],
    body: usize,
    /// Boot address from the file's boot parameters (0 when it has none).
    pub boot_addr: u32,
    /// Build number, week, year from the same command.
    pub version: Option<(u8, u8, u8)>,
}

impl Sfi {
    pub fn parse(data: Vec<u8>, layout: Layout) -> Result<Sfi, &'static str> {
        if data.len() < RSA_LEN {
            return Err("file shorter than the RSA header");
        }
        if css_version(&data, 0) != Some(CSS_RSA) {
            return Err("unknown CSS header version");
        }
        let rsa = [
            (FRAG_INIT, 0..128),
            (FRAG_PKEY, 128..384),
            (FRAG_SIGN, 388..RSA_LEN),
        ];
        let (headers, body) = match layout {
            Layout::Rsa => (rsa, RSA_LEN),
            Layout::Both { sbe_type } => {
                let body = RSA_LEN + ECDSA_LEN;
                if data.len() < body || data[RSA_LEN] != ECDSA_MAGIC {
                    return Err("no ECDSA block after the RSA header");
                }
                if css_version(&data, RSA_LEN) != Some(CSS_ECDSA) {
                    return Err("unknown ECDSA CSS header version");
                }
                match sbe_type {
                    0 => (rsa, body),
                    1 => (
                        [
                            (FRAG_INIT, RSA_LEN..RSA_LEN + 128),
                            (FRAG_PKEY, RSA_LEN + 128..RSA_LEN + 224),
                            (FRAG_SIGN, RSA_LEN + 224..body),
                        ],
                        body,
                    ),
                    _ => return Err("unknown secure boot engine type"),
                }
            }
        };
        let mut sfi = Sfi {
            data,
            headers,
            body,
            boot_addr: 0,
            version: None,
        };
        sfi.validate_body()?;
        Ok(sfi)
    }

    /// The header fragments, in the order they are sent.
    pub fn headers(&self) -> &[(u8, Range<usize>); 3] {
        &self.headers
    }

    pub fn bytes(&self, range: Range<usize>) -> &[u8] {
        self.data.get(range).unwrap_or_default()
    }

    pub fn body_start(&self) -> usize {
        self.body
    }

    /// Check every command header, find the boot parameters, and prove the
    /// stream ends on a group boundary.
    fn validate_body(&mut self) -> Result<(), &'static str> {
        let mut pos = self.body;
        let mut group = 0usize;
        while pos < self.data.len() {
            let (opcode, params) = self.command_at(pos).ok_or("command runs past the end")?;
            if opcode == WRITE_BOOT_PARAMS && self.version.is_none() {
                if let [a, b, c, d, nn, ww, yy, ..] = self.data[params.clone()] {
                    self.boot_addr = u32::from_le_bytes([a, b, c, d]);
                    self.version = Some((nn, ww, yy));
                }
            }
            group += params.end - pos;
            pos = params.end;
            if group.is_multiple_of(4) {
                group = 0;
            }
        }
        if pos == self.body {
            return Err("no commands");
        }
        if group != 0 {
            return Err("command stream ends inside a fragment group");
        }
        Ok(())
    }

    /// The opcode and parameter range of the command at `pos`, if it fits.
    fn command_at(&self, pos: usize) -> Option<(u16, Range<usize>)> {
        let head = self.data.get(pos..pos.checked_add(3)?)?;
        let opcode = u16::from_le_bytes([head[0], head[1]]);
        let start = pos + 3;
        let end = start.checked_add(usize::from(head[2]))?;
        (end <= self.data.len()).then_some((opcode, start..end))
    }

    /// The end of the 4-octet-aligned group of commands starting at `pos`.
    /// The bootloader wants whole commands, and a total length that is a
    /// multiple of four, in each Data fragment (the files carry Intel_NOP
    /// commands to make that work).
    pub fn next_group(&self, pos: usize) -> Option<usize> {
        let mut end = pos;
        loop {
            end = self.command_at(end)?.1.end;
            if (end - pos).is_multiple_of(4) {
                return Some(end);
            }
        }
    }
}

fn css_version(data: &[u8], base: usize) -> Option<u32> {
    let at = base.checked_add(CSS_VERSION_AT)?;
    let raw = data.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}
