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

## Browsing definitions

`midl_browser.py` is a stdlib-only Tk GUI that scans the repository for
`*.midl` files, parses them with the compiler's own parser, and shows a
navigable tree of interfaces, methods, structs and enums with method ids,
signatures, the interface hash and doc comments:

```bash
python tools/midlc/midl_browser.py                 # scan the repo
python tools/midlc/midl_browser.py idl             # scan a directory
python tools/midlc/midl_browser.py idl/echo.midl   # scan one file
```

Use the filter box to search across interface, method, struct and enum names;
`Copy` places the current detail pane on the clipboard.

CI runs all of the above in `.github/workflows/midlc.yml`.
