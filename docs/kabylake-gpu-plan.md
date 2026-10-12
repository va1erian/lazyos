# Kaby Lake GPU support (HD Graphics 620)

> **Status: draft, revision 2 (2026-10-12). Nothing here is built.**
> Goals: (1) mode setting, (2) audio over HDMI, (3) richer video,
> (4) hardware graphics acceleration where achievable, all on the i3-7100U
> mini PC ([kabylake-box-plan.md](kabylake-box-plan.md)).
>
> **Decisions (maintainer, 2026-10-12):** userspace `gpud` owns the hardware
> (§3.1); the box always runs at its native 1440p, so a runtime resolution
> change is low priority and the firmware's mode is *adopted*, not replaced;
> **audio first**, then **hardware H.264 decode as early as the engine
> plumbing allows**; where there is a choice, the simpler option wins;
> OpenGL/Vulkan is not a goal for now. The stage order in §4 reflects this.
>
> Confidence: facts about *this repo* and *this box* are quoted from
> [display-modeset-plan.md](display-modeset-plan.md), [compat/kabylake/](compat/kabylake/README.md),
> [architecture/display.md](architecture/display.md), [architecture/audio.md](architecture/audio.md)
> and [video-player-plan.md](video-player-plan.md). Register-level and
> engine-level statements come from memory of Intel's public programmer's
> reference manuals (PRMs, Gen9 volumes) and of Linux `i915`; each is
> **to confirm** against the PRM or a register dump from the box before code
> depends on it. Stage G0 exists to settle exactly those.

## 1. Short answer

1. **A real driver is possible and well documented.** Gen9.5 display, the
   blitter, the render engine and the media engines are all in Intel's public
   PRMs, and `i915` and Mesa are MIT-licensed, so (unlike `r8169`, which the
   Realtek plan had to avoid) we may translate from them, as `libs/rtw88`
   did under BSD-3.
2. **No firmware blob is needed for any of the four goals.** DMC (deep
   display power states) and GuC/HuC are optional; we skip all three in v1
   (DMC may be fetched later for idle power, `tools/firmware/fetch.py` style).
3. **The order follows the decisions and the dependencies**: probe → adopt
   the firmware's mode and observe it → HDMI audio (needs only the display
   side's audio enable, not a mode set) → engine plumbing (forcewake, GGTT,
   submission, reset) → hardware H.264 decode, with the NV12 video plane and
   flips alongside → zero-copy scanout, blitter, render kernels as optional
   follow-ons. Mode setting (plane/scaler, full DPLL/DDI) is deferred.
4. **"Achievable acceleration" means a compositor and video back end, not
   OpenGL.** Planes, scaler, page flips, blitter copies/fills and a small
   fixed set of hand-assembled render-engine kernels (alpha blend, YUV→RGB,
   scale) are realistic. A Mesa (`iris`) port is a stretch goal gated on
   everything before it (§5.5).
5. **There is no QEMU model of this GPU**, so unlike AHCI the work is
   verified on the box only. The plan therefore front-loads observability
   (register dumps over `dbgd`, pipe CRCs, underrun counters) and a recovery
   path that cannot leave the box blind (§6, §7).

## 2. The hardware

| Fact | Value | Source |
|---|---|---|
| Device | `8086:5916` HD Graphics 620, PCI 00:02.0, Gen9.5 | box survey |
| BARs | BAR0 `0xde000000` 16 MiB (MMIO in the lower half, GGTT in the upper), BAR2 `0xc0000000` 256 MiB (aperture) | display-modeset-plan |
| Output | one HDMI-A-1, monitor lists 2560x1440 first, 1920x1200, 1920x1080... | display-modeset-plan |
| Today | firmware GOP; loader takes the largest mode (2560x1440), kernel centres a 1080p logical screen; WC mapping on; windows laggy | boot-1 NOTES |
| Audio today | HDA `8086:9d71`, one codec "Intel Kabylake HDMI"; `sndd` reaches `SNDD:READY` (pin 3) but the display side never enabled audio, so no sound is expected | boot-1, box plan §3.3 |
| Interrupts | MSI available; I/O APIC in use | box plan §2 |
| CPU | 2C/4T, runs on CPU 0, SSE2 only, no XSAVE/AVX, no SMP | video plan §1 |
| IOMMU | VT-d present in firmware; LazyOS does not program it | box plan §2, drivers.md |

