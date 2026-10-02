# AGENTS.md

Guidance for AI agents working in this repository.

## Visual verification workflow

LazyOS renders to a framebuffer, so correctness is often visual. Do not claim a
graphics change works from source alone — capture and inspect real pixels.

1. **Ensure QEMU is available.** On Windows, `C:\Program Files\qemu` must be on
   `PATH`, or pass `--qemu "C:\Program Files\qemu\qemu-system-x86_64.exe"`.

2. **Capture a screenshot** (headless; no window needed):

   ```bash
   python tools/screenshot/qemu_shot.py --out shots --at 2,5,10 --image target/lazyos.img
   ```

   Omit `--image` to boot firmware only and validate the capture pipeline.

3. **Inspect it visually.** Read the PNG in the workspace (e.g. `shots/shot_10s.png`)
   with the Read tool to actually see the rendered output.

4. **Assert programmatically** so the check is reproducible and CI-able:

   ```bash
   python tools/screenshot/pngstats.py shots/*.png --min-nonblack 0.01
   ```

   Add `--expect-width`, `--expect-height`, `--min-colors`, or `--max-mean` as needed.

5. **Run CI** for a full headless pass: `.github/workflows/screenshots.yml`
   captures, verifies, uploads artifacts, publishes to the `screenshots` branch,
   and comments images on pull requests.

See `tools/screenshot/README.md` for full options.

## Driving the guest (input injection)

To interact with LazyOS — type commands, click, scroll — script it with
`qemu_session.py` and capture the resulting pixels:

```bash
python tools/screenshot/qemu_session.py --image target/lazyos.img \
    --out shots/session --script tools/screenshot/examples/type_and_shot.json
```

Then Read the resulting `shots/session/shot_*.png`. For custom agent loops,
import `tools/screenshot/qemu_qmp.py` and call `type_text`, `press_key`,
`mouse_move`, `mouse_click`, `mouse_scroll`, `mouse_abs`, and `screenshot`.
Input is delivered via QMP `input-send-event`, so it works headless.

## Running the demo

LazyOS is built around **Messenger**, a kernel-mediated, capability-based
IPC/pub-sub fabric, with userspace system services (`messengerd`, `init`,
`logd`, `healthd`, `keyd`, `accounts`, `clipboardd`, the `xuid` display
compositor, and more) running over a preemptive multitasking kernel with a
VFS (an ext2 read/write OS volume at `/`, a read-only FAT `/boot`, ramfs
`/transient` and `/tmp`, an optional ext2 home volume). The authoritative description of the
current architecture and the staged roadmap (S0–S9) is
[`docs/platform-plan.md`](docs/platform-plan.md), with per-subsystem detail in
[`docs/architecture/`](docs/architecture) (boot, memory, tasks, filesystem,
IPC, processes, display, etc.) and focused plans for
[`messenger.md`](docs/messenger.md), [`security-model.md`](docs/security-model.md),
[`linux-abi-plan.md`](docs/linux-abi-plan.md), and [`xui-plan.md`](docs/xui-plan.md).
Read those for anything about kernel internals, syscall numbers, or
window/task management rather than assuming from comments elsewhere.

Boot it with one command:

```bash
python tools/run_demo.py
```

QEMU hardware acceleration (WHPX on Windows, KVM on Linux) is auto-detected and
makes rendering several times faster than TCG; force it off with `--accel none`.
`run_demo.py` builds `target/lazyos.img` if needed (`--no-build`, `--headless`,
`-- --cpu max` are supported). It also creates `target/home.img` (the `/home`
volume, label `lazyhome`) when missing and attaches it as a second virtio-blk
disk (`--home-disk PATH`, `--no-home-disk`, `--reset-home`; `--reset-os` rebuilds
with `LAZYOS_RESET_OS=1`; `--data-disk` is opt-in). The screenshot tools attach
none unless given `--home-disk PATH`. For scripted visual verification, capture
a session script instead:

```bash
python tools/screenshot/qemu_session.py --image target/lazyos.img \
    --out shots/demo --script tools/screenshot/examples/multitask_demo.json
```

### The disk image

