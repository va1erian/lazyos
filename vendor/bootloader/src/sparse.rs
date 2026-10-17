//! Sparse image files.
//!
//! NTFS allocates and zero-fills a dense file as far as the first write past
//! its end, synchronously, so extending a disk image with `set_len` and then
//! writing its last partition cost seconds of zeros per GiB on a spinning
//! drive. A sparse file allocates what is written. Other hosts' file systems
//! are sparse already, and one that cannot be (FAT, exFAT) refuses the request,
//! which is ignored.

use std::fs::File;

/// Make `file` sparse; call it before `set_len`.
pub fn mark_sparse(file: &File) {
    #[cfg(windows)]
    {
        use std::ffi::c_void;
        use std::os::windows::io::AsRawHandle;

        const FSCTL_SET_SPARSE: u32 = 0x0009_00C4;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn DeviceIoControl(
                device: *mut c_void,
                code: u32,
                input: *const c_void,
                input_len: u32,
                output: *mut c_void,
                output_len: u32,
                returned: *mut u32,
                overlapped: *mut c_void,
            ) -> i32;
        }
        let mut returned = 0u32;
        // SAFETY: `file` owns a valid handle for the whole call; no input or
        // output buffer and no overlapped structure are passed, and
        // `returned` is a live local the call writes its byte count to.
        let _ = unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                FSCTL_SET_SPARSE,
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
    }
    #[cfg(not(windows))]
    let _ = file;
}
