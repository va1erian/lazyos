//! The key data of an EAPOL-Key frame, after AES key unwrap (IEEE 802.11-2020
//! 12.7.2 and 12.7.2 "Key Data" encapsulation, Table 12-6 KDE types).
//!
//! It is a list of information elements and vendor-specific KDEs
//! (`dd len 00-0F-AC type data...`). The unwrapped plaintext is padded to a
//! multiple of 8 octets (at least 16) with a `dd` octet followed by zeros, so
//! a `dd` with a zero length, or a lone trailing `dd`, starts the padding and
//! everything after it must be zero. Every length is checked against the
//! octets that remain.

use crate::Error;

/// Element ID of the RSN element in key data.
const ID_RSN: u8 = 48;
/// Element ID of a vendor-specific element or KDE.
const ID_KDE: u8 = 221;
const OUI: [u8; 3] = [0x00, 0x0F, 0xAC];
const KDE_GTK: u8 = 1;

/// A group temporal key from a GTK KDE.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gtk<'a> {
    /// Key index, 1 to 3 (0 is not a valid GTK index for a group handshake
    /// but is passed through: the chip decides).
    pub index: u8,
    /// The Tx bit: the AP transmits with this key.
    pub tx: bool,
    pub key: &'a [u8],
}

/// What the supplicant reads out of message 3 and the group message 1.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyData<'a> {
    /// The first RSN element, whole (ID, length, body).
    pub rsn_ie: Option<&'a [u8]>,
    pub gtk: Option<Gtk<'a>>,
    /// An IGTK KDE was present (not used: management frame protection is not
    /// supported).
    pub igtk_seen: bool,
}

/// Walk `data`. [`Error::BadKeyData`] for a truncated element, a KDE too
/// short for its OUI and type, non-zero padding, or a second GTK KDE; the GTK
/// KDE's own layout is checked by the caller (it knows the cipher).
pub fn parse(data: &[u8]) -> Result<KeyData<'_>, Error> {
    let mut out = KeyData::default();
    let mut rest = data;
    while let Some((&tag, after)) = rest.split_first() {
        // Padding: `dd` alone, or `dd 00` and then zeros to the end.
        if tag == ID_KDE && after.first().is_none_or(|&len| len == 0) {
            let zeros = after.get(1..).unwrap_or_default();
            if zeros.iter().any(|&byte| byte != 0) {
                return Err(Error::BadKeyData);
            }
            return Ok(out);
        }
        let &len = after.first().ok_or(Error::BadKeyData)?;
        let total = 2 + usize::from(len);
        if rest.len() < total {
            return Err(Error::BadKeyData);
        }
        let (element, tail) = rest.split_at(total);
        take(&mut out, tag, element)?;
        rest = tail;
    }
    Ok(out)
}

fn take<'a>(out: &mut KeyData<'a>, tag: u8, element: &'a [u8]) -> Result<(), Error> {
    let body = &element[2..];
    match tag {
        ID_RSN => {
            out.rsn_ie.get_or_insert(element);
        }
        ID_KDE => {
            let &[a, b, c, kind, ref data @ ..] = body else {
                return Err(Error::BadKeyData);
            };
            if [a, b, c] != OUI {
                return Ok(());
            }
            match kind {
                KDE_GTK => {
                    if out.gtk.is_some() {
                        return Err(Error::BadKeyData);
                    }
                    out.gtk = Some(gtk(data)?);
                }
                9 => out.igtk_seen = true,
                _ => {}
            }
        }
        _ => {}
    }
    Ok(())
}

/// GTK KDE data: key ID and Tx in the first octet, a reserved octet, the key.
fn gtk(data: &[u8]) -> Result<Gtk<'_>, Error> {
    let &[flags, _reserved, ref key @ ..] = data else {
        return Err(Error::BadGtk);
    };
    Ok(Gtk {
        index: flags & 0x03,
        tx: flags & 0x04 != 0,
        key,
    })
}
