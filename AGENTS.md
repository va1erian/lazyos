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
VFS (FAT boot volume, ramfs `/tmp`, an ext2 read/write driver that only the
kernel test suite exercises so far). The authoritative description of the
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
`-- --cpu max` are supported). For scripted visual verification, capture a
session script instead:

```bash
python tools/screenshot/qemu_session.py --image target/lazyos.img \
    --out shots/demo --script tools/screenshot/examples/multitask_demo.json
```

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
```

The runner exits non-zero on any failure, a missing summary, or a stale
non-test image. Normal boots are unaffected: without `LAZYOS_TESTS=1` the suite
is not compiled. Test-only hooks live behind `cfg(lazyos_tests)`; add new tests
under `kernel/src/tests/` (`mem_suite` is where allocator-specific tests go). CI
is `.github/workflows/kernel-tests.yml`; see `tools/test/README.md`.

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
- Prefer verifying with the existing scripts over ad-hoc commands so results are
  comparable across runs.

## Commands

```bash
python tools/screenshot/qemu_shot.py --out shots --at 2,5,10 --image <img>
python tools/screenshot/pngstats.py shots/*.png --min-nonblack 0.01
python tools/test/run.py --accel none
python tools/sound/run.py
```
