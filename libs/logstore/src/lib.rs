//! `logd`'s persistent journals (issue #508, docs/filesystem-plan.md F4).
//!
//! `logd` keeps its hash-chained ring in memory and also appends every record
//! to `/logs/<source>.log`. This crate is that store without Messenger or
//! syscall dependencies, so the same code runs in `logd`, in the kernel test
//! suite against a real ext2 volume, and in host tests:
//!
//! * [`source`]: which journal a record's (untrusted) topic maps to;
//! * [`line`]: the tab-separated, escaped, hash-chained line format and its
//!   verifier;
//! * [`rotate`]: the per-file cap, the rotation and the `/logs` budget as pure
//!   arithmetic;
//! * [`store`]: buffered appends, flushing and rotation over [`JournalFs`].
#![no_std]

extern crate alloc;

pub mod line;
pub mod rotate;
pub mod source;
pub mod store;

pub use line::{parse_line, verify, Broken, Line};
pub use rotate::{file_name, Action, Footprint, Ledger, Limits, BUDGET, FILE_CAP};
pub use source::{source_of, valid_source};
pub use store::{JournalFs, Store, TailError, FLUSH_RECORDS, FLUSH_TICKS, MAX_SOURCES};
