# Screenshot tooling

Headless tooling to capture and verify pixels from QEMU. It exists so that both
CI and an AI agent can *see* what LazyOS renders, without a physical display.

## Pieces

| File | Purpose |
|------|---------|
| `qemu_qmp.py` | Shared QMP client: screenshot capture + keyboard/mouse injection. |
| `qemu_shot.py` | Boot an image (or bare firmware) with `-display none` and capture PNGs. |
| `qemu_session.py` | Drive the guest with a scripted timeline of input + captures. |
| `pngstats.py`  | Decode a PNG (stdlib only) and report/assert pixel statistics. |
| `upload_image.py` | Upload PNGs to a public image host for PR comments. |
| `examples/*.json` | Session scripts, one per demo. Plain image: `type_and_shot`, `window_demo`, `mouse_demo`, `multitask_demo` (two windows, Tab focus), `fs_demo` (BusyBox `sh`: `ls` / `cat`), `shell_fork_stress` (`LAZYOS_CLI=1`: 150 shell loop iterations of pipelines, the fork/`SIGCHLD`/`wait` soak that reproduced issue #375; pass `--fail-on "user: task [0-9]+ killed by"`). `LAZYOS_SERVICES=1`: `input_keys` (physical keys through `inputd`; check the serial trace with `tools/input/verify_trace.py`, build with `LAZYOS_KBD_LAYOUT=fr` for the AZERTY run), `services_demo`, `login_demo`, `apps_demo`, `native_exec` (log in, run `top`/`confctl`/`faultprobe` from `sh`: exit statuses, a pipe, `&` + `wait`), `accounts_perms` (#508: log in as `user` then `admin`, the session environment, the home as working directory, `user` refused `/home/admin`, `/conf`, `/logs/kernel.log`), `accounts_failclosed` (`LAZYOS_OMIT_PASSWD=1`: no account file, `ACCOUNTS:LOAD:FAIL`, every login denied `no-accounts`). `LAZYOS_XUID=1`: `xuid_wm` (drag, raise, taskbar, close), `dnd_drop`/`dnd_cancel`, `xuid_shell` (+`LAZYOS_SHELLPROBE=1`: desktop, Alt+F4, Alt+Tab, Ctrl+Esc). xui apps (`LAZYOS_XUI_APP`): `xui_m0`, `xui_m1`, `xui_counter`, `xui_sysmon` (`LAZYOS_SERVICES=1`: waits for the Services tab's `SYSMON:SERVICES:PASS`), `xui_sysmon_noservices` (an image without services: the tab reports `SYSMON:SERVICES:NONE`), `xui_fabricmon`, `xui_client` (+`LAZYOS_XUI_CLIENT=1`), `xui_editor`/`xui_paint`/`xui_files` (`LAZYOS_DESKTOP=1 LAZYOS_XUI_APPS=<elf> LAZYOS_XUI_AUTOSTART=<stem>`; serial markers `EDITOR:`/`PAINT:`/`FILES:` `UP\|SAVE\|OPEN`), `xui_writer` (the same with `LAZYOS_XUI_AUTOSTART=writer`: LazyWriter formats, saves `/tmp/writer.lzw`, exports Markdown and reopens; markers `WRITER:` `UP\|SAVE\|EXPORT\|OPEN`, docs/xui-writer.md), `xui_desktop` (`LAZYOS_DESKTOP=1 LAZYOS_XUI_AUTOSTART=term,sysmon,fabricmon,counter` (nothing autostarts by default), optionally `LAZYOS_XUI_APPS=<term:sysmon:fabricmon:counter>` (`;` on Windows): Terminal + three viewers side by side, types into the BusyBox shell) -- these are readiness-gated (`wait_for`/`until`, see below) rather than fixed-timestamp, for the `xui-app` CI job (`.github/workflows/xui.yml`). The `rhai` command (#319, `.github/workflows/rhai.yml`): `rhai_demo` (`LAZYOS_CLI=1`: `rhai -e`, scripts, pipelines, limits, REPL on the console, serial markers `RHAI:<name>:PASS\|FAIL`) and `rhai_desktop` (`LAZYOS_DESKTOP=1 LAZYOS_XUI_AUTOSTART=term`: the same in the desktop Terminal). LazyShell (#157): `shell_demo` (`LAZYOS_DESKTOP=1 LAZYOS_XUI_AUTOSTART=term`: desktop + taskbar, Ctrl+Esc start menu, launch System Monitor from it, Alt+Tab, Alt+F4, then `kill -9` LazyShell from the Terminal and wait for `init` to restart it; markers `SHELL:*`) and `shell_wallpaper` (`LAZYOS_DESKTOP=1 LAZYOS_XUI_AUTOSTART=term`: sets the desktop picture `sys/ui/wallpaper` from the Terminal, a dark one, a light one and a missing file, then picks one in Settings; markers `SHELL:WALLPAPER:PASS\|FAIL`). Desktop scripts click LazyShell's layout at 1280x720: the "LazyOS" start button at (44, 704), taskbar entry *i* at (172 + 164*i, 704), configured start-menu row *j* of 13 at (134, 648 - (13 - j)*24) (Terminal 336 ... Package Installer 600, Devices 624; installed apps sit above them), then the power rows "Restart..." at (134, 648) and "Shut down..." at (134, 672) (`shutdown_menu`: the confirmation swaps them for "Restart now"/"Shut down now" and "Cancel" in place). `LAZYOS_SHELL=0` desktops and `LAZYOS_XUID=1` demo images have no taskbar: their scripts restore minimized windows with Alt+Tab. Networking (`LAZYOS_DESKTOP=1 LAZYOS_NETD=1 LAZYOS_NETD_ARGS=demo=0`, run with `--net`, docs/networking-host-access.md): `net_apps` (Network and Net Tools: four pings to the gateway, a lookup, a fetch of `http://10.0.2.2:8080/` that goes out through the host forward and back into Net Tools' own server; markers `NETAPP:`/`NETTOOLS:`) and `net_config` (Manual then Automatic in the Network app, `netd` restarting with each, then Renew). |
| `../run_demo.py` | Build and boot the interactive demo in QEMU with one command. |

### Why QMP instead of `-vnc`/`-nographic`

`qemu_shot.py` launches QEMU with `-display none` and a TCP QMP socket, then
issues the `screendump` command with `format=png` (QEMU >= 7.1). The graphics
device still emulates its display surface when `-display none` is set, so the
capture works with no window, display server, or X11 on the host. TCP is used
instead of a UNIX socket so the same script runs on Windows.

## Quick start

```bash
# Firmware-only smoke test (validates the capture pipeline itself)
python tools/screenshot/qemu_shot.py --out shots --at 2,5

# Capture from a real OS image
python tools/screenshot/qemu_shot.py --out shots --at 2,5,10 --image target/lazyos.img

# Assert the latest shot actually contains rendered content
python tools/screenshot/pngstats.py shots/shot_10s.png --min-nonblack 0.01
```

Outputs land in `shots/`: `shot_<t>s.png`, `serial.log`, `summary.json`.

On Windows, QEMU is often installed at `C:\Program Files\qemu`; if that
directory is not on `PATH`, either add it or pass `--qemu "C:\Program Files\qemu\qemu-system-x86_64.exe"`.

### Acceleration

All QEMU tools accept `--accel auto|none|tcg|whpx|kvm` (default `auto`). Auto
uses WHPX on Windows or KVM on Linux when available; this makes software
rendering many times faster than pure TCG emulation. KVM is only picked when
`/dev/kvm` is read/writable *and* a paused probe start of QEMU with
`-accel kvm` succeeds, so an unusable KVM falls back to TCG instead of failing.
Use `--accel none` to force TCG (e.g. for timing-independent reproduction).

CI runs with `auto`: GitHub-hosted `ubuntu-latest` runners have `/dev/kvm` but
only for `root:kvm` (mode 0660), so each QEMU workflow first runs
`tools/ci/enable_kvm.sh`, a best-effort udev rule that opens the device to the
runner user. If that ever stops working the jobs silently fall back to TCG.

## `qemu_shot.py` options

| Flag | Default | Meaning |
|------|---------|---------|
| `--image PATH` | none | Raw disk image to boot. Omit to boot firmware only. |
| `--out DIR` | `shots` | Output directory. |
| `--at 2,5,10` | `3,6,10` | Capture times in seconds after boot. |
| `--qemu PATH` | auto | QEMU binary. Auto-detected from `PATH` / common dirs. |
| `--timeout SECS` | `180` | Overall connect/capture timeout. |
| `--memory SIZE` | `256M` | Guest RAM. |
| `--extra-arg ARG` | none | Extra QEMU arg (repeatable), e.g. `--extra-arg=-vga --extra-arg=std`. |
| `--data-disk PATH` | none | Attach an existing ext2 volume as a second virtio-blk device (create one with `python -m tools.mkdisk PATH`). Also accepted by `qemu_session.py`. |
| `--home-disk PATH` | none | Attach an existing home volume (`python -m tools.mkdisk PATH --home-volume`) as a virtio-blk device after the boot disk and any `--data-disk`. Off by default so CI stays hermetic; also accepted by `qemu_session.py`. |

## `pngstats.py` assertions

Reports `width`, `height`, `channels`, `mean_rgb`, `nonbackground_ratio`,
`distinct_colors_q4`, and luminance range. Exits non-zero when any assertion
fails. Assertions:

| Flag | Fails when |
|------|-----------|
| `--min-nonblack R` | non-background pixel ratio `< R` (blank screen) |
| `--min-colors N` | distinct colours `< N` |
| `--expect-width W` / `--expect-height H` | dimensions differ |
| `--max-mean M` | mean RGB `> M` (all-white detection) |

Use `--json` for machine-readable output.

## Driving the guest (keyboard & mouse)

`qemu_session.py` injects input over QMP (`input-send-event`) and captures
screenshots at chosen moments, so an agent can interact with the OS without a
human at the keyboard:

```bash
python tools/screenshot/qemu_session.py --image target/lazyos.img \
    --out shots/session --script tools/screenshot/examples/type_and_shot.json
```

A script is a JSON list of steps; each has an optional `at` (seconds since boot,
or since the latest `wait_for` gate) and one action:

| Action | Example |
|--------|---------|
| capture a screenshot | `{"at": 2, "shot": "boot"}` |
| type text (US layout) | `{"type": "dir\\n"}` |
| press a named key | `{"key": "enter"}` / `{"key": "f5"}` |
| press several keys | `{"keys": ["up", "up", "enter"]}` |
| hold a key | `{"key_down": "alt"}` / `{"key_up": "alt"}` |
| move the mouse | `{"mouse_move": [dx, dy]}` (relative; sent as paced steps of at most 127 px per axis, see below) |
| hold a button | `{"mouse_down": "left"}` / `{"mouse_up": "left"}` |
| click | `{"mouse_click": "left"}` |
| scroll | `{"mouse_scroll": 3}` |
| absolute pointer | `{"mouse_abs": [x, y]}` (needs `--tablet`) |
| click a pixel or named target | `{"click_at": [x, y]}` or `{"click_at": "name"}` (needs `--tablet`; `--screen WxH`, `--targets FILE`) |
| wait / quit | `{"wait": 1.5}` / `{"quit": true}` |
| wait for a serial marker (N-th match) | `{"wait_for": "SYSMON:UP:PASS", "timeout": 240, "occurrence": 2}` |
| confirm an input was handled | `{"key": "r", "until": "SYSMON:REFRESH:PASS", "timeout": 60, "retries": 2}` |
| capture part of a marker | `{"wait_for": "app=lazyshell pid=(\\d+)", "regex": true, "capture": "pid"}`, then `{"type": "kill -9 ${pid}"}` |

### Readiness gating (prefer it to fixed `at` times)

Boot time under TCG varies a lot between a desktop and a shared CI runner, so a
script that fires input "at 95 s" is flaky: the app may not be listening yet,
or the session may quit before the guest handles the input. Gate on what the
guest prints instead:

- `wait_for` blocks until the serial log contains the text (a substring, or a
  regular expression with `"regex": true`) and fails the session after
  `timeout` seconds (default `--wait-timeout`, 240). `"occurrence": N` (an
  integer `>= 1`, default 1) waits for the N-th match, so a marker printed once
  per launch (e.g. a second `EDITOR:UP:PASS`) can be gated on.
- `until` on any input action waits for a marker printed *after* the input was
  sent, and re-sends the input up to `retries` times if it does not appear.
- After a `wait_for`, later `at` values count from that gate, so a timed
  choreography (relative mouse moves) starts only once the guest is ready.
- `"capture": "<name>"` on a `wait_for` keeps the regex's first group (or the
  whole match); `${<name>}` in a later `type`, `wait_for` or `until` is
  replaced by it. `shell_demo.json` kills LazyShell this way, with the pid from
  `INIT:LAUNCH:PASS app=lazyshell pid=<n>` (Linux `kill` numbers tasks by their
  slot, the same number `init` prints).
