//! Shared runtime for LazyOS ring-3 programs.
//!
//! Provides the native syscalls ([`sys`], from `lazyos-sys`) and a global heap
//! allocator, so user programs can use `alloc` (`Vec`, `String`, `format!`).

#![no_std]
#![feature(alloc_error_handler)]
#![feature(optimize_attribute)]

extern crate alloc;

pub mod sys;

pub mod trace;

pub mod audio;

pub mod audio_events;

pub mod dev;

pub mod files;

/// Who is logged in, followed from `logind`'s events and answers.
pub mod logins;

/// The system-stats snapshot, shared with the xui system monitor.
pub use lazyos_sys::sysinfo;

pub mod messenger;

pub mod central;

pub mod messenger_async;

pub mod task_snapshot;

pub mod wifi;

mod heap;
