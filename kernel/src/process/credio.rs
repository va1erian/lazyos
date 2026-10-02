//! Credential blocks crossing the native syscall boundary (syscall 10).

use crate::ipc::credentials::Cred;
use crate::user_ptr;

/// Read a 40-byte credential block from validated user memory; `None` for a
/// null pointer or a range that is not readable user memory.
pub(super) fn read_cred(ptr: u64) -> Option<Cred> {
    if ptr == 0 {
        return None;
    }
    let bytes = user_ptr::try_bytes(ptr, 5 * 8).ok()?;
    let mut words = [0u64; 5];
    for (word, chunk) in words.iter_mut().zip(bytes.as_chunks::<8>().0) {
        *word = u64::from_le_bytes(*chunk);
    }
    Some(Cred::from_words(words))
}

/// Write a 40-byte credential block into validated user memory; `false` on a
/// null pointer or a range that is not writable user memory.
pub(super) fn write_cred(ptr: u64, cred: Cred) -> bool {
    ptr != 0 && user_ptr::try_copy_words(ptr, &cred.to_words()).is_ok()
}
