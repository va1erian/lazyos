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

## Project conventions

- The OS is `no_std`; target `x86_64-unknown-none`; built via the root crate's
  `build.rs` + artifact dependency (see the roadmap issues).
- Do not commit generated screenshots (`shots/` is git-ignored); CI publishes
  them to the dedicated `screenshots` branch.
- Prefer verifying with the existing scripts over ad-hoc commands so results are
  comparable across runs.

## Commands

```bash
python tools/screenshot/qemu_shot.py --out shots --at 2,5,10 --image <img>
python tools/screenshot/pngstats.py shots/*.png --min-nonblack 0.01
```
