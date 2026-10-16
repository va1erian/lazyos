# Clean-build benchmark (Windows)

`measure_build.ps1` times a complete LazyOS build from empty build directories,
to compare machines (a CPU upgrade, another PC). It times the command a user
runs, `python tools\run_demo.py --desktop --build-only --yes --reset-os`, and
splits the time into phases.

```bat
tools\buildbench\measure_build.cmd -Label "old CPU" -Runs 3 -WarmUp
tools\buildbench\measure_build.cmd -Label "new CPU" -Runs 3 -WarmUp
```

The `.cmd` wrapper runs the script without changing the execution policy.

| Option | Meaning |
|---|---|
| `-Label TEXT` | a name for the machine, written to the CSV |
| `-Runs N` | timed clean builds (default 1; compare medians of 3) |
| `-WarmUp` | one untimed clean build first: caches what the build downloads and warms the file cache |
| `-Image desktop\|console` | the desktop image (default) or the kernel and services only |
| `-Release` | the release profile (`run_demo.py --release`) |
| `-Jobs N` | `CARGO_BUILD_JOBS` |
| `-AllowNetwork` | do not set `CARGO_NET_OFFLINE` for the timed builds |
| `-ShowOutput` | echo every build line (the log always has them) |
| `-Yes` | do not ask before starting |
| `-ResultsDir DIR` | default `%LOCALAPPDATA%\lazyos-build-bench` |
| `-Restore` | put back what an interrupted run set aside, then exit |

## What it does

1. **Prepares, untimed:** the pinned toolchain and the musl target,
   `cargo fetch` for the root, `xui-app`, `rhai-host` and the UEFI bootloader,
   and a current BusyBox (cached, or built once in Docker; a file named by
   `LAZYOS_BUSYBOX` is used instead).
2. **Sets the build directories aside:** `target\`, `xui-app\target\`,
   `rhai-host\target\` and `vendor\bootloader\target\` are renamed into
   `shots\.buildbench-saved\`. Nothing in them is deleted: WebKit builds, game
   data, keys and disk images all come back at the end. Each run needs the disk
   space of one full build.
3. **Runs:** each run starts from empty directories (marked with a
   `.buildbench` file; only marked directories are ever deleted) plus the
   cached BusyBox, and builds with `CARGO_NET_OFFLINE=true`, so a download
   fails the run instead of skewing the time. The bootloader's `build.rs`
   installs its BIOS stages from crates.io with `-Zbuild-std`: on a machine
   whose cargo cache lacks them, pass `-WarmUp` (the script says so when cargo
   asks for the network).
4. **Records:** one row per run in `results.csv` (CPU, cores, RAM, power plan,
   Defender, commit, toolchain, phases in seconds), plus a log of every line
   with its time. A run that succeeded without a part (no `rhai`, a BusyBox
   rebuild) is marked in `problems` and left out of the median. A CSV written
   with other columns is left alone and the rows go to `results-2.csv`.
5. **Restores** the original directories, also after a failure or Ctrl+C. A
   build directory this script did not make is moved aside into
   `shots\buildbench-after\`, never deleted. If the window was closed
   mid-run, `measure_build.cmd -Restore` puts the originals back (and moves
   whatever is there now aside). Do not `git clean -fdx` before that:
   `shots\` is ignored by git.

A build directory that did not exist before the benchmark keeps the last run's
output (with its `.buildbench` marker).

## Phases

| Phase   | From                                    | To                                      |
|---------|-----------------------------------------|-----------------------------------------|
| `setup` | start                                   | `building rhai`                         |
| `rhai`  | `building rhai`                         | the next phase                          |
| `xui`   | `building the xui apps`                 | the image's first cargo line (includes the core packages) |
| `image` | the root `cargo build`'s first status line | the end (bootloader, kernel, services, disk image) |

`-Image console` has no `xui` phase.

## For a fair comparison

- Same commit (recorded; `+dirty` flags local changes), same toolchain, same
  power plan, same `-Image`/`-Release`/`-Jobs`.
- The script clears `LAZYOS_*` (except `LAZYOS_BUSYBOX`), `LAZYRAD_*`,
  `RUSTFLAGS`, `RUSTC`, `CARGO_PROFILE_*`, `CARGO_TARGET_*`, `CARGO_BUILD_*`,
  `RUSTUP_TOOLCHAIN` and wrapper variables for the run, refuses a `rustc-wrapper` or `target-dir` in a cargo
  config file, and warns about `rustflags`, `jobs`, `incremental` or
  `[profile]` there.
- Windows Defender scanning `target\` slows Rust builds a lot: exclude the
  repository on both machines or on neither (the CSV records whether
  real-time protection is on). If `target\` is a junction to a Dev Drive, the
  benchmark builds on the repository's drive instead.
- Close IDEs (rust-analyzer) and QEMU first; the script checks.
