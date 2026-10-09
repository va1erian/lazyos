//! Parcel header, the object list, and the parcel-level codec (version 2).

use alloc::vec::Vec;

use crate::tlv::{read_u16, read_u32, read_u64};
use crate::{Error, HEADER_SIZE, MAX_BODY_BYTES, MAX_OBJECTS, MAX_PARCEL_BYTES, VERSION};

/// Bytes of one object-list entry: `u32 kind`, `u32 reserved`, `u64 handle`.
pub const OBJECT_ENTRY_SIZE: usize = 16;

/// Fixed header of a parcel.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Header {
    pub version: u16,
    pub flags: u16,
    pub interface_id: u64,
    pub method: u32,
    pub txn_id: u64,
    pub reply_to: u64,
    pub deadline_ns: u64,
}

/// The kind of a kernel object a message carries (`docs/messenger-core-plan.md`
/// 2.1). The discriminant is the wire tag of the object list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum ObjectKind {
    /// One end of a channel. It **moves**: the sender's handle closes when
    /// the message is queued and the receiver gets a fresh one.
    Channel = 1,
    /// Shared memory. It is **shared**: the sender keeps its handle and
    /// mapping, the receiver gets a new handle to the same pages.
    Buffer = 2,
}

impl ObjectKind {
    fn from_tag(tag: u32) -> Option<ObjectKind> {
        match tag {
            1 => Some(ObjectKind::Channel),
            2 => Some(ObjectKind::Buffer),
            _ => None,
        }
    }
}

/// One entry of a parcel's object list: the kind and a handle number. On the
/// sending side it is the sender's handle; the receiver ignores that number
/// and gets the one the kernel installed in its table instead.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Object {
    Channel(u64),
    Buffer(u64),
}

impl Object {
    pub fn kind(self) -> ObjectKind {
        match self {
            Object::Channel(_) => ObjectKind::Channel,
            Object::Buffer(_) => ObjectKind::Buffer,
        }
    }

    /// The handle number the entry carries.
    pub fn handle(self) -> u64 {
        match self {
            Object::Channel(handle) | Object::Buffer(handle) => handle,
        }
    }

    /// The same kind with another handle number (a receiver replaces the
    /// sender's number with the installed one).
    pub fn with_handle(self, handle: u64) -> Object {
        match self {
            Object::Channel(_) => Object::Channel(handle),
            Object::Buffer(_) => Object::Buffer(handle),
        }
    }

    fn new(kind: ObjectKind, handle: u64) -> Object {
        match kind {
            ObjectKind::Channel => Object::Channel(handle),
            ObjectKind::Buffer => Object::Buffer(handle),
        }
    }
}

/// A decoded `Buffer` field: a shared buffer (its handle in the reader's
/// table once received) and the byte range the message refers to. The
/// kernel never reads the range; the receiving library checks it against
/// the size `buffer_map` reports.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Buffer {
    pub handle: u64,
    pub offset: u64,
    pub len: u64,
}

impl Buffer {
    /// The first `len` bytes of `handle`.
    pub const fn whole(handle: u64, len: u64) -> Buffer {
        Buffer {
            handle,
            offset: 0,
            len,
        }
    }

    /// The end of the range, or `None` when it overflows.
    pub fn end(self) -> Option<u64> {
        self.offset.checked_add(self.len)
    }

    /// Whether the range lies inside a buffer of `size` bytes.
    pub fn fits(self, size: u64) -> bool {
        self.end().is_some_and(|end| end <= size)
    }
}

/// A complete message: header, TLV body, and the objects the body refers to.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Parcel {
    pub header: Header,
    pub body: Vec<u8>,
    pub objects: Vec<Object>,
}

impl Parcel {
    /// Encode into `out`, replacing its contents.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        let body_len = self.body.len();
        if body_len > MAX_BODY_BYTES || self.objects.len() > MAX_OBJECTS {
            return Err(Error::TooLarge);
        }
        let total = HEADER_SIZE + body_len + self.objects.len() * OBJECT_ENTRY_SIZE;
        if total > MAX_PARCEL_BYTES {
            return Err(Error::TooLarge);
        }

