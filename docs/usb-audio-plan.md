# Plan: USB audio playback (UAC1 and UAC2) on `usbd` + `sndd`

> **Status: A0-A4 implemented (2026-10-22): QEMU's `usb-audio` plays
> through it (`tools/sound/usb_run.py`, the recording judged), with output
> switching and hot-plug; awaiting hardware validation on the Kaby Lake and
> Alder Lake boxes (A5).** Opt-in with `LAZYOS_USB_AUDIO=1`
> (`run_demo.py --usb-audio`, the launcher's *USB audio* checkbox). Builds on
> [usb-hid-plan.md](usb-hid-plan.md) (`usbd`, `libs/xhci`),
> [audio-plan.md](audio-plan.md) (`sndd`, `audiod`, `os.lazy.audio.v1`) and
> [architecture/usb.md](architecture/usb.md), "Adding a device class".

## Goal and scope

Plug a USB headset, a USB DAC or a USB speaker into either test box and hear
everything LazyOS plays (`beep`, `modplay`, the NES Player, LazyWeb) on it.
The Alder Lake box has no working sound today (its HDA controller is in DSP
mode, class `04:01`, and nothing drives it); on the Kaby Lake box the USB
device wins over the built-in HDA codec while it is plugged in.

**In:** USB Audio Class 1.0 (full-speed headsets, most cheap dongles) and 2.0
(high-speed DACs); playback; PCM type I with 16-, 24- and 32-bit subslots;
1 to 8 channels (the mixer's stereo is mapped onto them); every sample rate
the device lists, discrete or as ranges; synchronous, adaptive and
asynchronous endpoints, the last with their explicit feedback endpoint; the
feature unit's mute switched off; hot-plug in both directions; QEMU's
`usb-audio` (UAC1, full speed, adaptive, 48 kHz stereo) as the CI target.

**Out (later stages):** capture (the microphone of a headset), hardware
volume controls exposed to the mixer, implicit feedback (devices whose
capture stream clocks playback: some pro interfaces), UAC3, compressed
formats (type II/III), SuperSpeed audio, HID buttons on headsets.

## Where the driver lives

USB devices sit behind `usbd`, which owns every xHCI controller (one claim per
controller; no other task can touch it). `sndd` owns the audio card model:
the `os.lazy.audio.v1` service, the one-owner stream, ring adoption and
period staging (`session.rs`, `service.rs`, `stream.rs`), and the
`system/audio/{card}/event` topic. Duplicating that in `usbd` would fork the
audio contract. So the work is split along the existing seam:

```
 audiod ──os.lazy.audio.v1──> sndd usb=1 ──os.lazy.usb.audio.v1──> usbd ──isoch TRBs──> xHCI ──> device
  (mixer)  "os.lazy.audio.card/usb-audio0"  (UsbCard)  "os.lazy.usb.audio"  (audio.rs)
     └──────os.lazy.audio.v1──> sndd dev=<id>  "os.lazy.audio.card/intel-hda0"  (built-in codec)
```

* **`usbd`** binds the audio function (`usbd/audio*.rs`): parses the class
  descriptors, picks the alternate setting, programs the isochronous OUT
  endpoint (and the feedback IN endpoint) with Configure Endpoint, sends
  `SET_INTERFACE` and the sample rate, and keeps the isochronous ring fed
  from the periods `sndd` submits, converting the mixer's stereo `S16Le` to
  the device's subslot and channel count as it copies. It serves the new
  `os.lazy.usb.audio.v1` (`idl/usb_audio.midl`) under `os.lazy.usb.audio`
  while a device is bound, and unregisters the name when it goes away. One
  audio device at a time (the first bound; a second is reported
  `USBD:AUDIO:BUSY`).
* **`sndd usb=1`** is a second instance of the sound driver (init row
  `sndd-usb`, same `_snd` identity, no device claim) whose `Card` is a
  `UsbCard` speaking `os.lazy.usb.audio.v1` instead of touching hardware.
  It waits for `os.lazy.usb.audio`, serves the card as
  `os.lazy.audio.card/usb-audio0` (events under `usb-audio0`), plays its
  self-test tone once, and exits when the link dies; `init` restarts it
  (`Restart::Always`) and it waits for the next device.

## Several outputs

Every card driver registers under `os.lazy.audio.card/<id>` (`virtio-snd0`,
`intel-hda0`, `usb-audio0`, and an HDMI codec as `intel-hda0-hdmi` once G2
drives one; the bare `os.lazy.audio.card` is gone), and `AudioInfo` carries
a `name` to show (`Built-in audio`, `USB: <product>`). `audiod` scans the
registry once a second for those names (owned by `_snd`, or root on a
kernel-spawned boot) and plays through one *output* at a time
(`audiomix::outputs`, host-tested):

* the output someone selected, whenever it is present;
* otherwise the best kind present: USB, then HDMI, then a built-in codec,
  then a virtual card (the newest of a kind). A headset plugged in takes over,
  unplugging it falls back, and what plays at boot does not depend on which
  driver registered first.

A switch opens the new card before the old one is let go; the open streams
keep their rings and positions and are resampled to the new card's rate
(`Mixer::retarget`), so a song goes on through the headset. The mixer's
control interface lists and selects outputs (`ListOutputs`, `SelectOutput`
in `idl/audio_mixer.midl`). The **Volume tray applet** shows the output
playing in its tooltip and, with two or more, an *Output* section of radio
rows in its menu; the choice is kept per user (`user/<uid>/audio/output` in
`confd`) and asked for again whenever that output is present and the mixer
does not prefer it (after a mixer restart). From the shell:
`mixer outputs` and `mixer output <id>`.

`devd` is not involved: it matches PCI functions, and a USB function is not
one. `usbd` is already started by `init`.

## The USB side (`usbd`)

* **Descriptors** (`libs/usbaudio`, `no_std`, host-tested and fuzzed). The
  raw configuration chain is kept by `Device::read_config` (now up to 2 KiB:
  the control buffer grew into the free half of page 1). The parser walks it
  once and yields, per audio function: the version (`bcdADC` 1.x or 2.x),
  the entities (input/output terminals, feature units, clock sources and
  selectors), and every AudioStreaming alternate with its terminal link,
  format (type I, subslot, bit resolution, channels), rates (UAC1 discrete
  list or continuous range; UAC2 from the clock's `GET_RANGE`), data endpoint
  (address, sync type, `wMaxPacketSize` and high-bandwidth transactions,
  `bInterval`) and feedback endpoint (`bSynchAddress` or the next IN
  endpoint of the alternate). A playback stream is one whose data endpoint is
  isochronous OUT and whose terminal link is a USB-streaming input terminal.
* **Choosing** (`usbaudio::choose`): for the mixer's request (48 kHz, 2
  channels, 16 bit) the alternate with the exact rate wins, then 2 channels,
  then the smallest subslot of 16 bits or more, then the smallest packet
  size; a rate the device lacks falls back to its rate closest to 48 kHz
  (the mixer resamples). `sndd`'s `PcmInfo` lists every rate any alternate
  supports.
* **Isochronous transfers** (`libs/xhci`: `EndpointType::IsochOut/IsochIn`,
  `EndpointContext::isoch`, `trb::isoch`; xHCI 4.11.2.5, 6.4.1.3). One TD per
  service interval, one Isoch TRB each, every TD with *Start Isoch ASAP*
  (what Linux does for audio unless the controller has CFC), `IOC` on the
  TD that ends a period, on one TD in every 2 ms of audio and on the last
  TD of every fill (so an event always comes back while the ring is fed). The ring has
  256 TRBs; the data lives in one DMA region per controller (allocated on
  first use and kept, like the bulk window: freeing DMA stops the device), in
  power-of-two slots of at most 4 KiB, so no TRB crosses a page and hence no
  64 KiB boundary. That region is the 16th DMA buffer of the claim (core,
  scratchpads, bulk window, 12 device regions, audio).
* **Pacing** (`usbaudio::packet`). Frames per service interval come from a
  16.16 accumulator: the nominal rate for synchronous and adaptive
  endpoints, the device's feedback for asynchronous ones (10.14 at full
  speed, 16.16 at high speed, the format confirmed against the nominal rate
  and clamped to ±1/8 of it). The ring is kept 32 ms ahead of the bus; when
  `sndd` has nothing queued, silence keeps it at least 16 ms ahead, so the
  device never sees a gap and real samples are never more than 32 ms late.
* **Completion.** A period counts as played when the TD carrying its last
  frame completes (Success, Short Packet, Missed Service). `Reap` reports
  the running count. `Stop` stops the endpoint (Stop Endpoint, Set TR Dequeue
  Pointer) and reports every queued period as completed, as a stopped virtio
  stream hands back its buffers. A Ring Underrun (usbd was starved for
  16 ms) restarts on its own: the next TD carries SIA.
* **Bandwidth.** A Configure Endpoint that fails with Bandwidth Error moves
  to the next alternate with a smaller packet; none left is
  `USBD:AUDIO:FAIL ... bandwidth`.
* **Identity.** Only `_snd` (uid 901) and root may call
  `os.lazy.usb.audio.v1` (the kernel-stamped caller); the shared buffer is
  read with raw copies of ranges checked against its mapped size.

## The protocol (`idl/usb_audio.midl`)

```
Describe() -> (device: Device)                  vendor, product, name, version (1|2), rates, channels, bits, sync, port
Attach(staging: Buffer) -> ()                   the period staging sndd fills (64 KiB)
Configure(rate: U32) -> (granted: U32)
Start() -> ()   Stop() -> ()   Release() -> ()  Release: alternate 0, endpoints dropped
Submit(slot: U32, offset: U32, len: U32) -> ()  one period, in order
Reap() -> (progress: Progress)                  periods played since Configure, underruns, missed intervals
```

## Stages

| Stage | What | Proof |
|---|---|---|
| A0 | This plan | review |
| A1 | `libs/usbaudio`: descriptors (UAC1/UAC2), choice, packetizer, feedback, sample conversion; fuzz target | `cargo test -p usbaudio` (QEMU's, a UAC1 headset's, a UAC2 async DAC's descriptors; hostile chains) |
| A2 | `libs/xhci` isochronous context and TRB; `usbd` audio binding, alternate switching, isochronous ring, the `os.lazy.usb.audio.v1` server | `cargo test -p xhci`; `USBD:AUDIO:*` lines in QEMU |
| A3 | `sndd usb=1` (`UsbCard`), the `sndd-usb` init row; card names `os.lazy.audio.card/<id>`, `audiod`'s outputs (`audiomix::outputs`, `ListOutputs`/`SelectOutput`), the Volume applet's Output menu, `mixer outputs` | `python tools/sound/usb_run.py`: the driver's 440 Hz and `beep`'s 880 Hz recorded from QEMU's `usb-audio` and judged; `--both` (a virtio card too: two outputs, two recordings) and `--hotplug` (unplug: the mixer falls back to virtio; replug: back to USB); `cargo test -p audiomix` |
| A4 | `LAZYOS_USB_AUDIO=1`, `run_demo.py --usb-audio`, launcher control, docs | `tools/lazygui/test_catalog.py`; `cargo test -p build-support-tests` |
| A5 | Hardware: a UAC1 headset and a UAC2 DAC on the Kaby Lake and Alder Lake boxes | `dbgctl log --source programs` shows `USBD:AUDIO:UP` and `SNDD:CARD model=usb-audio`; the user hears `beep` and a module |

## Validating on the boxes (A5)

```bash
python tools/run_demo.py --desktop --usb-audio --dbgd --usb-image --build-only
# boot the stick on the box with the headset plugged in, then from the host:
python tools/dbg/dbgctl.py --host <ip> log --source programs | grep -E "USBD:AUDIO|SNDD|AUDIOD"
# in the Terminal: beep (880 Hz for 0.5 s), modplay /system/share/... for music
```

What to send back when it does not work: every `USBD:DESC:CONFIG` line of the
device (its descriptors), the `USBD:AUDIO:*` lines and any `USBD:DIAG`.

* **Kaby Lake box.** Its HDA controller (class `04:03`) is driven by `sndd`,
  so with a headset plugged in there are two outputs: `AUDIOD:OUTPUTS
  intel-hda0,usb-audio0`, the Volume tray menu shows both, the headset plays
  (USB ranks first) and picking *Built-in audio* moves the sound to the
  codec at once. Unplug the headset while music plays: it goes on through the
  codec. The HDMI codec on the same link is not an output yet: `sndd` drives
  the first codec with an output path, and the display side is
  [kabylake-gpu-plan.md](kabylake-gpu-plan.md) G2; once it is served as
  `os.lazy.audio.card/intel-hda0-hdmi` it is a third row with nothing to change
  here (`audiomix::outputs` already ranks HDMI above the analog codec).
* **Alder Lake box.** Its HDA controller is in DSP mode (`04:01`) and has no
  driver, so the USB device is the only output (`AUDIOD:OUTPUTS usb-audio0`);
  the menu then has no Output section (there is no choice to make).
* Both: try a UAC 1.0 headset (full speed, `uac=1`, usually adaptive) and a
  UAC 2.0 DAC (high speed, `uac=2`, usually asynchronous: `feedback=true` in
  `USBD:AUDIO:UP` and the `pace=` of `USBD:AUDIO:STOP` close to the rate).
  `underruns=` and `missed=` should stay at 0 while nothing else loads the box.

## Known limits

* An output switch drops what the old card had queued (up to the mixer's
  lead, about 64 ms), and a stream draining at that moment reports
  `Drained` before its tail played anywhere.
* A device with implicit feedback (asynchronous, no feedback endpoint: its
  capture stream is the clock) runs at the nominal rate and slips a frame
  now and then.
* Every feedback read asks for an event (at high speed about a thousand
  extra `usbd` wakes a second while an asynchronous DAC plays).
* One card per kind: card ids are fixed (`intel-hda0`, `usb-audio0`), so two
  USB devices or two HDA controllers are not two outputs.
* SuperSpeed (UAC 3) devices are refused (`USBD:AUDIO:SKIP`): their burst
  and payload come from the SuperSpeed companion descriptor.
* A configuration chain over 2 KiB is read up to 2 KiB and cut after its
  last whole record (`USBD:DESC:TRUNCATED`): alternates past it are not seen.

## Risks

1. **SIA on every TD.** Linux does the same, but QEMU is the only controller
   this ran on before the boxes. Symptom: no sound and `USBD:AUDIO:XFER
   code=` lines with Missed Service (23) or TRB Error (5).
2. **UAC2 clock order.** Some DACs want the rate before `SET_INTERFACE`,
   others after; `usbd` sets it after (Linux's order), reads it back with
   `GET_CUR` and logs both values (`USBD:AUDIO:RATE`).
3. **Starvation.** A slow USB stick request blocks `usbd` for its duration;
   past 16 ms the device plays silence (`underruns=` in `Reap`).
4. **One audio device.** A second is ignored until the first goes.
5. **usbd is the keyboard driver.** A panic in the audio code takes the
   keyboard with it until `init` restarts `usbd`; hence the opt-in switch until
   A5 passes on both boxes.
