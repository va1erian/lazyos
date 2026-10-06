# Audio: the virtio-sound driver, the system mixer and `os.lazy.audio.v1`

**What it is.** Two ring-3 services. `sndd` is the first userspace driver on
the device core ([`devices.md`](devices.md)) and the reference for the ones
that follow: it claims QEMU's `virtio-sound-pci` function through syscall 23,
drives it with a modern virtio-PCI transport, and serves the class interface
`os.lazy.audio.v1` (`idl/audio.midl`) for the card under `os.lazy.audio.card`.
`audiod` is the system mixer: it serves the same interface under
`os.lazy.audio`, the name applications resolve, mixes any number of client
streams into the card's one stream, and serves the volume control panel
`os.lazy.audio.mixer.v1` (`idl/audio_mixer.midl`). Clients use
`libs/audioclient`; `beep` is the smallest. The kernel knows nothing about
audio: it provides buses, BARs, DMA and IRQs, exactly as `docs/driver-plan.md`
D2 promises. The staged plan for mixing and the client API is
[`audio-plan.md`](../audio-plan.md).

This is stage D6 of the driver plan, together with the part of D5 it needs (the
modern virtio transport). The NIC driver, `devd` and the second driver classes
are still open.

**Key files**

| Path | Role |
|---|---|
| `libs/virtio/` | Modern (virtio 1.x) PCI transport, pure `no_std` with host tests: `caps.rs` capability parsing, `transport.rs` status/feature negotiation/queue setup/notify, `queue.rs` split virtqueue with an in-struct free list, `regs.rs` |
| `libs/virtio-snd/` | virtio-sound wire protocol (`wire.rs`) and the mapping from an audio request to stream parameters (`params.rs`) |
| `libs/pcm/` | Fixed-point sine generator (`tone.rs`); user programs are soft-float |
| `idl/audio.midl`, `idl/audio_mixer.midl` | The stream interface and the mixer's control panel, compiled by `midlc` into `libs/generated` and `docs/idl/` |
| `user/src/bin/sndd.rs`, `sndd/` | The driver: `device.rs` claim + BAR mapping, `card.rs` queues and control/transmit paths, `stream.rs` slots and DMA staging, `session.rs` ownership/ring/commit/volume logic, `service.rs` request dispatch |
| `libs/audiomix/` | The mixer's engine (`mixer.rs`, `stream.rs`: stream table, contract checks, marks), `resample.rs`, `gain.rs`, `grant.rs`, and `service.rs`, the wire layer `audiod` and the host tests share |
| `user/src/bin/audiod.rs`, `audiod/` | The mixer: `card.rs` paces the card, `server.rs` handles transfers and deferred drains, `ring.rs` maps client rings, `demo.rs` runs the evidence clients |
| `libs/audioclient/` | The client library: `Client`, `MixerControl` and the blocking `PlaybackStream` over a `Transport` trait |
| `user/src/audio.rs` | The native `Transport` (Messenger endpoint + display shared buffers) |
| `user/src/messenger/audio.rs` | Service names and reply framing shared by the two servers |
| `user/src/bin/beep.rs`, `beep/` | Tone client, hostile-input probe, intruder and soak modes |
| `user/src/bin/mixer.rs` | `mixer`, the volume control command and the control-panel probe |
| `user/src/bin/modplay.rs`, `modplay/` | ProTracker module player on a `PlaybackStream`; `libs/modplay` does the parsing and mixing (`docs/tracker-plan.md`) |
| `kernel/src/process/linux/native.rs` | `beep`, `mixer` and `modplay` in the table of native programs a shell may run |
| `tools/sound/` | The harness: `run.py` boots QEMU with `-audiodev wav`, `analyze_wav.py` judges single tones, `mixcheck.py` chords and levels |

## How a sound gets out

1. `sndd` lists devices (`dev::list`) and claims `1af4:1059`. It sets memory
   decode and bus mastering through `cfg_write`, walks the virtio capabilities in
   config space (`cfg_read`), maps the BAR they point into (`map_bar`) and
   checks every structure lies inside the BAR the kernel reported.
2. `Transport::negotiate` runs reset, `ACKNOWLEDGE`, `DRIVER`, feature
   negotiation (`VERSION_1` required, nothing optional taken) and `FEATURES_OK`.
   The four queues (control, event, rx, tx) live in one 12 KiB DMA block from
   `dma_alloc`; `DRIVER_OK` follows.
