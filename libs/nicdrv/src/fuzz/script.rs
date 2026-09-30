//! Reads script bytes; past the end it yields zeros and reports exhaustion.

pub(super) struct Script<'a> {
    pub(super) data: &'a [u8],
    pub(super) at: usize,
}

impl Script<'_> {
    pub(super) fn done(&self) -> bool {
        self.at >= self.data.len()
    }
    pub(super) fn u8(&mut self) -> u8 {
        let b = self.data.get(self.at).copied().unwrap_or(0);
        self.at += 1;
        b
    }
    pub(super) fn u16(&mut self) -> u16 {
        u16::from(self.u8()) << 8 | u16::from(self.u8())
    }
    pub(super) fn u32(&mut self) -> u32 {
        u32::from(self.u16()) << 16 | u32::from(self.u16())
    }
}
