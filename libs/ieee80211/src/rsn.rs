//! The RSN element (IEEE 802.11-2020 9.4.2.24): parse and build.
//!
//! Layout of the body: version (2, little endian, must be 1), group data
//! cipher suite (4), pairwise suite count (2) and list, AKM suite count (2)
//! and list, RSN capabilities (2), PMKID count (2) and list (16 each), group
//! management cipher suite (4). Every field after the version is optional, and
//! the element may end after any complete field; absent fields take the
//! standard's defaults (group and pairwise CCMP-128, AKM 802.1X, no
//! capabilities). Counts are checked against the bytes that remain, so no
//! count allocates more than the element holds. Bytes after the last defined
//! field are [`RsnError::Trailing`].

use alloc::vec::Vec;

use crate::ie::{push_ie, ID_RSN};
use crate::Error;

/// The IEEE 802.11 suite OUI, 00-0F-AC.
pub const OUI_IEEE: [u8; 3] = [0x00, 0x0F, 0xAC];

/// A suite selector: OUI and type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Suite(pub [u8; 4]);

impl Suite {
    const fn ieee(kind: u8) -> Suite {
        Suite([OUI_IEEE[0], OUI_IEEE[1], OUI_IEEE[2], kind])
    }
}

/// A cipher suite (Table 9-149).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cipher {
    /// Pairwise only: use the group cipher.
    UseGroup,
    Wep40,
    Tkip,
    Ccmp128,
    Wep104,
    BipCmac128,
    Gcmp128,
    Gcmp256,
    Ccmp256,
    /// Anything else, kept so it can be reported and refused.
    Other(Suite),
}

impl Cipher {
    pub fn from_suite(suite: Suite) -> Cipher {
        let Suite([a, b, c, kind]) = suite;
        if [a, b, c] != OUI_IEEE {
            return Cipher::Other(suite);
        }
        match kind {
            0 => Cipher::UseGroup,
            1 => Cipher::Wep40,
            2 => Cipher::Tkip,
            4 => Cipher::Ccmp128,
            5 => Cipher::Wep104,
            6 => Cipher::BipCmac128,
            8 => Cipher::Gcmp128,
            9 => Cipher::Gcmp256,
            10 => Cipher::Ccmp256,
            _ => Cipher::Other(suite),
        }
    }

    pub const fn to_suite(self) -> Suite {
        match self {
            Cipher::UseGroup => Suite::ieee(0),
            Cipher::Wep40 => Suite::ieee(1),
            Cipher::Tkip => Suite::ieee(2),
            Cipher::Ccmp128 => Suite::ieee(4),
            Cipher::Wep104 => Suite::ieee(5),
            Cipher::BipCmac128 => Suite::ieee(6),
            Cipher::Gcmp128 => Suite::ieee(8),
            Cipher::Gcmp256 => Suite::ieee(9),
            Cipher::Ccmp256 => Suite::ieee(10),
            Cipher::Other(suite) => suite,
        }
    }

    /// A name for logs and error messages.
    pub const fn name(self) -> &'static str {
        match self {
            Cipher::UseGroup => "use-group",
            Cipher::Wep40 => "WEP-40",
            Cipher::Tkip => "TKIP",
            Cipher::Ccmp128 => "CCMP-128",
            Cipher::Wep104 => "WEP-104",
            Cipher::BipCmac128 => "BIP-CMAC-128",
            Cipher::Gcmp128 => "GCMP-128",
            Cipher::Gcmp256 => "GCMP-256",
            Cipher::Ccmp256 => "CCMP-256",
            Cipher::Other(_) => "unknown cipher",
        }
    }
}

/// An authentication and key management suite (Table 9-151).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Akm {
    Ieee8021x,
    Psk,
    Ft8021x,
    FtPsk,
    Ieee8021xSha256,
    PskSha256,
    Sae,
    FtSae,
    Owe,
    Other(Suite),
}

