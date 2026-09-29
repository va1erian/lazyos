//! Small user-memory helpers reused by several syscall families: reading a
//! NUL-terminated string, and single-value reads/writes narrower than a full
//! `struct` (`fill_stat` and friends validate and write whole structs
//! themselves; this is for the one-word cases). [`fill_random`] lives here
//! too since both the ELF start-stack (`AT_RANDOM`) and `getrandom(2)` need
//! the same weak PRNG.

use alloc::string::String;

use crate::user_ptr;

/// Read a NUL-terminated user string (bounded).
pub(super) fn read_cstr(ptr: u64) -> Option<String> {
    if ptr == 0 {
        return None;
    }
    let mut out = String::new();
    for i in 0..4096u64 {
        // Safety: user memory up to the NUL terminator (the syscall ABI's contract).
        let byte = unsafe { user_ptr::read::<u8>(ptr + i) };
        if byte == 0 {
            break;
        }
        out.push(byte as char);
    }
    Some(out)
}

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

/// Fill `buffer` with weak pseudo-randomness seeded from the tick counter.
/// Not cryptographically secure; good enough for `AT_RANDOM` and
/// `getrandom(2)` on a hobby kernel with no entropy source.
pub(super) fn fill_random(buffer: &mut [u8]) {
    let mut state =
        crate::arch::idt::TICKS.load(core::sync::atomic::Ordering::Relaxed) ^ 0x9E37_79B9_7F4A_7C15;
    for byte in buffer.iter_mut() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *byte = state as u8;
    }
}
