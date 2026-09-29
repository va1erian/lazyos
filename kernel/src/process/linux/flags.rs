//! ABI flag and mode-bit constants shared by more than one syscall family
//! (family-specific flags, such as `mmap`'s `MAP_FIXED` or `epoll`'s
//! `EPOLL_CLOEXEC`, stay local to the module that alone interprets them).

// `struct stat` file-type bits (the VFS carries full modes; only the bits the
// synthetic entries need are spelled out here).
pub(super) const S_IFREG: u32 = 0o100000;
pub(super) const S_IFCHR: u32 = 0o020000;
pub(super) const S_IFIFO: u32 = 0o010000;
pub(super) const S_IFSOCK: u32 = 0o140000;

// `pipe2`/`eventfd2`/`fcntl` non-blocking and close-on-exec bits, and the
// `socket`/`socketpair` domain and type constants: shared between the pipe,
// socket and epoll/eventfd families.
pub(super) const O_NONBLOCK: u64 = 0o4000;
pub(super) const O_CLOEXEC: u64 = 0o2000000;
pub(super) const AF_UNIX: u64 = 1;
pub(super) const SOCK_STREAM: u64 = 1;
pub(super) const SOCK_SEQPACKET: u64 = 5;
pub(super) const SOCK_NONBLOCK: u64 = 0o4000;
pub(super) const SOCK_CLOEXEC: u64 = 0o2000000;