3. Control requests (`PCM_INFO`, `SET_PARAMS`, `PREPARE`, `START`, `STOP`,
   `RELEASE`) are queued on the control queue and answered on the used ring.
   The driver **claims the device with an interrupt endpoint and arms the INTx
   line** (`irq_enable`, then clears INTx-disable in the command register). A
   wait for the device is a receive on that endpoint with a one-tick deadline:
   an interrupt wakes it at once, and the used ring is polled either way, so a
   lost interrupt costs one tick, never a hang. Each message is checked (sent by
   slot 0, well formed), the virtio ISR status is read to deassert the level
   interrupt, and `irq_ack` unmasks the line. If the line is not routable
   (`irq_enable` answers `ENOSYS`) or the kernel refuses the endpoint, the
   driver falls back to pure polling (`SNDD:IRQ:POLLING`), the CI-safe default of
   `docs/driver-plan.md` section 3.3. Completion is handled in the driver's own
   loop, so the serve loop still wakes every tick while a stream runs.
4. Audio goes out on the transmit queue: one chain per period, `[header]
   [samples] [status]`, with the samples in driver-owned DMA slots.

## The mixer

Applications never open the card. `audiod` holds the card's one stream for as
long as it runs and keeps three 1024-frame periods queued ahead of the card's
position; each period is every running client stream, converted to the card's
rate, scaled by its own gain, summed, scaled by the master gain and saturated
(`libs/audiomix`). A stream's `Position` advances when the card has played the
period that carried its frames, so `Drain` replies exactly when the stream is
audible no more; the reply is deferred, never blocking other clients. Details,
limits and the plan: [`audio-plan.md`](../audio-plan.md).

## The client protocol

Replies cannot carry buffers (the kernel refuses transfers in a reply,
`ipc::channels::reply`), and the driver must not let the device read memory a
client can rewrite, so **the client owns the ring and the driver copies out of
it** (`docs/driver-plan.md` section 3.4).

```
OpenStream(dir, format, rate, channels, period_bytes) -> StreamGrant   closest parameters
AttachRing(stream)      request carries buffers[0]: the shared ring
Commit(stream, frames)  total frames written; returns frames consumed
Start / Stop / Drain / Position / CloseStream
```

Frame *n* lives at ring byte `(n mod ring_frames) * frame_bytes`. `Commit`
counters must be monotonic and at most one ring ahead of what the server has
copied out; the server consumes whole periods as they are committed, and a final
short period during `Drain`. If the device runs dry it plays silence. `Drain` is
terminal for a stream; `Stop` discards the uncommitted tail and restarts frame
numbering at 0. `SetVolume(stream, gain_q16)` and `SetMute` scale the server's
own copy of the samples (16.16 gain, at most four times unity, saturating;
`EINVAL` above that, `ENOTSUP` for a gain other than unity on a format other
than `S16Le`). Mute stages silence and keeps the gain, and the stream keeps
consuming. The cost is one multiply and shift per staged sample, none at
unity. In `sndd` the state and the staging step are `audiomix::volume`
(`StreamVolume`), host-tested through the generated codec in
`libs/audiomix/src/tests_volume.rs`; `audiod` applies the same `gain` math per
stream and to the master.
`libs/audioclient`'s `PlaybackStream` does all of this for a client:

```rust
let audio = user::audio::connect_wait(user::audio::NAME, 500)?;
let mut out = PlaybackStream::open(&audio, Params::new(48_000, 2))?;
out.write(&samples)?;          // blocks while the ring is full
let played = out.finish()?;    // drain, then close (Drop closes too)
```

## Security

- **Identity.** Under `init` the driver runs as system uid 901 (`_snd`) with
  only `CAP_DEV_CLAIM` (`SNDD:CRED uid=901 caps=0x100` in the boot log), and the
  mixer as uid 905 (`_audio`) with no capabilities at all (`AUDIOD:CRED uid=905
  caps=0x0`); a kernel-spawned boot runs both as root. Neither has
  `CAP_SETUID`.
- **One owner per stream.** The owner is the kernel-stamped sender of
  `OpenStream`; nothing in a request body names a caller. Any other task gets
  `EACCES`. The card has one stream (a second open gets `EBUSY`), held by the
  mixer; the mixer allows four per client and sixteen in all. An owner silent
  for 10 s with nothing left to play loses the stream (`SNDD:RECLAIM`,
  `AUDIOD:RECLAIM`).
- **The control panel** (`os.lazy.audio.mixer.v1`) changes volumes only; it
  cannot read, feed, stop or close anyone's stream.
- **Untrusted inputs.** Everything the device writes (used ring, reply lengths,
  status words, PCM info) is bounds-checked and never indexes with a raw value
  (`libs/virtio` returns `DeviceError`). Everything the client says is checked:
  parameters snap or fail with `EINVAL`/`ENOTSUP`, commits are monotonic and
  bounded, the ring must cover the grant, and client memory is read only with a
  raw copy of a range computed from the driver's own counters, so a client
  rewriting its ring produces noise, never an out-of-bounds access. Buffers or
  endpoints attached to the wrong method are closed instead of leaking.
