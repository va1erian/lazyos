//! Information elements (IEEE 802.11-2020 9.4.2): a bounds-checked walker,
//! the summary a station reads from a beacon or probe response, and a builder.

use alloc::vec::Vec;

use crate::Error;

pub const ID_SSID: u8 = 0;
pub const ID_RATES: u8 = 1;
pub const ID_DS_PARAMS: u8 = 3;
pub const ID_COUNTRY: u8 = 7;
pub const ID_HT_CAP: u8 = 45;
pub const ID_RSN: u8 = 48;
pub const ID_EXT_RATES: u8 = 50;
pub const ID_VHT_CAP: u8 = 191;
pub const ID_VENDOR: u8 = 221;
/// Element ID Extension (255): the first body octet is the extension ID.
pub const ID_EXTENSION: u8 = 255;
/// Extension ID of the HE Capabilities element (9.4.2.248).
pub const EXT_HE_CAP: u8 = 35;

/// Longest SSID (9.4.2.2).
pub const MAX_SSID: usize = 32;
/// Vendor elements kept per frame; more are counted, not stored.
pub const MAX_VENDOR: usize = 32;
/// The WPA1 vendor element: OUI 00:50:F2, type 1.
pub const WPA_OUI: [u8; 3] = [0x00, 0x50, 0xF2];

/// One element: ID, body, and the whole element (ID, length, body) as it was
/// on the air (the RSN element is compared byte for byte in the 4-way
/// handshake).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Element<'a> {
    pub id: u8,
    pub body: &'a [u8],
    pub raw: &'a [u8],
}

/// Walks the elements of a buffer. It stops at the first element whose length
/// runs past the buffer and remembers that ([`ElementIter::truncated`]).
#[derive(Clone, Debug)]
pub struct ElementIter<'a> {
    rest: &'a [u8],
    truncated: bool,
}

impl<'a> ElementIter<'a> {
    pub fn new(buf: &'a [u8]) -> ElementIter<'a> {
        ElementIter {
            rest: buf,
            truncated: false,
        }
    }

    /// True once the walk hit a partial element (a lone ID octet, or a length
    /// past the end). Meaningful after the iterator is exhausted.
    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

impl<'a> Iterator for ElementIter<'a> {
    type Item = Element<'a>;

    fn next(&mut self) -> Option<Element<'a>> {
        if self.rest.is_empty() {
            return None;
        }
        let total = match self.rest {
            &[_, len, ..] => 2 + usize::from(len),
            _ => usize::MAX,
        };
        if self.rest.len() < total {
            self.truncated = true;
            self.rest = &[];
            return None;
        }
        let (raw, rest) = self.rest.split_at(total);
        self.rest = rest;
        Some(Element {
            id: raw[0],
            body: &raw[2..],
            raw,
        })
    }
}

/// A vendor-specific element (221): OUI, type octet and the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Vendor<'a> {
    pub oui: [u8; 3],
    pub kind: u8,
    pub data: &'a [u8],
}

/// True for a hidden network name: zero length, or every octet zero (the two
/// ways access points hide an SSID, 9.4.2.2).
pub fn ssid_is_hidden(ssid: &[u8]) -> bool {
    ssid.iter().all(|&byte| byte == 0)
}

/// What a station reads from a frame's elements. Slices borrow the frame.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Elements<'a> {
    /// SSID octets as sent (may be empty, all NULs, or not UTF-8).
    pub ssid: Option<&'a [u8]>,
    /// Supported Rates and Extended Supported Rates bodies (raw rate octets,
    /// bit 7 marks a basic rate).
    pub rates: Option<&'a [u8]>,
    pub ext_rates: Option<&'a [u8]>,
    /// DS Parameter Set: the channel the AP says it is on.
    pub ds_channel: Option<u8>,
    /// Country element body (two letters, environment, triplets).
    pub country: Option<&'a [u8]>,
    /// HT, VHT and HE Capabilities bodies, read-only (HE without its
    /// extension ID octet).
    pub ht_cap: Option<&'a [u8]>,
    pub vht_cap: Option<&'a [u8]>,
    pub he_cap: Option<&'a [u8]>,
    /// The first RSN element: body, and the whole element for byte comparison.
    pub rsn: Option<&'a [u8]>,
    pub rsn_ie: Option<&'a [u8]>,
    /// The WPA1 vendor element (whole element), if present.
    pub wpa_ie: Option<&'a [u8]>,
    pub vendor: Vec<Vendor<'a>>,
    /// Vendor elements past [`MAX_VENDOR`].
    pub vendor_dropped: u16,
    /// Repeated singleton elements ignored (first wins).
    pub duplicates: u16,
    /// Known elements with an impossible length, ignored.
    pub malformed: u16,
    /// The buffer ended inside an element.
    pub truncated: bool,
}

/// Store `value` in `slot` unless it is taken (then count a duplicate).
fn once<T>(slot: &mut Option<T>, value: T, duplicates: &mut u16) {
    if slot.is_some() {
        *duplicates = duplicates.saturating_add(1);
    } else {
        *slot = Some(value);
    }
}

