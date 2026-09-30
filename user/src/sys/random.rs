//! The native entropy syscall (26, docs/networking-plan.md N2): random bytes
//! from the kernel CSPRNG, for services that have no capabilities to seed a
//! pool with.

use core::arch::asm;

/// `random(buf, len)` — see `kernel/src/process/randsys.rs`.
pub const SYS_RANDOM: u64 = 26;

/// Most bytes one call returns; larger requests are short reads.
pub const RANDOM_MAX: usize = 256;

/// One call: the byte count written (at most [`RANDOM_MAX`]), or the negative
/// errno the kernel refused with.
fn random_once(buf: &mut [u8]) -> i64 {
    let result: u64;
    // Safety: `int 0x80` with syscall 26; the kernel validates the destination
    // and writes at most `min(len, 256)` bytes into it.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_RANDOM,
            in("rdi") buf.as_mut_ptr() as u64,
            in("rsi") buf.len() as u64,
            lateout("rax") result,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    result as i64
}

/// Fill `buf` with random bytes, looping over the kernel's short reads.
/// Returns the negative errno on failure (the buffer is then partly filled).
pub fn random(buf: &mut [u8]) -> Result<(), i64> {
    let mut filled = 0;
    while filled < buf.len() {
        let got = random_once(&mut buf[filled..]);
        if got < 0 {
            return Err(got);
        }
        if got == 0 {
            // A non-empty request that returns nothing would loop forever.
            return Err(-5);
        }
        filled += got as usize;
    }
    Ok(())
}
