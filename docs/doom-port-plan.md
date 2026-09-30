# Plan: Doom (doomgeneric) + Freedoom on LazyOS

> **Status: draft.** Nothing here is implemented. Written against the state of
> `main` on 2026-09-30 (Linux ABI shim complete, `xuid` desktop with client
> mode, MIDL display protocol).

**Goal:** a windowed Doom in the LazyOS desktop, launched from the app registry
like Paint or Files, running the **Freedoom** IWAD (freely redistributable), built
as an ordinary `x86_64-unknown-linux-musl` static binary and run on the Linux
ABI shim. No kernel changes are the target; every kernel gap we hit becomes a
fixture in `tools/abi/` first.

**Non-goals (v1):** sound and music, networked multiplayer, savegame
persistence beyond what `/data` gives for free, fullscreen scanout.

## Decisions

| Question | Choice | Why |
|---|---|---|
| Engine | [`doomgeneric`](https://github.com/ozkl/doomgeneric) (GPL-2.0, from id's linuxdoom) | Designed for exactly this: the whole platform layer is 6 functions (`DG_Init`, `DG_DrawFrame`, `DG_SleepMs`, `DG_GetTicksMs`, `DG_GetKey`, `DG_SetWindowTitle`) and a `DG_ScreenBuffer` of 32-bit XRGB. No SDL, no sockets, no sound required. |
| Game data | Freedoom `freedoom1.wad` (Phase 1), optionally `freedoom2.wad` | BSD-3-Clause data, so it can ship in the image and CI. Never commit a WAD. |
| Language split | C engine compiled to a static lib, linked into a **small Rust binary** that implements the `DG_*` hooks | The display client (`LazyOSBackend`, raw syscall-5 shim, MIDL stubs from `messenger-generated`) is Rust. Hand-porting it to C would violate the "every Messenger interface goes through `midlc`" rule and duplicate the protocol. The Rust `main` owns the loop and calls `doomgeneric_Create`/`doomgeneric_Tick`. |
| Toolchain | `cc` crate driving **Alpine/`musl-gcc`** (Docker on Windows, as `tools/abi/busybox.py` does), falling back to `zig cc -target x86_64-linux-musl` | musl-native C toolchain is already an accepted host requirement for BusyBox. `zig cc` is the only Windows-native option with no Docker and no linker fiddling. |
| Where | New crate `doom/` beside `xui-app/` (not in the OS workspace), built by `tools/doom/build.py` -> `target/doom/lazydoom.elf` | Mirrors `tools/xui/build.py`; keeps the C build out of the `no_std` workspace. |
| Packaging | ELF embedded as `XDOOM.ELF` via the existing `LAZYOS_XUI_APPS` list; WAD fetched, hash-checked, and placed on the image (see phase 4) | Reuses `embed_xui_apps` and `init`'s app registry. |

## Architecture

```
xuid (compositor)
   ^  os.lazy.display.v1 over Messenger (client mode, syscall-5 shim)
   |
lazydoom.elf  (static musl, Linux ABI personality)
   |- Rust: main(), DG_* hooks, LazyOSBackend surface client, key map
   `- C:    doomgeneric engine (libdoomgeneric.a, built with musl)
        fopen/fread/fseek on freedoom1.wad -> openat/read/lseek/fstat
```

Per frame: `doomgeneric_Tick()` renders into `DG_ScreenBuffer` (640x400 XRGB by
default, configurable via `DOOMGENERIC_RESX/RESY`). `DG_DrawFrame` copies it
into the attached shared surface buffer and commits full-surface damage.
`DG_GetKey` drains a queue filled from the event endpoint (`KEY_DOWN`/`KEY_UP`,
`WINDOW_CLOSE`).

## Phases

Each phase ends with a screenshot or serial evidence per `AGENTS.md`, not a
source-only claim.

### D0 — Toolchain spike (no LazyOS involved)

- Confirm a musl C toolchain works on the dev host and in CI:
  `python tools/doom/build.py --engine-only` produces `libdoomgeneric.a`.
- Vendor doomgeneric as a **pinned git revision fetched at build time** (SHA
  recorded in the script, like BusyBox's tarball digest), not copied into the
  tree. Its GPL-2.0 license and a `NOTICE` go in `doom/`.
- Build flags: `-static -O2 -fno-pie -no-pie -fno-stack-protector -ffreestanding`
  is *not* wanted (we use libc); use `-DFEATURE_SOUND` **off**, `-D_DEFAULT_SOURCE`,
  `-Wno-implicit-function-declaration` only where doomgeneric needs it.
- Exit criterion: a host-run (`qemu-user` or WSL) smoke test renders one frame
  of the Freedoom title screen to a PPM. This isolates engine problems from
  LazyOS problems.

### D1 — Headless boot on the ABI shim

Add a fixture `doomboot` under `tools/abi/fixtures/` (or a `--headless` mode of
`lazydoom`) that loads the WAD, runs 35 ticks with a stub `DG_DrawFrame` that
hashes the framebuffer, prints `ABI:doom:PASS:<crc>` and exits.

Expected shim pressure, and what to do about each:

| Need | Status | Action |
|---|---|---|
| `openat`/`read`/`lseek`/`fstat`/`close` on a ~28 MB file | supported | Measure read throughput on FAT vs `/data` ext2; Doom reads lumps lazily via `fseek`+`fread` |
| `brk`/`mmap` for the zone (default 6 MB) and stdio buffers | supported | Raise `-mb` default if the shim's address space is tight; check RSS against kernel task memory limits |
| `clock_gettime(CLOCK_MONOTONIC)`, `nanosleep` | supported | `DG_GetTicksMs`/`DG_SleepMs` map straight through |
| `getenv`/`HOME` for the config dir | trivial | Pass `-savedir /tmp/doom` so saves and `default.cfg` land on ramfs (or `/data` for persistence) |
| `mprotect`, `arch_prctl`, `set_tid_address`, TLS | supported | none |
| `ioctl`/`isatty` on stdout | returns a benign error | none expected |

Any `ENOSYS` shows up in `python tools/abi/coverage.py`; fix in the shim with a
test in `kernel/src/tests/` (correctness + stress, per the kernel testing rule),
not with a Doom-side workaround.

### D2 — Windowed video

- Extract the client-mode display setup from `xui-app` into a small reusable
  crate (e.g. `lazyos-surface-client`): resolve `os.lazy.display.v1`, `create
  surface`, `create_buffer`, `commit(damage)`, event polling. Today this lives
  inside `LazyOSBackend`, which is coupled to xui widgets; Doom only needs the
  raw-pixels half. **This is the one refactor with real design content**, so do
  it as its own reviewed change and keep both consumers on it.
- Use the `surfbuf` swapchain (`libs/surfbuf`) if the compositor honours
  pipelined slots, else single-buffer with per-frame damage.
- Window: 640x400 (`Role` normal window), title "Freedoom" via
  `DG_SetWindowTitle`. Optional integer upscale later (2x = 1280x800) done in
  the copy loop, not by asking the compositor to scale.
- Pixel format: verify against `display.md` (framebuffer is 4 bytes/pixel; check
  channel order and whether the alpha byte is honoured) before writing the copy.
- Evidence: `tools/screenshot/examples/doom.json` boots the desktop with the app,
  waits for the title screen, and `pngstats.py --min-colors 32 --min-nonblack 0.5`
  guards regressions. Read the PNG to confirm it is actually the Freedoom logo.

### D3 — Input

> **Preferred path:** build on the system-wide input redesign in
> [input-plan.md](input-plan.md) (physical HID key codes, explicit
> press/release/repeat, focus enter/leave with held-key resync, poll bitmap,
> keyboard grab, `/dev/input/event*`). D3 then becomes a thin consumer of
> phase I3/I4. The list below is the **stopgap** that works on today's
> `KeyDown`/`KeyUp` if Doom lands first.

- Map `KEY_DOWN`/`KEY_UP` codes (low 24 bits, see "Key codes clients receive" in
  `display.md`) to Doom keys (`KEY_UPARROW`, `KEY_FIRE` = Ctrl, `KEY_USE` = Space,
  strafe = `,`/`.`, Enter, Esc, Tab for automap, Shift = run). A table in one
  file, unit-tested on the host.
- `DG_GetKey` returns `pressed`+`doomKey` pairs from a ring buffer; the event
  pump runs once per tick (non-blocking), so the game loop never stalls on
  Messenger.
- Known gaps to design around, not assume away:
  - **Held-key repeat.** PS/2 typematic gives repeated `KeyDown` with no
    `KeyUp`; Doom wants a clean edge model. Track "down" state and ignore repeats.
  - **Compositor-reserved chords** (Alt+Tab, Ctrl+Tab, Ctrl+Esc/Super, Alt+F4).
    Doom's default fire key is Ctrl; verify plain Ctrl and Ctrl+arrow reach the
    client (only listed chords are taken). If not, default-bind fire to `F`/Right-Ctrl.
  - **Mouse look.** Pointer events are surface-relative and absolute; no relative
    mode or capture exists. Ship keyboard-only in v1. Mouse strafe/turn needs a
    pointer-capture feature in `xuid` (a separate proposal, with its own IDL
    change in `idl/display.midl`).
  - **Focus loss.** Release all keys on blur (`KillFocus`) so Doom does not
    run forever after Alt+Tab.

### D4 — Packaging and launch

- `tools/doom/build.py`: builds the engine + Rust wrapper, and fetches
  `freedoom-<ver>.zip` from the official GitHub release with a **pinned SHA-256**
  (extract `freedoom1.wad`), cached under `target/doom/`. Same "artifact, never a
  committed blob" policy as BusyBox; if unavailable it reports `unavailable`
  and exits 0.
- WAD placement, decide by measurement in D1:
  1. **FAT boot volume** (`build.rs` `set_file_contents`): simplest, read-only,
     but adds ~28 MB to *every* desktop image and slows boot-image builds. Gate it
     behind `LAZYOS_DOOM=1` so the default image is unchanged (existing pattern).
  2. **`/data` ext2 volume** via `tools/mkdisk`: keeps the boot image small and
     gives writable saves; needs the data disk attached in the demo/CI runs.
  Recommendation: (1) behind `LAZYOS_DOOM=1` for the first landing, (2) once a
  demo data disk is standard.
- Register `XDOOM.ELF` in the app registry (`XAPPS.LST` / `init`), *not*
  autostarted; appears in the start menu. Command line:
  `linux:/XDOOM.ELF -iwad /FREEDOOM1.WAD -savedir /tmp/doom --client`.
- Fail soft: if the WAD is missing, show a message in-window (or exit with a
  clear serial line) instead of aborting.
- `LAZYOS_DESKTOP=1` build recipe stays unchanged unless `LAZYOS_DOOM=1`.

### D5 — CI and regression coverage

- `.github/workflows/abi-compat.yml`: add `doomboot` to the matrix (needs the
  fetched WAD, so it runs on Linux CI only, `n/a` elsewhere like `persist`).
- `.github/workflows/screenshots.yml`: add the `doom.json` session and publish
  the title screen + a demo-loop frame.
- Deterministic gameplay check: run doomgeneric's `-timedemo demo1` (Freedoom
  ships demos) and assert the final frame CRC / "gametics" line over serial,
  which doubles as a **CPU/memory soak** of the shim (millions of syscall-free
  cycles plus steady `read` traffic). Also run a loop of repeated
  start/exit of the app (spawn/exit soak) to catch fd/page leaks.
- Docs: add a `docs/architecture/` note under display or processes, and a row in
  `tools/abi/README.md`.

### D6 — Stretch (each independent)

- **Sound:** blocked on the audio driver plan (`docs/driver-plan.md`,
  `os.lazy.audio.v1` is not defined yet). Doomgeneric has a sound-module
  interface; SFX-only first, no music.
- **Mouse capture** in `xuid` (see D3).
- **Scaling/fullscreen** and vsync-aligned commits via swapchain frame-done.
- **Freedoom Phase 2 / Freedoom DM / other IWADs** through a file picker.
- **Savegames on `/data`** by pointing `-savedir` there.

## Risks

| Risk | Impact | Mitigation |
|---|---|---|
| No suitable musl C toolchain on Windows dev hosts | High friction | Docker/Alpine path (already used for BusyBox) and `zig cc`; document in `tools/doom/README.md`. Mac/Linux use host musl-gcc. |
| Display client is coupled to xui widgets | D2 slips | Extract a raw-surface client crate first; small, reviewable, keeps both apps on one path. |
| 28 MB WAD reads are slow through FAT/copy-up overlay | Long startup | Measure in D1; ext2 `/data`, larger read buffers, or `mmap`-backed lumps via `-mmap` if the shim supports file `mmap`. |
| Frame pacing: Doom expects 35 Hz and busy-waits | Wasted CPU or jerky | Implement `DG_SleepMs` with `nanosleep`; measure with `tools/bench/`. |
| Keyboard model mismatch (repeat, chords, blur) | Unplayable controls | Edge-tracking layer and blur release, D3. |
| Repo size / license | Legal or clone bloat | Fetch at build time with pinned hashes; ship `NOTICE` (GPL-2.0 engine, BSD-3 Freedoom); never commit WADs (also add `target/doom/` to ignore rules if not covered). |
| Source-file limit (<500 lines) | Review noise | Split wrapper into `main.rs`, `video.rs`, `input.rs`, `keymap.rs`, `hooks.rs`. |

## Open questions for the owner

1. Is Docker-based C building acceptable for Windows contributors, or is `zig cc` preferred?
2. Ship the WAD on the FAT volume (bigger default image) or require a `/data` disk?
3. Is a raw-surface client crate extraction from `xui-app` acceptable, or should Doom link `xui-app` directly?
4. Any appetite for a pointer-capture feature in `xuid`, or keyboard-only for now?

## Suggested issue breakdown

1. `tools/doom/build.py`: fetch + build doomgeneric and Freedoom with pinned hashes (D0, D4 part).
2. `doomboot` headless ABI fixture and shim fixes it finds (D1).
3. Extract raw-surface display client crate from `xui-app` (D2 prerequisite).
4. `lazydoom` windowed video + keymap + app registration (D2-D4).
5. CI: ABI matrix row, screenshot session, timedemo soak (D5).
