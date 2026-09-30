//! Parse the virtio vendor-specific PCI capabilities (virtio 1.x, 4.1.4.3).
//!
//! The caller supplies a config-space reader, so the walk works over the
//! `cfg_read` syscall in a driver and over a byte array in a test. Everything
//! read from the device is untrusted: the list walk is bounded, capabilities
//! shorter than their type requires are ignored, and a location whose
//! `offset + length` wraps is rejected.

use crate::Error;

const STATUS: u8 = 0x06;
const STATUS_CAP_LIST: u32 = 1 << 4;
const CAP_POINTER: u8 = 0x34;
const CAP_ID_VENDOR: u32 = 0x09;
/// A well-formed list cannot hold more entries than fit in the 256-byte config
/// space at 4-byte spacing; the bound stops a looping list.
const MAX_CAPS: usize = 48;

const CFG_COMMON: u8 = 1;
const CFG_NOTIFY: u8 = 2;
const CFG_ISR: u8 = 3;
const CFG_DEVICE: u8 = 4;

/// Bytes a capability must span to carry the fields the parser reads.
const BASE_LEN: u8 = 16;
const NOTIFY_LEN: u8 = 20;

/// Where one configuration structure lives: a BAR index plus a byte range.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Location {
    pub bar: u8,
    pub offset: u32,
    pub length: u32,
}

impl Location {
    /// The exclusive end of the range, or `None` if it would wrap.
    pub fn end(&self) -> Option<u32> {
        self.offset.checked_add(self.length)
    }
}

/// The configuration structures a modern virtio-PCI function exposes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    pub common: Location,
    pub notify: Location,
    /// Queue notify address is `notify + queue_notify_off * multiplier`.
    pub notify_multiplier: u32,
    pub isr: Location,
    /// Device-specific configuration; absent for devices that have none.
    pub device: Option<Location>,
}

