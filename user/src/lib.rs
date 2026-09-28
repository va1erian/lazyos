//! Shared runtime for LazyOS ring-3 programs.
//!
//! Provides the `int 0x80` syscall wrappers ([`sys`]) and a global heap
//! allocator, so user programs can use `alloc` (`Vec`, `String`, `format!`).

#![no_std]
#![feature(alloc_error_handler)]

extern crate alloc;

pub mod sys;

pub mod sysinfo;

pub mod lang;

pub mod messenger;

pub mod central;

pub mod messenger_async;

pub mod task_snapshot;

mod heap;