`cargo build` writes `target/lazyos.img` as an MBR disk with three partitions:
the bootloader's stage 2, a FAT `/boot` (only the kernel and a generated
`lazyos.cfg`), and an ext2 OS volume at LBA 131072 (64 MiB; `LAZYOS_OS_SIZE`,
default `512M`, minimum `128M`) that holds every other file at the flat names
it always had (`SUPER.ELF`, `PASSWD`, `docs/...`) plus `/data`. The volume is
written by `libs/ext2fs`, the code the kernel mounts it with (`build_support/os_*.rs`).
A rebuild **updates the OS volume in place**: installed apps, settings, logs
and your own files survive, and only paths listed in `/system/.image-manifest`
are replaced or deleted. `LAZYOS_RESET_OS=1 cargo build` (or
`python tools/run_demo.py --reset-os`) recreates it with a new UUID; so does an
image that fails validation, with a `cargo:warning=` giving the reason. Changing
`LAZYOS_OS_SIZE` on an existing image needs the reset. Do not rebuild while QEMU
has the image open (the build fails with a message). CI sets `LAZYOS_RESET_OS=1`
everywhere. ext2 is case-sensitive: look names up exactly as stored, through
`libs/fhs`. Host tests: `cargo test -p build-support-tests`.

## Docs app and the C++ toolchain

`xui-docs` renders Markdown with litehtml, which is C++, so it is built with zig
(`pip install ziglang==0.16.0`, then `python tools/xui/build.py`; see
[`docs/xui-docs.md`](docs/xui-docs.md)). Without zig the script skips it with a
warning and every other app still builds. `python tools/xui/test_zig.py` tests
the toolchain helper. Screenshot sessions: `tools/screenshot/examples/xui_docs.json`
(wheel scrolling) and `xui_docs_open.json` (Open dialog and `/TESTDOC.MD`).

## Doom (an installable `.lzp` package)

