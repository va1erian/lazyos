# midlc - the Messenger IDL compiler

`midlc.py` turns `.midl` interface definitions into:

- **Rust wire helpers** (`libs/generated/src/lib.rs`): typed `encode_*`/`decode_*`
  functions over the [`libmessenger`](../../libs/messenger) parcel codec,
- a **Markdown reference** per interface (`docs/idl/`),
- a machine-readable **manifest** (`idl/manifest.json`) with method ids and the
  interface hash.

The grammar and versioning rules are described in
[`docs/messenger.md`](../../docs/messenger.md) section 11. Method ids are stable:
an explicit `= 7` wins, otherwise a deterministic hash of the method name is
used, and adding a method never renumbers existing ones.

## Commands

```bash
# Regenerate the checked-in stubs and docs.
python tools/midlc/midlc.py --out libs/generated/src/lib.rs \
    --manifest idl/manifest.json --docs docs/idl idl/echo.midl

# Fail if the checked-in stubs are stale (CI does this).
python tools/midlc/midlc.py --check --out libs/generated/src/lib.rs idl/echo.midl

# Compiler tests.
python tools/midlc/test_midlc.py

# Generated-code round trips (host).
cargo test -p messenger-generated
```

CI runs all of the above in `.github/workflows/midlc.yml`.
