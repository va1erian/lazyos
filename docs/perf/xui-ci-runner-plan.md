# Faster `xui-app` CI job on GitHub runners

The `xui-app` workflow (`.github/workflows/xui.yml`) builds the xui apps, the
OS image and BusyBox, then drives sixteen headless QEMU sessions. A green run
takes about 16 minutes on `ubuntu-latest`. Measured on run 36778645702
(2026-09-30), the time goes to:

| Step | Time | Cause |
|---|---|---|
| Build the app (static musl) | 5.5 min | The pure-Rust apps take ~1.5 min. The Docs app (`xui-docs`) takes the other ~4 min: zig compiles libc++/libc++abi/libunwind/musl from source on first use (nothing is cached across runs), and litehtml's ~80 C/C++ files compile one at a time because `cc-rs` is built without its `parallel` feature. |
| Capture M0 | 2 min | The first `cargo build` of the whole OS, with a cold registry and an empty `target/`. |
| Capture Editor | 1.5 min | `tools/screenshot/examples/xui_editor.json` waits a fixed 45 s (`"at": 76.5`) after the menu click for the second Editor window; under KVM it is up within seconds. |
| Verify screenshots | 1 min | `tools/screenshot/pngstats.py` unfilters and scans 62 PNGs with per-pixel Python loops, one file at a time. |
| Build BusyBox | 0.5 min | Downloads and compiles BusyBox from source every run. |
| 16 × `cargo build` | ~2.5 min | ~9 s each to re-embed apps and rebuild the image; every step has a different env, so this stays. |

No workflow in the repository uses any caching; git history shows no reason
for that. The plan below should bring a warm-cache run to roughly 7–8 minutes
without changing what the job verifies.

## 1. Cache cargo registries and target directories

Add `Swatinem/rust-cache@v2` right after `actions/setup-python` (before any
cargo invocation) in the `xui` job of `.github/workflows/xui.yml`:

```yaml
      - uses: Swatinem/rust-cache@v2
        with:
          workspaces: |
            . -> target
            xui-app -> target
          cache-on-failure: "true"
          key: xui
```

Notes:

- The OS workspace (`.`) and the app workspace (`xui-app`) have separate
  lockfiles and target dirs; both must be listed.
- The Docs app builds into `target/xui-zig` (`DOCS_TARGET_DIR` in
  `tools/xui/build.py`), which is a nested cargo target dir under `target/`;
  rust-cache recurses into nested target dirs (they carry `CACHEDIR.TAG`), so
  no extra entry is needed. Do not move `DOCS_TARGET_DIR`.
- `cache-on-failure` keeps the builds when a capture step is red, so a retry
  does not pay the cold build again.
- rust-cache sets `CARGO_INCREMENTAL=0`; that is desired on CI.

## 2. Cache the zig toolchain's global cache

zig keeps the per-target libc/libc++ it builds in its global cache
(`~/.cache/zig` on Linux; `python -m ziglang env` prints `global_cache_dir`).
Pin the location and cache it, keyed on the pinned zig version from
`tools/xui/zig.py`:

```yaml
      - name: Pin and cache the zig global cache
        shell: bash
        run: echo "ZIG_GLOBAL_CACHE_DIR=$HOME/.cache/zig" >> "$GITHUB_ENV"
      - uses: actions/cache@v4
        with:
          path: ~/.cache/zig
          key: zig-0.16.0-${{ runner.os }}
```

Put these before the "Install zig" step (the env var must be set before the
first zig invocation). Keep the version string in step with `zig.ZIG_VERSION`;
mention in a comment that the key must be bumped with the pin.

## 3. Compile litehtml in parallel

`litehtml-sys` (a git dependency of `xui-litehtml`, see `xui-app/Cargo.lock`)
drives `cc-rs` from its build script; `cc` 1.5.1 only compiles files in
parallel with its `parallel` feature, which nothing enables today. Cargo
unifies host (build-dependency) features across the graph, so enabling it from
`xui-docs` turns it on for `litehtml-sys` too:

- In `xui-app/docs/Cargo.toml` add

  ```toml
  [build-dependencies]
  # litehtml-sys compiles ~80 C/C++ files through cc-rs; without this feature
  # it does so one at a time. Host features unify, so it applies there too.
  cc = { version = "1", features = ["parallel"] }
  ```

- Add a minimal `xui-app/docs/build.rs` (a build script is what makes cargo
  honour the build-dependency); it only needs
  `println!("cargo:rerun-if-changed=build.rs");` and a doc comment saying why
  it exists.