impl<'a> Elements<'a> {
    /// Walk `buf` once. The only error is an SSID over 32 octets.
    pub fn parse(buf: &'a [u8]) -> Result<Elements<'a>, Error> {
        let mut out = Elements::default();
        let mut walk = ElementIter::new(buf);
        for element in walk.by_ref() {
            out.take(element)?;
        }
        out.truncated = walk.truncated();
        Ok(out)
    }

    fn take(&mut self, element: Element<'a>) -> Result<(), Error> {
        let body = element.body;
        let dup = &mut self.duplicates;
        match (element.id, body) {
            (ID_SSID, _) if body.len() > MAX_SSID => return Err(Error::BadSsid),
            (ID_SSID, _) => once(&mut self.ssid, body, dup),
            (ID_RATES, _) => once(&mut self.rates, body, dup),
            (ID_EXT_RATES, _) => once(&mut self.ext_rates, body, dup),
            (ID_DS_PARAMS, &[channel]) => once(&mut self.ds_channel, channel, dup),
            (ID_COUNTRY, [_, _, _, ..]) => once(&mut self.country, body, dup),
            (ID_HT_CAP, _) => once(&mut self.ht_cap, body, dup),
            (ID_VHT_CAP, _) => once(&mut self.vht_cap, body, dup),
            (ID_EXTENSION, [EXT_HE_CAP, rest @ ..]) => once(&mut self.he_cap, rest, dup),
            (ID_RSN, _) => {
                if self.rsn.is_none() {
                    self.rsn_ie = Some(element.raw);
                }
                once(&mut self.rsn, body, dup);
            }
            (ID_VENDOR, _) => self.take_vendor(element),
            (ID_DS_PARAMS | ID_COUNTRY, _) => {
                self.malformed = self.malformed.saturating_add(1);
            }
            _ => {}
        }
        Ok(())
    }

    fn take_vendor(&mut self, element: Element<'a>) {
        let &[a, b, c, kind, ref data @ ..] = element.body else {
            // Too short for an OUI and a type: nothing to recognise.
            self.malformed = self.malformed.saturating_add(1);
            return;
        };
        let oui = [a, b, c];
        if oui == WPA_OUI && kind == 1 && self.wpa_ie.is_none() {
            self.wpa_ie = Some(element.raw);
        }
        if self.vendor.len() >= MAX_VENDOR {
            self.vendor_dropped = self.vendor_dropped.saturating_add(1);
            return;
        }
        self.vendor.push(Vendor { oui, kind, data });
    }

    /// True when the SSID is absent, zero-length or all NULs.
    pub fn hidden(&self) -> bool {
        self.ssid.is_none_or(ssid_is_hidden)
    }

    /// The two-letter country string, if the element is present.
    pub fn country_code(&self) -> Option<[u8; 2]> {
        match self.country {
            Some(&[a, b, ..]) => Some([a, b]),
            _ => None,
        }
    }
}

/// Append one element. Fails when `body` is over 255 octets.
pub fn push_ie(out: &mut Vec<u8>, id: u8, body: &[u8]) -> Result<(), Error> {
    let len = u8::try_from(body.len()).map_err(|_| Error::IeTooLong)?;
    out.push(id);
    out.push(len);
    out.extend_from_slice(body);
    Ok(())
}

/// Builds an element list.
#[derive(Clone, Debug, Default)]
pub struct IeBuilder {
    buf: Vec<u8>,
}

impl IeBuilder {
    pub fn new() -> IeBuilder {
        IeBuilder::default()
    }

    /// An SSID element (0 to 32 octets; empty is the wildcard).
    pub fn ssid(mut self, ssid: &[u8]) -> Result<IeBuilder, Error> {
        if ssid.len() > MAX_SSID {
            return Err(Error::BadSsid);
        }
        push_ie(&mut self.buf, ID_SSID, ssid)?;
        Ok(self)
    }

    /// Rates: the first eight in Supported Rates, the rest in Extended.
    pub fn rates(mut self, rates: &[u8]) -> Result<IeBuilder, Error> {
        let (first, rest) = rates.split_at(rates.len().min(8));
        push_ie(&mut self.buf, ID_RATES, first)?;
        if !rest.is_empty() {
            push_ie(&mut self.buf, ID_EXT_RATES, rest)?;
        }
        Ok(self)
    }

    pub fn ds_channel(mut self, channel: u8) -> Result<IeBuilder, Error> {
        push_ie(&mut self.buf, ID_DS_PARAMS, &[channel])?;
        Ok(self)
    }

    /// An element with an arbitrary body.
    pub fn raw(mut self, id: u8, body: &[u8]) -> Result<IeBuilder, Error> {
        push_ie(&mut self.buf, id, body)?;
        Ok(self)
    }

    /// The RSN element for `rsn`.
    pub fn rsn(mut self, rsn: &crate::Rsn) -> Result<IeBuilder, Error> {
        self.buf.extend_from_slice(&rsn.to_ie()?);
        Ok(self)
    }

    pub fn finish(self) -> Vec<u8> {
        self.buf
    }
}
