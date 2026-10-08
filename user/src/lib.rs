//! Shared runtime for LazyOS ring-3 programs.
//!
//! Provides the native syscalls ([`sys`], from `lazyos-sys`) and a global heap
//! allocator, so user programs can use `alloc` (`Vec`, `String`, `format!`).

#![no_std]
#![feature(alloc_error_handler)]

extern crate alloc;

pub mod sys;

pub mod audio;

pub mod audio_events;

pub mod dev;

pub mod files;

/// The system-stats snapshot, shared with the xui system monitor.
pub use lazyos_sys::sysinfo;

pub mod messenger;

pub mod central;

pub mod messenger_async;

pub mod task_snapshot;

mod heap;
