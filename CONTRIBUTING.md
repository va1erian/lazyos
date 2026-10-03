# Contributing to LazyOS

Read [`AGENTS.md`](AGENTS.md) (workflow and verification commands) and the
"Code standards" section of [`README.md`](README.md) first. In short: security
first, small readable functions, every `unsafe` block minimal with a
`// SAFETY:` comment, and source files under 500 lines.

## Setup

- Rust: `rustup` installs the toolchain pinned in `rust-toolchain.toml`.
- Python 3 and QEMU (`qemu-system-x86_64`); on Windows put `C:\Program Files\qemu`
  on `PATH`.

## Before you push

```bash
cargo fmt --all                                  # CI runs `cargo fmt --all -- --check`
cargo clippy -p kernel -p user --target x86_64-unknown-none \
    -Zbuild-std=core,alloc -- -D warnings        # no_std crates
cargo clippy -p libmessenger -p messenger-generated -p lazyos-crypto -p font-atlas \
    -p confd -p timezone -p inputmap -- -D warnings  # host libraries (CI also lints
                                                     # usbhid, xhci, ext2fs: ci.yml)
cargo build                                      # produces target/lazyos.img
python tools/test/run.py --accel none            # in-kernel unit + soak suite
```

Also run whichever of these matches your change (see `AGENTS.md` for details):

- graphics or input: `tools/screenshot/qemu_shot.py` / `qemu_session.py`, then
  look at the PNG and run `pngstats.py`;
- Linux ABI: `python tools/abi/run.py --at 8`;
- shell: `tools/screenshot/examples/fs_demo.json` (BusyBox `sh`).

## Kernel changes need tests

Every kernel component ships correctness tests and stress/soak tests under
`kernel/src/tests/` (one `*_suite` per subsystem, `TEST:<name>:PASS|FAIL`
protocol). A change is not done until `python tools/test/run.py --accel none`
passes.

## Pull requests

Keep them focused, describe what you verified (and how), and do not commit
generated screenshots (`shots/` is git-ignored). Link the issue the change
closes and note any gap you left.
