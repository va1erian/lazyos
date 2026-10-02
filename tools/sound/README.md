# Sound harness

Proves the virtio-sound driver and the system mixer (`audiod`,
[`docs/audio-plan.md`](../../docs/audio-plan.md)) by **listening to them**. QEMU runs with
`-audiodev wav`, so everything the guest sends to the virtual sound card lands in
a WAV file; `analyze_wav.py` then finds the tones in the recording and checks
their pitch, length and level. A serial marker alone is never the verdict.

## Quick start

```bash
# Build with LAZYOS_SOUND=1, boot headless, record, verify (about 10 s on WHPX/KVM)
python tools/sound/run.py

# Reuse target/lazyos.img, force TCG
python tools/sound/run.py --no-build --accel none

# init supervises the driver as _snd (uid 901) and the mixer as _audio (uid 905)
python tools/sound/run.py --services

# Two clients at once must be one chord at unity; a volume=50 tone half as loud
python tools/sound/run.py --mix

# q35 has no IDE controller the kernel drives: attach the image as virtio-blk
python tools/sound/run.py --machine q35 --virtio-disk

# No sound card: the driver must say so and exit cleanly
python tools/sound/run.py --no-device

# -audiodev none: check the driver, skip the recording
python tools/sound/run.py --smoke
```

QEMU is discovered like the other tools (`--qemu`, then `PATH`, then
`C:\Program Files\qemu` on Windows). Outputs go to `shots/sound/`
(`serial.log`, `out.wav`); `shots/` is git-ignored.

## What a run checks

The image is built with `LAZYOS_SOUND=1`, which embeds `/system/bin/sndd`,
`/system/bin/audiod`, `/system/bin/beep` and `/system/bin/mixer` and starts
`sndd demo=1` and `audiod demo=1` (from the kernel, or from `init`'s manifest
with `LAZYOS_SERVICES=1`). The driver plays its own tone straight through the
card; the mixer then runs the evidence clients, which reach the card only
through it. The guest prints, in order:

