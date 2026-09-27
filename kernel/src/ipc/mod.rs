//! Messenger kernel IPC core (issues #64+).
//!
//! This module owns the kernel-side objects the Messenger fabric is built on:
//! the per-process handle table that gives every object reference an
//! unforgeable, rights-carrying name, and the channels that carry one-way
//! messages and synchronous transactions. Syscalls and shared buffers land in
//! later issues.

pub mod channels;
pub mod handles;
