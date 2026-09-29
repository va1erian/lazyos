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
- `formats: U32` — supported [`Format`](#enum-format) bitmap: bit *n* is set
  when the `Format` with ordinal *n* is supported (bit 0 `S16Le`, 1 `S24Le`,
  2 `S32Le`, 3 `Float32`)
- `rates: U32` — supported sample-rate bitmap: bit 0 8000 Hz, 1 11025, 2 16000,
  3 22050, 4 32000, 5 44100, 6 48000, 7 88200, 8 96000, 9 176400, 10 192000

Bits not listed are reserved: a driver sets them to zero and a client ignores
them.
- `channels: U32` — maximum channels

## enum `Direction`

- Playback, Capture

## enum `Format`

- S16Le, S24Le, S32Le, Float32
