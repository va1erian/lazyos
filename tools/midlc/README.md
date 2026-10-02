# midlc - the Messenger IDL compiler

`midlc.py` turns `.midl` interface definitions into:

- **Rust wire helpers** (`libs/generated/src/lib.rs`): typed `encode_*`/`decode_*`
  functions over the [`libmessenger`](../../libs/messenger) parcel codec, plus
  typed `publish_*`/`subscribe_*` helpers for declared topics,
- a **Markdown reference** per interface (`docs/idl/`),
- a machine-readable **manifest** (`idl/manifest.json`) with method ids and the
  interface hash.

The grammar and versioning rules are described in
[`docs/messenger.md`](../../docs/messenger.md) section 11; topic declarations,
`transfers (...)` clauses (the handles and shared buffers a request carries
outside its body) and `ring` declarations (shared-memory rings for bulk data)
are documented in [`docs/midl.md`](../../docs/midl.md). Method ids are stable:
an explicit `= 7` wins, otherwise a deterministic hash of the method name is
used, and adding a method never renumbers existing ones.

The real interfaces live in `idl/` (registry, topics, echo, keyd, ...); all
Rust consumers, including the separate static-musl `xui-app` workspace, depend
on `messenger-generated` instead of copying ids or field numbers. A file may hold several interfaces (e.g. a service and its ACL scope interfaces, see
`idl/topics.midl`). Enums travel as `U32`; each variant is emitted as a
`{ENUM}_{VARIANT}` constant (`QOS_RELIABLE`), so callers never hand-type a discriminant.

## Commands

```bash
# Regenerate the checked-in stubs and docs.
python tools/midlc/midlc.py --out libs/generated/src/lib.rs \
    --manifest idl/manifest.json --docs docs/idl idl/echo.midl

# Fail if the checked-in stubs are stale (CI does this).
python tools/midlc/midlc.py --check --out libs/generated/src/lib.rs idl/echo.midl

# Compiler tests.
python tools/midlc/test_midlc.py
python tools/midlc/test_midlc_transfers.py
python tools/midlc/test_midlc_rings.py
python tools/midlc/test_midl_browser.py

# Generated-code round trips (host).
cargo test -p messenger-generated
```

## Browsing definitions

`midl_browser.py` is a stdlib-only Tk GUI that scans the repository for
`*.midl` files, parses them with the compiler's own parser, and shows a
navigable tree of every interface (a file may hold several) with its methods,
events, structs, enums, topics and rings. The detail pane gives method ids,
signatures including their `transfers (...)`, each transfer's slot, each
ring's layout, producer and doorbell or advance method (and which method
transfers it), the interface hash and doc comments. Discovery, loading and
filtering are in `midl_browser_model.py`:

```bash
python tools/midlc/midl_browser.py                 # scan the repo
python tools/midlc/midl_browser.py idl             # scan a directory
python tools/midlc/midl_browser.py idl/echo.midl   # scan one file
```

Use the filter box to search across interface, method, struct, enum, topic and
ring names (a method also matches on its transfers, so `Channel<` or `Ring<`
lists every method that carries one);
`Copy` places the current detail pane on the clipboard.

CI runs all of the above in `.github/workflows/midlc.yml`.