**To confirm in G0** (each changes the design):

- **How is HDMI wired?** Many Kaby Lake boards drive HDMI 2.0 through an
  *LSPCON* (DP→HDMI converter on a DDI in DP mode, needing AUX and a PCON
  mode switch), others a native TMDS HDMI 1.4 DDI. The VBT (Video BIOS Table,
  found through the ACPI OpRegion, `ASLS` in PCI config) names the DDI port,
  its DDC pin, level-shifter setting and `lspcon` flag. This decides whether
  DP AUX is in scope at all.
- Which DDI/pipe/transcoder the firmware used, its CDCLK, DPLL, DDB and
  watermark state (to reuse as a baseline, §5.1).
- Whether `sndd`'s HDMI pin has a valid ELD and presence-detect set once the
  firmware's mode is up, and whether the monitor has HDMI audio at all.
- Stolen memory (DSM) base and size (`BDSM`), where the firmware framebuffer
  sits, and the aperture vs GGTT split.

## 3. Architecture

### 3.1 The decision to make: who owns the display hardware

| | A. Userspace `gpud` owns everything, kernel gets a scanout descriptor (**recommended**) | B. Kernel owns display, `gpud` owns engines |
|---|---|---|
| Fit | Matches `netdrv`/`usbd`/`wifid`: `_gpu` uid, device grant (`map_bar`, `dma_alloc`, `irq_enable`), crash-isolated, restartable | Matches `bochs.rs` and the present path |
| Hardware state | One owner for display, GGTT, engines, power | Two owners of one PCI function; `dev/claims.rs` is one claimant per function; GGTT shared |
| Panic / boot console | Kernel keeps drawing into the last scanout descriptor it was given (base, size, stride, format) with no driver cooperation, so a panic still shows | Natural |
| Cost | New kernel op to publish a scanout; present goes through the aperture mapping | Display code in the kernel heap and syscall paths: untrusted EDID/VBT parsing in ring 0 |

**Decided: A.** `gpud` programs the display and GGTT and tells the
kernel `display::set_scanout(...)`; the kernel's `present` blits into the
aperture window (a WC window like `mem/fbwindow.rs` already makes for Bochs).
If `gpud` dies the last mode keeps scanning out and the kernel console keeps
working; `init` restarts it and it re-adopts the live state from registers.

### 3.2 Layers

```
 xuid / xui apps / video player / games
        │  os.lazy.display.v1 (existing, + GetModes/SetMode)      os.lazy.gpu.v1 (new)
        ▼                                                          ▼
   kernel display grant ◄── set_scanout ── gpud (_gpu, device grant: BAR0, BAR2, MSI, DMA)
        │ present (copy into aperture)        ├─ libs/gen9gfx: display (edid, vbt, gmbus, dpll, ddi,
        ▼                                      │   pipe, plane, wm), gtt, power/forcewake, ring/exec,
   GGTT-mapped scanout  ───────────────────────┘   blitter, render kernels, media
        │
        ▼ HDMI  ─ ELD / audio enable ─►  sndd (HDA codec side)  ◄─ os.lazy.audio.card
```

- **`libs/gen9gfx`** (`no_std`, host-tested like `libs/rtw88`): pure
  register programming and parsing behind an `Mmio` trait so a host fake can
  check sequences. Modules: `edid` (parse, fuzzed), `vbt`, `gmbus`/`aux`,
  `clock` (CDCLK check, DPLL1–3 params from pixel clock), `ddi`, `pipe`,
  `plane` (incl. YUV and scaler), `wm` (DDB/watermarks), `gtt`, `power`
  (forcewake, wells, RC6 later), `ring`/`exec`, `blit`, `render`, `media`.
