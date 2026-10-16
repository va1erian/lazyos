# Clean-build benchmark (Windows)

`measure_build.ps1` times a complete LazyOS build from empty build directories,
to compare machines (a CPU upgrade, another PC). It times the command a user
runs, `python tools\run_demo.py --desktop --build-only --yes --reset-os`, and
splits the time into phases.

```bat
tools\buildbench\measure_build.cmd -Label "old CPU" -Runs 3 -WarmUp
tools\buildbench\measure_build.cmd -Label "new CPU" -Runs 3
```

The `.cmd` wrapper runs the script without changing the execution policy.

## What it does

1. **Prepares, untimed:** the pinned toolchain and the musl target,
   `cargo fetch` for every workspace the build uses, and a current BusyBox
   (cached, or built once in Docker).
2. **Sets the build directories aside:** `target\`, `xui-app\target\`,
   `rhai-host\target\` and `vendor\bootloader\target\` are renamed into
   `shots\.buildbench-saved\`. Nothing in them is deleted: WebKit builds, game
   data, keys and disk images all come back at the end. Each run needs the disk
   space of one full build.
3. **Runs:** each run starts from empty directories (plus the cached BusyBox)
   and builds with `CARGO_NET_OFFLINE=true`, so a download fails the run
   instead of skewing the time. The bootloader's BIOS stages are installed
   from crates.io by its `build.rs`: on a machine whose cargo cache lacks
   them, pass `-WarmUp` once (an untimed build first) or `-AllowNetwork`.
4. **Records:** one row per run in `%LOCALAPPDATA%\lazyos-build-bench\results.csv`
   (CPU, cores, RAM, power plan, Defender, commit, toolchain, phases), plus a
   log of every line with its time. A run that succeeded without a part (no
   `rhai`, a BusyBox rebuild) is marked in `problems`.
5. **Restores** the original directories, also after a failure or Ctrl+C. If
   the window was closed mid-run, `measure_build.cmd -Restore` puts them back.

## Phases

| Phase   | From                                    | To                                      |
|---------|-----------------------------------------|-----------------------------------------|
| `setup` | start                                   | `building rhai`                         |
| `rhai`  | `building rhai`                         | the next phase                          |
| `xui`   | `building the xui apps`                 | the image's first cargo line (includes the core packages) |
| `image` | the root `cargo build`'s first status line | the end (bootloader, kernel, services, disk image) |

`-Image console` builds without the desktop (no `xui` phase).

## For a fair comparison

- Same commit (recorded; `+dirty` flags local changes), same toolchain, same
  power plan, same `-Image`/`-Release`/`-Jobs`.
- The script clears `LAZYOS_*`, `RUSTFLAGS`, `CARGO_PROFILE_*`, wrapper and
  target-dir variables for the run, and refuses a `rustc-wrapper` (sccache) in
  a cargo config file.
- Windows Defender scanning `target\` slows Rust builds a lot: exclude the
  repository on both machines or on neither (the CSV records whether real-time
  protection is on).
- Use `-Runs 3` and compare medians; close IDEs (rust-analyzer) first, which
  the script checks.
