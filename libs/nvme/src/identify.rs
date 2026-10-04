//! The Identify Controller and Identify Namespace data structures
//! (NVMe 1.4, figures 247 and 249): the few fields the driver uses, read
//! from a 4096-byte page the controller wrote, so every value is checked.

/// Bytes in an Identify page.
pub const PAGE_BYTES: usize = 4096;

fn u16_at(page: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([page[at], page[at + 1]])
}

fn u32_at(page: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([page[at], page[at + 1], page[at + 2], page[at + 3]])
}

fn u64_at(page: &[u8], at: usize) -> u64 {
    u64::from(u32_at(page, at)) | u64::from(u32_at(page, at + 4)) << 32
}

/// An ASCII field, space padded, kept as fixed storage for logs.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Text<const N: usize> {
    bytes: [u8; N],
    len: usize,
}

impl<const N: usize> Text<N> {
    /// Printable ASCII only (anything else becomes `?`), trailing spaces and
    /// NULs dropped.
    pub fn from_field(field: &[u8]) -> Text<N> {
        let mut bytes = [0u8; N];
        let take = field.len().min(N);
        for (out, &byte) in bytes.iter_mut().zip(&field[..take]) {
            *out = if (0x20..0x7F).contains(&byte) {
                byte
            } else {
                b'?'
            };
        }
        let mut len = take;
        while len > 0 && (field[len - 1] == b' ' || field[len - 1] == 0) {
            len -= 1;
        }
        Text { bytes, len }
    }

    pub fn as_str(&self) -> &str {
        // Every stored byte is printable ASCII.
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("?")
    }
}

impl<const N: usize> core::fmt::Debug for Text<N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

/// What the driver keeps of Identify Controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControllerInfo {
    pub vendor: u16,
    pub serial: Text<20>,
    pub model: Text<40>,
    pub firmware: Text<8>,
    /// Maximum Data Transfer Size as a power of two of the minimum page size
    /// (0: no limit).
    pub mdts: u8,
    /// Number of namespaces.
    pub namespaces: u32,
    /// A volatile write cache is present, so Flush matters.
    pub volatile_cache: bool,
    /// Required (low nibble) submission and completion entry sizes, as
    /// powers of two.
    pub sqes_min: u8,
    pub cqes_min: u8,
}

impl ControllerInfo {
    pub fn parse(page: &[u8]) -> Option<ControllerInfo> {
        if page.len() < PAGE_BYTES {
            return None;
        }
        Some(ControllerInfo {
            vendor: u16_at(page, 0),
            serial: Text::from_field(&page[4..24]),
            model: Text::from_field(&page[24..64]),
            firmware: Text::from_field(&page[64..72]),
            mdts: page[77],
            namespaces: u32_at(page, 516),
            volatile_cache: page[525] & 1 != 0,
            sqes_min: page[512] & 0xF,
            cqes_min: page[513] & 0xF,
        })
    }

    /// The largest transfer in bytes for a minimum page size of
    /// `2^page_shift` (`None`: unlimited).
    pub fn max_transfer(&self, page_shift: u32) -> Option<u64> {
        if self.mdts == 0 {
            return None;
        }
        // A huge MDTS is as good as unlimited; never overflow.
        let shift = u32::from(self.mdts) + page_shift;
        (shift < 63).then(|| 1u64 << shift)
    }
}

/// Why a namespace was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NamespaceError {
    /// The page is short or the namespace has no blocks (inactive).
    Empty,
    /// `FLBAS` names a format past `NLBAF`.
    BadFormat,
    /// The format's block size is outside 512 B to 64 KiB.
    BadBlockSize,
    /// The format carries metadata, which this driver does not transfer.
    Metadata,
    /// `NCAP` or `NUSE` exceeds `NSZE`.
    Inconsistent,
}

/// What the driver keeps of Identify Namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Namespace {
    /// Namespace size in logical blocks.
    pub blocks: u64,
    /// Bytes per logical block (a power of two).
    pub block_bytes: u32,
    /// The index of the format in use.
    pub format: u8,
}

impl Namespace {
    pub fn parse(page: &[u8]) -> Result<Namespace, NamespaceError> {
        if page.len() < PAGE_BYTES {
            return Err(NamespaceError::Empty);
        }
        let nsze = u64_at(page, 0);
        let ncap = u64_at(page, 8);
        let nuse = u64_at(page, 16);
        if nsze == 0 {
            return Err(NamespaceError::Empty);
        }
        if ncap > nsze || nuse > nsze {
            return Err(NamespaceError::Inconsistent);
        }
        let formats = u16::from(page[25]) + 1; // NLBAF is 0-based
        let flbas = page[26];
        // Bits 3:0, with bits 6:5 as the high bits when more than 16
        // formats exist (NVMe 2.0); a 1.4 controller leaves them zero.
        let format = (flbas & 0xF) | ((flbas >> 5) & 0x3) << 4;
        if u16::from(format) >= formats || usize::from(format) >= 64 {
            return Err(NamespaceError::BadFormat);
        }
        let lbaf = u32_at(page, 128 + 4 * usize::from(format));
        let metadata = lbaf & 0xFFFF;
        let lbads = (lbaf >> 16) & 0xFF;
        if !(9..=16).contains(&lbads) {
            return Err(NamespaceError::BadBlockSize);
        }
        if metadata != 0 {
            return Err(NamespaceError::Metadata);
        }
        Ok(Namespace {
            blocks: nsze,
            block_bytes: 1 << lbads,
            format,
        })
    }

    /// Bytes in the namespace (saturating; a hostile `NSZE` cannot wrap).
    pub fn bytes(&self) -> u64 {
        self.blocks.saturating_mul(u64::from(self.block_bytes))
    }
}