impl Akm {
    pub fn from_suite(suite: Suite) -> Akm {
        let Suite([a, b, c, kind]) = suite;
        if [a, b, c] != OUI_IEEE {
            return Akm::Other(suite);
        }
        match kind {
            1 => Akm::Ieee8021x,
            2 => Akm::Psk,
            3 => Akm::Ft8021x,
            4 => Akm::FtPsk,
            5 => Akm::Ieee8021xSha256,
            6 => Akm::PskSha256,
            8 => Akm::Sae,
            9 => Akm::FtSae,
            18 => Akm::Owe,
            _ => Akm::Other(suite),
        }
    }

    pub const fn to_suite(self) -> Suite {
        match self {
            Akm::Ieee8021x => Suite::ieee(1),
            Akm::Psk => Suite::ieee(2),
            Akm::Ft8021x => Suite::ieee(3),
            Akm::FtPsk => Suite::ieee(4),
            Akm::Ieee8021xSha256 => Suite::ieee(5),
            Akm::PskSha256 => Suite::ieee(6),
            Akm::Sae => Suite::ieee(8),
            Akm::FtSae => Suite::ieee(9),
            Akm::Owe => Suite::ieee(18),
            Akm::Other(suite) => suite,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Akm::Ieee8021x => "802.1X",
            Akm::Psk => "PSK",
            Akm::Ft8021x => "FT-802.1X",
            Akm::FtPsk => "FT-PSK",
            Akm::Ieee8021xSha256 => "802.1X-SHA256",
            Akm::PskSha256 => "PSK-SHA256",
            Akm::Sae => "SAE",
            Akm::FtSae => "FT-SAE",
            Akm::Owe => "OWE",
            Akm::Other(_) => "unknown AKM",
        }
    }
}

/// RSN capabilities (9.4.2.24.4).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RsnCaps(pub u16);

impl RsnCaps {
    pub const PREAUTH: u16 = 1 << 0;
    pub const NO_PAIRWISE: u16 = 1 << 1;
    /// Management frame protection required / capable.
    pub const MFPR: u16 = 1 << 6;
    pub const MFPC: u16 = 1 << 7;

    pub const fn mfp_required(self) -> bool {
        self.0 & Self::MFPR != 0
    }

    pub const fn mfp_capable(self) -> bool {
        self.0 & Self::MFPC != 0
    }
}

/// Why an RSN element was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RsnError {
    /// A field or list runs past the end of the element.
    Truncated,
    /// The version is not 1.
    BadVersion(u16),
    /// Bytes remain after the last defined field.
    Trailing,
    /// The element ID is not 48, or its length disagrees with the buffer.
    NotRsn,
}

/// A parsed RSN element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rsn {
    pub group: Cipher,
    pub pairwise: Vec<Cipher>,
    pub akms: Vec<Akm>,
    pub caps: RsnCaps,
    pub pmkids: Vec<[u8; 16]>,
    pub group_mgmt: Option<Cipher>,
}

