//! Messenger kernel IPC core (issues #64+).
//!
//! This module owns the kernel-side objects the Messenger fabric is built on.
//! Channels, syscalls, and shared buffers land in later issues; today it is the
//! per-process handle table that gives every object reference an unforgeable,
//! rights-carrying name.

pub mod handles;
