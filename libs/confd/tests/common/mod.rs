//! Test support shared by the integration binaries.
//!
//! Cargo compiles this module into each `tests/*.rs` binary separately, and
//! not every binary uses every helper, so the module opts out of the
//! per-binary dead-code lint.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fmt;

use confd::store::Caller;
use confd::{StoreFs, Value};

/// Root's uid.
pub const ROOT: Caller = Caller { uid: 0 };
/// A regular user.
pub const ALICE: Caller = Caller { uid: 1000 };
/// A second regular user.
pub const BOB: Caller = Caller { uid: 1001 };

/// Builds a caller with the given uid.
pub fn caller(uid: u32) -> Caller {
    Caller { uid }
}

/// Shortcut for a string value.
pub fn text(value: &str) -> Value {
    Value::Str(String::from(value))
}

/// Shortcut for a bytes value.
pub fn blob(value: &[u8]) -> Value {
    Value::Bytes(value.to_vec())
}

/// A deterministic SplitMix64 generator, so "random" tests are reproducible
/// without pulling in a dependency.
#[derive(Clone)]
pub struct SplitMix64(u64);

impl SplitMix64 {
    /// Seeds the generator.
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0..bound`; panics only if the test passes `bound == 0`.
    pub fn below(&mut self, bound: u64) -> u64 {
        assert!(bound > 0);
        self.next_u64() % bound
    }
}

/// An independent bitwise CRC-32/ISO-HDLC, used to build valid-CRC malformed
/// inputs so `decode` must reject them on structure alone, not the checksum.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Wraps a body in the real magic header and a matching CRC trailer, so
/// structural validation can be tested without `encode`.
pub fn envelope(body: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(body.len() + 8);
    data.extend_from_slice(b"CNFD");
    data.extend_from_slice(body);
    let crc = crc32(&data);
    data.extend_from_slice(&crc.to_le_bytes());
    data
}

/// An in-memory [`StoreFs`] with fault injection.
///
/// Every operation can be made to fail either permanently (the `fail_*`
/// flags) or after a number of successful calls (`arm`), which lets the
/// crash-injection test stop a `persist` at each individual step. A failing
/// `rename` is atomic — it leaves the target untouched — because that is the
/// contract `confd` relies on; `write_file` can optionally leave partial bytes
/// behind, since `persist` only ever writes the temporary file.
#[derive(Clone, Default, Debug)]
pub struct MemoryFs {
    files: BTreeMap<String, Vec<u8>>,
    ops: u32,
    fuel: Option<u32>,
    partial_writes: bool,
    fail_reads: bool,
    fail_writes: bool,
    fail_fsyncs: bool,
    fail_renames: bool,
    fail_removes: bool,
}

/// The injected filesystem error.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FsError;

impl fmt::Display for FsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("injected filesystem failure")
    }
}

impl MemoryFs {
    /// An empty filesystem.
    pub fn new() -> Self {
        Self::default()
    }

    /// Puts a file in the filesystem directly.
    pub fn put(&mut self, name: &str, data: &[u8]) {
        self.files.insert(String::from(name), data.to_vec());
    }

    /// The file's contents, if it exists.
    pub fn file(&self, name: &str) -> Option<&[u8]> {
        self.files.get(name).map(Vec::as_slice)
    }

    /// Number of filesystem operations attempted so far.
    pub fn ops(&self) -> u32 {
        self.ops
    }

    /// Allows `fuel` more successful operations, then fails all further ones.
    pub fn arm(&mut self, fuel: u32) {
        self.fuel = Some(fuel);
    }

    /// Removes any armed fault.
    pub fn disarm(&mut self) {
        self.fuel = None;
    }

    /// Makes a failing `write_file` leave half of the bytes behind.
    pub fn partial_writes(&mut self, enabled: bool) {
        self.partial_writes = enabled;
    }

    /// Fails every read.
    pub fn fail_reads(&mut self) {
        self.fail_reads = true;
    }

    /// Fails every write.
    pub fn fail_writes(&mut self) {
        self.fail_writes = true;
    }

    /// Fails every fsync.
    pub fn fail_fsyncs(&mut self) {
        self.fail_fsyncs = true;
    }

    /// Fails every remove.
    pub fn fail_removes(&mut self) {
        self.fail_removes = true;
    }

    /// Fails every rename.
    pub fn fail_renames(&mut self) {
        self.fail_renames = true;
    }

    /// Counts an attempt and reports whether the fault list says it must
    /// fail. The armed fuel is only consumed by attempts that reach it.
    fn tick(&mut self, permanently_blocked: bool) -> Result<(), FsError> {
        self.ops += 1;
        if permanently_blocked {
            return Err(FsError);
        }
        match &mut self.fuel {
            Some(0) => Err(FsError),
            Some(left) => {
                *left -= 1;
                Ok(())
            }
            None => Ok(()),
        }
    }
}

impl StoreFs for MemoryFs {
    type Error = FsError;

    fn read_file(&mut self, name: &str) -> Result<Option<Vec<u8>>, FsError> {
        let blocked = self.fail_reads;
        self.tick(blocked)?;
        Ok(self.files.get(name).cloned())
    }

    fn write_file(&mut self, name: &str, data: &[u8]) -> Result<(), FsError> {
        let blocked = self.fail_writes;
        if let Err(error) = self.tick(blocked) {
            if self.partial_writes {
                self.files
                    .insert(String::from(name), data[..data.len() / 2].to_vec());
            }
            return Err(error);
        }
        self.files.insert(String::from(name), data.to_vec());
        Ok(())
    }

    fn fsync(&mut self, name: &str) -> Result<(), FsError> {
        let blocked = self.fail_fsyncs;
        self.tick(blocked)?;
        if self.files.contains_key(name) {
            Ok(())
        } else {
            Err(FsError)
        }
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), FsError> {
        let blocked = self.fail_renames;
        self.tick(blocked)?;
        match self.files.remove(from) {
            Some(data) => {
                self.files.insert(String::from(to), data);
                Ok(())
            }
            None => Err(FsError),
        }
    }

    fn remove(&mut self, name: &str) -> Result<(), FsError> {
        let blocked = self.fail_removes;
        self.tick(blocked)?;
        self.files.remove(name);
        Ok(())
    }
}
