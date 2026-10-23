# AGENTS.md

Guidance for AI agents working in this repository. This file holds what every
change needs: the values, the performance rules, how to verify, and the
conventions CI enforces. Anything specific to one subsystem, app or harness
lives in [`docs/agents/`](docs/agents/index.md); the topic index at the end
says which file to open. Read the matching file before you touch that area.

## Core values

1. **Security first.** No ambient authority: apps run as the logged-in user
   with no capability, nobody is root, and privileged calls go through
   `elevd` or a kernel-stamped identity, never "uid 0". Validate all untrusted
   input (the wire, userspace pointers, files, pages). Every `unsafe` block is
   minimal and has a `// SAFETY:` comment. A new interface that carries
   secrets or authority is denied to developer tools.
2. **Messenger is the fabric.** LazyOS is a kernel-mediated, capability-based
   IPC/pub-sub system (`messengerd`, `init`, `logd`, `healthd`, `keyd`,
   `accounts`, `clipboardd`, the `xuid` compositor, ...) over a preemptive
   multitasking kernel with a VFS (ext2 read/write OS volume at `/`, read-only
   FAT `/boot`, ramfs `/transient` and `/tmp`, optional ext2 home volume).
   Every interface is a `.midl` file under `idl/` with generated code. An
   object field (`Channel<I>` moves, `Buffer` is shared) is declared like any
   other field, its index is its position in the method's declared order, and
   `midlc` refuses the old `transfers (...)` clause. Read
   [`docs/messenger.md`](docs/messenger.md) (sections 1 and 5) before touching
   IPC. The architecture and roadmap are
   [`docs/platform-plan.md`](docs/platform-plan.md) and
   [`docs/architecture/`](docs/architecture); read those, not code comments,
   for kernel internals, syscall numbers and window/task management.
3. **Performance is a feature.** See the next section. A change that makes
   input, scheduling, drawing or boot slower is a regression even if it is
   correct.
4. **Evidence over assertion.** Graphics are verified by reading real pixels,
   audio by the recording, networking by the wire, kernel code by its suite.
   Do not claim a change works from source or serial markers alone. Prefer
   the existing harnesses so results compare across runs.
5. **Small, readable, one way to do it.** Small single-purpose functions,
   comments that explain why, files under 500 lines, well-known paths from
   `libs/fhs`, evidence through the `trace` API. One web engine (WebKit in
   `webd`); small apps are Rhai/LazyRAD forms before they are Rust.
6. **Every app is launchable from both front ends** (`run_demo.py` and the GUI
   launcher), with a build switch and a core package. See "Project conventions".

## Performance

Latency and throughput are checked, not assumed.

