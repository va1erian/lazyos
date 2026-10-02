# `os.lazy.audio.v1`

Interface id: `0x536f1f4639cf07f0`

An audio card's control and data-plane interface (docs/driver-plan.md §3.8).

Two services serve it (docs/audio-plan.md). The system mixer `audiod`
serves it under `os.lazy.audio`, the name applications resolve: any number
of streams from any number of clients, each resampled to the card's rate,
scaled by its own volume and the master volume, and mixed. A userspace
audio driver (`sndd`, virtio-sound first) serves it for one card under
`os.lazy.audio.card`; its one stream belongs to the mixer. Control is
request/reply; sample data flows through a shared ring buffer per stream,
so the wire never carries audio. A stream has exactly one owner, the task
that opened it.

**The client owns the ring.** Replies cannot carry buffers (the kernel
refuses transfers in a reply), and the driver must not trust memory a
client can rewrite while the device reads it, so the driver keeps its own DMA
slots and copies committed periods out of the client's ring. A playback
client therefore: `OpenStream` (learn the granted parameters), create a
shared buffer of at least `periods * period_bytes` bytes, `AttachRing` it
(`buffers[0]` of the request), write interleaved samples into it, `Commit`
how many frames it has written, `Start`, and finally `Drain` and
`CloseStream`.

Frame *n* of the stream lives at ring byte `(n mod ring_frames) *
frame_bytes`, where `ring_frames = periods * period_bytes / frame_bytes`.
`Commit` carries the **total** frames written since the ring was attached
(frame numbering restarts at 0 after `Stop`); it must never go backwards and
never run more than `ring_frames` ahead of the `consumed` count the driver
last reported (`consumed` is what it has already copied out of the ring, so
those frames are safe to overwrite), or the call fails with `EINVAL`.
`Position`, which counts frames the device has *played*, never exceeds
`consumed`, so a client that paces itself on `Position` is always safe. The
driver consumes whole periods as they are committed, plus a final short
period during `Drain`. If the device runs out of committed data it plays
silence and the stream keeps running.

The driver grants the closest supported parameters and reports them in the
reply, never failing for a merely unsupported rate or period size. A
request outside `AudioInfo` (unknown format, zero or oversized channel
count, a zero period) fails with `EINVAL`; a stream the card cannot provide
(capture, today) with `ENOTSUP`; a busy card, or a mixer at its stream
limit, with `EBUSY`. Calls on a stream
by anyone but its owner fail with `EACCES`. Failures are returned as the
shared structured error field (see `services::error_field`) instead of the
declared reply fields.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Info | 266462757 | sync | `() -> (info: AudioInfo)` |
| OpenStream | 410137073 | sync | `(dir: U32, format: U32, rate: U32, channels: U32, period_bytes: U32) -> (grant: StreamGrant)` |
| AttachRing | 62355614 | sync | `(stream: U32) -> () transfers (ring: Ring<Samples>)` |
| Commit | 2036391452 | sync | `(stream: U32, written_frames: U64) -> (consumed: U64)` |
| Start | 182978943 | sync | `(stream: U32) -> (ok: Bool)` |
| Stop | 1266644741 | sync | `(stream: U32) -> (ok: Bool)` |
| Drain | 101727161 | sync | `(stream: U32) -> (ok: Bool)` |
| Position | 1652503594 | sync | `(stream: U32) -> (frames: U64)` |
| CloseStream | 973774059 | sync | `(stream: U32) -> ()` |
| SetVolume | 1919741053 | sync | `(stream: U32, gain_q16: U32) -> ()` |
| SetMute | 1285443642 | sync | `(stream: U32, mute: Bool) -> ()` |

## Transfers

Objects a request carries outside its body, in the parcel's
`handles` and `buffers` vectors.

| Method | Name | Slot |
|---|---|---|
| AttachRing | `ring` | `buffers[0]`, a shared buffer holding the rings `Samples` back to back |

## Topics

| Topic | Payload | QoS | Retained | Permissions |
|---|---|---|---|---|
| `system/audio/+/event` | `AudioEvent` | latest | no | `publish:system/audio/+/event`, `subscribe:system/audio/+/event` |

## Rings

| Ring | Layout | Producer | Doorbell / advance | |
|---|---|---|---|---|
| `Samples` | stream | client | advance `Commit` | Interleaved samples, client to driver; `Commit` reports how far the client wrote and replies how far the driver consumed. |

## struct `AudioInfo`

- `streams: U32`
- `formats: U32`
- `rates: U32`
- `channels: U32`

## struct `StreamGrant`

- `stream: U32`
- `dir: U32`
- `format: U32`
- `rate: U32`
- `channels: U32`
- `period_bytes: U32`
- `periods: U32`

## struct `AudioEvent`

- `stream: U32`
- `kind: U32`
- `frames: U64`

## enum `EventKind`

- Underrun, Overrun, Drained, DeviceError

## enum `Direction`

- Playback, Capture

## enum `Format`

- S16Le, S24Le, S32Le, Float32
