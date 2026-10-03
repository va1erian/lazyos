//! Whether the crypto's SIMD dispatch could pick an AVX path, recomputed.
//!
//! The RustCrypto crates behind `nettls-crypto` (AES, GHASH/POLYVAL,
//! ChaCha20, SHA-2, curve25519) choose backends at run time through the
//! `cpufeatures` crate. Its `avx`/`avx2` checks (0.2.17, `src/x86.rs`)
//! require CPUID.1:ECX.XSAVE and OSXSAVE (bits 26 and 27) and then the XMM
//! and YMM bits of XCR0. LazyOS saves FPU state with FXSAVE and leaves
//! CR4.OSXSAVE clear, so on LazyOS no AVX path may be chosen: YMM state would
//! not survive a context switch. This module reads the same inputs.

use std::arch::x86_64::{__cpuid, __cpuid_count, _xgetbv};

/// The CPUID and XCR0 bits the dispatch depends on.
pub struct Features {
    pub xsave: bool,
    pub osxsave: bool,
    pub xcr0: Option<u64>,
    pub avx: bool,
    pub avx2: bool,
    pub aesni: bool,
    pub pclmulqdq: bool,
    pub ssse3: bool,
    pub sha: bool,
}

impl Features {
    pub fn probe() -> Features {
        let leaf1 = __cpuid(1);
        let max_leaf = __cpuid(0).eax;
        let leaf7 = (max_leaf >= 7).then(|| __cpuid_count(7, 0));
        let xsave = bit(leaf1.ecx, 26);
        let osxsave = bit(leaf1.ecx, 27);
        // SAFETY: XGETBV is only executed when CPUID says the OS enabled
        // it (OSXSAVE), the architectural condition for it not to fault.
        let xcr0 = (xsave && osxsave).then(|| unsafe { _xgetbv(0) });
        Features {
            xsave,
            osxsave,
            xcr0,
            avx: bit(leaf1.ecx, 28),
            avx2: leaf7.is_some_and(|l| bit(l.ebx, 5)),
            aesni: bit(leaf1.ecx, 25),
            pclmulqdq: bit(leaf1.ecx, 1),
            ssse3: bit(leaf1.ecx, 9),
            sha: leaf7.is_some_and(|l| bit(l.ebx, 29)),
        }
    }

    /// `cpufeatures`' rule for "avx2" (YMM state enabled in XCR0, plus the
    /// CPUID bits): true means an AVX2 backend may be chosen.
    pub fn avx_path(&self) -> bool {
        let ymm = self.xcr0.is_some_and(|x| x & 0b110 == 0b110);
        ymm && (self.avx || self.avx2)
    }

    /// One report line.
    pub fn line(&self) -> String {
        let b = |v: bool| u8::from(v);
        format!(
            "TLSFIX:CPU xsave={} osxsave={} xcr0={} avx={} avx2={} aesni={} pclmulqdq={} ssse3={} sha={} avx_path={}",
            b(self.xsave),
            b(self.osxsave),
            self.xcr0.map_or("unread".to_string(), |x| format!("{x:#x}")),
            b(self.avx),
            b(self.avx2),
            b(self.aesni),
            b(self.pclmulqdq),
            b(self.ssse3),
            b(self.sha),
            if self.avx_path() { "yes" } else { "no" }
        )
    }
}

fn bit(word: u32, n: u32) -> bool {
    word & (1 << n) != 0
}

/// The kernel's name (`uname`'s sysname): "LazyOS" there, "Linux" on a host.
pub fn sysname() -> String {
    let mut buf = [0u8; 6 * 65];
    let ret: isize;
    // SAFETY: uname(2) (syscall 63) writes at most six 65-byte fields into
    // `buf`, which is exactly that size; rcx/r11 are clobbered by `syscall`.
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") 63isize => ret,
            in("rdi") buf.as_mut_ptr(),
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack)
        );
    }
    if ret != 0 {
        return String::new();
    }
    let end = buf[..65].iter().position(|&c| c == 0).unwrap_or(65);
    String::from_utf8_lossy(&buf[..end]).into_owned()
}
