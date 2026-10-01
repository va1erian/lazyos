# Package tooling

Everything that builds or validates a LazyOS application package (`.lzp`,
`docs/packages.md`). The rule is the one the other harnesses follow: **the
builder and the reader must agree**, so both enforce the same layout and
manifest rules and both are tested against failure cases, not only the happy
path.

| Layer | What | Run |
|---|---|---|
| Host unit | `lazypkg`: zip container, path safety, layout, manifest semantics, inflate/CRC, the package API | `cargo test -p lazypkg --all-features` |
| Seeded fuzz | `lazypkg::fuzz::run` over mutated valid archives, random bytes, and every truncation, inside plain `cargo test` | `cargo test -p lazypkg fuzz::` |
| Coverage-guided fuzz | libFuzzer via `cargo-fuzz`, Linux (CI runs it for a bounded time) | `mkdir -p fuzz/corpus/lazypkg`, then `cargo fuzz run lazypkg --fuzz-dir fuzz fuzz/corpus/lazypkg fuzz/seeds/lazypkg -- -max_total_time=60` |
| Seed corpus | Checked in under `fuzz/seeds/lazypkg/`, generated deterministically (a valid stored archive, a valid deflated one, one with a bad path, one truncated) | `python fuzz/gen_corpus.py` (`--check` in CI) |
| Builder | Turns a source tree into `<system_name>-<version>.lzp`, running the same checks before the OS sees it | `python tools/pkg/build.py path/to/tree` |
| Sample packages | Builds the sample `.lzp` (`PKGDEMO.LZP`) from the built xui apps into `target/pkg/` for the image; `make_icons.py` generates the PNG icons | `python tools/pkg/build_samples.py` (also run by `python tools/xui/build.py`) |
| Package manager (host) | `pkgstore`: policy compilation, the explanation table (covers every `idl/` interface), the audit chain, install paths, access rules | `cargo test -p pkgstore` |
| Package manager (guest) | install, run labelled, remove, in a headless desktop session | `python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/pkg --script tools/screenshot/examples/pkg_install.json` |
| Builder tests | A valid tree builds and reopens; a missing icon, a bad `system_name`, and an unknown directory each fail | `python tools/pkg/test_build.py` |

## Building a package

```bash
python tools/pkg/build.py examples/paint --out dist
# -> dist/org.lazy.paint-1.2.0.lzp
```

The source tree is exactly the layout in `docs/packages.md`: `manifest.toml`,
`bin/*.elf`, the three `icons/app-*.png`, and optional `idl/`, `docs/`,
`icons/`, and `resources/`. `.png` files are stored (they are already
compressed); everything else is deflated.

The builder fails with a list of every problem it found, so a manifest can be
fixed in one pass:

```text
error: cannot build package:
app.system_name 'Bad' is not a reverse-DNS name
entry 'icons/app-32.png' is missing
```

## Reading a package

`libs/lazypkg` is pure `no_std` + `alloc`; it takes the whole archive as
`&[u8]` and never touches the OS. `Package::open` returns either a fully
validated package or an error, and `Package::read` inflates one entry while
checking its size and CRC-32. The installer is the only caller.

## Fuzzing

`lazypkg::fuzz::run` (behind the `fuzz` feature) opens the input as a package
and, on success, reads every entry and calls `digest` and `install_dir`; it
must never panic. The seeded tests in `libs/lazypkg/src/fuzz.rs` drive the same
function with `libs/fuzzkit`, and `fuzz/fuzz_targets/lazypkg.rs` is the
libFuzzer target. A crash found by libFuzzer is copied to
`fuzz/regressions/lazypkg/` once fixed, where the `checked_in_corpus_replays`
test runs it under plain `cargo test`.
