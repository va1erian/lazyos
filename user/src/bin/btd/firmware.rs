//! The firmware files the host asks for (`bthost::Effect::LoadFirmware`),
//! read from the firmware tree through `libs/fwload` the way `wifid` reads
//! its own: a name relative to `fhs::FIRMWARE`, checked before it becomes a
//! path, a size cap checked before anything is allocated.

use alloc::vec::Vec;

use user::files::{self, Kind};
use user::trace::{self, Channel};

/// The most a controller's firmware file may weigh. Intel's images are about
/// 800 KB; the heap recycles blocks up to 1 MiB, and a re-enumeration loop
/// (capped by the host) reads the file again each time.
const MAX_BYTES: usize = 2 << 20;

struct Files;

impl fwload::Reader for Files {
    fn size(&mut self, path: &str) -> Result<u64, i64> {
        match files::stat(path)? {
            (_, Kind::Dir) => Err(21), // EISDIR
            (size, Kind::File) => Ok(size),
        }
    }

    fn read(&mut self, path: &str, max: usize, out: &mut Vec<u8>) -> Result<(), i64> {
        files::read_large(path, max, out)
    }
}

/// The file `name`, or `None` (with the reason on the console) if it cannot
/// be loaded. A missing optional file is the loader's business, not an error.
pub(super) fn read(name: &str) -> Option<Vec<u8>> {
    match fwload::load(&mut Files, name, MAX_BYTES) {
        Ok(bytes) => Some(bytes),
        Err(why) => {
            trace::line!(Channel::Bt, "FW:READ name={name} error={why}");
            None
        }
    }
}
