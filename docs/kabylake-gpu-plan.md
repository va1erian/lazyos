# Kaby Lake GPU support (HD Graphics 620)

> **Re-scoped 2026-10-17: the goal is the whole OS at 60 fps at 1440p, LazyWeb+ first (§11, with a measured baseline). Read §11 before §4: it changes the order.**
>
> **Status: draft, revision 4 (2026-10-13). The Linux-side G0 survey ran on the box and settled most of §2 (§2.1); the hardware-independent prep is built (survey script, `libs/gen9gfx` parsers, now also tested against the box's real EDID and VBT; §10). No kernel, `user/` or `idl/` code yet.**
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
> **Revision 3** folds in review comments: stage ids renumbered to execution
> order, G2 (audio) acknowledged as a live-pipe write with its own safety net,
> engine-submission and decode risk made explicit, logical cap decided.
>
> **Revision 4** folds in the G0 survey of the box under Linux
> ([compat/kabylake/gpu/](compat/kabylake/gpu/README.md)): HDMI is native TMDS
> on DDI B with no LSPCON and no AUX, so DP AUX is out of scope; the sink is a
> 2-channel LPCM, HDMI 1.x (280 MHz TMDS, no Forum block) monitor; Gen9 submits through execlists
> (G3 is bigger than rev 3 assumed); DMC, GuC and HuC stay unnecessary. What the
> survey could **not** settle is listed at the end of §2.1: it captured i915's
> state, not the firmware's, and missed the audio and DDI-buffer registers.
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
| CPU | i3-7100U: 2C/4T with AVX2. **LazyOS** runs on CPU 0 only, SSE2 only (no XSAVE/AVX, no SMP) | video plan §1 |
| IOMMU | VT-d present in firmware; LazyOS does not program it | box plan §2, drivers.md |

### 2.1 What the G0 survey settled (Linux, 2026-10-13)

Source: [compat/kabylake/gpu/linux/](compat/kabylake/gpu/README.md), captured by
`tools/boot/survey_gpu.sh` on the box while Linux's i915 drove the monitor with
the HDMI PCM open. The EDID serial and the hostname are redacted; the EDID,
VBT and OpRegion are in the tree as binaries and `libs/gen9gfx` tests them.

| Question | Answer | Where |
|---|---|---|
| How is HDMI wired? | **Native TMDS HDMI on DDI B**, no LSPCON, no DP AUX channel. VBT BDB 209 (Intel SKL/KBL BIOS build of 2017-04-28): child device `0x60D2`, DVO port HDMI-B, DDC on GMBUS pin 5, level shifter `0x08`, HDMI iboost level `2` with `Iboost enable: no`, lane reversal no. A second HDMI child (DDI C, DDC pin 4) exists with nothing attached (`HDMI-A-2` disconnected). **DP AUX is out of scope.** | `03-intel_vbt_decode.txt`, `03-dri128-vbt.bin`, `tests_real` |
| Pipe, transcoder, DDI, PLL | Pipe A, transcoder A, DDI B, `DPLL1` (HDMI mode), `DPLL_CTRL2` overrides DDI B to PLL1, `TRANS_CLK_SEL_A` = DDI B. | `01-intel_reg-dump.txt`, `regs.rs` tests |
| Link mode, infoframes | `TRANS_DDI_FUNC_CTL_A` = `0x90030000`: enabled, **HDMI mode**, 8 bpc, +HSync +VSync. `VIDEO_DIP_CTL_A` = `0x11001`: GCP, AVI and SPD infoframes on. (Linux's state; see the open list below.) | `01-intel_reg-dump.txt` |
| Mode on the wire | 2560x1440 **at 70 Hz, 277.82 MHz** (not the 241.5 MHz / 59.95 Hz preferred DTD). Sink limit: HDMI VSDB max TMDS **280 MHz**, no HDMI Forum block, AMD vendor block 48 to 70 Hz. 277.82 MHz is the fastest 1440p timing the EDID lists; the range limits (50 to 76 Hz, 280 MHz dot clock) would allow a custom one, which LazyOS does not need. | `i915_display_info`, `04-*-edid-decode.txt` |
| CDCLK | **337.5 MHz** (`CDCLK_CTL` = `0x080002A1`, LCPLL1 locked), above the 277.82 MHz pixel clock. | `01-intel_reg-dump.txt` |
| Display buffer | Plane 1A: 0 to 856 blocks (`PLANE_BUF_CFG_1_A` `0x03570000`), cursor 856 to 892; i915's view has pipes B and C empty, but the raw registers of the disabled pipe B still read `PLANE_BUF_CFG_1_B` = `0x037B01BE` (446 to 892, overlapping pipe A's cursor) with watermarks `0x4007`: adoption reads pipe A only and does not trust an idle pipe's registers. 2 scalers on pipes A and B, 1 on C. | `i915_ddb_info`, `i915_display_info`, `01-intel_reg-dump.txt` |
| Memory map | `BDSM` = `0xBE000001`: stolen base `0xBE000000`, **32 MiB** (`GMCH_CTRL` `0x01C1`: GMS 32 MiB, GGMS 8 MiB). BAR0 `0xDE000000` 16 MiB = 8 MiB registers + 8 MiB GGTT (1 M PTEs, 4 GiB of GGTT space); BAR2 `0xC0000000` 256 MiB aperture. `ASLS` = `0xBCFB1018` (OpRegion; its mailbox 4 holds the VBT at +0x400: a 6144-byte mailbox with a 4300-byte VBT, BDB 4252 bytes). | `00-gpu-config-words.txt`, `00-iomem-gpu.txt` |
| Firmware blobs | **None needed for the four goals.** i915 loaded DMC `kbl_dmc_ver1_04` only; no GuC or HuC (`enable_guc` default, execlists submission). `DC_STATE_EN` is 0 and DC3→DC5 count 0 even with DMC. | `06-dmesg-*`, `i915_dmc_info`, `00-i915-params.txt` |
| Command submission | **Execlists on all four engines** (`rcs0` `0x2000`, `vcs0` `0x12000`, `bcs0` `0x22000`, `vecs0` `0x1A000`): ELSP/CSB, a per-context image, HWSP, per-context PPGTT in use. One VDBOX (`vcs0`), one VEBOX: GT2, one video decode engine. | `i915_engine_info`, `i915_gem_framebuffer` |
| Audio codec | `8086:9D71` HDA, one codec at address 2, `Intel Kabylake HDMI` (`8086:280B`): AFG nid 1, converters nid 2 to 4, **pins nid 5 to 7 = DDI B, C, D**, pin 5's connection list names the three converters (pins 6 and 7, with no monitor, read `Connection: 0`, so whether theirs do too was not observed); Pin Default `0x18560010`, pincap `0x0B000094` (out, detect, HBR, HDMI, DP). Linux used **pin 5 → converter 2** (stream 1, "HDMI 0", PCM device 3). | `05-codecs.txt`, `05-eld.txt`, `05-aplay-cards.txt` |
| Sink audio | ELD valid, `monitor_present`: **one SAD, LPCM, 2 channels, 32/44.1/48 kHz, 16/20/24 bit**, speakers FL/FR, no AI, no HDCP, `eld_version` CEA-861D. No multichannel, no 88.2 kHz and up, no compressed formats. | `05-eld.txt`, EDID audio block |
| Audio power | `AUDIO_MMIO` and `AUDIO_PLAYBACK` power domains each held once while the HDMI PCM was open (`snd_hda_intel` had bound the GPU's audio component); which reference is the PCM's and which the bind's is not shown. | `i915_power_domain_info`, `06-dmesg-gpu-audio.txt` |
| Idle baseline (plan §7) | GT 300 MHz (RPn) of 1000 MHz (RP0); RC6 working; package 49 °C, PCH 39.5 °C, ACPI zones 28 and 30 °C, with Linux's desktop running. | `07-*.txt`, `i915_frequency_info` |

The two modes this box can run, and what that does to G2's N/CTS (computed
with `libs/gen9gfx::hdmi_audio`, 8 bpc so TMDS = pixel clock). Neither clock
is in i915's audio table, so there is no table N to reuse:

| Pixel clock | 32 kHz | 48 kHz | 44.1 kHz |
|---|---|---|---|
| 241.5 MHz (59.95 Hz) | N 4096, CTS 241500 (exact) | N 6144, CTS 241500 (exact) | default N 6272 gives CTS 268333.33; **N 6384, CTS 273125 is exact** |
| 277.82 MHz (70 Hz) | N 4096, CTS 277820 (exact) | N 6144, CTS 277820 (exact) | default N gives CTS 308688.9; **N 7056, CTS 347275 is exact** |

**Not settled; each needs a LazyOS-side read before any write (G1, step 2 of §10):**

- **What the firmware leaves.** i915 re-programmed (or inherited) the display
  before the capture, so every register above is *Linux's* state. Whether the
  firmware's link is HDMI or DVI mode, which timing it scans out (241.5 or
  277.82 MHz), its CDCLK and its framebuffer address are read by LazyOS's own
  `gpu probe` on a boot where nothing has written yet. Until then G2's
  "partial mode set on a DVI link" risk (§4) stays.
