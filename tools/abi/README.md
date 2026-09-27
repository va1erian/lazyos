# Linux ABI conformance bench

Tooling that measures how far LazyOS is from running prebuilt
`x86_64-unknown-linux-musl` static binaries. See the wiki **Linux ABI Plan** and
**Linux ABI Compatibility**.

## Pieces

| File | Purpose |
|------|---------|
| `fixtures/` | Small Rust programs (std) built static for `x86_64-unknown-linux-musl`, one per contract. |
| `build.py` | Builds the fixtures into `target/abi/fixtures/*.elf` (adds the musl target if missing; tolerant if the host can't link). |
| `run.py` | For each fixture: embeds it as `INIT.ELF`, boots headless QEMU, parses the serial log, writes `docs/compat/matrix.md` + `compat.json`. |
| `coverage.py` | Scans the bench serial logs for `ENOSYS <nr> <name>` and writes `docs/compat/coverage.md` + `coverage.json`. |

## Convention

- A fixture prints `ABI:<name>:PASS` or `ABI:<name>:FAIL:<reason>` and exits
  `0`/`1`.
- The kernel, when it sees an injected `INIT.ELF` but cannot run Linux binaries
  yet, logs `ABI:INIT:SKIP:<reason>`.
- `run.py` classifies each fixture as `pass` / `fail` / `skip` / `not-run` /
  `unavailable` accordingly.

## The `INIT.ELF` hook

`build.rs` embeds the file named by the `LAZYOS_INIT` environment variable as
`INIT.ELF` in the disk image. The kernel, if `INIT.ELF` exists, runs *only* it
(instead of the demo programs) — so a bench run is deterministic. This is how the
runner isolates one fixture per boot.

## Local use

```bash
python tools/abi/build.py                 # build fixtures (skips if no musl cc)
python tools/abi/run.py --only hello --at 8
python tools/abi/coverage.py
```

On Windows the fixtures may not link (`cc` missing); the bench then reports them
`unavailable`. CI (Linux) builds and runs them.

## CI

`.github/workflows/abi-compat.yml` builds the fixtures, runs the bench, uploads
`docs/compat/**` and `shots/abi/**`, and publishes `Linux-ABI-Matrix` and
`Linux-ABI-Coverage` to the wiki. Each PR gets a comment with the current matrix.