- `--fail-on REGEX` (repeatable) aborts as soon as the serial log matches, e.g.
  `--fail-on "SYSMON:(BIND|UP|RUN):FAIL"`, rather than waiting out a gate.

`xui_desktop.json` types into the Terminal's BusyBox `sh`, so the image needs
`/system/bin/busybox` (`build.rs` embeds it when present). Without it the Terminal prints
`TERM:SPAWN:FAIL: No such file or directory` and `TERM:UP:PASS` never comes;
run it with `--fail-on "TERM:SPAWN:FAIL"` to fail at once instead of after the
gate timeout. `python tools/abi/busybox.py` builds the pinned BusyBox (it downloads
the tarball and verifies its SHA-256): natively on Linux with `musl-gcc`, otherwise
inside an `alpine` container when a Docker engine is running (so Windows/macOS hosts
work: start Docker Desktop first). When neither is available it prints how to supply
one by hand (`tools/abi/busybox` or `LAZYOS_BUSYBOX`). Re-run `cargo build`
afterwards so the image embeds it.

A failed gate captures `shot_failed.png`, prints the serial tail, and exits 1.
`summary.json` records `ok`, `failure`, and a per-step `timeline` (seconds
since QMP connected, plus when each gate or confirmation was seen), which
shows how much headroom a run had.