/// A cursor that never reads past its slice.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], RsnError> {
        if self.0.len() < n {
            return Err(RsnError::Truncated);
        }
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        Ok(head)
    }

    fn u16(&mut self) -> Result<u16, RsnError> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn suite(&mut self) -> Result<Suite, RsnError> {
        let bytes = self.take(4)?;
        Ok(Suite([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// A counted list of `size`-byte entries; the count is checked against the
    /// remaining bytes before anything is allocated.
    fn list<T>(
        &mut self,
        size: usize,
        mut item: impl FnMut(&[u8]) -> T,
    ) -> Result<Vec<T>, RsnError> {
        let count = usize::from(self.u16()?);
        let bytes = self.take(count * size)?;
        Ok(bytes.chunks_exact(size).map(&mut item).collect())
    }

    fn done(&self) -> bool {
        self.0.is_empty()
    }
}

impl Rsn {
    /// WPA2-Personal as LazyOS asks for it: CCMP-128 group and pairwise, PSK.
    pub fn wpa2_psk() -> Rsn {
        Rsn {
            group: Cipher::Ccmp128,
            pairwise: alloc::vec![Cipher::Ccmp128],
            akms: alloc::vec![Akm::Psk],
            caps: RsnCaps(0),
            pmkids: Vec::new(),
            group_mgmt: None,
        }
    }

    /// The same with AKM 6, PSK-SHA256.
    pub fn wpa2_psk_sha256() -> Rsn {
        Rsn {
            akms: alloc::vec![Akm::PskSha256],
            ..Rsn::wpa2_psk()
        }
    }

    /// Parse an element body (after the ID and length octets).
    pub fn parse_body(body: &[u8]) -> Result<Rsn, RsnError> {
        let mut r = Reader(body);
        let version = r.u16()?;
        if version != 1 {
            return Err(RsnError::BadVersion(version));
        }
        let mut rsn = Rsn {
            group: Cipher::Ccmp128,
            pairwise: alloc::vec![Cipher::Ccmp128],
            akms: alloc::vec![Akm::Ieee8021x],
            caps: RsnCaps(0),
            pmkids: Vec::new(),
            group_mgmt: None,
        };
        if r.done() {
            return Ok(rsn);
        }
        rsn.group = Cipher::from_suite(r.suite()?);
        if !r.done() {
            rsn.pairwise = r.list(4, |s| Cipher::from_suite(Suite([s[0], s[1], s[2], s[3]])))?;
        }
        if !r.done() {
            rsn.akms = r.list(4, |s| Akm::from_suite(Suite([s[0], s[1], s[2], s[3]])))?;
        }
        if !r.done() {
            rsn.caps = RsnCaps(r.u16()?);
        }
        if !r.done() {
            rsn.pmkids = r.list(16, |p| {
                let mut id = [0u8; 16];
                id.copy_from_slice(p);
                id
            })?;
        }
        if !r.done() {
            rsn.group_mgmt = Some(Cipher::from_suite(r.suite()?));
        }
        if !r.done() {
            return Err(RsnError::Trailing);
        }
        Ok(rsn)
    }

    /// Parse a whole element (ID 48, length, body) that fills `ie` exactly.
    pub fn parse_ie(ie: &[u8]) -> Result<Rsn, RsnError> {
        match ie {
            [id, len, body @ ..] if *id == ID_RSN && usize::from(*len) == body.len() => {
                Rsn::parse_body(body)
            }
            _ => Err(RsnError::NotRsn),
        }
    }

    /// The element body. The optional tail is written only as far as needed:
    /// the PMKID list when there are PMKIDs or a group management cipher, and
    /// the group management cipher when set. [`Rsn::parse_body`] of the result
    /// equals `self`.
    pub fn to_body(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&self.group.to_suite().0);
        out.extend_from_slice(&(self.pairwise.len() as u16).to_le_bytes());
        for cipher in &self.pairwise {
            out.extend_from_slice(&cipher.to_suite().0);
        }
        out.extend_from_slice(&(self.akms.len() as u16).to_le_bytes());
        for akm in &self.akms {
            out.extend_from_slice(&akm.to_suite().0);
        }
        out.extend_from_slice(&self.caps.0.to_le_bytes());
        if !self.pmkids.is_empty() || self.group_mgmt.is_some() {
            out.extend_from_slice(&(self.pmkids.len() as u16).to_le_bytes());
            for pmkid in &self.pmkids {
                out.extend_from_slice(pmkid);
            }
        }
        if let Some(cipher) = self.group_mgmt {
            out.extend_from_slice(&cipher.to_suite().0);
        }
        out
    }

    /// The whole element (ID, length, body). [`Error::IeTooLong`] when the
    /// lists do not fit the one-octet length.
    pub fn to_ie(&self) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        push_ie(&mut out, ID_RSN, &self.to_body())?;
        Ok(out)
    }
}
