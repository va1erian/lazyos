//! Bookkeeping of the kernel's memory allocators (issue #485), apart from
//! the memory they manage so it can be tested on the host and under Miri.
//!
//! * [`slab`]: the size classes, the intrusive free list a class threads
//!   through its free slots, and the per-class and per-owner ledgers. The
//!   kernel's typed-object slabs (`kernel/src/mem/slab.rs`) and the heap's
//!   small-object front (`kernel/src/mem/heap.rs`) are both built on it.
//! * [`frames`]: the physical frame allocator's reference-count rules (who
//!   may share or free a frame at which count), its free chain and its
//!   counters (`kernel/src/mem/frames.rs`). Physical memory is reached only
//!   through the [`frames::FrameMemory`] trait, which the kernel implements
//!   over its physical-memory mapping and the tests over plain vectors.
//!
//! `no_std`, no dependencies. The only `unsafe` is the slab free list, which
//! writes links into the slots themselves; `cargo +nightly miri test -p
//! membook` checks it against real allocations (CI: `.github/workflows/miri.yml`).

#![cfg_attr(not(test), no_std)]

pub mod frames;
pub mod slab;