- **`gpud`** (`user/src/bin/gpud/`): the machine side. Serves `os.lazy.gpu.v1`
  (`idl/gpu.midl`, MIDL as AGENTS.md demands): `GetOutputs`, `GetModes`,
  `SetMode` (with revert-on-no-ack), `Flip`, `SetCursor`, `Fill`, `Copy`,
  `Compose`, `Convert` (YUV→RGB), `Decode`. Buffers are `Buffer` objects
  (shared pages), never raw addresses.
- **Kernel**: the `display::set_scanout` op, an aperture window, and nothing
  else. `GPU:*` evidence lines through `trace::`.
- **Isolation**: apps never submit raw command batches in v1; `gpud` exposes a
  fixed operation set and builds every batch itself. Raw batches (needed
  only for Mesa) wait for per-client PPGTT (§5.5).

## 4. Stages

Sizes: S ≈ days, M ≈ 1–2 weeks, L ≈ several weeks, of focused work. Stage
ids keep their names from revision 1; the table is in **execution order**.

| Order | Stage | What | Exit criterion | Size |
|---|---|---|---|---|
| 1 | **G0** Probe | Read-only `gpu probe` over `dbgd`: PCI config (`BDSM`, `ASLS`), MMIO dump of display/pipe/plane/DDI/DPLL/CDCLK/DDB/WM/audio registers, VBT and OpRegion copy, EDID, GGTT size, engine/forcewake state. Capture the same from Linux (`intel_reg`, `i915_display_info`, `/sys/kernel/debug/dri/0`) into `docs/compat/kabylake/gpu/`. Also the `beep` precheck (§5.2). | Every "to confirm" in §2 has an answer in the repo; LSPCON vs native HDMI known; GuC/HuC/DMC confirmed unnecessary | S |
| 2 | **G1** Adopt + observe | `gpud` skeleton, claims 00:02.0, maps BAR0/BAR2, forcewake, **adopts** the firmware's mode without writing display state, snapshots it (the revert baseline, §7), publishes `set_scanout`, exposes pipe CRC, underrun and vblank counters, `GPU:*` markers, MSI → vblank counter. Opt-in switch (§8). | Desktop identical with `gpud` running at 2560x1440 (kernel logical cap raised via `display.max`); vblank ticks at the monitor's rate; CRC stable on an idle frame | M |
| 3 | **G4** HDMI audio | Display side: ELD, `AUD_CONFIG` N/CTS for 32/44.1/48 kHz, audio enable on the adopted pipe/DDI, audio infoframe, audio power domain. `sndd` side: ELD read, presence detect and unsolicited responses, pin/converter choice by the active DDI, digital stream format and channel mapping, rate negotiation from the ELD's SADs. `system/display/<n>/audio` topic (MIDL) carries `{pin, rates, channels, valid}`. | A tone and `modplay` are heard through the monitor; the recording (line-in or the monitor's output) is judged with the sound harness's detectors; monitor power-cycle and unplug mute and recover cleanly | M–L |
| 4 | **GE** Engine plumbing | The part of old G7 that decode needs: forcewake discipline, GT power wells, GGTT page insertion (kernel-allocated pages, `Buffer` objects mapped into it), HW status page, ring/execlist submission (**to confirm** which Gen9 supports for the video ring), seqno fences, interrupt on completion, engine reset (GDRST) with a per-engine watchdog and `gpud` restart. A trivial batch (`MI_STORE_DATA_IMM`, `MI_NOOP`) on the video and blitter rings proves it. | 1 M no-op/store batches soak with every seqno observed and the memory written; an injected hang is detected, the engine reset, `gpud` carries on; no effect on the CPU compose path | L |
| 5 | **G9** H.264 decode | VDBOX (MFX) H.264 first: CPU parses SPS/PPS/slice headers (reusing the video plan's V3 parser), `gpud` builds the MFX pipe/surface/state and BSD object commands, hardware does entropy decode, reconstruction and deblock into NV12 surfaces with a DPB of GGTT surfaces. `os.lazy.gpu.v1.Decode(bitstream: Buffer, params, out: Buffer)`. The player first reads the NV12 surface back and converts on the CPU (works before G5), then hands it to the plane. | The V3 golden-stream CRC matches software decode bit-exactly (Baseline/Main/High 4:2:0 8-bit, progressive first; interlaced and 10-bit are listed as unsupported, not attempted); 1080p30 plays at CPU < 15% (convert included until G5) | L–XL |
| 6 | **G5** Flip + video plane | Page flip armed at vblank with a `Flip` event carrying the vblank timestamp; an NV12 sprite plane with the plane scaler on the adopted pipe (no mode set), BT.601/709 and range flags; fullscreen hand-off from `xuid`. A/V clock from vblank timestamps. | Decoded 1080p/720p plays tear-free at the monitor's refresh, zero CPU colour convert; `VIDEO:POS` dropped = 0 | M |
| 7 | **G6** GGTT scanout (optional) | The compositor's screen buffer pages mapped into the GGTT and scanned out directly (zero-copy scanout, "S8 follow-up" in architecture/display.md); dirty-region cache flush; kernel `present` becomes flush + flip. Hardware cursor plane rides along. | latency in `perf/report.md` not worse; CPU in `present` ≈ 0 at 1440p | M |
| 8 | **G7** Blitter (optional) | `Fill`/`Copy` on BCS using GE's plumbing (`XY_COLOR_BLT`, `XY_SRC_COPY_BLT`). | 1 M-op soak equals a CPU reference byte for byte | S–M |
| 9 | **G8** Render kernels (optional) | RCS context, small batch builder, hand-assembled EU kernels: src-over blend, YUV→RGB, bilinear scale. Offloads `xuid` compose. No shader compiler. | `xuid` composes 1440p with 10 alpha windows in measurably less CPU; output within ±1/255 of software | L |
| — | **G2 / G3** Mode setting | **Deferred** (the box always runs 1440p). G2: plane size/stride + pipe scaler for hardware-scaled logical sizes (simpler, no PLL). G3: EDID, DPLL1–3, DDI/TMDS, timings, watermarks, revert-on-no-ack. Do G2 first if a lower logical resolution is ever wanted; G3 only if a different native timing is. | — | M / L |
| — | **G10** 3D / Mesa | **Not a goal** (decision 5). Kept as a note in §5.5. | — | — |

Goal coverage: audio = G1+G4; video = GE+G9+G5 (G6 helps); acceleration =
GE, G5, G6, G7, G8 as wanted; mode setting = G1 (adopt) now, G2/G3 later.

**Why decode before the blitter or render engine.** G9 needs only the
video ring, the GGTT and surface state, none of which depends on G7/G8, and
its output (NV12) is directly consumable by the G5 plane, so the whole
decode-to-glass path never touches the 3D pipeline.

## 5. Per-goal notes

### 5.1 Mode setting (deferred: G1 adopts, G2/G3 only if needed)

The box runs its native 1440p, so only the *adopt* half is on the critical
path (G1: read the live pipe/plane/DDI state, snapshot it, publish the
scanout). The notes below are the design for G2/G3 if a change is ever
wanted; they are kept so the register knowledge gathered in G0 has a home.

- **Baseline, not blank-slate.** Treat the firmware's programmed state as the
  starting point and copy what G0 shows is safe to keep: CDCLK, power wells,
  DDB layout and watermarks. Recomputing SKL-style watermarks and DDB from
  scratch is the classic source of underruns; v1 uses conservative values
  (one pipe, one plane, all levels allowed, whole DDB to pipe A) and checks
  the underrun status bit after every set.
- **Order** (**to confirm**, Gen9 modeset sequence): disable plane → pipe →
  transcoder/DDI function → buffer; program DPLL and select it for the DDI;
  program transcoder timings, pipe size, plane, scaler; enable transcoder
  function and DDI buffer with VBT translations; enable pipe; enable plane;
  infoframes; audio (G4). Reverse to disable.
- **CDCLK**: not changed in v1. A mode whose pixel clock does not fit the
  firmware CDCLK is refused with a reason, not clocked anyway (2560x1440@60
  is about 241.5 MHz; the firmware already scans it out).
- **EDID** is parsed in `libs/gen9gfx::edid` with a seeded fuzz entry point
  shared with a cargo-fuzz target, like every other parser here. Block 0 plus
  CEA extension (modes, audio SADs, speaker allocation, HDMI VSDB).
- **Single output** (HDMI-A-1) in v1; the model is `Output{ddi, pipe}` so
  more can be added, but hotplug-driven re-routing, DP, eDP and multi-head
  are out of scope.

### 5.2 HDMI audio

The codec side already exists (`libs/hda`, `sndd`: "HDMI codecs" listed as
not done in audio.md). What is missing is on both sides of the link:

- **Display side** (`gpud`): audio enable and N/CTS in the display engine's
  audio registers, ELD upload/valid bit, audio power domain, infoframes. The
  HDA controller and codec only wake up cleanly once the display driver has
  powered the audio domain (Linux gates it with a driver-to-driver binding;
  **to confirm** the Kaby Lake sequence, including the HDA link reset
  workaround).
- **Codec side** (`sndd`): the Intel HDMI codec exposes up to three
  converter/pin pairs (one per DDI); choose the pin whose ELD is valid. Read
  the ELD for rates and channels, set the digital converter format (and the
  codec's own audio infoframe/channel-allocation verbs), program stream and
  channel mapping. Handle the unsolicited "pin sense / ELD valid" event for
  unplug and monitor power cycles.
- **Interface**: `sndd` learns the active pin from a retained topic
  `system/display/<n>/audio` published by `gpud`; it never touches display
  registers, `gpud` never touches the codec. Two services, one narrow MIDL
  topic (and `sndpolicy` gets the subscription).
- **Routing**: `audiod` keeps its single-card model; the HDMI codec simply
  *is* the card on this box. Multi-sink switching is a later audio-plan item.
- **Cheap precheck in G0**: with the firmware mode up, play `beep` on the box.
  If the monitor already makes sound, G4 shrinks to the hotplug/rate work.
- **Alternative that needs no GPU**: the box has a USB audio device (boot-1),
  so a USB Audio Class driver in `usbd` also gives sound. Not part of this
  plan; noted so the owner can pick it independently.

### 5.3 Video

Software decode stays the baseline (video-player-plan V3: 720p30 at ~7% of a
core). The GPU removes the parts that do not scale on one SSE2-only core:

| Piece | Stage | Win |
|---|---|---|
| YUV plane with scaler | G5 | no CPU colour convert or scale; fullscreen is free |
| Vblank clock + flip events | G5 | tear-free, frame pacing and A/V sync against real refresh |
| Zero-copy scanout | G6 | no `present` copy for fullscreen video or games |
| Render-engine `Convert`/`Compose` for windowed video | G8 | windowed video without CPU blits |
| HW H.264 (then HEVC/VP9) decode | G9 | 1080p and beyond; the CPU has no AVX or SMP to get there |

The decoder boundary is `os.lazy.gpu.v1.Decode` taking a bitstream `Buffer`
plus parsed slice/picture parameters and returning an NV12 surface handle
that `Present` accepts directly. The CPU keeps bitstream header parsing
(reusing the V3 libavcodec parser); the MFX pipeline commands come from the
PRM and from `intel-vaapi-driver` (MIT). Container, audio, network and
YouTube stay on the video plan's V2–V6; they are independent.

### 5.4 Acceleration, honestly

| Capability | Achievable? | Notes |
|---|---|---|
| Hardware cursor, plane scale, flips, YUV planes | **yes**, G2/G5 | the biggest win per line of code |
| Zero-copy scanout | **yes**, G6 | cache coherency handled by WC or `clflush` |
| Blitter fill/copy/convert | **yes**, G7 | no alpha blending on the blitter |
| Alpha-blended compositing, scale, YUV→RGB, blur | **yes**, G8 | hand-assembled EU kernels (small, fixed set), no compiler |
| Hardware video decode (H.264/HEVC/VP9) | **yes**, G9, large | fixed-function, no GuC/HuC needed for decode |
| General 3D (OpenGL/Vulkan via Mesa) | **stretch**, G10 | needs per-client PPGTT, i915-style GEM shim, C++ (zig toolchain exists), threads; plan after G8 |
| Compute (OpenCL), video encode, HDR, multi-monitor, DP | no | out of scope |
| Power management (RC6, DC states, frequency scaling) | later | needed for idle watts, not for function; DMC then |

### 5.5 Mesa, in one paragraph

`iris` (Gen8+, includes Gen9.5) needs a DRM-ish buffer manager, execbuf with
relocation or softpin, fences, `mmap` of buffers, threads and C++ (all
present or proven: JSC and the zig C++ toolchain). The cost is the winsys,
the GEM shim over `os.lazy.gpu.v1`, and a security story: raw batches from
untrusted clients need per-client PPGTT and unprivileged batches (the GPU can
then only touch pages `gpud` mapped for that client). Value is real only if
an app wants GL/Vulkan (Blitz/vello on wgpu, a 3D app). Decision point after
G8, with the compositor numbers in hand.

## 6. Verification without a QEMU model

AGENTS.md: *do not claim a graphics change works from source alone*.

| Layer | What | How |
|---|---|---|
| Host unit | EDID/VBT parsers, DPLL parameter search (pixel clock → dividers, checked against the PRM's tables), timing math, watermark/DDB conservative rules, command-batch builders, ELD/SAD parsing | `cargo test -p gen9gfx`, seeded fuzz entry points shared with `fuzz/` |
| Sequence fake | A register-file model of the display engine that enforces the PRM's ordering rules (enable only after PLL locked, no pipe enable before transcoder timings, plane off before pipe off) and fails on violations, like `libs/hda/src/fake.rs` | in `cargo test -p gen9gfx` |
| Software render reference | Every blitter/render kernel result compared with the CPU path | host tests over recorded batches; box run for the real engine |
| Kernel suite | `set_scanout`, aperture window bounds, `present` into it, a soak of mode changes (display-grant rebinding), stress of repeated scanout re-publish | `LAZYOS_TEST_FILTER=display python tools/test/run.py --accel none`, per the kernel-component rule |
| Box, observable | `GPU:*` markers over `dbgd`; pipe **CRC** registers (compare a known test pattern run-to-run and against a golden per mode); FIFO underrun and vblank counters; register diffs against Linux's dump | `python tools/gpu/run.py` driving the box through `dbgctl` (build, `app-install`/`reload gpud`, set mode, read CRC, judge); `tools/gpu/test_judge.py` self-test |
| Box, by eye/ear | Screenshots of the box by the person, HDMI capture or line-in WAV for audio; recorded in `docs/compat/kabylake/gpu/` like boot-1 | manual, noted as such |

QEMU still has a role for the *non-hardware* half: `set_scanout`, the
display-grant ops and `xuid` re-binding run against Bochs/VGA in CI so the
kernel and compositor side of a mode change is covered without the box.

## 7. Safety and recovery

The failure mode is a dark screen on a machine with no serial port.

- **Opt-in.** `LAZYOS_GEN9GFX=1` / `display.gen9=1` in `lazyos.cfg`; off by
  default until G3 has soaked on the box. `devd` matches `8086:5916` (and
  the Skylake/Coffee Lake ids once confirmed) only when on.
- **Revert timer.** `SetMode` returns after the mode is up and starts a
  15 s timer; `xuid` (or Settings) must `Confirm`. Unconfirmed → restore the
  snapshot of the firmware's register state taken at adopt time (G1).
- **Boot-pending flag.** `gpud` sets `sys/display/gpu/boot_pending` in `confd`
  before the first modeset of a boot and the compositor clears it after its
  first frame. If a boot starts with it set, `gpud` stays in adopt-only
  mode (no writes) and logs why.
- **Remote eyes.** Everything above is observable over `dbgd` on Wi-Fi or
  Ethernet (both work on the box); `dbgctl reload gpud` swaps the driver
  without a reboot.
- **No ring 0 parsing.** EDID, VBT and ELD are parsed in `gpud`, not the
  kernel, and are fuzzed.
- **GPU hang.** Watchdog per engine, engine reset, `gpud` restart; a hung
  render engine must never stall `xuid`'s CPU path (it falls back to
  software compose).
- **IOMMU.** The GPU is a bus master under a trusted driver (the same trust
  level drivers.md states); VT-d for it is issue #615's territory. Until
  per-client PPGTT exists, no untrusted batch reaches the GPU.

## 8. Project conventions checklist

Per AGENTS.md, a feature is not done until each is true:

- **Switches and launchers**: `LAZYOS_GEN9GFX=1` in `build.rs`/`build_support`,
  `run_demo.py --gen9gfx`, a control on the GUI's Advanced tab
  (`tools/lazygui/catalog.py` `build_env`/`build_plan`, tests in
  `test_catalog.py`). It is a service, not an app, so no core package; if a
  *Display* page is added to Settings, it is an addition to the existing app.
- **Messenger**: `idl/gpu.midl` and the display-audio topic generated by
  `midlc`; no hand-written TLVs; `LAZYOS_LABEL_TRACE=1` run to derive the
  `_gpu` label's permissions and `messengerd` `system/` gating (as `sndpolicy`).
- **Paths and files**: new well-known paths from `libs/fhs`; files under 500
  lines (`libs/gen9gfx` is split by block precisely for this).
- **Tests**: correctness and soak per kernel component (§6); `trace::`
  channels in `docs/trace/channels.md` (`python tools/trace/catalog.py`).
- **Licences**: translated code keeps its MIT notice and a `THIRD_PARTY.md`
  row; the PRM and any fetched firmware (DMC, later) are never committed,
  `tools/firmware/manifest.txt` pins hashes.
- **Docs**: `architecture/display.md`, `architecture/drivers.md`,
  `architecture/audio.md` and `compat/hardware.md` updated per stage.

## 9. Decisions and what remains open

Decided (2026-10-12):

1. **Ownership**: userspace `gpud` (§3.1).
2. **Resolution change**: not a priority; the box runs 1440p permanently, so
   the firmware's mode is adopted and G0b (software scale / loader exact
   mode) is dropped from this plan (it stays in display-modeset-plan.md).
3. **Mode setting**: the simpler path, i.e. none for now; G2 before G3 if it
   returns.
4. **Hardware decode**: as early as possible, after audio, via GE then G9.
5. **3D**: not a goal.

Still open (answer at G0/G1, none blocks starting):

- **Display-surface choice for the player**: plane (G5) from the start, or
  read-back and CPU convert first (works the day G9 lands)? The plan does
  the latter then the former.
- **Kernel logical cap**: the box shows a centred 1080p today; run at the
  full 1440p (`display.max=2560x1440`, ~14.7 MiB per RGBA surface, three
  surfaces by default) or keep 1080p logical until G6 removes the copy?
- **Codecs after H.264**: HEVC 8-bit and VP9 are in the same engine; add
  only if a source needs them.
- **One box**: every stage is a flash, test, observe loop on the single
  machine; a second Gen9 machine would be useful for G0 comparisons but is
  not required.
