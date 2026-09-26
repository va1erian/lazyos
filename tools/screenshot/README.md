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
| `examples/type_and_shot.json` | Example session script. |
| `examples/window_demo.json` | Session script exercising window move/scroll. |
| `examples/cli_demo.json` | Session script for the CLI demos (help, box, ball). |
| `examples/bench.json` | Session script that runs the `bench` command. |
| `examples/user_demo.json` | Session script that runs the ring-3 `HELLO.ELF` program. |
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
rendering many times faster than pure TCG emulation. Use `--accel none` for
deterministic CI behaviour.

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

A script is a JSON list of steps; each has an optional `at` (seconds since boot)
and one action:

| Action | Example |
|--------|---------|
| capture a screenshot | `{"at": 2, "shot": "boot"}` |
| type text (US layout) | `{"type": "dir\\n"}` |
| press a named key | `{"key": "enter"}` / `{"key": "f5"}` |
| press several keys | `{"keys": ["up", "up", "enter"]}` |
| move the mouse | `{"mouse_move": [dx, dy]}` |
| click | `{"mouse_click": "left"}` |
| scroll | `{"mouse_scroll": 3}` |
| absolute pointer | `{"mouse_abs": [x, y]}` (needs `--tablet`) |
| wait / quit | `{"wait": 1.5}` / `{"quit": true}` |

Keyboard uses a US layout (Shift handled automatically for symbols/capitals).
Mouse uses relative motion/buttons (PS/2) by default; pass `--tablet` to attach
`usb-tablet` for absolute positioning. Guest-side handling requires a driver —
the tooling is ready before then, and events are delivered via QMP regardless.

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

## Adding graphic checks later

Once LazyOS draws known content, add assertions to the CI "Verify" step, e.g.
`--min-nonblack 0.2 --min-colors 16 --expect-width 1280 --expect-height 720`.
For richer verification, add a small reference-image comparison using the
decoder in `pngstats.py`.
