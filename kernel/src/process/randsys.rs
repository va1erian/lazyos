//! Native entropy syscall 26 (docs/networking-plan.md N2): random bytes for
//! native services, from the same kernel CSPRNG that backs Linux `getrandom(2)`
//! and `AT_RANDOM` ([`crate::entropy`]).
//!
//! `rdi` is the destination, `rsi` the byte count. The call writes
//! `min(count, MAX_BYTES)` bytes and returns how many, so an absurd count is a
//! short read, never a long loop; a count of zero returns 0 without touching
//! the pointer; an unwritable destination is `-EFAULT` and nothing is written.
//! It is open to every task and needs no capability, exactly like `getrandom`:
//! randomness grants no authority, and `smoltcp` (initial sequence numbers,
//! DHCP transaction ids, ephemeral ports) needs it in `netd`, which has no
//! capabilities at all. Before this call a native service could only seed its
//! own pool from `RDRAND` and the tick counter, as `keyd` does, which is
//! predictable on a CPU model without `RDRAND`.

use crate::entropy;

/// Most bytes one call returns.
pub const MAX_BYTES: usize = 256;

/// Route one syscall-26 call.
pub fn dispatch(buffer: u64, len: u64) -> u64 {
    let count = len.min(MAX_BYTES as u64) as usize;
    if count == 0 {
        return 0;
    }
    let mut bytes = [0u8; MAX_BYTES];
    entropy::fill(&mut bytes[..count]);
    // Validates the whole destination as writable user memory first: this gate
    // is open to every task, so a raw write would be a kernel-write primitive.
    let outcome = crate::ipc::syscalls::copy_out(buffer, &bytes[..count]);
    // Do not leave key-stream bytes on the kernel stack.
    core::hint::black_box(&mut bytes).fill(0);
    match outcome {
        Ok(()) => count as u64,
        Err(code) => (code as u64).wrapping_neg(),
    }
}
