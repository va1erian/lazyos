# Sentinel Journal

## 2026-09-25 - Memory Scrubbing for Named Secrets
**Vulnerability:** `Entry` in `libs/secretstore` held sensitive Wi-Fi passphrases and derived PMK keys in heap memory without zeroing them upon deletion or replacement.
**Learning:** `AppEntry` in `secretstore::app` implemented `Drop` to scrub secret memory using volatile writes, but `Entry` in `secretstore::lib` lacked `Drop`, leaving deleted secrets in heap memory.
**Prevention:** Always implement `Drop` with `core::ptr::write_volatile` on structs that hold raw passphrases, credentials, or derived cryptographic keys to ensure memory scrubbing before deallocation.
