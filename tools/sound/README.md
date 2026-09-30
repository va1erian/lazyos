# Sound harness

Proves the virtio-sound driver by **listening to it**. QEMU runs with
`-audiodev wav`, so everything the guest sends to the virtual sound card lands in
a WAV file; `analyze_wav.py` then finds the tones in the recording and checks
their pitch, length and level. A serial marker alone is never the verdict.

## Quick start

```bash
# Build with LAZYOS_SOUND=1, boot headless, record, verify (about 10 s on WHPX/KVM)
python tools/sound/run.py

# Reuse target/lazyos.img, force TCG
python tools/sound/run.py --no-build --accel none

# init supervises the driver as the unprivileged _snd user (uid 901)
python tools/sound/run.py --services

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

The image is built with `LAZYOS_SOUND=1`, which embeds `SNDD.ELF` and `BEEP.ELF`
and starts `sndd demo=1` (from the kernel, or from `init`'s manifest with
`LAZYOS_SERVICES=1`). The guest then prints, in order:

| Marker | Meaning |
|---|---|
| `SND:PLAY:PASS freq=440 ...` | the driver played a tone straight through the card |
| `SND:IRQ:PASS delivered=N` | the armed INTx line delivered interrupts (`SNDD:IRQ:POLLING` instead when the machine's line is not routable, which the harness reports but accepts) |
| `SNDD:READY` | `os.lazy.audio.v1` is registered |
| `BEEP:PLAY:PASS freq=880 ...` | a real client played a tone through the service |
| `BEEP:PROBE:PASS checks=28` | malformed and hostile requests were all refused correctly, and the driver survived |
| `BEEP:INTRUDER:PASS` | a second task was refused on the owner's stream |
| `BEEP:SOAK:PASS iterations=40` | 40 open/play/close cycles leaked nothing |

The harness waits for all of them (or any `FAIL`), stops QEMU through QMP so the
wav backend patches its header, and requires the recording to hold exactly two
tone segments, 440 Hz then 880 Hz, each at least 640 ms, loud enough, within 2%
of the target pitch. With `--services` it also requires
`SNDD:CRED uid=901 caps=0x100`.

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

## Desktop: the `beep` command

The desktop profile (`LAZYOS_DESKTOP=1`) always ships the sound stack: `sndd` in
`init`'s manifest (as `_snd`, silent, no boot tones) and `BEEP.ELF`, which the
kernel exposes as a shell command (`kernel/src/process/linux/native.rs`). In the
desktop Terminal:

```
/ # beep              # 880 Hz for 800 ms
/ # beep 440 500      # frequency in Hz, then milliseconds
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