- **DMA is trusted.** A driver with `DMA` can write any physical memory until an
  IOMMU exists (`docs/driver-plan.md` D5); the uid, the class ACL and the audit
  ring limit who can be that driver.
- **Class rules.** `libs/sndpolicy` gives `_snd` claim, map and DMA on
  `os.kernel.dev.audio` and nothing else. The kernel installs it at boot with
  every other driver's class rules (`dev::policy`, issue #481), so `_snd`
  cannot claim a NIC or a USB controller and no other non-root uid can claim
  the sound card (`dev_sys_boot_policy_confines_each_driver_to_its_class`).

## A lesson worth keeping: never free a DMA buffer while the device runs

The kernel treats a driver freeing its own DMA buffer as an explicit stop of the
device (`dev::dma_buffer_freed`: bus mastering and decode off). The first
version of `sndd` freed each stream's staging on close and the next control
request never completed. `sndd` now allocates its DMA (queues and 64 KiB of
staging) once and lends the staging to one stream at a time; regions live until
the driver exits. Any future driver should do the same or re-enable bus
mastering after a free.

## Testing

| Layer | What | Run |
|---|---|---|
| Host unit | `virtio` (20): capability parsing incl. looping/hostile lists, negotiation, queue setup bounds, split-queue chains, exhaustion, index wraparound over 70k round trips, hostile used entries. `virtio-snd` (15): request layouts, `PCM_INFO` parsing, every rate/format/channel/period policy case. `pcm` (6): sine accuracy against libm, pitch, amplitude, degenerate arguments | `cargo test -p virtio -p virtio-snd -p pcm` |
| Mixer and client | `audiomix` (44): stream contract, ownership, limits, mixing/saturation/gain/mute/master, mono, resampled pitch, ring wrap, underruns, hostile ring rewrites, the wire layer's errnos and ring adoption, seeded call scripts. `audioclient` (8): `PlaybackStream` against the real engine, exact output, volume, two streams, stalls, seeded write patterns | `cargo test -p audiomix -p audioclient` |
| Harness unit | The WAV detector and the chord/level detector must fail when they should | `python tools/sound/test_analyze_wav.py`, `python tools/sound/test_mixcheck.py` |
| End to end | QEMU with `-audiodev wav`: the driver's own tone (440 Hz) and `beep`'s through the mixer (880 Hz) must both be in the recording, in order, and an armed interrupt line must have delivered interrupts (`SND:IRQ:PASS delivered=N`); plus `beep probe=1` (malformed/hostile checks and an intruder task), `beep soak=40` (40 open/play/close cycles) and `mixer probe` (the control panel) | `python tools/sound/run.py` |
| Mixing | Two clients at once must sound as one chord at unity level, and a stream at `volume=50` at half the amplitude | `python tools/sound/run.py --mix` |
| Variants | `--services` (supervised by `init` as `_snd` and `_audio`), `--machine q35 --virtio-disk`, `--no-device` (the driver exits cleanly), `--smoke` (`-audiodev none`) | see `tools/sound/README.md` |

The serial markers (`SND:PLAY:PASS`, `BEEP:PLAY:PASS`, ...) only tell the
harness when the guest is done; the verdict is the recording.

## Stream events (#453)

`system/audio/{card}/event` (`idl/audio.midl`, on the central broker) carries
`AudioEvent {stream, kind, frames}`. `audiod` publishes the application
streams' under `{card}` = `mixer`: one `Underrun` per dry spell (a running
stream that played everything it had, counted even when no other stream keeps
the mixer mixing), one `Drained` per completed drain, and while someone
subscribes a `Period` at most every 50 ms per running stream as its position
advances. `sndd` publishes the card's own stream under `virtio-snd0`:
`Underrun` when the device runs out of queued periods (which happens when the
mixer stops feeding it), `Drained`, and `DeviceError` when a device failure
reclaims the stream. The exactly-once logic is `audiomix::events` (host
tests, including a soak); the messengerd `system/` gate lets `_snd` and
`_audio` publish under `system/audio/` alone (`sndpolicy`). Harness:
`python tools/sound/run.py --starve --services` (`beep starve=1` runs its
stream dry and checks its own events).

## Not done

See [`audio-plan.md`](../audio-plan.md) stages A6-A8: a `PlaybackStream`
that sleeps on `system/audio/<card>/event` (published since #453, see above)
instead of polling, and tickless loops (both services still tick at 100 Hz
while sound plays),
persisted volumes and a Settings page, a std/musl transport for xui apps,
capture (`OpenStream` for capture is `ENOTSUP`), formats other than `S16Le`
through the mixer, MSI/MSI-X (INTx only), and `devd` matching with a driver
manifest (the driver is started by `init`'s manifest or the kernel directly).
