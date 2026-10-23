## 2026-03-31 - Memory Scrubbing in Keyd Service
**Vulnerability:** Key material and password verifier bytes remained in memory when KeyEntry or Account instances were dropped or when candidate verifier arrays went out of scope.
**Learning:** `keyd` stored raw symmetric key material (`material`) and password hashes (`verifier`) without explicit scrubbing on `Drop`. Furthermore, `wrap`/`unwrap`/`sign` copied key material onto the stack, leaving un-scrubbed stack copies.
**Prevention:** Implement `Drop` for secret-holding structs using `core::ptr::write_volatile(byte, 0)` to scrub sensitive memory on drop, pass references directly instead of making stack copies, and scrub temporary derivation buffers.