Keyboard uses a US layout (Shift handled automatically for symbols/capitals).
Mouse uses relative motion/buttons (PS/2) by default; pass `--tablet` to attach
`usb-tablet` for absolute positioning. The guest's PS/2 keyboard and mouse
drivers consume the injected events, so a script drives the real OS input path.

## `monkey.py` (random-input soak)

The Android-`monkey` equivalent: boots the desktop headless, then fires seeded
random mouse / keyboard / drag / scroll / chord / typing / burst input over QMP
for `--duration` seconds and stops at the first crash signature (`EXCEPTION:`,
`LazyOS PANIC`, `HANG:`, ring-3 `killed by`) or a **freeze** (the display stops
changing while the pointer is nudged; registers, a 256-word stack dump and an
NMI `HANG:` report are captured).

```bash
python tools/screenshot/monkey.py --build --image target/lazyos.img     --duration 300 --seed 1 --runs 4 --out shots/monkey
```

`--build` builds the desktop image exactly as the launcher's Desktop mode does
(`tools/xui/build.py`, then `cargo build` with `LAZYOS_DESKTOP=1`), plus the
Terminal at boot (`LAZYOS_XUI_AUTOSTART=term` unless already set); a plain
`cargo build` has no userspace and never reaches a desktop. Every action is
logged to `actions.jsonl` *before* it is sent, so the last line is the input in
flight at the fault; `--replay actions.jsonl [--replay-tail N]` re-sends it.
Guest timing is not deterministic, so use `--runs N` (seeds `seed..seed+N-1`)
to hunt a rare fault. To symbolize a freeze, subtract the kernel load base
(`0x8000000000`) from the addresses in `freeze_registers.txt` and run
`addr2line -f -C -e <kernel ELF>`. Findings keep `shot_fault.png`,
`serial_tail.txt`, `report.json` and the registers; freeze findings also keep
`freeze_registers.txt` and `freeze_hang_report.txt`. Exit status is 1.

