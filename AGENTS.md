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
Click by name, not by measured relative moves: `{"click_at": {"window":
"MOD Player", "widget": "play_button"}}`, `{"click_at": {"menu": "Games"}}`
(and `move_to`) resolve against the `UI:RECT`/`UI:WIDGET` lines an image built
with `LAZYOS_UI_PROBE=1` prints (`tools/screenshot/README.md`, issue #538).

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
default `512M`, minimum `128M`) that holds every other file: programs at their
real names in `/system/bin` (`/system/bin/init`, `/system/bin/busybox`), data in
`/system/etc` and `/system/share` (`/system/etc/passwd`), the docs in `/docs/os`,
and each service's state at its place in the tree (F4): `confd`'s store in
`/conf` (0700 root; an F3 image's `/data/confd` is merged in once), `logd`'s
journals and `pkgd`'s `pkg.log` in `/logs` (0750 root), installed apps in
`/apps` and their docs in `/docs/apps`, and a 0700 home per passwd account in
`/home/<name>` (hidden by the home volume when one is mounted). Nothing new is
written under `/data`; no regular file sits at the root (docs/filesystem-plan.md
F3). Binary data (samples, wallpapers, a demo's module files) goes in
`assets/` with one `path | licence | install | provenance` line per file in
`assets/manifest.txt` and lands at `/system/share/<path>`; `LAZYOS_ASSETS=<dir>`
(`run_demo.py --assets DIR`, the GUI's *Asset dirs*) adds a tree of your own
with the same manifest (`build_support/assets_embed.rs`, issue #454). The volume is written by `libs/ext2fs`, the code the kernel mounts it with
(`build_support/os_*.rs`); `cargo run -q -p ext2fs --example osread -- target/lazyos.img
cat /logs/service.log` reads it from the host (`/logs` is root-only in the guest).
A rebuild **updates the OS volume in place**: installed apps, settings, logs
and your own files survive, and only paths listed in `/system/.image-manifest`
are replaced or deleted. `LAZYOS_RESET_OS=1 cargo build` (or
`python tools/run_demo.py --reset-os`) recreates it with a new UUID; so does an
image that fails validation, with a `cargo:warning=` giving the reason. An
update also checks a volume that was not cleanly unmounted (a closed QEMU
window) and marks it clean once it has repaired what a crash can leave
(leaked blocks and inodes, link counts, counters; data-holding orphans go to
`/lost+found`) and the independent checker finds nothing wrong; the kernel
never does (it has no fsck), so until a rebuild every boot of such an image
prints `ext2: ... was not cleanly unmounted`. Damage no crash leaves fails the
build before anything is written (copy your files off, then reset);
`LAZYOS_UPDATE_DAMAGED_OS=1` updates such a volume anyway, at the risk of an
updated file reusing a damaged user file's block. Changing
`LAZYOS_OS_SIZE` on an existing image needs the reset. Do not rebuild while QEMU
has the image open (the build fails with a message). CI sets `LAZYOS_RESET_OS=1`
everywhere. ext2 is case-sensitive: look names up exactly as stored, through
`libs/fhs`. Host tests: `cargo test -p build-support-tests`.

## Resource limits and guest memory

Every launcher boots QEMU with **1 GiB** of RAM by default (`DEFAULT_MEMORY`
in `tools/screenshot/qemu_qmp.py`; `--memory 4G` on any of them, including
the abi/usb/shutdown/rhai runners and the GUI's Memory field). The kernel's
tunable ceilings live in one module, `kernel/src/limits.rs`
([`docs/architecture/limits.md`](docs/architecture/limits.md)): defaults are
derived from usable RAM and the screen size, and `limit.<key>=<value>` lines
in `/boot/lazyos.cfg` override them at boot (clamped, logged, never fatal).
The build writes them from `LAZYOS_LIMIT_<KEY>` variables:

```bash
LAZYOS_LIMIT_HEAP_MAX=768M LAZYOS_LIMIT_FD_MAX=4096 cargo build
python tools/run_demo.py --limit heap_max=768M --limit stack_size=16M
```

Keys: `heap_max` (kernel heap ceiling; the heap grows on demand), `fd_max`
(descriptors per task, 1024), `stack_size` (Linux main stack, 8 MiB,
demand-zero), `quota_user_memory`, `quota_kernel_memory`, `shared_buffer_max`.
The boot log prints the table (`limits: ...`). `task::MAX_TASKS` (256) stays a
compile-time constant. The kernel image runs at `0xffff_8000_0000_0000`
(`mem::layout`): symbolize with `addr2line -e <kernel> <rip - 0xffff800000000000>`.

## HiDPI (a 720p desktop at 2x)

`python tools/run_demo.py --desktop --hidpi` (GUI: *HiDPI* on the Simple tab,
*Display mode* on the Advanced tab) builds with `LAZYOS_DISPLAY_MODE=2560x1440`:
the kernel switches QEMU's std VGA to that mode after boot (`display.mode` in
`lazyos.cfg`, `kernel/src/display/bochs.rs`; the BIOS bootloader stops at
1280x720), and the desktop draws a 1280x720 layout natively at 2x. `xuid`
picks the scale (`sys/ui/scale`: `auto`, `1`, `2`; auto is 2 from 2560x1440)
and hands it to clients with `GetOutput`; xui apps run at `96 * scale` DPI.
The wire stays in physical pixels. Code laid out in pixel constants uses
`xui_app::hidpi` (`design_bounds`, `rect`, `design_rect`). Plan and status:
[`docs/hidpi-plan.md`](docs/hidpi-plan.md).

```bash
LAZYOS_DESKTOP=1 LAZYOS_XUI_AUTOSTART=term LAZYOS_DISPLAY_MODE=2560x1440 LAZYOS_RESET_OS=1 cargo build
python tools/screenshot/qemu_session.py --image target/lazyos.img     --out shots/hidpi --script tools/screenshot/examples/hidpi_apps.json
python tools/screenshot/pngstats.py shots/hidpi/*.png --expect-width 2560 --expect-height 1440
LAZYOS_TEST_FILTER=display_mode python tools/test/run.py --accel none
```

## Docs app and the C++ toolchain

`xui-docs` renders Markdown with litehtml, which is C++, so it is built with zig
(`pip install ziglang==0.16.0`, then `python tools/xui/build.py`; see
[`docs/xui-docs.md`](docs/xui-docs.md)). Without zig the script skips it with a
warning and every other app still builds. `python tools/xui/test_zig.py` tests
the toolchain helper. Screenshot sessions: `tools/screenshot/examples/xui_docs.json`
(wheel scrolling) and `xui_docs_open.json` (Open dialog and `/system/share/samples/testdoc.md`).

## LazyWriter (word processor)

`writer` (`os.lazy.writer`) is a core desktop app on xui's `xui-rich-text`
editor: `.lzw` documents, Markdown export, pictures; see
[`docs/xui-writer.md`](docs/xui-writer.md). Screenshot session:
`tools/screenshot/examples/xui_writer.json` (format, save, export, reopen; markers
`WRITER:UP|SAVE|EXPORT|OPEN:PASS`, build with `LAZYOS_XUI_AUTOSTART=writer`)
and `xui_writer_light.json` (light theme, build with `LAZYOS_XUI_AUTOSTART=term`).
Printing (`Ctrl+P`, `libs/ipp`, `libs/raster`, docs/printing-plan.md):
`python tools/print/run.py` builds, boots, prints through the `printd` spooler
to a fake IPP printer on the host and judges the PWG Raster page it gets
(`WRITER:PRINT:PASS:<pages>`); `--quit` quits LazyWriter once the job is
queued and checks the page still arrives whole. The queue is
`xui-app/crates/printd` (`cd xui-app && cargo test -p printd`).

## Archiver (archive manager) and desktop drag and drop

`xui-archiver` (`os.lazy.archiver`) is a 7-Zip-style archive manager shipped
in every desktop image: browse, extract, test, create, add, delete, for zip,
tar, tar.gz/zst/xz, gz/zst/xz and 7z (xz and 7z read-only). Formats live in
`xui-app/crates/archive` (`lazyarc`, host-tested and fuzzed, real 7-Zip/xz
fixtures in `tests/fixtures`), the app in `xui-app/crates/archiver`; see
[`docs/xui-archiver.md`](docs/xui-archiver.md). It added drag and drop for xui
apps: `LazyOSBackend::on_drag_gesture`/`on_drag_event` over the compositor's
`DragStart`/`Drop` and `clipboardd` tokens, carrying `text/uri-list`; Files
is a source and a target. The `xui-app` lib tests are Linux-only: on Windows
build them for musl (`tools/xui/build.py`'s rust-lld settings) and run the
ELF under WSL. Session: `tools/screenshot/examples/xui_archiver.json`
(`LAZYOS_XUI_AUTOSTART=archiver`; markers `ARCHIVER:UP|OPEN|EXTRACT|TEST:PASS`).

```bash
cargo test --manifest-path xui-app/Cargo.toml -p lazyarc -p xui-archiver -p xui-explorer
FUZZ_CASES=20000 cargo test --manifest-path xui-app/Cargo.toml -p lazyarc --release seeded
```

## Doom (an installable `.lzp` package)

Doom is `doom/` (doomgeneric, fetched at a pinned revision and compiled with
zig, plus a Rust platform layer on `xui-app`'s client window) shipped as the
package `org.lazy.doom` with the Freedoom IWAD inside; see
[`doom/README.md`](doom/README.md) and [`docs/doom-port-plan.md`](docs/doom-port-plan.md).

```bash
python tools/doom/build.py          # target/doom/doom.elf + target/pkg/doom.lzp (fetches doomgeneric, Freedoom)
python tools/run_demo.py --doom     # desktop with /system/share/samples/doom.lzp (a user package)
cargo test --manifest-path doom/Cargo.toml --lib
python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/doom     --script tools/screenshot/examples/doom.json   # needs a fresh OS volume (LAZYOS_RESET_OS=1) and LAZYOS_UI_PROBE=1
```

The Terminal reports one `TERM:OUT` per command, and a command that wraps past
80 columns reports its own tail instead: keep typed commands short (`doom.json`
sets `PS1='# '` first). Shell command substitution (`$(...)`) works since #518
(`cmdsubst_console.json`, `cmdsubst_desktop.json`).

## Real Linux programs (`LAZYOS_LINUXAPPS=1`)

`python tools/linuxapps/build.py` builds unmodified dash, lua, sqlite3, jq and
ripgrep from pinned, hash-checked sources (zig for C) into
`target/linuxapps/bin`; `LAZYOS_LINUXAPPS=1` (`run_demo.py --linuxapps`) puts
them in `/system/bin`. The ABI bench runs them (`tools/abi/run.py --only
dash,lua,sqlite3,jq,rg`), and `linuxapps_console.json`/`linuxapps_desktop.json`
drive them interactively (the desktop Terminal runs its shell on a pty, so
`vi`, `less`, `^C` and cooked-mode REPLs work). See `tools/linuxapps/README.md`.

## HTTPS clients (`LAZYOS_TLS=1`: `curl`, `wget`, `fetch`)

`nettls/` (a standalone workspace, like `rhai-host/`) builds one static-musl
program that runs as `fetch`, `curl` or `wget` by its name: rustls with
certificates verified against `/etc/ssl/certs/ca-certificates.crt` (the
Mozilla roots, written by the image build), `ureq` for HTTP/1.1, and an
in-tree pure-Rust crypto provider (`nettls/crypto`, MIT). Every linked crate
must have a GPLv2-compatible licence (a NetSurf port will link this stack):
`python tools/nettls/licenses.py` enforces it, so never add `ring`, `aws-lc`
or an Apache-2.0-only crate. `-k`/`--no-check-certificate` do not exist. The
plan and its decisions are [`docs/tls-plan.md`](docs/tls-plan.md).

```bash
python tools/run_demo.py --tls            # networking + curl/wget/fetch (then: curl https://...)
python tools/nettls/build.py --require    # target/nettls/fetch.elf
cargo test --manifest-path nettls/Cargo.toml --workspace
python tools/nettls/test_host.py          # the host binary against Python ssl servers
python tools/nettls/licenses.py           # GPLv2-compatible dependency tree
python tools/net/tls_run.py               # build, boot, run every check against the harness servers, judge
python tools/net/test_tls_pcap.py         # the wire judge fails when it should
python tools/net/tls_run.py --live        # real sites with the Mozilla roots only (manual, needs internet)
```

`tls_run.py` (also `tools/net/run.py --tls`) builds a test image with a
throwaway CA appended to the bundle (`LAZYOS_TLS_TEST_CA`) and `tls.test`
mapped to the host (`LAZYOS_TLS_TEST_HOSTS`); never set those in a normal
image. Behind an egress proxy that re-signs TLS, `--live --extra-ca PEM`
trusts that proxy's CA too.

## Mail (esMail over TLS, `LAZYOS_MAIL=1`)

`xui-mail` (`os.lazy.mail`, `xui-app/mail/`) is va1erian/esmail's IMAP/SMTP
core (a pinned git dependency, built with its `rustls` feature over
`nettls-crypto`) behind a xui window; zig builds it like the Docs app
(`tools/xui/build.py --mail`). Passwords live only in memory (`secrets.rs`).
See [`docs/mail.md`](docs/mail.md).

```bash
python tools/run_demo.py --mail          # desktop + HTTPS + Mail
python tools/mail/run.py                 # mock IMAPS/SMTPS server under the test CA, session, judge
python tools/mail/run.py --label-trace   # the same, listing LABEL:DENY lines
cargo test --manifest-path xui-app/Cargo.toml -p xui-mail
```

## LazyWeb browser (`LAZYOS_LAZYWEB=1`)

LazyWeb (`os.lazy.lazyweb`, crate `lazyweb` in `xui-app/web`) is the web
browser: NetSurf, compiled with zig by `tools/xui/build.py` into
`target/xui/xui-lazyweb.elf`, over the HTTPS stack above; a core package
(`xui-app/packages/lazyweb`) that only `LAZYOS_LAZYWEB=1` images ship, which
needs `LAZYOS_DESKTOP=1` and `LAZYOS_NETD=1` (the build refuses it otherwise).
NetSurf is GPL-2.0-only, so LazyWeb is too: link only GPLv2-compatible crates
into it (no GPL-3.0, no Apache-2.0-only). See [`docs/lazyweb.md`](docs/lazyweb.md).

```bash
python tools/run_demo.py --lazyweb        # desktop + networking + HTTPS + the browser (needs zig)
python tools/web/run.py                   # build, browse example.com then theoldnet.com, judge
python tools/web/run.py --precheck-only   # the harness alone (console image, curl), no browser needed
python tools/web/run.py --live            # the real sites (manual, needs internet)
python tools/web/test_judge.py            # the judge fails when it should; fixtures and session current
```

`tools/web/run.py` serves stand-ins for both sites from the host on ports 80
and 443 (root, or `net.ipv4.ip_unprivileged_port_start=80`): a copy of
example.com and a 90s-style theoldnet.com with PNG, JPEG and (animated) GIF
pictures (`tools/web/fixtures/`, drawn by `gen_fixtures.py`), with the names
mapped to `10.0.2.2` through `LAZYOS_TLS_TEST_HOSTS` and the run's CA through
`LAZYOS_TLS_TEST_CA`. It judges the browser's `WEB:UP:PASS`,
`WEB:LOAD:<url>`, `WEB:TITLE:<title>` (never `WEB:FAIL:<reason>`), the
requests the host saw (Host, SNI, every picture) and the screenshots. The
session (`tools/screenshot/examples/lazyweb.json`, generated by
`tools/web/session.py --write`) starts the browser through `mimed` from a
`rhai` script (`sys::mimed::open("http://example.com/", "open")`; `mimed` maps
a URL to `x-scheme-handler/<scheme>`, `init` launches with a URL argument), uses **Ctrl+L** for the address bar, then
downloads a file (`WEB:DOWNLOAD:DONE:<name>:<bytes>`, saved to `~/Downloads`),
hands a `mailto:` link to the OS (`WEB:LAUNCH:<url>:OK|FAIL`) and opens
`about:history` (**Ctrl+H**) and `about:downloads` (**Ctrl+J**)
([`tools/web/README.md`](tools/web/README.md)).

## Rhai scripting (`rhai` command and `msg` module)

`rhai` (`rhai-host/`, bindings in `libs/rhai-lazy/`) is a static-musl command
embedded as `/system/bin/rhai`; the plan is [`docs/rhai-plan.md`](docs/rhai-plan.md).
Its `msg` module calls any Messenger service from a script, driven by a table
`midlc --schema` generates from `idl/` ([`docs/rhai/msg.md`](docs/rhai/msg.md)),
and `midlc --rhai-api` generates one documented module per interface on top of
it (`sys::confd::get(...)`, `libs/rhai-lazy/api/`). LazyRAD form scripts on
LazyOS get both, with events delivered by the form's window
([`docs/lazyrad-messenger-plan.md`](docs/lazyrad-messenger-plan.md)).
The LazyRAD IDE and its player ship as the core package `os.lazy.lazyrad` in
images built with `LAZYOS_LAZYRAD=1` (`--lazyrad` implies `--desktop`): `pkgd`
installs it at boot under its own label like every other desktop app, so the
`lazyrad_*.json` sessions start the IDE through `init.Launch` and run the player
from `/apps/os.lazy.lazyrad/*/bin/lrplay.elf`; Make LazyOS App hands the package
to the Installer instead of calling `pkgd`
([`docs/lazyrad-package-plan.md`](docs/lazyrad-package-plan.md)).
One command builds `rhai`, BusyBox and the image, boots it and judges it:

```bash
python tools/rhai/run.py              # console checks (rhai_demo.json)
python tools/rhai/run.py --desktop    # plus the desktop Terminal and the msg session
python tools/rhai/run.py --lazyrad    # the LazyRAD Messenger sample (lazyrad_msg.json)
cargo test --manifest-path libs/rhai-lazy/Cargo.toml   # bindings vs an in-memory fabric
python tools/midlc/midlc.py --schema libs/rhai-lazy/src/msg/idl.rs --rhai-api libs/rhai-lazy/api idl/*.midl   # after an IDL change
```

`python tools/run_demo.py` rebuilds `rhai` before each image (`--no-rhai` skips it).

## Packages and the label-policy trace

Every desktop app except the Terminal, Devices, the Installer and LazyShell
(the opt-in LazyRAD IDE, `LAZYOS_LAZYRAD=1`, included) is a core package (`xui-app/packages/<short>/`, [`docs/packages.md`](docs/packages.md)):
`pkgd` installs it into `/apps` at boot and the kernel confines it to the
permissions its manifest declares. `LAZYOS_LABEL_TRACE=1` is the supported
debug switch for that policy: an image built with it (`kernel/build.rs`, cfg
`lazyos_label_trace`; off by default, and nothing else changes) prints one
`LABEL:DENY label=<label> iface=<id> method=<id>` (or `resolve=<name>`,
`topic=<name>`) serial line per refused call. Use it to derive a package's
permissions from a run instead of by hand:

```bash
LAZYOS_DESKTOP=1 LAZYOS_XUI_AUTOSTART=term LAZYOS_LABEL_TRACE=1 LAZYOS_RESET_OS=1 cargo build
python tools/screenshot/qemu_session.py --image target/lazyos.img \
    --out shots/core_apps --script tools/screenshot/examples/core_apps.json
grep LABEL:DENY shots/core_apps/serial.log   # map iface ids with idl/manifest.json
```

An IDE package with `develop = true` runs the project it edits under
`dev:<system_name>` (issue #529; `docs/packages.md`, "Development runs";
`kernel/src/ipc/devspawn.rs`). The session `lazyrad_devplay.json` plays a sample
that way from a test package of the IDE (build it with
`python tools/lazyrad/build.py && python tools/lazyrad/devtest.py`, then an
image with `LAZYOS_DESKTOP=1 LAZYOS_LAZYRAD=1 LAZYOS_XUI_AUTOSTART=term
LAZYRAD_SAMPLES=lazyrad-os/samples/devplay LAZYOS_LABEL_TRACE=1
LAZYOS_RESET_OS=1`) and must show no `LABEL:DENY`.

## App failures (the "stopped unexpectedly" notice)

`init` restarts services and the desktop shell as before, but a launched app
that fails while starting (or keeps crashing) is not restarted: `init`
publishes `system/events/app/<id>` on the central broker and LazyShell shows
one notice with the app's name, its exit status and the reason the app gave
through `init.ReportFailure` (`lrplay` sends the player's error). The rules are
`libs/svcpolicy` (host-tested), the notice `xui-app/src/shell/notice.rs`.

```bash
cargo test -p svcpolicy
python tools/crash/run.py            # build, install crashload.lzp, open it from the menu, judge
python tools/crash/test_judge.py     # the judge fails when it should
```

## LazyRAD MOD player (`modplay` module, `.lzp` package)

A ProTracker player written as a LazyRAD project (`lazyrad-os/samples/modplayer`)
over the player's `modplay` script module (`lazyrad-os/src/tracker`, mixing in
`libs/modplay`, sound through `audiod`); it is embedded as `/system/share/lazyrad/modplayer`
with LazyRAD and packaged as `/system/share/samples/modplayer.lzp` (`LAZYOS_MODPLAYER=1`); see
[`docs/lazyrad-modplay.md`](docs/lazyrad-modplay.md). The verdict on its sound is
the recording, judged against a host render:

```bash
python tools/run_demo.py --modplayer          # desktop, LazyRAD, the package, sound card
python tools/lazyrad/modplayer_run.py         # build, Terminal + installed sessions, record, judge
cd lazyrad-os && cargo test                   # tracker unit tests; tests/modplayer.rs runs the real form offscreen
python tools/lazyrad/gen_demo_song.py --check # the built-in song (an original, CC0) is current
```

## Latency harness

`python tools/perf/run.py` builds the desktop with `LAZYOS_PERF=1` (kernel cfg
`lazyos_perf`, hooks in `kernel/src/perf/`), boots it headless, knocks on its
virtio-net card and moves the PS/2 mouse over QMP, and writes
`docs/perf/report.md` from the kernel's `PERF:` lines (IRQ-to-task wake,
input to `inputd`, input to present, interrupts-off syscall stretches,
in-kernel IPC round trip). `--label X` appends a row to `docs/perf/history.md`;
re-run it after a scheduling or wake-path change. See `tools/perf/README.md`.

## Linux ABI conformance bench

Compatibility with Linux (`x86_64-unknown-linux-musl`) binaries is tracked by a
bench that runs from day one, before any ABI support exists:

```bash
python tools/abi/build.py            # build the static musl fixtures
python tools/abi/run.py --at 8       # run each fixture in headless QEMU, write the matrix
python tools/abi/coverage.py         # summarise ENOSYS syscalls from the logs
```

`run.py` embeds one fixture as `/system/bin/abi-init` (via `LAZYOS_INIT`), boots, and
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

## Interrupt latency

Syscalls run with interrupts off. Long kernel work calls
`arch::irq_window::poll_point()` (the ext2 library through `BlockIo::pace`),
which takes pending interrupts in a window whose handlers take no lock but
the i8042 FIFO's, so a window is safe under any other lock (never reach a
poll point while holding that one) and never switches tasks
([`docs/architecture/arch.md`](docs/architecture/arch.md)). A new loop that
can run long inside a syscall needs a poll point. Every boot logs each
syscall's new worst interrupts-off stretch of 2 ms or more as
`IRQOFF:MAX ... from=<file:line> to=<file:line>`; the missing poll point lies
between the two lines. Under WHPX/KVM a loaded host inflates those numbers;
for numbers free of host noise run the session under TCG with
`--accel tcg --extra-arg=-icount --extra-arg=shift=0,sleep=off` (scale the
script's timeouts up). Tests: `LAZYOS_TEST_FILTER=irq python tools/test/run.py
--accel none`.

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

Real mounts (and the host build) go through the library's write-back block cache
([`docs/architecture/block-cache.md`](docs/architecture/block-cache.md)): writes reach
the disk at a commit (sync, fsync, every 5 s, memory pressure), in a crash-safe phase
order. `Ext2::open` stays uncached for tests that judge the disk after every write;
anything that needs one step on disk before the next inside an operation calls
`Ext2::barrier`. `cargo test -p ext2fs cache` covers it (crash prefixes, byte-identical
images), the kernel side is `LAZYOS_TEST_FILTER=bcache`, and
`cargo test -p ext2fs --release bench -- --ignored --nocapture` prints the I/O cost of a
30 MB tree. `LAZYOS_BLOCK_CACHE_KB=0` builds a kernel that mounts uncached.
A session's `quit` kills QEMU without a sync, so a session whose files a later
boot reads (`lazyrad_home.json` before `lazyrad_home_project.json`) waits a few
seconds past the flusher's 5 s before it quits.

## ext2 journal (`LAZYOS_JOURNAL=1`)

The OS volume can carry an internal JBD2 journal ([`docs/architecture/journal.md`](docs/architecture/journal.md)):
metadata commits are atomic transactions, replayed at mount, so a power cut
needs no repair. `python tools/run_demo.py --journal` (or the launcher's
Advanced tab) builds it in; an existing image gets one on the next in-place
update. Host tests: `cargo test -p ext2fs journal` (a power cut at every write,
then the independent checker must find nothing) and
`cargo test -p build-support-tests journal`.

## Shutdown and reboot

Only `init` stops the machine ([`docs/shutdown.md`](docs/shutdown.md)): its
`Shutdown` method stops the apps, then the services in reverse dependency
order, then calls the kernel's `power` (syscall 21). Never call `power()` from
anything else; ask `init` (`powerctl`, or `services::shutdown`). A service
that holds durable state serves `os.lazy.lifecycle.v1` (`idl/lifecycle.midl`)
and is listed in `user/src/bin/init/shutdown.rs` (`GRACEFUL`). The harness
boots the desktop twice (power-off from the shell, reboot from the menu) and
judges the serial logs (`logd` must report `persisted>0`, `confd` must stop
with `dir=/conf`, `pkgd` must stop through the lifecycle before `confd`, and
the second boot finds the first one's records in `/logs/service.log`); the
session scripts
drive the same paths by hand:

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

The virtio-sound driver (`sndd`) and the system mixer (`audiod`,
[`docs/audio-plan.md`](docs/audio-plan.md)) are verified by listening: QEMU
records what the guest plays (`-audiodev wav`) and a detector measures the
recording. One command builds with `LAZYOS_SOUND=1`, boots headless, records and
checks it:

```bash
python tools/sound/run.py                          # driver tone + beep through the mixer
python tools/sound/run.py --mix                    # two clients as one chord, a half-volume tone
python tools/sound/run.py --starve --services     # a stream run dry: exactly one underrun event (#453)
python tools/sound/run.py --services               # init supervises sndd (_snd) and audiod (_audio)
python tools/sound/run.py --machine q35 --virtio-disk
python tools/sound/test_analyze_wav.py             # the detectors' own tests
python tools/sound/test_mixcheck.py
cargo test -p virtio -p virtio-snd -p pcm          # the driver libraries
cargo test -p audiomix -p audioclient              # the mixer engine and the client library
```

Applications play sound through `libs/audioclient` (`PlaybackStream`), never by
opening the card. Do not claim an audio change works from the serial markers
alone; the verdict is the recording. See `tools/sound/README.md` and
`docs/architecture/audio.md` (including why a driver must never free a DMA
buffer while its device runs).

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
python tools/usb/run.py --hub            # keyboard and mouse behind a usb-hub, then the hub unplugged
python tools/usb/run.py --full-speed     # USB 1.1 devices on root ports
python tools/usb/run.py --controllers 2  # two xHCI controllers, keyboard on the second
python tools/usb/test_judge.py           # the judge fails when it should
cargo test -p usbhid -p xhci             # descriptor/report parsers and xHCI rings (host)
```

Under TCG the harness paces input (USB is polled; see the README): KVM runs are
the verdict.

USB sticks (`/home` on the boot stick, `docs/architecture/usb-storage.md`)
have their own harness, `tools/storage/README.md`:

```bash
python tools/storage/run.py              # two boots: write /home/alice on the stick, power off, read it back; e2fsck
python tools/storage/test_judge.py       # the judge fails when it should
cargo test -p usbmsc --features fuzz     # Bulk-Only Transport and SCSI (host, fuzz seeds)
## Networking in an interactive boot

`python tools/run_demo.py --net` (the launcher: *Networking* on the Simple
tab, the *Networking* group on the Advanced tab) builds the stack
(`LAZYOS_NETD=1`, with `LAZYOS_NETD_ARGS=demo=0` so `netd` runs without the
harness's evidence clients) and attaches a virtio-net card on QEMU's user
network, forwarding host `127.0.0.1:8080` to the guest (`--net-forward`,
`--net-restrict`, `--net-pcap`; the same flags on `qemu_session.py` and
`qemu_shot.py`, all from `tools/net/qemu_net.py`). A `--net` desktop ships two
core packages: **Network** (`xui-app/src/bin/network.rs`: status, DHCP or a
manual address written to `confd`'s `sys/net/eth0/*`) and **Net Tools**
(`nettools.rs`: ping, lookups, an HTTP fetch and a web server on 8080), sharing
`xui-app/src/net/`. How to reach the guest from the host:
[`docs/networking-host-access.md`](docs/networking-host-access.md).

```bash
python tools/run_demo.py --desktop --net        # then open Net Tools, and http://localhost:8080 on the host
LAZYOS_DESKTOP=1 LAZYOS_XUI_AUTOSTART=term LAZYOS_NETD=1 LAZYOS_NETD_ARGS=demo=0 cargo build
python tools/screenshot/qemu_session.py --image target/lazyos.img --net --out shots/net_apps     --script tools/screenshot/examples/net_apps.json      # ping, lookup, a fetch through the host forward
python tools/screenshot/qemu_session.py --image target/lazyos.img --net --out shots/net_config     --script tools/screenshot/examples/net_config.json    # Manual, back to DHCP, Renew
python tools/net/test_qemu_net.py                         # the QEMU argument helper
```

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
- Well-known paths and program paths come from `libs/fhs`; never write
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
  with tests in `tools/lazygui/test_catalog.py`, and (d) for a desktop app, a
  core package under `xui-app/packages/<short>/` (listed in
  `tools/xui/core_packages.py`) with complete permissions, derived from a run
  under `LAZYOS_LABEL_TRACE=1` (every refused call is printed as `LABEL:DENY`;
  `tools/screenshot/examples/core_apps.json` launches every core app), so `pkgd`
  installs it at boot and Settings -> Menu offers it (`docs/packages.md`, core
  packages). Verify it by starting it through the launcher or `run_demo.py`, not
  only by hand-built env vars. Start a desktop app with
  `python tools/xui/new_app.py <short> --name "..." --description "..."`: it
  writes a working app on xui layouts (`xui_app::launch::run`, `UP`/`QUIT`
  evidence) and registers it for (a)-(d), icons included; then derive its
  permissions from a traced run.
- **Rhai first for small apps.** A new utility-class app (a dialog, a settings
  pane, a monitor) is a LazyRAD form by default: a `.lfm` plus its `.rhai`
  script, packaged as an `.lzp` that runs on `lrplay`
  (`lazyrad-os/samples/messenger` is a working start;
  `python tools/lazyrad/package.py --project <dir> --out <app>.lzp` or the
  IDE's Make LazyOS App packages it). Write Rust only for apps that need it:
  editors, Paint, browsers, a custom painter, heavy data or threads; a Rust
  app starts from `tools/xui/new_app.py` (above). Verification goes headless
  first (offscreen renders and host tests), QEMU sessions last.
- Prefer verifying with the existing scripts over ad-hoc commands so results are
  comparable across runs.

## Commands

```bash
python tools/screenshot/qemu_shot.py --out shots --at 2,5,10 --image <img>
python tools/screenshot/pngstats.py shots/*.png --min-nonblack 0.01
python tools/test/run.py --accel none
python tools/sound/run.py
```
