//! SDP data elements (Core Specification Vol 3 Part B 3): a type descriptor
//! byte (type in the top five bits, size index in the low three), an optional
//! length, then the data.
//!
//! [`parse`] reads one element and returns the rest, so a sequence is walked
//! with [`Elem::children`]. Every length is checked against the bytes
//! present. Nothing here recurses: the record reader descends exactly as far
//! as the HID record is deep, and charges every element to a [`Budget`].

use crate::Error;

/// Elements one parse may visit before it gives up on the answer.
pub const MAX_ELEMENTS: u32 = 1024;

/// An element's type (the top five bits of its descriptor byte).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Nil,
    Uint,
    Int,
    Uuid,
    Text,
    Bool,
    /// A sequence (DES).
    Seq,
    /// An alternative (DEA).
    Alt,
    Url,
}

/// One element: its type and its data (the header is already consumed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Elem<'a> {
    pub kind: Kind,
    pub data: &'a [u8],
}

impl<'a> Elem<'a> {
    /// An unsigned integer of up to four octets, big endian.
    pub fn uint(&self) -> Option<u32> {
        if self.kind != Kind::Uint || self.data.is_empty() || self.data.len() > 4 {
            return None;
        }
        Some(self.data.iter().fold(0, |v, &b| v << 8 | u32::from(b)))
    }

    /// A boolean element.
    pub fn boolean(&self) -> Option<bool> {
        match (self.kind, self.data) {
            (Kind::Bool, [b]) => Some(*b != 0),
            _ => None,
        }
    }

    /// A 16-bit UUID, or a 32-bit one on the Bluetooth base UUID that fits.
    pub fn uuid16(&self) -> Option<u16> {
        match (self.kind, self.data) {
            (Kind::Uuid, [a, b]) => Some(u16::from_be_bytes([*a, *b])),
            (Kind::Uuid, [0, 0, a, b]) => Some(u16::from_be_bytes([*a, *b])),
            _ => None,
        }
    }

    /// The children of a sequence or alternative.
    pub fn children(&self) -> Result<Children<'a>, Error> {
        match self.kind {
            Kind::Seq | Kind::Alt => Ok(Children {
                rest: self.data,
                failed: false,
            }),
            _ => Err(Error::BadElement),
        }
    }
}

/// Counts what a parse visits so a hostile answer cannot make it long.
#[derive(Debug, Default)]
pub struct Budget {
    seen: u32,
}

impl Budget {
    pub fn new() -> Budget {
        Budget::default()
    }

    /// Charge one element.
    pub fn visit(&mut self) -> Result<(), Error> {
        self.seen += 1;
        if self.seen > MAX_ELEMENTS {
            return Err(Error::BadElement);
        }
        Ok(())
    }
}

/// Iterates the elements of a sequence body; an element that does not fit
/// ends it with one `Err`.
pub struct Children<'a> {
    rest: &'a [u8],
    failed: bool,
}

impl<'a> Iterator for Children<'a> {
    type Item = Result<Elem<'a>, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.rest.is_empty() {
            return None;
        }
        match parse(self.rest) {
            Ok((elem, rest)) => {
                self.rest = rest;
                Some(Ok(elem))
            }
            Err(e) => {
                self.failed = true;
                Some(Err(e))
            }
        }
    }
}

/// Read one element from the front of `bytes`; returns it and what follows.
pub fn parse(bytes: &[u8]) -> Result<(Elem<'_>, &[u8]), Error> {
    let (&head, rest) = bytes.split_first().ok_or(Error::BadElement)?;
    let (ty, index) = (head >> 3, head & 7);
    let kind = match ty {
        0 => Kind::Nil,
        1 => Kind::Uint,
        2 => Kind::Int,
        3 => Kind::Uuid,
        4 => Kind::Text,
        5 => Kind::Bool,
        6 => Kind::Seq,
        7 => Kind::Alt,
        8 => Kind::Url,
        _ => return Err(Error::BadElement),
    };
    // Size indexes 0..=4 are fixed (1, 2, 4, 8, 16 octets; nil has none);
    // 5..=7 carry a one, two or four octet length.
    let (len, rest) = match index {
        0 if kind == Kind::Nil => (0, rest),
        0..=4 if kind != Kind::Nil => (1usize << index, rest),
        5 if kind != Kind::Nil => {
            let (&n, rest) = rest.split_first().ok_or(Error::BadElement)?;
            (usize::from(n), rest)
        }
        6 if kind != Kind::Nil => {
            let n = rest.get(..2).ok_or(Error::BadElement)?;
            (usize::from(u16::from_be_bytes([n[0], n[1]])), &rest[2..])
        }
        7 if kind != Kind::Nil => {
            let n = rest.get(..4).ok_or(Error::BadElement)?;
            let n = u32::from_be_bytes([n[0], n[1], n[2], n[3]]);
            (
                usize::try_from(n).map_err(|_| Error::BadElement)?,
                &rest[4..],
            )
        }
        _ => return Err(Error::BadElement),
    };
    if len > rest.len() {
        return Err(Error::BadElement);
    }
    let (data, rest) = rest.split_at(len);
    Ok((Elem { kind, data }, rest))
}
