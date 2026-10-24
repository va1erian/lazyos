## 2026-03-31 - Constant-time Argon2id password verification against account enumeration
**Vulnerability:** `keyd` and `accountsd` short-circuited password verification for non-existent user accounts, skipping Argon2id KDF calculation and exposing timing side-channels that allowed user account enumeration.
**Learning:** `accountsd::check` short-circuited `known && verify(...)`, and `keyd::verify` returned `false` immediately on missing user accounts.
**Prevention:** In authentication services, always execute dummy KDF derivation over dummy salt/verifier for unknown users and avoid boolean short-circuiting before password checks.