- **The audio registers.** `intel_reg dump` does not list `AUD_CONFIG`,
  `AUD_PIN_ELD_CP_VLD`, the ELD buffer or `AUD_MISC_CTRL`, and
  `02-mmio-ranges.txt` failed (`mmap` of the BAR resource gave `EINVAL`), so
  the audio offsets in `libs/gen9gfx::regs` are still **to confirm**, as are
  `DDI_BUF_CTL` and the DDI buffer translations. `survey_gpu.sh` now captures
  the ranges with `intel_reg read` and needs one more run (§10).
- **Why `sndd` saw "pin 3".** boot-1's `SNDD:HDA codec=2 afg=1 pin=3` names a
  nid that is a *converter* in Linux's graph (pins are 5 to 7), and
  `libs/hda` only returns a widget whose type is pin. Two explanations fit and
  the evidence does not separate them: the codec answered from an unpowered
  audio domain before any display driver ran (§5.2), or the graph walk reads
  the wrong nids. Have `sndd` log the whole widget list (nid, caps, type) on
  the next boot, and again after G2 powers the domain, before trusting either.

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
Until G6 the present path is the same CPU copy into write-combining memory
the box already uses and finds laggy, so this design does not by itself
improve responsiveness. The scanout pages' lifetime belongs to the kernel
(it allocated them for the descriptor); `gpud` may add GGTT entries for
them but a rewrite or restart never frees them under the console. If `gpud` dies the last mode keeps scanning out and the kernel console keeps
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
  check sequences. Modules: `edid` (parse, fuzzed), `vbt`, `gmbus` (EDID over DDC; DP `aux` is out of scope here),
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
ids are in execution order (G = GPU stages, M = deferred mode-setting stages, so they cannot be confused with the options of display-modeset-plan.md).