        out.clear();
        out.reserve(total);
        let h = &self.header;
        out.extend_from_slice(&h.version.to_le_bytes());
        out.extend_from_slice(&h.flags.to_le_bytes());
        out.extend_from_slice(&(self.objects.len() as u32).to_le_bytes());
        out.extend_from_slice(&h.interface_id.to_le_bytes());
        out.extend_from_slice(&h.method.to_le_bytes());
        out.extend_from_slice(&(body_len as u32).to_le_bytes());
        out.extend_from_slice(&h.txn_id.to_le_bytes());
        out.extend_from_slice(&h.reply_to.to_le_bytes());
        out.extend_from_slice(&h.deadline_ns.to_le_bytes());
        debug_assert_eq!(out.len(), HEADER_SIZE);

        out.extend_from_slice(&self.body);
        for object in &self.objects {
            out.extend_from_slice(&(object.kind() as u32).to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&object.handle().to_le_bytes());
        }
        Ok(())
    }

    /// Decode a parcel, validating every length and limit.
    pub fn decode(bytes: &[u8]) -> Result<Parcel, Error> {
        let view = ParcelView::parse(bytes)?;
        Ok(Parcel {
            header: view.header,
            body: view.body().to_vec(),
            objects: view.objects().collect(),
        })
    }
}

/// A validated parcel read in place: the header decoded, the body and the
/// object list left in the caller's bytes. [`ParcelView::parse`] applies
/// exactly the checks of [`Parcel::decode`] (which is built on it) without
/// copying or allocating, so a kernel can validate a parcel once at its
/// boundary and keep the bytes as they arrived.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ParcelView<'a> {
    pub header: Header,
    bytes: &'a [u8],
    body_len: usize,
    object_count: usize,
}

impl<'a> ParcelView<'a> {
    /// Validate `bytes` as one complete parcel.
    pub fn parse(bytes: &'a [u8]) -> Result<ParcelView<'a>, Error> {
        if bytes.len() < HEADER_SIZE {
            return Err(Error::Truncated);
        }
        if bytes.len() > MAX_PARCEL_BYTES {
            return Err(Error::TooLarge);
        }
        let version = read_u16(bytes, 0)?;
        if version != VERSION {
            return Err(Error::BadVersion);
        }
        let object_count = read_u32(bytes, 4)? as usize;
        let body_len = read_u32(bytes, 20)? as usize;
        let header = Header {
            version,
            flags: read_u16(bytes, 2)?,
            interface_id: read_u64(bytes, 8)?,
            method: read_u32(bytes, 16)?,
            txn_id: read_u64(bytes, 24)?,
            reply_to: read_u64(bytes, 32)?,
            deadline_ns: read_u64(bytes, 40)?,
        };
        if body_len > MAX_BODY_BYTES || object_count > MAX_OBJECTS {
            return Err(Error::TooLarge);
        }
        let end = HEADER_SIZE + body_len + object_count * OBJECT_ENTRY_SIZE;
        if end > bytes.len() {
            return Err(Error::Truncated);
        }
        if end != bytes.len() {
            return Err(Error::TrailingBytes);
        }
        let view = ParcelView {
            header,
            bytes,
            body_len,
            object_count,
        };
        for index in 0..object_count {
            view.object(index)?;
        }
        Ok(view)
    }

    /// The TLV body (not decoded).
    pub fn body(&self) -> &'a [u8] {
        &self.bytes[HEADER_SIZE..HEADER_SIZE + self.body_len]
    }

    /// Number of objects in the list.
    pub fn object_count(&self) -> usize {
        self.object_count
    }

    /// The object list, in order (validated by [`ParcelView::parse`]).
    pub fn objects(&self) -> impl Iterator<Item = Object> + 'a {
        let view = *self;
        (0..self.object_count).filter_map(move |index| view.object(index).ok())
    }

    /// Entry `index` of the object list: an unknown kind or a reserved word
    /// that is not zero is [`Error::BadObject`].
    fn object(&self, index: usize) -> Result<Object, Error> {
        let at = HEADER_SIZE + self.body_len + index * OBJECT_ENTRY_SIZE;
        let kind = ObjectKind::from_tag(read_u32(self.bytes, at)?).ok_or(Error::BadObject)?;
        if read_u32(self.bytes, at + 4)? != 0 {
            return Err(Error::BadObject);
        }
        Ok(Object::new(kind, read_u64(self.bytes, at + 8)?))
    }
}