| Marker | Meaning |
|---|---|
| `SND:PLAY:PASS freq=440 ...` | the driver played a tone straight through the card |
| `SND:IRQ:PASS delivered=N` | the armed INTx line delivered interrupts (`SNDD:IRQ:POLLING` instead when the machine's line is not routable, which the harness reports but accepts) |
| `SNDD:READY` | the card is registered (`os.lazy.audio.card`) |
| `AUDIOD:CARD rate=48000 ...` | the mixer opened the card's stream |
| `BEEP:PLAY:PASS freq=880 ...` | a real client played a tone through the mixer |
| `BEEP:PROBE:PASS checks=<n>` | malformed and hostile requests (limits, commits, volumes, a 300-request transfer flood) were all refused correctly, and the mixer survived |
| `BEEP:INTRUDER:PASS` | a second task was refused on the owner's stream, and got one of its own |
| `DEV:CROSSCLAIM:snd:PASS` | `--services`: as `_snd`, every device of another class was refused (`SKIP` as root) |
| `BEEP:SOAK:PASS iterations=40` | 40 open/play/close cycles leaked nothing |
| `MIXER:PROBE:PASS checks=<n>` | the control panel lists streams and sets stream and master volumes, refusing bad ones |

The harness waits for all of them (or any `FAIL`), stops QEMU through QMP so the
wav backend patches its header, and requires the recording to hold exactly two
tone segments, 440 Hz then 880 Hz, each at least 640 ms, loud enough, within 2%
of the target pitch. With `--services` it also requires
`SNDD:CRED uid=901 caps=0x100` and `AUDIOD:CRED uid=905 caps=0x0`.

## Mixing: `--mix`

`python tools/sound/run.py --mix` builds with `LAZYOS_SOUND_MIX=1`, so
`audiod demo=1` starts two `beep`s at once (660 Hz and 990 Hz, 1.5 s each)
and then `beep 880 800 50` (half volume). `mixcheck.py` measures each expected
frequency per 40 ms window with a Hann-windowed Goertzel filter, labels the
windows with the frequencies present, and requires the steps 440, 660+990, 880
in that order, each at least 600 ms; each chord member at the level of a lone
tone (mixing adds, it does not attenuate); and 880 Hz at half the amplitude of
440 Hz (within 20%). `test_mixcheck.py` proves it fails for sequential tones, a
missing partner, the wrong volume, an attenuated mix, a short chord, silence
and garbage.

## The detector

`analyze_wav.py` splits a recording into tone segments by *pitch*, not silence
(QEMU's backend writes only while the guest plays, so two tones can abut), then
measures each segment's frequency from rising zero crossings. It tolerates the
unpatched headers of a killed emulator. `test_analyze_wav.py` proves it fails
when it should (silence, wrong pitch, missing/extra/swapped tones, too short or
quiet, garbage files):

```bash
python tools/sound/test_analyze_wav.py
cargo test -p virtio -p virtio-snd -p pcm     # the driver libraries
```

## Tracker player: `--modplay`

`python tools/sound/run.py --modplay` builds with `LAZYOS_SOUND_MODPLAY=1`, so
`audiod demo=1` runs `modplay selftest` (a built-in single-voice melody,
`libs/modplay/examples/gen_selftest.rs`) instead of the `beep` clients. The
recording must hold the driver's 440 Hz tone and then the melody's seven notes
(259, 389, 518, 389, 259, 518, 389 Hz) in order; the marker is
`MODPLAY:PLAY:PASS frames=<n> ...`. `modplay <file.mod>` plays any four-channel
ProTracker module from the desktop Terminal.

## Desktop: the `beep` command

The desktop profile (`LAZYOS_DESKTOP=1`) always ships the sound stack: `sndd` and
`audiod` in `init`'s manifest (as `_snd` and `_audio`, silent, no boot tones),
and `/system/bin/beep` and `/system/bin/mixer`, which the kernel exposes as shell
commands (`kernel/src/process/linux/native.rs`). In the desktop Terminal:

```
/ # beep              # 880 Hz for 800 ms
/ # beep 440 500      # frequency in Hz, then milliseconds
/ # beep 440 3000 & beep 660 3000   # two at once: the mixer plays both
/ # beep 440 500 25   # a quarter of full volume
/ # mixer             # master volume and every open stream
/ # mixer master 50   # halve everything; `mixer mute` / `mixer unmute`
```

It needs the xui apps and a BusyBox (`python tools/xui/build.py`,
`python tools/abi/busybox.py`). To hear it, or to record it:

```bash
python tools/run_demo.py --desktop --sound              # host speakers
python tools/run_demo.py --desktop --sound wav:out.wav  # recorded
```

The launcher GUI (`python tools/lazyos_gui.py`) does this for you: its Simple
tab's **Desktop** start attaches the sound card, and the Advanced tab has a
**Sound card** checkbox (on by default; on the CLI image it means boot-time test
tones, so Simple leaves it off there).

A scripted, headless check (types `beep 660 600` into the Terminal and measures
the recording; the tone must come out at 660 Hz):

```bash
python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/desk   --script tools/screenshot/examples/beep_desktop.json   --extra-arg=-audiodev --extra-arg=wav,id=a0,path=shots/desk/out.wav   --extra-arg=-device --extra-arg=virtio-sound-pci,audiodev=a0
python tools/sound/analyze_wav.py shots/desk/out.wav --expect-freq 660 --min-ms 500
```

## Interactive

```bash
python tools/run_demo.py --sound            # host speakers (dsound / pa / coreaudio)
python tools/run_demo.py --sound wav:out.wav
```

builds with `LAZYOS_SOUND=1` and plays the boot tones (plain image only; the
desktop stays quiet until you run `beep`). See
[`docs/architecture/audio.md`](../../docs/architecture/audio.md) for the driver.
