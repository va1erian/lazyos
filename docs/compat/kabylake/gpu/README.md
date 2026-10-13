# Kaby Lake box: GPU and HDMI audio capture from Linux (plan stage G0)

Stage G0 of [`../../../kabylake-gpu-plan.md`](../../../kabylake-gpu-plan.md),
Linux half. **First capture: 2026-10-13** (`linux/`); the plan's section 2.1
reads it. It settled the "to confirm" list of section 2 except for what the
*firmware* (rather than i915) leaves on the display and for the audio and
DDI-buffer registers, which the first run did not capture (see "What is
missing" below). The goal is to settle every such fact from the box's own
i915 and HDA drivers before any LazyOS code writes a display register.

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
`<survey>/gpu/`, so a full survey already includes it. Run it with the
**same Linux kernel parameters the box normally boots with** and not from a
live image that lacks i915: the debugfs and register captures need i915 bound
to the GPU.

The script is read-only: it loads no module, mounts nothing, writes no
register and enables no option ROM. Registers are read with `intel_reg read`
(a read-only mapping of BAR0), falling back to a read-only `mmap` from Python.
Anything it cannot collect is listed in `MISSING.txt`; run the missing pieces
by hand or note why they are absent.

## What it writes

| File | Content | Answers (plan section 2) |
|---|---|---|
| `00-lspci-gpu.txt`, `-audio.txt`, `*-xxxx.txt`, `00-gpu-config-words.txt` | `lspci -vvv` and full config space of 00:02.0 and 00:1f.3; `GMCH_CTRL`, `BDSM`, `ASLS` | stolen memory base and size, OpRegion address, MSI state |
| `00-i915-*.txt`, `00-gpu-sysfs.txt` | module parameters, firmware names, sysfs identity | whether DMC/GuC/HuC are loaded at all |
| `01-intel_reg-dump.txt` | named register dump (intel-gpu-tools) | display, pipe, DDI, DPLL, CDCLK registers by name (**not** the audio block or `DDI_BUF_CTL`) |
| `02-mmio-ranges.txt` | raw dwords of the display ranges (power wells, CDCLK, transcoder, DDI, audio, scalers, DPLL, pipe/plane, DDB/watermarks, forcewake) | the baseline G1 adopts; the audio and DDI-buffer offsets. **Empty in the first capture** (see below) |
| `03-dri*-i915_display_info`, `-i915_dmc_info`, `-i915_ddb_info`, `-i915_shared_dplls_info`, `-i915_power_domain_info`, `-i915_engine_info`, ... | the driver's own decode | pipe/DDI/PLL in use, DDB layout, engines and their MMIO bases |
| `03-dri*-vbt.bin`, `03-intel_vbt_decode.txt`, `03-dri*-opregion.bin` | the VBT blob and its decode, the OpRegion | **DDI port, DDC pin, HDMI vs DP vs DVI, LSPCON flag**; `libs/gen9gfx` `tests_real` parses these |
| `04-card*-connector.txt`, `04-*-edid.bin`, `.hex`, `-edid-decode.txt` | connector state, raw EDID, decode | modes, CEA audio blocks, HDMI VSDB; `tests_real` parses the EDID |
| `05-eld.txt`, `05-codecs.txt`, `05-aplay-cards.txt`, `05-pcm-info.txt`, `05-amixer.txt` | ELD per pin, full HDA codec dump, card and PCM listing | ELD valid/presence detect, converter-to-pin mapping, rates |
| `06-dmesg-*.txt`, `06-firmware.txt` | `i915`/`snd_hda`/`hdmi`/`dmc`/`guc`/`huc` log lines | DMC/GuC state, stepping, any quirk i915 applied |
| `07-gt-freq.txt`, `07-thermal.txt` | frequency and temperature | the idle baseline of plan section 7 |

## What is missing, and why it matters

The first capture (`linux/`) has two gaps; one more run fixes both:

1. **`02-mmio-ranges.txt` is empty**: the Python `mmap` of the BAR resource
   returned `EINVAL`. The script now uses `intel_reg read` for the ranges.
2. **No audio registers**: `intel_reg dump` does not list `AUD_CONFIG`,
   `AUD_PIN_ELD_CP_VLD`, `AUD_MISC_CTRL` or the ELD buffer, nor
   `DDI_BUF_CTL`. These are the offsets `libs/gen9gfx::regs` still marks
   `TO CONFIRM`, and G2 (HDMI audio) depends on them.

Also worth knowing when reading the capture: i915 had already driven the
display, so every register is **Linux's** state (2560x1440 at 70 Hz, DDI B,
DPLL1, CDCLK 337.5 MHz), not what the firmware left. The firmware's own state is
read by a LazyOS boot (plan section 10, step 2).

## What to commit, and what to redact first

Copy the directory to `docs/compat/kabylake/gpu/linux/` (this README stays here).
Commit the text files and the small binaries (`*-edid.bin`, `*-vbt.bin`,
`*-opregion.bin`: a few hundred bytes to a few KiB).

Redact before committing:

0. The default output directory name and `README.txt` embed the machine's
   hostname; rename or edit them if that is private.
1. Run `python tools/boot/redact_survey.py <dir>` over the **text** files
   (MACs, GUIDs and `lspci` serial numbers; the same rules as `hw-survey`). It
   treats every file as text and **mangles the `.bin` files**: copy only the
   text it writes.
2. **EDID serial**: the raw EDID holds the monitor's serial number in base
   block bytes 12 to 15 and, often, in a 0xFF display descriptor as text. The
   hex and `edid-decode` files show them. Zero bytes 12 to 15, blank the 0xFF
   descriptor text (replace its 13 text bytes with `REDACTED\n` padded with
   spaces: **13 bytes exactly**, anything else changes the block length) and
   fix block 0's checksum (byte 127 so the block sums to 0 mod 256); update the
   hex dump embedded in the `edid-decode` text and its checksum line to match.
   `libs/gen9gfx::edid` validates checksums and `tests_real` parses the file,
   so a wrong edit shows up in `cargo test -p gen9gfx`.
3. Do not commit the firmware image blobs (`/lib/firmware/i915/*`); only the
   names and versions in `06-firmware.txt` are kept.
4. File names must not contain `:` and must not differ only in case
   (`aplay -l` and `aplay -L` outputs are `05-aplay-cards.txt` and
   `05-aplay-pcms.txt` for that reason; neither can be checked out on Windows):
   the script names debugfs files `03-dri<name>-...` with `:` turned into `_`
   and skips the symlinked duplicates (`dri/1`, `dri/128`).

Then mark the `// TO CONFIRM against dump` register offsets in `libs/gen9gfx`
as confirmed or corrected from `02-mmio-ranges.txt`.
