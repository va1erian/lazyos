# Kaby Lake box: GPU and HDMI audio capture from Linux (plan stage G0)

Stage G0 of [`../../../kabylake-gpu-plan.md`](../../../kabylake-gpu-plan.md),
Linux half. Nothing has been collected yet. The goal is to settle every
"to confirm" of that plan's section 2 from the box's own i915 and HDA drivers
before any LazyOS code writes a display register: LSPCON or native HDMI, the
DDI/pipe/transcoder the firmware used, its CDCLK/DPLL/DDB state, whether the
link is in HDMI or DVI mode, the stolen-memory base, the ELD and the codec
widget graph.

## What to run

On the box, in its Linux, as root, with the monitor connected **and playing
audio** if you can (play any sound in Linux first: the ELD and the HDMI audio
registers are only populated while the sink is active):

```bash
sudo apt install pciutils intel-gpu-tools alsa-utils edid-decode python3
sudo mount -t debugfs none /sys/kernel/debug 2>/dev/null   # usually mounted already
# (start any sound on the HDMI output now, e.g. with speaker-test or a media player)
sudo sh tools/boot/survey_gpu.sh          # writes ./gpu-survey-<host>-<date>/
```

`tools/boot/survey_linux_box.sh` runs the same script into its own
`<survey>/gpu/`, so a full survey already includes it. Run it a second time
with the **same Linux kernel parameters the box normally boots with** and not
from a live image that lacks i915: the debugfs and register captures need i915
bound to the GPU.

The script is read-only: it loads no module, mounts nothing, writes no
register and enables no option ROM. Registers are read through a read-only
mapping of BAR0 (what `intel_reg` does). Anything it cannot collect is listed
in `MISSING.txt`; run the missing pieces by hand or note why they are absent.

## What it writes

| File | Content | Answers (plan section 2) |
|---|---|---|
| `00-lspci-gpu.txt`, `-audio.txt`, `*-xxxx.txt`, `00-gpu-config-words.txt` | `lspci -vvv` and full config space of 00:02.0 and 00:1f.3; `GMCH_CTRL`, `BDSM`, `ASLS` | stolen memory base and size, OpRegion address, MSI state |
| `00-i915-*.txt`, `00-gpu-sysfs.txt` | module parameters, firmware names, sysfs identity | whether DMC/GuC/HuC are loaded at all |
| `01-intel_reg-dump.txt` | named register dump (intel-gpu-tools) | display, pipe, DDI, DPLL, CDCLK, audio registers by name |
| `02-mmio-ranges.txt` | raw dwords of the display ranges (power wells, CDCLK, transcoder, DDI, audio, scalers, DPLL, pipe/plane, DDB/watermarks) | the baseline G1 adopts; compare with the register map in `libs/gen9gfx` |
| `03-dri*-i915_display_info.txt`, `-i915_dmc_info`, `-i915_ddb_info`, `-i915_shared_dplls_info`, `-i915_power_domain_info`, `-i915_gem_gtt` | the driver's own decode | pipe/DDI/PLL in use, DDB layout, GGTT size |
| `03-dri*-vbt.bin`, `03-intel_vbt_decode.txt`, `03-dri*-i915_opregion*` | the VBT blob and its decode | **DDI port, DDC pin, HDMI vs DP vs DVI, LSPCON flag** (feed `libs/gen9gfx::vbt` tests) |
| `04-card*-connector.txt`, `04-*-edid.bin`, `.hex`, `-edid-decode.txt` | connector state, raw EDID, decode | modes, CEA audio blocks, HDMI VSDB (feed `libs/gen9gfx::edid` fixtures) |
| `05-eld.txt`, `05-codecs.txt`, `05-aplay-l.txt`, `05-pcm-info.txt`, `05-amixer.txt` | ELD per pin, full HDA codec dump, card and PCM listing | ELD valid/presence detect, converter-to-pin mapping, rates |
| `06-dmesg-*.txt`, `06-firmware.txt` | `i915`/`snd_hda`/`hdmi`/`dmc`/`guc`/`huc` log lines | DMC/GuC state, stepping, any quirk i915 applied |
| `07-gt-freq.txt`, `07-thermal.txt` | frequency and temperature | the idle baseline of plan section 7 |

## What to commit, and what to redact first

Copy the directory to `docs/compat/kabylake/gpu/linux/` (this README stays here).
Commit the text files and the small binaries (`*-edid.bin`, `*-vbt.bin`,
`*-opregion.bin`: a few hundred bytes to a few KiB).

Redact before committing:

1. Run `python tools/boot/redact_survey.py <dir>` over the text files (MACs,
   GUIDs and `lspci` serial numbers; the same rules as `hw-survey`).
2. **EDID serial**: the raw EDID holds the monitor's serial number in base
   block bytes 12 to 15 and, often, in a 0xFF display descriptor as text. The
   hex and `edid-decode` files show them. Zero bytes 12 to 15, blank the 0xFF
   descriptor text and fix block 0's checksum (byte 127 so the block sums to
   0 mod 256); do the same for each extension block's checksum if you changed
   it. `libs/gen9gfx::edid` validates checksums, so a wrong edit shows up
   in `cargo test`.
3. Do not commit the firmware image blobs (`/lib/firmware/i915/*`); only the
   names and versions in `06-firmware.txt` are kept.

Then settle each *to confirm* in the plan from the files, and mark the
`// TO CONFIRM against dump` register offsets in `libs/gen9gfx` as
confirmed or corrected from `02-mmio-ranges.txt`.
