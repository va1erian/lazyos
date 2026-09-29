# `os.lazy.audio.v1`

Interface id: `0x536f1f4639cf07f0`

An audio card's control and data-plane interface. The driver owns one or more
streams; playback/capture data flows through a shared ring buffer with a
position fence, and underrun/xrun is published on `system/audio/<card>/event`.
Mixing and per-app volume are a later `audiod` service, not the driver's job.
See [`docs/driver-plan.md`](../driver-plan.md) §3.7.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Info | 266462757 | sync | `() -> (info: AudioInfo)` |
| OpenStream | 410137073 | sync | `(dir: Direction, format: Format, rate: U32, channels: U32, period_bytes: U32) -> (stream: U32, buffer: Buffer)` |
| Start | 182978943 | sync | `(stream: U32) -> (ok: Bool)` |
| Stop | 1266644741 | sync | `(stream: U32) -> (ok: Bool)` |
| Drain | 101727161 | sync | `(stream: U32) -> (ok: Bool)` |
| Position | 1652503594 | sync | `(stream: U32) -> (frames: U64)` |

`OpenStream` returns a `Buffer` handle for the stream's ring plus its index;
the caller maps it at the returned offset and fences against the position.

## struct `AudioInfo`

- `streams: U32` — number of concurrent streams
- `formats: U32` — supported [`Format`](#enum-format) bitmap
- `rates: U32` — supported rates bitmap (Hz)
- `channels: U32` — maximum channels

## enum `Direction`

- Playback, Capture

## enum `Format`

- S16Le, S24Le, S32Le, Float32
