//! Credential blocks crossing the native syscall boundary (syscall 10).

use alloc::string::String;

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

/// Words in a labelled-spawn block: the five credential words, then the label
/// string's user pointer and byte length.
const LABELLED_WORDS: usize = 7;

/// Read a labelled-spawn block: a credential (its `label_id` word is ignored;
/// the kernel interns the string and chooses the id) plus the label string.
/// `None` for any unreadable pointer, an over-long or non-UTF-8 label.
pub(super) fn read_labelled(ptr: u64) -> Option<(Cred, String)> {
    if ptr == 0 {
        return None;
    }
    let bytes = user_ptr::try_bytes(ptr, LABELLED_WORDS * 8).ok()?;
    let mut words = [0u64; LABELLED_WORDS];
    for (word, chunk) in words.iter_mut().zip(bytes.as_chunks::<8>().0) {
        *word = u64::from_le_bytes(*chunk);
    }
    let len = usize::try_from(words[6]).ok()?;
    if len == 0 || len > crate::ipc::labels::MAX_LABEL_BYTES {
        return None;
    }
    let label = user_ptr::try_bytes(words[5], len).ok()?;
    let label = core::str::from_utf8(label).ok()?;
    let cred = Cred::from_words([words[0], words[1], words[2], 0, words[4]]);
    Some((cred, String::from(label)))
}
