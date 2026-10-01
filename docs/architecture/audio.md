# Audio: the virtio-sound driver and `os.lazy.audio.v1`

**What it is.** The first userspace driver on the device core
([`devices.md`](devices.md)) and the reference for the ones that follow. `sndd`
is an ordinary ring-3 program: it claims QEMU's `virtio-sound-pci` function
through syscall 23, drives it with a modern virtio-PCI transport, and serves the
class interface `os.lazy.audio.v1` (`idl/audio.midl`) over Messenger. `beep` is
the smallest client. The kernel knows nothing about audio: it provides buses,
BARs, DMA and IRQs, exactly as `docs/driver-plan.md` D2 promises.

This is stage D6 of the driver plan, together with the part of D5 it needs (the
modern virtio transport). The NIC driver, `devd` and the second driver classes
are still open.

**Key files**

| Path | Role |
|---|---|
| `libs/virtio/` | Modern (virtio 1.x) PCI transport, pure `no_std` with host tests: `caps.rs` capability parsing, `transport.rs` status/feature negotiation/queue setup/notify, `queue.rs` split virtqueue with an in-struct free list, `regs.rs` |
| `libs/virtio-snd/` | virtio-sound wire protocol (`wire.rs`) and the mapping from an audio request to stream parameters (`params.rs`) |
| `libs/pcm/` | Fixed-point sine generator (`tone.rs`); user programs are soft-float |
| `idl/audio.midl` | The interface, compiled by `midlc` into `libs/generated` and `docs/idl/os.lazy.audio.v1.md` |
| `user/src/bin/sndd.rs`, `sndd/` | The driver: `device.rs` claim + BAR mapping, `card.rs` queues and control/transmit paths, `stream.rs` slots and DMA staging, `session.rs` ownership/ring/commit logic, `service.rs` request dispatch |
| `user/src/messenger/audio.rs` | Blocking client of the interface |
| `user/src/bin/beep.rs`, `beep/` | Tone client, hostile-input probe, intruder and soak modes |
| `user/src/bin/modplay.rs`, `modplay/` | ProTracker module player over a blocking ring sink; `libs/modplay` does the parsing and mixing (`docs/tracker-plan.md`) |
| `kernel/src/process/linux/native.rs` | `beep` and `modplay` in the table of native programs a shell may run, so the desktop Terminal has a `beep [freq_hz [ms]]` command |
| `tools/sound/` | The harness: `run.py` boots QEMU with `-audiodev wav`, `analyze_wav.py` judges the recording |

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
counters must be monotonic and at most one ring ahead of what the driver has
copied out; the driver consumes whole periods as they are committed, and a final
short period during `Drain`. If the device runs dry it plays silence. `Drain` is
terminal for a stream; `Stop` discards the uncommitted tail and restarts frame
numbering at 0.

## Security

- **Identity.** Under `init` the driver runs as system uid 901 (`_snd`) with
  only `CAP_DEV_CLAIM` (`SNDD:CRED uid=901 caps=0x100` in the boot log); a
  kernel-spawned boot runs it as root. It has no `CAP_SETUID`.
- **One owner per stream.** The owner is the kernel-stamped sender of
  `OpenStream`; nothing in a request body names a caller. Any other task gets
  `EACCES`, and a second open gets `EBUSY`. An owner silent for 10 s with
  nothing left to play loses the stream (`SNDD:RECLAIM`).
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
| Harness unit | The WAV detector must fail when it should: silence, wrong pitch, missing/extra/swapped tones, too short, too quiet, killed-emulator headers | `python tools/sound/test_analyze_wav.py` |
| End to end | QEMU with `-audiodev wav`: the driver's own tone (440 Hz) and `beep`'s (880 Hz) must both be in the recording, in order, and an armed interrupt line must have delivered interrupts (`SND:IRQ:PASS delivered=N`); plus `beep probe=1` (28 malformed/hostile checks and an intruder task), `beep soak=40` (40 open/play/close cycles) | `python tools/sound/run.py` |
| Variants | `--services` (supervised by `init` as `_snd`), `--machine q35 --virtio-disk`, `--no-device` (the driver exits cleanly), `--smoke` (`-audiodev none`) | see `tools/sound/README.md` |

The serial markers (`SND:PLAY:PASS`, `BEEP:PLAY:PASS`, ...) only tell the
harness when the guest is done; the verdict is the recording.

## Not done

The class ACL rule for `os.kernel.dev.audio` (the fabric is still in its
bootstrap-allow window; once a policy loads, the `_snd` label needs claim, map
and DMA rules), capture (`OpenStream` for capture is `ENOTSUP`), the `system/audio/<card>/event`
topic (declared in the IDL, not yet published), MSI/MSI-X (INTx only), a
tickless serve loop (interrupts wake control waits, but the loop still ticks at
100 Hz while a stream runs), mixing (a later `audiod`), volume, `devd` matching and a
driver manifest (the driver is started by `init`'s manifest or the kernel
directly), and a per-client stream count above one.