| # | Stage | What | Exit criterion | Size |
|---|---|---|---|---|
| 1 | **G0** Probe | Read-only `gpu probe` over `dbgd`: PCI config (`BDSM`, `ASLS`), MMIO dump of display/pipe/plane/DDI/DPLL/CDCLK/DDB/WM/audio registers, VBT and OpRegion copy, EDID, GGTT size, engine/forcewake state. Capture the same from Linux (`intel_reg`, `i915_display_info`, `/sys/kernel/debug/dri/0`) into `docs/compat/kabylake/gpu/`. Also the `beep` precheck (§5.2). **Linux half done (§2.1); the LazyOS half and the audio-register re-run remain.** | Every "to confirm" in §2 has an answer in the repo; LSPCON vs native HDMI known (**yes: native**); GuC/HuC/DMC confirmed unnecessary (**yes**); the firmware's own link mode, timing and framebuffer read by LazyOS before any write | S (rest) |
| 2 | **G1** Adopt + observe | `gpud` skeleton, claims 00:02.0, maps BAR0/BAR2, forcewake, **adopts** the firmware's mode without writing display state, snapshots it (the revert baseline, §7), publishes `set_scanout`, exposes pipe CRC, underrun and vblank counters, `GPU:*` markers, MSI → vblank counter, idle temperature/power readings as the baseline (§7). Groundwork for G2 and G3 (claims, forcewake, vblank, snapshot), not a user-visible change. Opt-in switch (§8). | Desktop identical with `gpud` running at 2560x1440 (`display.max=2560x1440` set in `lazyos.cfg`; it is boot-time and refused once a compositor binds); vblank ticks at the monitor's rate; CRC stable on an idle frame | M |
| 3 | **G2** HDMI audio | **Writes a live pipe**: if the firmware left the link in DVI mode, G2 changes `TRANS_DDI_FUNC_CTL`/data-island/infoframe state, a partial mode set that can blank the screen, so it runs under §7's snapshot, revert and boot-pending rules. Neither 241.5 MHz (2560x1440@59.95) nor 277.82 MHz (@70) is a standard HDMI audio clock, so N/CTS is computed (§2.1 has the exact pairs), not looked up; 32 and 48 kHz are exact at the default N, 44.1 kHz needs a non-default N or fractional hardware CTS. Display side: ELD, `AUD_CONFIG` N/CTS for 32/44.1/48 kHz, audio enable on **DDI B** (pipe A on the adopted state), audio power domain. **The sink is 2-channel LPCM only (32/44.1/48 kHz, 16/20/24 bit)**, so there is no channel-allocation or multichannel work and no rate above 48 kHz to negotiate. `sndd` side: ELD read, presence detect and unsolicited responses, pin 5 / converter choice by the active DDI, digital stream format, rate negotiation from the ELD's SADs. The audio registers' offsets are still **to confirm** (§2.1: the survey missed them), so the first G2 task is reading them over `dbgd`. `system/display/<n>/audio` topic (MIDL) carries `{pin, rates, channels, valid}`. | A tone and `modplay` are heard through the monitor; the recording (line-in or the monitor's output) is judged with the sound harness's detectors; monitor power-cycle and unplug mute and recover cleanly | M–L |
| 4 | **G3** Engine plumbing | The shared engine plumbing (also G7's base) that decode needs: forcewake discipline, GT power wells, GGTT page insertion (kernel-allocated pages, `Buffer` objects mapped into it), HW status page, **execlist submission** (settled by the survey: i915 submits to every Gen9 engine, `vcs0` included, through ELSP and the context status buffer; there is no GuC), logical-ring context images, HWSP, seqno fences, interrupt on completion, engine reset (GDRST) with a per-engine watchdog and `gpud` restart. A trivial batch (`MI_STORE_DATA_IMM`, `MI_NOOP`) on the video and blitter engines proves it. **Risk:** an execlist needs a valid context image (register state page, ring, PPGTT root pointers) before anything runs, which is most of the size of this stage (the legacy ring mode may still run on this silicon, but i915 no longer uses it, so that is a research option, not the plan); a reset needs the right wake domain held. **Fallback:** if VCS submission cannot be made reliable, stop after G1, G2 and G5 (audio, plane, flips) and keep software decode. | 1 M no-op/store batches soak with every seqno observed and the memory written; an injected hang is detected, the engine reset, `gpud` carries on; no effect on the CPU compose path | L |
| 5 | **G4** H.264 decode | VDBOX (MFX) H.264 first: CPU parses SPS/PPS/slice headers (reusing the video plan's V3 parser), `gpud` builds the MFX pipe/surface/state and BSD object commands, hardware does entropy decode, reconstruction and deblock into NV12 surfaces with a DPB of GGTT surfaces. `os.lazy.gpu.v1.Decode(bitstream: Buffer, params, out: Buffer)`. **De-risk first:** one intra-only Baseline I-frame, NV12 output compared with software, gates everything else (the MFX VLD command set, DPB and per-slice state have no QEMU model and no reference on this box beyond `intel-vaapi-driver`'s source). The player first reads the NV12 surface back and converts on the CPU (works before G5), then hands it to the plane. | The V3 golden-stream CRC matches software decode bit-exactly (Baseline/Main/High 4:2:0 8-bit, progressive first; interlaced and 10-bit are listed as unsupported, not attempted); 1080p30 plays at CPU below the measured software 1080p30 figure (taken in the guest first; the video plan's 7% is 720p30 on a host; convert included until G5) | L–XL |
| 6 | **G5** Flip + video plane | Page flip armed at vblank with a `Flip` event carrying the vblank timestamp; an NV12 sprite plane with the plane scaler on the adopted pipe (no mode set), BT.601/709 and range flags; fullscreen hand-off from `xuid`. A/V clock from vblank timestamps. | Decoded 1080p/720p plays tear-free at the monitor's refresh, zero CPU colour convert; `VIDEO:POS` dropped = 0 | M |
| 7 | **G6** GGTT scanout (optional) | The compositor's screen buffer pages mapped into the GGTT and scanned out directly (zero-copy scanout, "S8 follow-up" in architecture/display.md); dirty-region cache flush; kernel `present` becomes flush + flip. Hardware cursor plane rides along. | latency in `perf/report.md` not worse; CPU in `present` ≈ 0 at 1440p | M |
| 8 | **G7** Blitter (optional) | `Fill`/`Copy` on BCS using G3's plumbing (`XY_COLOR_BLT`, `XY_SRC_COPY_BLT`). | 1 M-op soak equals a CPU reference byte for byte | S–M |
| 9 | **G8** Render kernels (optional) | RCS context, small batch builder, hand-assembled EU kernels: src-over blend, YUV→RGB, bilinear scale. Offloads `xuid` compose. No shader compiler. | `xuid` composes 1440p with 10 alpha windows in measurably less CPU; output within ±1/255 of software | L |
| — | **M1 / M2** Mode setting | **Deferred** (the box always runs 1440p). M1: plane size/stride + pipe scaler for hardware-scaled logical sizes (simpler, no PLL). M2: EDID, DPLL1–3, DDI/TMDS, timings, watermarks, revert-on-no-ack. Do M1 first if a lower logical resolution is ever wanted; M2 only if a different native timing is. | — | M / L |
| — | **G9** 3D / Mesa | **Not a goal** (decision 5). Kept as a note in §5.5. | — | — |

Goal coverage: audio = G1+G2; video = G3+G4+G5 (G6 helps); acceleration =
G3, G5, G6, G7, G8 as wanted; mode setting = G1 (adopt) now, M1/M2 later.

**Why decode before the blitter or render engine.** G4 needs only the
video ring, the GGTT and surface state, none of which depends on G7/G8, and
its output (NV12) is directly consumable by the G5 plane, so the whole
decode-to-glass path never touches the 3D pipeline.

## 5. Per-goal notes

### 5.1 Mode setting (deferred: G1 adopts, M1/M2 only if needed)

The box runs its native 1440p, so only the *adopt* half is on the critical
path (G1: read the live pipe/plane/DDI state, snapshot it, publish the
scanout). The notes below are the design for M1/M2 if a change is ever
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
  infoframes; audio (G2). Reverse to disable.
- **CDCLK**: not changed in v1. A mode whose pixel clock does not fit the
  firmware CDCLK is refused with a reason, not clocked anyway. Under Linux the
  box ran CDCLK 337.5 MHz with a 277.82 MHz pixel clock (2560x1440@70, the
  sink's 280 MHz TMDS ceiling), so 1440p at 59.95 or 70 Hz both fit; the
  firmware's own CDCLK is read in G0's LazyOS half.
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
  converter/pin pairs (one per DDI); choose the pin whose ELD is valid. On
  this box that is **pin nid 5 (DDI B)**; pins 6 and 7 (DDI C, D) have no
  monitor and report `eld_valid 0`. Converters are nids 2 to 4; pin 5 lists
  all three (Linux used converter 2; the other pins' lists were empty in the
  dump). Read
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
  If the monitor already makes sound, G2 shrinks to the hotplug/rate work.
- **Alternative that needs no GPU**: the box has a USB audio device (boot-1),
  so a USB Audio Class driver in `usbd` also gives sound. Not part of this
  plan; noted so the owner can pick it independently.

### 5.3 Video

Software decode stays the baseline (video-player-plan V3: 720p30 at ~7% of a
core). The GPU removes the parts that do not scale on LazyOS's one SSE2-only core:

| Piece | Stage | Win |
|---|---|---|
| YUV plane with scaler | G5 | no CPU colour convert or scale; fullscreen is free |
| Vblank clock + flip events | G5 | tear-free, frame pacing and A/V sync against real refresh |
| Zero-copy scanout | G6 | no `present` copy for fullscreen video or games |
| Render-engine `Convert`/`Compose` for windowed video | G8 | windowed video without CPU blits |
| HW H.264 (then HEVC/VP9) decode | G4 | 1080p and beyond without waiting for LazyOS to gain XSAVE/AVX2 and SMP (the video plan's next software step, roughly 1.5-2.5x); also CPU headroom and power |

The decoder boundary is `os.lazy.gpu.v1.Decode` taking a bitstream `Buffer`
plus parsed slice/picture parameters and returning an NV12 surface handle
that `Present` accepts directly. The CPU keeps bitstream header parsing
(reusing the V3 libavcodec parser); the MFX pipeline commands come from the
PRM and from `intel-vaapi-driver` (MIT). Container, audio, network and
YouTube stay on the video plan's V2–V6; they are independent.

### 5.4 Acceleration, honestly

| Capability | Achievable? | Notes |
|---|---|---|
| Plane scale, flips, YUV planes (G5); hardware cursor (G6) | **yes** | the biggest win per line of code |
| Zero-copy scanout | **yes**, G6 | cache coherency handled by WC or `clflush` |
| Blitter fill/copy/convert | **yes**, G7 | no alpha blending on the blitter |
| Alpha-blended compositing, scale, YUV→RGB, blur | **yes**, G8 | hand-assembled EU kernels (small, fixed set), no compiler |
| Hardware video decode (H.264/HEVC/VP9) | **yes**, G4, large | fixed-function, no GuC/HuC needed for decode |
| General 3D (OpenGL/Vulkan via Mesa) | **not a goal**, G9 | needs per-client PPGTT, i915-style GEM shim, C++ (zig toolchain exists), threads; plan after G8 |
| Compute (OpenCL), video encode, HDR, multi-monitor, DP | no | out of scope |
| Power management (RC6, DC states, frequency scaling) | later | not needed for function. DC states are off even with DMC loaded (`DC_STATE_EN` 0, DC3→DC5 count 0 under Linux), so running without DMC loses nothing the box was using; a busy engine still runs hotter, so G1 and G3 record idle temperature/power over `dbgd` as a baseline |

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
  default until G2 (audio) and G5 (flips) have soaked on the box, the first
  stages that write display state. `devd` matches `8086:5916` (and
  the Skylake/Coffee Lake ids once confirmed) only when on.
- **Revert timer.** Any write to a live pipe (G2's HDMI-mode/audio enable,
  G5's plane enable, M1/M2 `SetMode` if built) starts a 15 s timer; the
  compositor (or a `gpu` CLI) must `Confirm`. Unconfirmed → restore the
  snapshot of the firmware's register state taken at adopt time (G1).
- **Boot-pending flag.** `gpud` sets `sys/display/gpu/boot_pending` in `confd`
  before the first display-state write of a boot and the compositor clears it after its
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
- **GGTT entries.** Only pages `gpud` was granted (`dma_alloc`, `Buffer`
  objects) may be inserted; a `gpud` bug is a ring-0-equivalent write with no
  IOMMU and the kernel cannot validate entries, so the panic path never
  depends on GGTT state (it uses the descriptor's aperture window).
- **`gpud` restart mid-batch.** Re-adoption reads display registers back, but
  engine state and GGTT contents are lost: the new instance resets every
  engine first, drops in-flight decode (the player sees an error and
  re-opens), and re-publishes the scanout.
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
   the firmware's mode is adopted and the no-GPU options of
   display-modeset-plan.md (software scale, loader exact mode) are not part of
   this plan.
3. **Mode setting**: the simpler path, i.e. none for now; M1 before M2 if it
   returns.
6. **Logical cap**: run the full 1440p (`display.max=2560x1440`, ~14.7 MiB per
   RGBA surface, three by default), as the box is always used that way.
4. **Hardware decode**: as early as possible, after audio, via G3 then G4.
5. **3D**: not a goal.

Still open (answer at G0/G1, none blocks starting):

- **Display-surface choice for the player**: plane (G5) from the start, or
  read-back and CPU convert first (works the day G4 lands)? The plan does
  the latter then the former.
- **Codecs after H.264**: HEVC 8-bit and VP9 are in the same engine; add
  only if a source needs them.
- **Which timing is "the box's mode"**: Linux ran 2560x1440 at 70 Hz
  (277.82 MHz); the monitor's preferred DTD is 59.95 Hz (241.5 MHz). The plan
  adopts whatever the firmware scans out (§2.1's last list), so the answer
  only matters for G2's N/CTS pair and for how much headroom the 280 MHz TMDS
  ceiling leaves (2.18 MHz at the listed 70 Hz timing).
- **One box**: every stage is a flash, test, observe loop on the single
  machine; a second Gen9 machine would be useful for G0 comparisons but is
  not required.

## 10. Status and next steps

**Status (2026-10-13): G1's read-only skeleton is built (`gpud`, `idl/gpu.midl`, `libs/gen9gfx::adopt`, switch `LAZYOS_GEN9GFX=1`, `run_demo.py --gen9gfx`); it has not run on the box yet, and no QEMU model of the GPU exists.**

Hardware-independent work and the Linux-side G0; the G0 prep itself touches
neither the kernel, `user/`, `idl/` nor `build.rs`.

- **G0, Linux side: done once, with two gaps.** `tools/boot/survey_gpu.sh`
  ran on the box on 2026-10-13 and its output is in
  `docs/compat/kabylake/gpu/linux/` (§2.1 reads it). Gaps: the raw register
  ranges failed (`02-mmio-ranges.txt`: `mmap` of the BAR resource gave
  `EINVAL` while `intel_reg` worked) and `intel_reg dump` does not name the
  audio or DDI-buffer registers. The script now reads its ranges with
  `intel_reg read`, adds the forcewake request and acknowledge registers, and
  names debugfs files without `:` (the first capture's `dri0000:00:02.0-*`
  files cannot be checked out on Windows; `dri1` and `dri128` were symlink
  duplicates of it, so the tree keeps one copy, named
  `03-dri0000_00_02.0-*` as the script now writes it, and a re-run
  overwrites it in place). **One more run on the box fills both gaps.**
- **`libs/gen9gfx`** (`cargo test -p gen9gfx`; seeded fuzz shared with
  `fuzz/fuzz_targets/gen9gfx.rs`): `Mmio` trait, `edid` and `cea` (base block,
  CEA extension: timings, VICs, SADs, speaker allocation, HDMI and HDMI Forum
  vendor blocks), `eld` (parse, build from an EDID), `vbt` (header, block
  walk, child devices: DDI port, DDC pin, HDMI/DVI/DP/DP++, LSPCON),
  `hdmi_audio` (N/CTS table and computed form), and `regs`. New in rev 4:
  `tests_real` runs the parsers over the box's **real** EDID, VBT and
  OpRegion (DDI B native HDMI, DDC pin 5, no LSPCON, 2560x1440 preferred
  timing, 280 MHz TMDS cap, 2-channel LPCM ELD equal to the codec's), and
  `regs` marks which offsets the dump **confirmed** (plane, CDCLK, DPLL,
  `TRANS_DDI_FUNC_CTL`, transcoder timing) against which are still **to
  confirm** (audio block, DDI buffers) and decodes the box's real register
  values in its tests (CDCLK 337.5 MHz, DDI B, HDMI mode).
- **A real-fixture lesson**: the first redacted EDID was 257 bytes and failed
  `tests_real` at once. Keep the redaction recipe in
  `docs/compat/kabylake/gpu/README.md` (serial bytes 12 to 15 and the 0xFF
  descriptor, then fix the checksum) and let `cargo test -p gen9gfx` judge any
  new capture.
- **Finding for G2**: N/CTS for both modes the box can run is in §2.1: 32 and
  48 kHz are exact at the default N; 44.1 kHz needs N 6384 (241.5 MHz) or
  N 7056 (277.82 MHz) for an integer CTS, or fractional hardware CTS at the
  default N. Programming an exact N, or checking that hardware CTS auto mode
  accepts the default, is a G2 decision (the audio registers are the missing
  input, below).

**Next, in order:**

1. **Read what the firmware leaves, on a LazyOS boot, with the dbgd ops that
   already exist.** `pci.config` and `mmio.read` landed in #829
   ([dbgd.md](dbgd.md), `libs/pciprobe`): build the box's image with
   `run_demo.py --dbgd --dbgd-mmio`, boot it, and
   `dbgctl.py mmio 00:02.0 0 <offset> <len>` the registers of §2.1's last list
   (`TRANS_DDI_FUNC_CTL_A`, the transcoder timings, `CDCLK_CTL`,
   `PLANE_SURF_1_A`, the audio block). The built-in HD 620 allowlist already
   covers all of them except `0x51000` (DFSM fuses) and `0x130040` (the blitter
   forcewake ack), which `survey_gpu.sh` also reads under Linux; add them to
   `libs/pciprobe/src/builtin.rs` when G3 needs the latter. The audio block
   sits behind a power well: read `0x45400` first (a gated well may not return
   cleanly), and the ELD data ports (`0x65050`, `0x65150`, `0x65250`) stay out
   as deliberate holes. A small `tools/gpu/probe.py` that runs the whole read
   list and writes `docs/compat/kabylake/gpu/lazyos/` would make it
   repeatable and diffable against the Linux capture. Nothing writes: this is
   the "before anything writes" baseline G1 and G2 need.
2. **Re-run `tools/boot/survey_gpu.sh` on the box** (with `intel-gpu-tools`
   installed and HDMI audio playing, as the README says), redact, commit over
   `docs/compat/kabylake/gpu/linux/`. That settles the audio offsets and
   `DDI_BUF_CTL` *as Linux uses them while playing*; then correct `regs.rs`
   (move the audio block above the `TO CONFIRM` marker or fix the offsets).
3. **G1**: the `gpud` skeleton (`user/src/bin/gpud/`, `idl/gpu.midl`, the
   `LAZYOS_GEN9GFX` switch and launcher flags of §8, the device claim for
   00:02.0, then `set_scanout` in the kernel). Its first job is the same read
   as step 1, decoded with `libs/gen9gfx::regs` and reported as `GPU:*`
   markers.

## 11. Re-scoped goal: 60 fps at 1440p, LazyWeb+ first (2026-10-17)

**The goal changed** (maintainer, 2026-10-17): the whole OS at 60 fps on the
box's native 2560x1440, with **LazyWeb+ (WebKit) the primary target**. 3D,
video decode, audio and mode setting are out of scope. That drops G2, G4 and
M1/M2 from the critical path and leaves G1, G5 (flips and vblank, without the
video plane), G6 (zero-copy scanout, hardware cursor) and, only if the
numbers ask for it, G3/G7/G8. The order below is by what is **measured**.

### 11.1 Measured baseline (QEMU, WHPX, one run each; not the box)

`python tools/webplus/frame_run.py --perf` builds LazyWeb+ at 2560x1440
(scale 2: a 1280x720 layout), loads a long text page
(`webplus/scroll.html`, 600 lines, no script), turns the wheel for 25 s and
reads the guest's own counters. The window is 2000x900 pixels (1.8 Mpx).

| Measured | Value |
|---|---|
| Paints | 22.9 a second, **every one the whole window** (`px` = 1.80 Mpx each) |
| CPU, of the one core | LazyWeb+ UI process 44 to 46%, `xuid` 14%, WebProcess 6%: about **28 ms of CPU per frame** all told |
| UI process, paint thread (`INFO:PAINT`) | `WKPagePaint` 5.5 to 5.8 ms, conversion 0.7 to 0.9, hand-over 1.2 to 1.5: 8 to 10 ms a paint |
| Wheel to first damage report | 15 ms average, 75 ms worst |
| Kernel present copy (`PERF:present_mpx`) | 437 µs per Mpx at p50, 946 at p99, so about 0.8 ms for this window and 1.6 ms for a whole 3.7 Mpx frame; RAM to RAM under QEMU |

Read from those numbers (inference, not counters):

- **The UI process spends about 20 ms a frame, and its counted paint stages
  are 8 to 10 of them**, so roughly half is the xui window thread (building
  the `Image`, its premultiply and opaque scan, the copy into the slot; no
  counter covers it yet, see `lazywebplus-performance-plan.md` §8).
- **`xuid` costs about 6 ms a frame, of which the kernel copy is under
  1 ms**: the rest, about 5 ms, is composition in user space.
- **A plain scroll is not damage-limited.** The page area scrolled is a few
  hundred pixels of new rows, yet the whole window is painted, converted,
  copied, composed and presented each time. Everything per-pixel on the path
  is therefore paid about 7 times over what a scroll needs.
- At 60 fps one core has **16.7 ms** a frame for all of it. The baseline
  above is 28 ms on a fast host; the i3-7100U will be slower. No GPU stage
  closes that gap by itself.

A caution that cost a run: the first runs read "worst wheel to damage 420 ms,
every few seconds". That was the harness, not the system: WebKit's `tall.html`
has about 39 notches of travel, and a wheel turn that moves nothing paints
nothing (the counter forgets it after 0.5 s). `scroll.html` is long enough,
and `frame_run.py` turns round before the end of the page.

### 11.1a After the scroll work (2026-10-17, same harness)

The first item of 11.2 was built (`lazywebplus-performance-plan.md`, "Scroll as a delta"): CPU per painted frame fell
from **28.7 to 17.0 ms**, a ceiling of about 59 frames a second on this host's one core (two runs, within 1%). The
parts that did not move are the ones no WebKit or window change touches: `xuid` is now the largest single piece (about
6.8 ms a frame for composing and presenting a window that still changes whole on screen), the page's process about
2.3 ms, the kernel's copy into the framebuffer 0.8 ms under QEMU. So the order below holds, and its second item has
moved up: **what stands between this box and 60 fps at 1440p is the compositor and the present, on a slower core**, and
that is where a GPU stage would act. The decisive number is still the box's `PERF:present_mpx`.

### 11.2 What the GPU stages can and cannot buy here

| Stage | Removes | Measured share it addresses | Verdict |
|---|---|---|---|
| Damage-limited scroll in the WebKit port (**not GPU**: paint the exposed rows, move the rest) | most of every per-pixel stage above | up to about 7x of the paint thread, window thread, compose and present for a scroll | **first**, software, verifiable in QEMU; see `lazywebplus-performance-plan.md` P1 |
| G6 zero-copy scanout | the kernel's copy into the framebuffer | 0.8 ms of a 1.8 Mpx frame under QEMU; **unknown on the box** (write-combining) | **decided by one number from the box**, below |
| G5 flips at vblank | tearing, frame pacing | none of the CPU cost | cheap once G1 has vblank; makes 60 fps *visible* |
| Hardware cursor plane (G6) | the cursor's save-under and two presents per move | small per move | do with G6 |
| G7 blitter, G8 render kernels | copies and blends done by the CPU | the 5 ms of composition, if it were copies | **not yet**: they need G3 (the largest, riskiest stage) and compose is not known to be copy-bound |

**Rule for G6.** On the box, `PERF:present_mpx` p50 times the window's
megapixels, plus the same for the 3.7 Mpx of a full-screen change, against the
16.7 ms frame. If a 1.8 Mpx present costs under about 1.5 ms there (the same
order as QEMU), G6 buys little and the work is software. If it is several
milliseconds (a write-combining aperture that is slow), G6 is the first GPU
stage. Do not start G3 before that number and a compose profile exist.

### 11.3 The box run that decides it (needs you)

Build a stick with the kernel perf hook and the full logical screen, with
`dbgd` so nothing needs a serial port. `LAZYOS_PERF` has no launcher flag (only
`tools/perf/run.py` builds it); through the environment `run_demo.py` should
pass it to `cargo build`, but that route is **untested**:

```bash
LAZYOS_PERF=1 LAZYOS_DISPLAY_MAX=2560x1440 python tools/run_demo.py --lazywebplus --dbgd --usb-image --build-only
# boot the box, open LazyWeb+, load a long page, turn the wheel for 20 s, then from the host:
python tools/dbg/dbgctl.py --host <ip> log --lines 400 | grep -E "PERF:(present_mpx|present|CPU)"
python tools/dbg/dbgctl.py --host <ip> log --source programs --lines 400 | grep INFO:PAINT
```

Wanted: `PERF:present_mpx` (n, p50, p99), `PERF:CPU` for the UI process, `xuid`
and the WebProcess, and the `INFO:PAINT` lines. Compare with 11.1. Any page
longer than the window will do (`tools/web/fixtures/theoldnet.com/webplus/scroll.html`
if the box can reach a host serving it).

### 11.4 Status of the implementation for this goal

- **Merged and building**: `gpud` G1 (read-only, `LAZYOS_GEN9GFX=1`) on current
  main; `cargo test -p gen9gfx -p gpupolicy -p devmatch` passes; a console image
  with `LAZYOS_GEN9GFX=1 LAZYOS_NETD=1` builds. **Not run on the box.**
- **Built for the measurement**: `tools/webplus/frame_run.py` (+
  `test_frame.py`, `scroll_page.py`), the kernel's `present_mpx` metric
  (`LAZYOS_PERF=1`; kernel test `display_present_damage_pixels`, the display
  suite passes 50 of 50).
- **Built 2026-10-19, still read-only, not run on the box**: the beam
  (`libs/gen9gfx::vblank`, `gpud/beam.rs`). `gpud` reads the adopted pipe's
  frame counter (`PIPE_FRMCOUNT`, +0x40) and scan line (`PIPEDSL`, +0x00)
  twice a second apart and prints `GPU:VBLANK:PASS measured_mhz=...
  expected_mhz=...` when the counter agrees with the timing registers within
  0.2% (G1's "vblank ticks at the monitor's rate", without taking an
  interrupt: the counter counts whether or not anybody asked), and the DE
  pipe interrupt registers as `GPU:UNDERRUN state=none|latched|masked`
  (IIR is read, never cleared; a masked underrun is reported as unobserved,
  not as none). `os.lazy.gpu.v1.GetBeam` answers the beam live (frame, line,
  vertical total, monotonic time): what a presenter needs to keep its copy
  out of the beam's way before G5 flips exist. The two pipe offsets are
  i915's and **to confirm** on the box (the Linux dump does not name them);
  the interrupt registers are confirmed. Host tests: `cargo test -p gen9gfx
  vblank`.
- **Not built yet** (honestly): vblank *interrupts*, `set_scanout`, CRC, any
  display write, G3 and everything after it. None of it is justified by the
  numbers above until the box answers 11.3; on the box run, also read
  `GPU:VBLANK` and `GPU:UNDERRUN` (`dbgctl.py log --source programs | grep
  GPU:`).
