# Screenshot tooling

Headless tooling to capture and verify pixels from QEMU. It exists so that both
CI and an AI agent can *see* what LazyOS renders, without a physical display.

## Pieces

| File | Purpose |
|------|---------|
| `qemu_shot.py` | Boot an image (or bare firmware) with `-display none` and capture PNGs via the QEMU Machine Protocol. Stdlib only. |
| `pngstats.py`  | Decode a PNG (stdlib only) and report/assert pixel statistics. |

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

## CI

`.github/workflows/screenshots.yml` runs on push, pull requests, and manually.
It installs QEMU, builds the OS image if a Cargo project is present, captures
screenshots headless, verifies they are non-blank, uploads them as an artifact,
and (for same-repo PRs) publishes them to a force-pushed `screenshots` branch
and comments them onto the PR.

### Attaching images to a pull request

GitHub renders external image URLs in comments, so CI uploads each PNG to a
public image host and embeds the returned URL in the PR comment:

- **Default:** anonymous [Catbox](https://catbox.moe) upload (no account needed).
- **Optional:** set the repository secret `IMGUR_CLIENT_ID` to upload to Imgur
  instead. The upload helper is `tools/screenshot/upload_image.py`; run it
  locally too:

  ```bash
  python tools/screenshot/upload_image.py shots/shot_10s.png
  ```

If the external upload fails, the comment falls back to the raw URLs of the
`screenshots` branch. The branch is force-pushed, so its CDN URLs can briefly be
stale; the PR comment links the exact commit SHA of that run instead.

Stable image URL pattern:

```
https://raw.githubusercontent.com/<owner>/<repo>/screenshots/<file>.png
```

## Adding graphic checks later

Once LazyOS draws known content, add assertions to the CI "Verify" step, e.g.
`--min-nonblack 0.2 --min-colors 16 --expect-width 1280 --expect-height 720`.
For richer verification, add a small reference-image comparison using the
decoder in `pngstats.py`.
