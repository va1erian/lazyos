//! Small user-memory helpers reused by several syscall families:
//! single-value reads/writes narrower than a full `struct` (`fill_stat` and
//! friends validate and write whole structs themselves; this is for the
//! one-word cases). [`fill_random`] lives here too since both the ELF
//! start-stack (`AT_RANDOM`) and `getrandom(2)` need the kernel CSPRNG. Path
//! strings are read by [`super::cwd::read_path`].

use crate::user_ptr;

pub(super) fn write_u64(addr: u64, value: u64) {
    // Safety: caller ensures the address is valid user memory (the syscall
    // ABI's contract).
    unsafe { user_ptr::write::<u64>(addr, value) };
}

pub(super) fn write_u32(addr: u64, value: u32) {
    // Safety: caller ensures the address is valid user memory (the syscall
    // ABI's contract).
    unsafe { user_ptr::write::<u32>(addr, value) };
}

pub(super) fn read_u64(addr: u64) -> u64 {
    // Safety: caller ensures the address is valid user memory (the syscall
    // ABI's contract).
    unsafe { user_ptr::read::<u64>(addr) }
}

pub(super) fn read_u32(addr: u64) -> u32 {
    // Safety: caller ensures the address is valid user memory (the syscall
    // ABI's contract).
    unsafe { user_ptr::read::<u32>(addr) }
}

/// Fill `buffer` from the kernel CSPRNG ([`crate::entropy`]): `AT_RANDOM`
/// (stack canaries) and `getrandom(2)` share it.
pub(super) fn fill_random(buffer: &mut [u8]) {
    crate::entropy::fill(buffer);
}