Doom is `doom/` (doomgeneric, fetched at a pinned revision and compiled with
zig, plus a Rust platform layer on `xui-app`'s client window) shipped as the
package `org.lazy.doom` with the Freedoom IWAD inside; see
[`doom/README.md`](doom/README.md) and [`docs/doom-port-plan.md`](docs/doom-port-plan.md).

```bash
python tools/doom/build.py          # target/doom/doom.elf + target/pkg/DOOM.LZP (fetches doomgeneric, Freedoom)
python tools/run_demo.py --doom     # desktop with /DOOM.LZP; then `pkgctl install /DOOM.LZP`
cargo test --manifest-path doom/Cargo.toml --lib
python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/doom     --script tools/screenshot/examples/doom.json   # needs a fresh OS volume (LAZYOS_RESET_OS=1)
```

The Terminal reports one `TERM:OUT` per command, and a command that wraps past
80 columns reports its own tail instead: keep typed commands short (`doom.json`
sets `PS1='# '` first). Shell command substitution (`$(...)`) currently hangs
the desktop Terminal's shell; avoid it in session scripts.

## Rhai scripting (`rhai` command and `msg` module)

`rhai` (`rhai-host/`, bindings in `libs/rhai-lazy/`) is a static-musl command
embedded as `RHAI.ELF`; the plan is [`docs/rhai-plan.md`](docs/rhai-plan.md).
Its `msg` module calls any Messenger service from a script, driven by a table
`midlc --schema` generates from `idl/` ([`docs/rhai/msg.md`](docs/rhai/msg.md)).
One command builds `rhai`, BusyBox and the image, boots it and judges it:

```bash
python tools/rhai/run.py              # console checks (rhai_demo.json)
python tools/rhai/run.py --desktop    # plus the desktop Terminal and the msg session
cargo test --manifest-path libs/rhai-lazy/Cargo.toml   # bindings vs an in-memory fabric
python tools/midlc/midlc.py --schema libs/rhai-lazy/src/msg/idl.rs idl/*.midl   # after an IDL change
```

`python tools/run_demo.py` rebuilds `rhai` before each image (`--no-rhai` skips it).

## Linux ABI conformance bench

Compatibility with Linux (`x86_64-unknown-linux-musl`) binaries is tracked by a
bench that runs from day one, before any ABI support exists:

```bash
python tools/abi/build.py            # build the static musl fixtures
python tools/abi/run.py --at 8       # run each fixture in headless QEMU, write the matrix
python tools/abi/coverage.py         # summarise ENOSYS syscalls from the logs
```

`run.py` embeds one fixture as `INIT.ELF` (via `LAZYOS_INIT`), boots, and
classifies it from the serial log (`ABI:<name>:PASS|FAIL`, or `ABI:INIT:SKIP`).
It writes `docs/compat/matrix.md` (+ `compat.json`). CI
(`.github/workflows/abi-compat.yml`) runs the bench, publishes the matrix and
coverage to the wiki, and comments them on PRs. See `tools/abi/README.md` and the
wiki **Linux ABI Plan**.

## Kernel test harness

The in-kernel unit/soak suite (issue #62) runs instead of the normal boot when
the image is built with `LAZYOS_TESTS=1`, and prints one machine-parseable line
per test over serial (`TEST:<name>:PASS|FAIL:<detail>`, ending with
`TEST:SUMMARY:PASS=<n> FAIL=<n>`). One command builds, boots headless, parses
and reports:

```bash
python tools/test/run.py                 # build + run; writes docs/test/report.md
python tools/test/run.py --accel none    # force TCG (CI uses auto: KVM when usable)
python tools/test/run.py --no-build      # re-run the current image
python tools/test/run.py --ide-disk      # boot from IDE (ATA) instead of the default virtio-blk
```

While working on one subsystem, `LAZYOS_TEST_FILTER=<text> python tools/test/run.py
--accel none` runs only the tests whose name contains the text (touch
`kernel/src/main.rs` after changing the filter: the build does not notice an
environment variable on its own). Run the whole suite before you finish.
The runner exits non-zero on any failure, a missing summary, or a stale
non-test image. Normal boots are unaffected: without `LAZYOS_TESTS=1` the suite
is not compiled. Test-only hooks live behind `cfg(lazyos_tests)`; add new tests
under `kernel/src/tests/` (`mem_suite` is where allocator-specific tests go). CI
is `.github/workflows/kernel-tests.yml`; see `tools/test/README.md`.

## ext2 library

The ext2 driver is `libs/ext2fs` (`no_std` + `alloc`, depends only on `spin`), shared by
the kernel (`kernel/src/fs/ext2.rs` and `ext2/fsimpl.rs` are the adapter) and the host
image build. It also holds the formatter and populator (`format`, `mkdir_p`,
`write_file`, `remove_tree`). It is tested on the host over an in-memory `BlockIo`,
with an independent fsck-style checker (`src/check.rs`), a 1k-cycle soak and a seeded
fuzz entry point shared with the cargo-fuzz target (`fuzz/fuzz_targets/ext2fs.rs`):

```bash
cargo test -p ext2fs
FUZZ_CASES=30000 cargo test -p ext2fs seeded     # a longer seeded soak
FUZZ_SEED=0x<seed> cargo test -p ext2fs <test>   # replay a printed failing seed
python fuzz/gen_corpus.py --check                # the checked-in seeds are current
```

The kernel's `ext2_suite` still runs unchanged against the adapter, and
`mount_library_formatted_root` mounts a library-made image at `/` through `lazyos.cfg`.

## Shutdown and reboot

Only `init` stops the machine ([`docs/shutdown.md`](docs/shutdown.md)): its
`Shutdown` method stops the apps, then the services in reverse dependency
order, then calls the kernel's `power` (syscall 21). Never call `power()` from
anything else; ask `init` (`powerctl`, or `services::shutdown`). A service
that holds durable state serves `os.lazy.lifecycle.v1` (`idl/lifecycle.midl`)
and is listed in `user/src/bin/init/shutdown.rs` (`GRACEFUL`). The harness
boots the desktop twice (power-off from the shell, reboot from the menu) and
judges the serial logs; the session scripts drive the same paths by hand:

```bash
python tools/shutdown/run.py             # build, boot twice, judge (tools/shutdown/README.md)
python tools/shutdown/test_judge.py      # the judge fails when it should
python tools/screenshot/qemu_session.py --image target/lazyos.img \
    --out shots/shutdown --script tools/screenshot/examples/shutdown_shell.json \
    --extra-arg=-no-shutdown
python tools/screenshot/qemu_session.py --image target/lazyos.img \
    --out shots/reboot --script tools/screenshot/examples/shutdown_menu.json \
    --extra-arg=-no-shutdown
```

## Sound harness

The virtio-sound driver (`sndd`) is verified by listening: QEMU records what the
guest plays (`-audiodev wav`) and a detector measures the recording. One command
builds with `LAZYOS_SOUND=1`, boots headless, records and checks it:

```bash
python tools/sound/run.py                          # driver tone + beep client tone
python tools/sound/run.py --services               # init supervises sndd as _snd
python tools/sound/run.py --machine q35 --virtio-disk
python tools/sound/test_analyze_wav.py             # the detector's own tests
cargo test -p virtio -p virtio-snd -p pcm          # the driver libraries
```

Do not claim an audio change works from the serial markers alone; the verdict is
the recording. See `tools/sound/README.md` and `docs/architecture/audio.md`
(including why a driver must never free a DMA buffer while its device runs).

## USB harness

The USB HID driver (`usbd`, [`docs/usb-hid-plan.md`](docs/usb-hid-plan.md)) is
verified by what reached `inputd` (architecture: `docs/architecture/usb.md`): QEMU runs with `qemu-xhci`, a USB keyboard
and mouse and no i8042, and the judge checks every key edge `inputd` saw came
from `usbd` and that the descriptors are QEMU's. See `tools/usb/README.md`.

```bash
python tools/usb/run.py                  # build (LAZYOS_SERVICES=1 LAZYOS_USB=1), boot, judge
python tools/usb/run.py --ps2            # PS/2 and USB side by side
python tools/usb/run.py --hotplug 200    # unplug/replug over QMP: nothing stuck, DMA bounded
python tools/usb/run.py --tablet         # usb-tablet: report descriptor, absolute cursor
python tools/usb/run.py --restart        # usbd crashes holding a key: released, restarted, re-enumerated
python tools/usb/test_judge.py           # the judge fails when it should
cargo test -p usbhid -p xhci             # descriptor/report parsers and xHCI rings (host)
```

Under TCG the harness paces input (USB is polled; see the README): KVM runs are
the verdict.

## Network tooling

Networking (`docs/networking-plan.md`) is verified like audio: serial markers
only say when the guest is done, the verdict is what crossed the wire. Testing
and fuzzing tooling ships with every stage; the full description is
`tools/net/README.md`.

```bash
cargo test -p framering -p virtio-net -p nicdrv -p netstack -p ftpwire -p netpolicy -p virtio -p messenger-generated   # host unit + seeded fuzz
FUZZ_CASES=20000 cargo test -p framering -p virtio-net -p nicdrv -p netstack fuzz::   # a longer seeded soak
FUZZ_SEED=0x<seed> cargo test -p framering clean_scripts                # replay a printed failing seed
python fuzz/gen_corpus.py --check                                       # the checked-in fuzz seeds are current
python tools/net/test_analyze_pcap.py                                   # the capture judge fails when it should
python tools/net/test_sockets_pcap.py                                   # the TCP/UDP/DNS judge fails when it should
python tools/net/run.py                                                 # build (LAZYOS_NET=1), boot QEMU, judge the pcap
python tools/net/run.py --services | --poll | --no-device | --machine q35 --virtio-disk   # variants
python tools/net/run.py --netd                                          # stages N2+N3: netd, DHCP, ping, nslookup, nc and the socket probe/soak; judged from the pcap and the host echo servers (combines with the variants)
mkdir -p fuzz/corpus/netstack; cargo fuzz run netstack --fuzz-dir fuzz fuzz/corpus/netstack fuzz/seeds/netstack -- -max_total_time=60   # Linux
mkdir -p fuzz/corpus/framering                                          # once; libFuzzer's working corpus (git-ignored)
cargo fuzz run framering --fuzz-dir fuzz fuzz/corpus/framering fuzz/seeds/framering -- -max_total_time=60  # Linux; CI runs it
```

Fuzz entry points (`fuzz::run(&[u8])`) are shared by the in-tree seeded tests
and the `fuzz/` cargo-fuzz targets, so a libFuzzer crash replays under plain
`cargo test`; save fixed crashes in `fuzz/regressions/<target>/`. The `fuzz/`
crate is outside the OS workspace on purpose: local development never needs
libFuzzer, which is Linux-only in CI.

## Testing requirement for kernel components

Every kernel component (scheduler, memory/allocators, IPC/Messenger, VFS/FS,
drivers, signals, etc.) MUST ship with both:

1. **Correctness tests** — unit tests under `kernel/src/tests/` (grouped into
   per-subsystem suites, e.g. `mem_suite`) exercising normal behavior, edge
   cases, and known-bad inputs, following the existing
   `TEST:<name>:PASS|FAIL:<detail>` protocol.
2. **Stress/soak tests** — tests that drive the component under sustained load
   or many iterations/generations (e.g. millions of allocations/frees, many
   `fork`/COW generations, repeated task spawn/exit, high-volume IPC
   transactions) to catch leaks, races, and resource-exhaustion bugs that a
   single-pass unit test won't surface. Add these alongside the correctness
   tests in the relevant suite rather than as a separate ad-hoc mechanism.

New kernel code (new subsystem, new syscall, new service-facing kernel
surface) is not done until both kinds of coverage exist and
`python tools/test/run.py --accel none` passes. Do not rely on the ABI bench
or screenshot pipeline as a substitute — those catch integration/visual
regressions, not kernel-internal correctness or resource leaks.

## Project conventions

- The OS is `no_std`; target `x86_64-unknown-none`; built via the root crate's
  `build.rs` + artifact dependency (see the roadmap issues).
- Do not commit generated screenshots (`shots/` is git-ignored); CI publishes
  them to the dedicated `screenshots` branch.
- Code quality bar: security first (validate all untrusted input, no ambient
  authority, every `unsafe` block minimal with a `// SAFETY:` comment),
  readable and elegant (small single-purpose functions, comments explain why).
  See the "Code standards" section of `README.md`.
- Well-known paths and boot-volume file names come from `libs/fhs`; never write
  one as a literal (`python tools/fhs/check_literals.py` enforces it).
- Keep source files **under 500 lines**; split by responsibility instead of
  growing a file past it. Existing oversized files are tracked in issue #194;
  never make one bigger, extract a module when touching it.
- **Every interface published on Messenger MUST be defined in a `.midl` file
  under `idl/`** and its client/server code generated with `midlc` (see
  `docs/messenger.md` §11 and `idl/confd.midl` as the model). This is
  non-negotiable: no new hand-written method/field constants or TLV encoders
  for a service, topic, or capability interface, and no copying a protocol into
  another crate by hand. Touching a legacy hand-rolled protocol means migrating
  it to MIDL, or at minimum not extending it by hand.
- **Every application must be launchable from both front ends**: the Python
  GUI launcher (`python tools/lazyos_gui.py`, `tools/lazygui/`) and the CLI
  (`python tools/run_demo.py`). A new app or optional image feature is not done
  until it has (a) a build switch the image build understands (an env var in
  `build.rs`/`build_support/`, e.g. `LAZYOS_LAZYRAD=1`), (b) a `run_demo.py`
  flag that builds its artifacts and sets that switch, (c) a control in the GUI
  (the Simple tab for what a normal user wants, the Advanced tab for the raw
  switch) wired through `tools/lazygui/catalog.py` (`build_env`, `build_plan`)
  with tests in `tools/lazygui/test_catalog.py`, and (d) for a desktop app, an
  `init` registry row (`user/src/bin/init/apps.rs`) plus an `XAPPS.LST` line so
  Settings -> Menu offers it. Verify it by starting it through the launcher or
  `run_demo.py`, not only by hand-built env vars.
- Prefer verifying with the existing scripts over ad-hoc commands so results are
  comparable across runs.

## Commands

```bash
python tools/screenshot/qemu_shot.py --out shots --at 2,5,10 --image <img>
python tools/screenshot/pngstats.py shots/*.png --min-nonblack 0.01
python tools/test/run.py --accel none
python tools/sound/run.py
```
