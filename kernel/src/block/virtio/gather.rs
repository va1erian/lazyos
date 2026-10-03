//! Scatter/gather between the bounce region and a caller's segment list.
//!
//! A vectored transfer moves one byte range made of several caller buffers
//! (the ext2 cache's pages, which are scattered frames). Each request
//! carries up to the bounce region's size of that range; a [`Cursor`]
//! remembers where in the segment list the next request starts, so a
//! segment may straddle two requests.

/// A position in a segment list: which segment, and how far into it.
#[derive(Default)]
pub(super) struct Cursor {
    segment: usize,
    offset: usize,
}

impl Cursor {
    /// Copy `from` into the segments at the cursor, advancing it.
    pub(super) fn scatter(&mut self, mut from: &[u8], to: &mut [&mut [u8]]) {
        while !from.is_empty() {
            let Some(segment) = to.get_mut(self.segment) else {
                return; // the caller sized the transfer from these segments
            };
            let room = &mut segment[self.offset..];
            let count = room.len().min(from.len());
            room[..count].copy_from_slice(&from[..count]);
            from = &from[count..];
            let len = segment.len();
            self.advance(count, len);
        }
    }

    /// Fill `to` from the segments at the cursor, advancing it.
    pub(super) fn gather(&mut self, from: &[&[u8]], mut to: &mut [u8]) {
        while !to.is_empty() {
            let Some(segment) = from.get(self.segment) else {
                return;
            };
            let rest = &segment[self.offset..];
            let count = rest.len().min(to.len());
            to[..count].copy_from_slice(&rest[..count]);
            to = &mut to[count..];
            self.advance(count, segment.len());
        }
    }

    /// Move `count` bytes on in a segment of `len` bytes (empty segments are
    /// stepped over).
    fn advance(&mut self, count: usize, len: usize) {
        self.offset += count;
        if self.offset >= len {
            self.segment += 1;
            self.offset = 0;
        }
    }
}