- **Interrupts-off stretches.** Syscalls run with interrupts off. A loop that
  can run long inside a syscall needs `arch::irq_window::poll_point()` (never
  while holding a lock other than the i8042 FIFO's). Every boot logs
  `IRQOFF:MAX ... from=<file:line> to=<file:line>` for a new worst stretch of
  2 ms or more; the missing poll point lies between those lines.
- **Locks.** No new raw `spin` lock in the kernel trees
  (`python tools/sync/check_locks.py --ratchet`); no I/O or long work under a
  lock; mind lock order.
- **Never poll on a timer.** Wake on a doorbell/eventfd or a Messenger
  subscription (`xui_app::fabric::Subscriber`); the broker never frees its
  receive buffers for a poller.
- **The UI thread does no I/O.** No network, JSON, decoding or shaping on it;
  do that on worker threads and hand over pixels. Do not push a clip on a hot
  paint path (LazySky's feed made every draw several times dearer). Draw only
  damaged regions, and pace to `GetOutput.refresh_millihz`.
- **Write-back, not write-through.** Real ext2 mounts go through the block
  cache and commit at sync/fsync/5 s/memory pressure; call `Ext2::barrier`
  only where an operation needs one step on disk before the next.
- **Pay for what you use.** Tracing compiles out with `LAZYOS_TRACE=0`; give a
  tag that can fire in a loop its own `Budget`; build tools write an output
  only when its bytes changed; programs ship with `-Os`, `--icf=safe`, no
  unwind tables and cut ICU data.
- **Measure.** Use hardware acceleration (WHPX on Windows, KVM on Linux; auto
  detected, `--accel none` forces TCG) for any timing; a loaded host inflates
  IRQOFF numbers (see "Interrupt latency"). After a scheduling or wake-path
  change run `python tools/perf/run.py` (see "Latency harness"). Memory
  ceilings are tunable in `kernel/src/limits.rs`; see
  [`docs/agents/kernel.md`](docs/agents/kernel.md) and
  [`docs/architecture/limits.md`](docs/architecture/limits.md).

## Build and run

```bash
python tools/run_demo.py                  # build target/lazyos.img if needed and boot it
python tools/run_demo.py --desktop --net  # common flags; --suite base|full, --headless, --no-build
```

`cargo build` writes `target/lazyos.img`: an MBR disk with the bootloader, a
FAT `/boot` (kernel and `lazyos.cfg`) and an ext2 OS volume holding everything
else. A rebuild **updates the OS volume in place** (installed apps, settings
and logs survive); `LAZYOS_RESET_OS=1` (`--reset-os`) recreates it, and CI sets
it everywhere. Do not rebuild while QEMU has the image open. Every optional
feature is a `LAZYOS_*` build switch with a matching `run_demo.py` flag; the
list is [`docs/build-switches.md`](docs/build-switches.md). A desktop image
boots to a login screen (`user`/`lazy`, `admin`/`nimda`); session scripts log
`user` in. Image layout, suites, login and elevation details:
[`docs/agents/build-and-run.md`](docs/agents/build-and-run.md).

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
   (manual only: Actions -> screenshots -> Run workflow) captures, verifies,
   uploads artifacts and publishes to the `screenshots` branch.

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

## Tracing (mandatory)

**Every evidence line goes through the `trace` API of its runtime, never a
print.** An evidence line is anything a harness, session script or `dbgctl`
reads off the serial port or program ring (`UP`/`QUIT`/`PASS`/`FAIL`,
`UI:RECT`, `TEST:`, `LABEL:DENY`, ...). Desktop (xui) apps, `lazyrad-os`,
`doom`, `emusic`, `quake`, `srb2kart` use `xui_app::trace`; services and
commands in `user/` use `user::trace`; the kernel uses `kernel/src/trace.rs`.
Name the channel (`libs/tracefmt/src/channel.rs`), start an xui app with
`launch::run(Channel::X, ...)`, keep free text last and unquoted, never put a
secret in a trace. CI runs `python tools/trace/check_adhoc.py --ratchet`; never
raise a baseline entry. Macros, the diagnostic-vs-evidence rule and the lint's
limits: [`docs/agents/tracing.md`](docs/agents/tracing.md).


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

## Latency harness

`python tools/perf/run.py` builds the desktop with `LAZYOS_PERF=1` (kernel cfg
`lazyos_perf`, hooks in `kernel/src/perf/`), boots it headless, knocks on its
virtio-net card and moves the PS/2 mouse over QMP, and writes
`docs/perf/report.md` from the kernel's `PERF:` lines (IRQ-to-task wake,
input to `inputd`, input to present, interrupts-off syscall stretches,
in-kernel IPC round trip). `--label X` appends a row to `docs/perf/history.md`;
re-run it after a scheduling or wake-path change. See `tools/perf/README.md`.

## Remote inspection (`dbgd`, `LAZYOS_DBGD=1`): use the `lazyos-dbg` skill

To read a live box (QEMU or a real PC) over its network card (kernel boot log,
what services printed, tasks, PCI devices, `devd` drivers, USB controller
state, Messenger registry, allowlisted registers), **load the `lazyos-dbg`
skill** (`.claude/skills/lazyos-dbg/SKILL.md`). It holds the build switches
(`run_demo.py --dbgd`, `--dbgd-control`, `--dbgd-mmio`), the `dbgctl.py` and MCP
usage, the method for reading the data, the rules for hardware reads and
service hot reload, and the harness that verifies a change to `dbgd`
(`python tools/dbg/run.py`). Plan, protocol and threat model:
[`docs/dbgd-plan.md`](docs/dbgd-plan.md); user guide: [`docs/dbgd.md`](docs/dbgd.md).
Prefer it to screenshots and serial scraping whenever a box is reachable.

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
- **Evidence markers go through the `trace` API, never a print.** A desktop
  (xui) app, or any code in `xui-app/`, `lazyrad-os/`, `games/doom/`, `emusic/`,
  `games/quake/`, `games/srb2kart/`, writes `UP`/`QUIT`/`PASS`/`FAIL` lines, probe
  rectangles and stalls with `xui_app::trace::{pass!, fail!, event!, line!}`
  on a `Channel` from `libs/tracefmt`, starts with
  `launch::run(Channel::X, ...)`, and never `println!("X:UP:PASS")`; services
  use `user::trace`, the kernel `kernel/src/trace.rs`. A new app is started
  with `tools/xui/new_app.py`, which writes the calls and the channel.
  `python tools/trace/check_adhoc.py --ratchet` is a CI step: a new file with a
  marker print, or a file with more of them than
  `tools/trace/adhoc_baseline.json` records, fails (see "Tracing"). A portable
  crate under `xui-app/crates/` that `xui-app` itself depends on (`writer`,
  `settings`, `confd-editor`, `pdfview`) cannot call `xui_app::trace` (a
  cycle): it reports through an event its host bin turns into a trace, and its
  markers do not grow; ask for that seam before adding one.
- Keep source files **under 500 lines**; split by responsibility instead of
  growing a file past it. `python tools/check_file_length.py` (a CI step)
  enforces it for every tracked `.rs`/`.py` file except `@generated` output;
  its allowlist is empty (issue #484).
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
- **A new interface that carries secrets or authority is added to
  `devtool::DENY`** (`libs/devtool`): developer tools (`kind = "developer-tool"`,
  `docs/devtool-packages-plan.md`) are default allow minus that list. Any other
  new interface goes in `tools/devtool/reviewed_interfaces.txt` after deciding
  a script the user runs may call it; `python tools/devtool/check_interfaces.py`
  (a CI step) fails until every interface in `idl/` is in one of the two.
- Prefer verifying with the existing scripts over ad-hoc commands so results are
  comparable across runs.

## Before you push: run the CI basics (required)

CI runs the cheap gates first and a failure there costs a whole round trip
(push, wait, read logs, fix, push again, wait). Run the ones that apply to
what you touched **before every push**, not after the first red run. All but
the first two take seconds to a few minutes; "it compiles" is not enough.

Always, for any code change:

```bash
cargo fmt --all -- --check                       # `cargo fmt --all` fixes it (CI's `fmt` job); also run it in a standalone workspace you edited, with --manifest-path
python tools/check_file_length.py                # every .rs/.py under 500 lines (generated midlc output exempt)
python tools/trace/check_adhoc.py --ratchet      # no new ad hoc marker prints (use the trace API)
python tools/trace/catalog.py --check            # channel catalogue current after touching libs/tracefmt
python tools/fhs/check_literals.py               # well-known paths come from libs/fhs, never literals
python tools/sync/check_locks.py --ratchet       # no new raw `spin` lock in the kernel trees
python tools/devtool/check_interfaces.py         # every idl/ interface is denied or reviewed for developer tools
```

Then the checks for the area you changed:

- **Rust in the OS workspace:** `cargo clippy -p <crate> --all-targets -- -D warnings` and `cargo test -p <crate>` for
  each crate you touched. Kernel and `user/`: `cargo clippy -p kernel -p user --target x86_64-unknown-none
  -Zbuild-std=core,alloc -- -D warnings -D clippy::undocumented_unsafe_blocks` (every `unsafe` block needs its
  `// SAFETY:`; a bare `unwrap`/`expect` in `kernel/src` fails). Then `cargo build` must succeed.
- **`xui-app/`:** `cargo test --manifest-path xui-app/Cargo.toml -p <crate>` (the crate's snapshot tests too;
  Read the PNGs it writes). `python tools/xui/check_pin.py` and `python tools/xui/check_call_bounds.py` if you
  touched manifests or a UI-thread Messenger call.
- **`idl/*.midl`:** regenerate and check with the exact command in `.github/workflows/midlc.yml`
  (`python tools/midlc/midlc.py --check ...`) plus `python tools/midlc/test_midlc.py`; the generated files are
  committed, so a forgotten regeneration fails CI. Add a new interface to `devtool::DENY` or
  `tools/devtool/reviewed_interfaces.txt`.
- **Syscalls:** the frozen table `libs/lazyos-sys/abi/syscalls.abi` only grows
  (`cargo test -p lazyos-sys --all-features`).
- **Kernel changes:** the kernel suite, `python tools/test/run.py --accel none`, in full (see "Testing
  requirement for kernel components"), not only your `LAZYOS_TEST_FILTER`.
- **Image build (`build.rs`, `build_support/`):** `cargo test -p build-support-tests`.
- **Docs (`docs/**`, `README.md`):** `python tools/docs/check_snippets.py` and `python tools/site/build.py --strict`.
- **A harness you changed:** its judge's self-test (`tools/<area>/test_judge.py`), and the harness itself for
  graphics, webd, audio, USB and network work (the sections above name them).

`.github/workflows/ci.yml` is the authoritative list (the `lint`, `fmt`,
`file-length`, `trace`, `locks`, `fhs`, `build-and-boot` jobs) and each area
workflow (`webd.yml`, `xui.yml`, `midlc.yml`, `kernel-tests.yml`, ...) lists the
paths that trigger it: **open the workflow of every path you touched and run its
steps that you can run locally.** If one cannot run in your environment (no
Docker, no KVM, no zig), say which in the PR description instead of leaving it
for CI to find. Two clean local runs beat one green-looking guess; fix a red
check at its cause, never by skipping, `#[ignore]`-ing or loosening it.

## Pre-PR review (required)

Before an agent opens or finalizes a pull request, it MUST run a thorough
review of the branch with the **`/code-review` skill** (invoke it through the
`Skill` tool: `code-review`, and pass `--fix` or fix the findings yourself). An
agent without that skill uses the equivalent: a review in a fresh context that
did not write the change (a separate reviewer agent given the diff, not your
conclusions), at the highest effort it offers, over the scope below. Do not
skip this because the change is small or the tests pass.

1. Run it when the work is done and the branch is committed, **after** the CI
   basics above are green and before opening the PR (or before marking a draft
   ready).
2. Review scope: correctness, security (untrusted input, `unsafe` soundness,
   capability and label-policy changes), the kernel testing requirement
   (correctness and stress tests), the project conventions above (MIDL for every
   interface, `libs/fhs` paths, 500-line file limit, launcher and GUI wiring for
   new apps), and visual verification for graphics changes.
3. **Kernel, drawing and webd changes get a stronger review, not the default
   one.** That means `kernel/`, anything the kernel shares with userspace
   (`libs/lazyos-sys`, `libs/ext2fs`, `libs/gpt`, memory, scheduler, syscalls,
   IPC/Messenger, VFS, drivers and the services that own hardware such as `usbd`,
   `netdrv`, `sndd`, `gpud`, `devd`, `init`), **drawing** (the framebuffer and
   `kernel/src/{gfx,display,surface,xuid,cursor,text}*`, the `xuid` compositor,
   `libs/{raster,cursorgfx}`, `xui-app/crates/damage`, any software rasteriser
   or painter, HiDPI and clip/damage code) and **webd** (`xui-app/crates/webd*`,
   `xui-app/webd`, `webkit/`, `idl/webd*.midl`, the WebKit port and its sandbox
   and label policy). For these:
   - run `/code-review high` (an equivalent agent: its `high` effort, with
     `--max-findings all`; `max` takes about an hour, too long for a routine
     review), and run it **again** after fixing the findings, until a pass
     finds nothing real;
   - give the reviewer the specific risks to attack: lock order, interrupts-off
     stretches and missing `poll_point`s, `unsafe` and DMA lifetimes, user
     pointer validation, integer overflow on sizes from userspace or the wire,
     use-after-free of a slot/generation, capability or label widening, resource
     exhaustion, and for drawing off-by-one clips, stride/pitch and format
     assumptions, damage that misses a region, and a pointer or frame drawn
     twice; for webd, quotas, view/slot isolation, navigation policy and
     hostile page behaviour;
   - confirm the tests the review asks for exist (kernel correctness **and**
     stress tests; host or fuzz tests for webd) and that you looked at real
     pixels (`Read` the screenshot) for any drawing change.
4. Fix every real finding, or say in the PR description why one was declined.
5. Put a short note in the PR description: which review ran (skill and effort
   level, or the equivalent agent), what was found and what changed, and which
   CI checks you ran locally.

## Commands

```bash
python tools/screenshot/qemu_shot.py --out shots --at 2,5,10 --image <img>
python tools/screenshot/pngstats.py shots/*.png --min-nonblack 0.01
python tools/test/run.py --accel none
python tools/sound/run.py
```

## Topic index

Open the guide before changing the area; each is a section-per-subsystem file.

- [Build, run and the disk image](docs/agents/build-and-run.md): Running the demo, software suites, login and elevation, the OS volume layout, and the documentation site.
- [Tracing in full](docs/agents/tracing.md): The `trace` API, channels, masks and the ad hoc print lint.
- [Kernel, filesystem and conformance](docs/agents/kernel.md): Resource limits, the kernel test harness, ext2 and its journal, the Linux ABI bench and real Linux programs.
- [System services, packages and security](docs/agents/services.md): Developer tools and the Playground, Rhai, packages and the label trace, app failures, notifications, tray, accounts attacks, keyd secrets, shutdown.
- [Desktop apps](docs/agents/apps.md): Terminal, Docs, Writer, Archiver, PDF, Video, Paint, About, Picture Viewer, the MOD player, Mail and LazySky.
- [Games](docs/agents/games.md): LazyGolf, the NES Player, Doom, emusic, Quake and SRB2Kart.
- [WebKit, webd and the browser](docs/agents/web.md): JavaScriptCore, the WebKit dependency bundle and port, LazyWeb and webd.
- [Networking, TLS, SMB and Wi-Fi](docs/agents/networking.md): HTTPS clients, networking in a boot, network drives, SMB, network time, Wi-Fi and the network tooling.
- [Display, drivers and hardware](docs/agents/hardware.md): HiDPI, the Intel GPU service, devd, interrupt routing, USB, sound, FAT sticks and installing beside Linux.