/// Walk the capability list through `read(offset, width)`; `width` is 1, 2 or 4.
pub fn parse(mut read: impl FnMut(u8, u8) -> u32) -> Result<Caps, Error> {
    if read(STATUS, 2) & STATUS_CAP_LIST == 0 {
        return Err(Error::MissingCapability("capability list"));
    }
    let mut common = None;
    let mut notify = None;
    let mut multiplier = 0;
    let mut isr = None;
    let mut device = None;

    let mut pointer = (read(CAP_POINTER, 1) & 0xFC) as u8;
    for _ in 0..MAX_CAPS {
        if pointer == 0 {
            break;
        }
        let header = read(pointer, 4);
        let next = ((header >> 8) & 0xFC) as u8;
        // The fields read below must also fit inside the 256-byte space.
        let fits = usize::from(pointer) + usize::from(NOTIFY_LEN) <= 256;
        if header & 0xFF == CAP_ID_VENDOR && fits {
            let length = ((header >> 16) & 0xFF) as u8;
            let kind = (header >> 24) as u8;
            let bar = (read(pointer + 4, 1) & 0xFF) as u8;
            let location = Location {
                bar,
                offset: read(pointer + 8, 4),
                length: read(pointer + 12, 4),
            };
            let usable = bar < 6 && location.end().is_some() && location.length != 0;
            match kind {
                CFG_COMMON if usable && length >= BASE_LEN && common.is_none() => {
                    common = Some(location)
                }
                CFG_NOTIFY if usable && length >= NOTIFY_LEN && notify.is_none() => {
                    notify = Some(location);
                    multiplier = read(pointer + 16, 4);
                }
                CFG_ISR if usable && length >= BASE_LEN && isr.is_none() => isr = Some(location),
                CFG_DEVICE if usable && length >= BASE_LEN && device.is_none() => {
                    device = Some(location)
                }
                _ => {}
            }
        }
        pointer = next;
    }
    Ok(Caps {
        common: common.ok_or(Error::MissingCapability("common config"))?,
        notify: notify.ok_or(Error::MissingCapability("notify config"))?,
        notify_multiplier: multiplier,
        isr: isr.ok_or(Error::MissingCapability("isr config"))?,
        device,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 256-byte config space with helpers to lay capabilities out.
    struct Space([u8; 256]);

    impl Space {
        fn new() -> Space {
            let mut space = Space([0; 256]);
            space.0[STATUS as usize] = 0x10; // capability list present
            space
        }

        fn put32(&mut self, at: usize, value: u32) {
            self.0[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }

        /// One virtio capability at `at`, chained to `next`.
        #[allow(clippy::too_many_arguments)]
        fn cap(&mut self, at: usize, next: u8, len: u8, kind: u8, bar: u8, off: u32, size: u32) {
            self.0[at] = CAP_ID_VENDOR as u8;
            self.0[at + 1] = next;
            self.0[at + 2] = len;
            self.0[at + 3] = kind;
            self.0[at + 4] = bar;
            self.put32(at + 8, off);
            self.put32(at + 12, size);
        }

        fn reader(&self) -> impl FnMut(u8, u8) -> u32 + '_ {
            move |offset, width| {
                let offset = usize::from(offset);
                let mut value = 0u32;
                for index in 0..usize::from(width) {
                    value |= u32::from(self.0[offset + index]) << (8 * index);
                }
                value
            }
        }
    }

    /// The layout QEMU's virtio-sound-pci uses: common, isr, device, notify.
    fn qemu_like() -> Space {
        let mut space = Space::new();
        space.0[CAP_POINTER as usize] = 0x98;
        space.cap(0x98, 0x84, 16, CFG_COMMON, 4, 0x0000, 0x1000);
        space.cap(0x84, 0x70, 16, CFG_ISR, 4, 0x1000, 0x1000);
        space.cap(0x70, 0x50, 16, CFG_DEVICE, 4, 0x2000, 0x1000);
        space.cap(0x50, 0x00, 20, CFG_NOTIFY, 4, 0x3000, 0x1000);
        space.put32(0x50 + 16, 4);
        space
    }

    #[test]
    fn parses_the_qemu_layout() {
        let space = qemu_like();
        let caps = parse(space.reader()).expect("caps");
        assert_eq!(
            caps.common,
            Location {
                bar: 4,
                offset: 0,
                length: 0x1000
            }
        );
        assert_eq!(caps.notify.offset, 0x3000);
        assert_eq!(caps.notify_multiplier, 4);
        assert_eq!(caps.isr.offset, 0x1000);
        assert_eq!(caps.device.map(|l| l.offset), Some(0x2000));
    }

    #[test]
    fn no_capability_list_is_an_error() {
        let mut space = qemu_like();
        space.0[STATUS as usize] = 0;
        assert!(matches!(
            parse(space.reader()),
            Err(Error::MissingCapability(_))
        ));
    }

    #[test]
    fn a_missing_structure_is_named() {
        let mut space = qemu_like();
        space.0[0x84] = 0x00; // no longer a vendor capability: ISR disappears
        assert_eq!(
            parse(space.reader()),
            Err(Error::MissingCapability("isr config"))
        );
    }

    #[test]
    fn looping_list_terminates() {
        let mut space = Space::new();
        space.0[CAP_POINTER as usize] = 0x40;
        space.cap(0x40, 0x40, 16, CFG_COMMON, 0, 0, 0x100); // points at itself
        assert!(parse(space.reader()).is_err());
    }

    #[test]
    fn hostile_locations_are_ignored() {
        let mut space = qemu_like();
        // A wrapping range, a BAR index out of range and a zero length.
        space.cap(0x98, 0x84, 16, CFG_COMMON, 4, 0xFFFF_F000, 0x2000);
        assert!(parse(space.reader()).is_err());
        space.cap(0x98, 0x84, 16, CFG_COMMON, 7, 0, 0x1000);
        assert!(parse(space.reader()).is_err());
        space.cap(0x98, 0x84, 16, CFG_COMMON, 4, 0, 0);
        assert!(parse(space.reader()).is_err());
    }

    #[test]
    fn short_notify_capability_is_ignored() {
        let mut space = qemu_like();
        space.0[0x50 + 2] = 16; // no room for notify_off_multiplier
        assert_eq!(
            parse(space.reader()),
            Err(Error::MissingCapability("notify config"))
        );
    }

    #[test]
    fn first_capability_of_a_type_wins() {
        let mut space = qemu_like();
        // A second, later common config pointing elsewhere must not override.
        space.cap(0x50, 0x40, 20, CFG_NOTIFY, 4, 0x3000, 0x1000);
        space.cap(0x40, 0x00, 16, CFG_COMMON, 2, 0x8000, 0x1000);
        let caps = parse(space.reader()).expect("caps");
        assert_eq!(caps.common.bar, 4);
    }
}