- Verify with `cargo tree --manifest-path xui-app/Cargo.toml -p xui-docs
  --target x86_64-unknown-linux-musl -e features -i cc` that `cc` now has
  `parallel`, and with a `cargo build -vv` of `xui-docs` (through
  `python tools/xui/build.py`) that the C++ objects compile concurrently.
- `Cargo.lock` of `xui-app` gains `jobserver` (and keeps `libc`); commit it.

## 4. Gate the Editor session on readiness instead of a fixed 45 s

`tools/screenshot/qemu_session.py`'s `wait_for` searches the serial log from
offset 0, so a second `EDITOR:UP:PASS` cannot be waited for today. Add an
optional `occurrence` key (default 1) to `wait_for` steps meaning "the N-th
match", implemented in `SerialLog.wait_for` (count matches with
`pattern.finditer` or by iterating `search` from each match's end), and
validate it (`occurrence >= 1`, integer) when the step is parsed. Then in
`tools/screenshot/examples/xui_editor.json` replace the fixed timeline after
the menu click:

```json
  {"mouse_click": "left", "until": "XUID:MENU:LAUNCH:", "timeout": 30, "retries": 1, "at": 31.5},
  {"wait_for": "EDITOR:UP:PASS", "occurrence": 2, "timeout": 120},
  {"at": 2.0, "shot": "11_second_editor_up"},
  {"at": 2.5, "mouse_move": [-580, -90]},
  {"at": 3.0, "mouse_click": "left"},
  {"key_down": "ctrl", "at": 3.5},
  {"key": "v", "at": 4.0},
  {"key_up": "ctrl", "at": 4.5},
  {"at": 5.5, "shot": "12_second_editor_pasted"},
  {"at": 6.5, "quit": true}
```

(`at` counts from the latest `wait_for` gate, see the README.) Keep the
relative mouse move and the paste exactly as they are. Document `occurrence`
in `tools/screenshot/README.md` next to `wait_for`, and add a unit test for
the N-th-occurrence logic (`tools/screenshot/` has `test_*.py` files to model
on; the `SerialLog` class can be tested against a temporary file).

## 5. Make the screenshot check parallel and cheaper

`tools/screenshot/pngstats.py` must stay standard-library only (it is used by
agents and CI without Pillow/numpy). Two changes:

- Analyse files in a `concurrent.futures.ProcessPoolExecutor` (one task per
  file; keep output order deterministic, i.e. print results in argument order).
  Keep `decode_png`, `analyse` and the module API unchanged: the `docs` and
  `docs_open` steps of `xui.yml` import `decode_png` from it.
- Replace the per-pixel Python loop in `analyse` with
  `collections.Counter` over `zip(pixels[0::channels], pixels[1::channels],
  pixels[2::channels])` (and the alpha channel when present), then compute the
  mean, non-background ratio, distinct 4-bit colours and min/max luminance
  from the (colour, count) pairs. Results must be bit-identical to the old
  implementation for the same input; keep the alpha==0 → black rule.

Add a test (`tools/screenshot/test_pngstats.py`, stdlib `unittest`) that
encodes small synthetic PNGs with each filter type (0–4) via `zlib`/`struct`
and checks that the new `analyse` matches a straightforward reference
implementation, including an RGBA image with transparent pixels and a
greyscale one.

## 6. Cache the BusyBox build

`tools/abi/busybox.py` prefers `tools/abi/busybox` (git-ignored) over a fresh
build. Cache that path:

```yaml
      - uses: actions/cache@v4
        id: busybox
        with:
          path: tools/abi/busybox
          key: busybox-${{ hashFiles('tools/abi/busybox.py') }}
```

and after `python tools/abi/busybox.py` copy `target/abi/busybox/busybox` to
`tools/abi/busybox` when the cache missed (`steps.busybox.outputs.cache-hit !=
'true'`). Place this cache step after the rust-cache step: post-steps run in
reverse order, and rust-cache's cleanup must not run before this cache saves.

## Out of scope (follow-ups)

- Splitting the capture steps into a job matrix (each group restores the
  caches and runs 3–4 sessions) would roughly halve wall-clock again, at the
  cost of a different artifact layout for the `publish` job.
- The same rust-cache/zig-cache steps apply to `rhai.yml`, which also runs
  `tools/xui/build.py`.

## Verification

- `python tools/xui/test_zig.py`, `python tools/screenshot/test_qmp_mouse.py`
  and the new tests pass.
- `python tools/screenshot/pngstats.py` on a real QEMU screenshot gives the
  same numbers as before the change.
- A workflow run on the PR is green and the `xui` job's step times drop as
  listed above; the second run (warm caches) is the one that shows the gain.