A frozen display with the CPU in ring 0 at a port instruction (`in`/`out`,
`ins`/`outs`; decoded by `freeze_probe.py` from the monitor) may be a long
device poll rather than a hang (issue #449), so the guest first gets
`--io-grace` seconds (default 20, `0` disables) to draw again; a recovery is
recorded under `io_stalls` in `report.json` and the run goes on. `--ide-disk`
attaches the image as IDE, so the ATA driver serves the disk as it did when
#449 was found. `python tools/screenshot/test_freeze_probe.py` tests the decoder.

## CI

`.github/workflows/screenshots.yml` runs on push, pull requests, and manually.
It installs QEMU, builds the OS image if a Cargo project is present, captures
screenshots headless, verifies they are non-blank, uploads them as an artifact,
and (for same-repo PRs) publishes them to a force-pushed `screenshots` branch
and comments them onto the PR.

### Attaching images to a pull request

GitHub renders external image URLs in comments, so CI uploads each PNG to a
public image host and embeds the returned URL in the PR comment. Hosts are tried
in order (some block CI datacenter IPs, e.g. Catbox returns HTTP 412):

1. **Imgur** — if the repository secret `IMGUR_CLIENT_ID` is set (most reliable)
2. **Catbox** — anonymous
3. **Litterbox** — Catbox temporary (72h)
4. **Uguu**, **0x0.st**, **tmpfiles.org** — further fallbacks

Run the helper locally too:

```bash
python tools/screenshot/upload_image.py shots/shot_10s.png
```

If every host fails, the comment falls back to the `screenshots` branch. CI
appends each run under `runs/<run_id>/` (no force-push) so historical links stay
reachable:

```
https://raw.githubusercontent.com/<owner>/<repo>/screenshots/runs/<run_id>/<file>.png
```

## Stronger graphic checks

Beyond the blank-screen assertions CI runs today, a session can add
`--min-colors`, `--expect-width`/`--expect-height` or `--max-mean`. Golden
reference-image comparison is not implemented; the decoder in `pngstats.py` is
the place to build it.

## Relative mouse moves are split into paced steps

A PS/2 packet moves the pointer at most 127 px per axis, and QEMU's PS/2 mouse
queue holds only 16 bytes. A large relative move needs several packets, and the
ones that do not fit are held back until the *next* input event. A click sent
right after a long move would then fire before the pointer arrived (with a wheel
mouse's 4-byte packets this starts at about 250 px). `Qmp.mouse_move` therefore
sends every move as single-packet steps with a short pause, so `mouse_move` is
exact and a following click lands where the script says. `python
tools/screenshot/test_qmp_mouse.py` tests the splitting.
