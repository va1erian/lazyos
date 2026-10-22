# `sys::audio`

The Rhai module for the Messenger interface `os.lazy.audio.v1`. Wire-level reference: [os.lazy.audio.v1](../../idl/os.lazy.audio.v1.md).

An audio card's control and data-plane interface (docs/driver-plan.md §3.8).

Two services serve it (docs/audio-plan.md). The system mixer `audiod`
serves it under `os.lazy.audio`, the name applications resolve: any number
of streams from any number of clients, each resampled to the card's rate,
scaled by its own volume and the master volume, and mixed. A userspace
audio driver (`sndd`) serves it for one card under `os.lazy.audio.card/<id>`
(`virtio-snd0`, `intel-hda0`, `usb-audio0`); its one stream belongs to the mixer. Control is
request/reply; sample data flows through a shared ring buffer per stream,
so the wire never carries audio. A stream has exactly one owner, the task
that opened it.

**The client owns the ring.** Replies cannot carry buffers (the kernel
refuses objects in a reply), and the driver must not trust memory a
client can rewrite while the device reads it, so the driver keeps its own DMA
slots and copies committed periods out of the client's ring. A playback
client therefore: `OpenStream` (learn the granted parameters), create a
shared buffer of at least `periods * period_bytes` bytes, `AttachRing` it
(the request's `ring`), write interleaved samples into it, `Commit`
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

## Functions

### `info()`

`Info() -> (info: AudioInfo)`

Describe the card: stream count and the formats, rates and channel
counts it accepts.

Returns `info`: `AudioInfo` (map (`AudioInfo`)).

```rhai
let result = sys::audio::info();
```

### `open_stream(dir, format, rate, channels, period_bytes)`

`OpenStream(dir: U32, format: U32, rate: U32, channels: U32, period_bytes: U32) -> (grant: StreamGrant)`

Open a stream. `dir` and `format` are `Direction` and `Format`
ordinals (enums travel as `U32`). The reply carries the stream index and
the granted parameters. The stream is idle until a ring is attached and
it is started.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `dir` | `U32` | int |
| `format` | `U32` | int |
| `rate` | `U32` | int |
| `channels` | `U32` | int |
| `period_bytes` | `U32` | int |

Returns `grant`: `StreamGrant` (map (`StreamGrant`)).

```rhai
let result = sys::audio::open_stream(dir, format, rate, channels, period_bytes);
```

### `commit(stream, written_frames)`

`Commit(stream: U32, written_frames: U64) -> (consumed: U64)`

Announce that the client has written up to `written_frames` (a total,
see the interface notes). Replies with the frames the driver has
consumed so far.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `stream` | `U32` | int |
| `written_frames` | `U64` | int |

Returns `consumed`: `U64` (int).

```rhai
let result = sys::audio::commit(stream, written_frames);
```

### `start(stream)`

`Start(stream: U32) -> (ok: Bool)`

Start (or restart) transfer on a stream with an attached ring.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `stream` | `U32` | int |

Returns `ok`: `Bool` (bool).

```rhai
let result = sys::audio::start(stream);
```

### `stop(stream)`

`Stop(stream: U32) -> (ok: Bool)`

Stop transfer; committed periods not yet queued to the device are
discarded. The stream can be started again.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `stream` | `U32` | int |

Returns `ok`: `Bool` (bool).

```rhai
let result = sys::audio::stop(stream);
```

### `drain(stream)`

`Drain(stream: U32) -> (ok: Bool)`

Play everything committed, then stop. Replies once drained.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `stream` | `U32` | int |

Returns `ok`: `Bool` (bool).

```rhai
let result = sys::audio::drain(stream);
```

### `position(stream)`

`Position(stream: U32) -> (frames: U64)`

Frames the device has played since the last `Start`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `stream` | `U32` | int |

Returns `frames`: `U64` (int).

```rhai
let result = sys::audio::position(stream);
```

### `close_stream(stream)`

`CloseStream(stream: U32) -> ()`

Close a stream and release its ring. Also implied when the owner exits.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `stream` | `U32` | int |

Returns `()`.

```rhai
let result = sys::audio::close_stream(stream);
```

### `set_volume(stream, gain_q16)`

`SetVolume(stream: U32, gain_q16: U32) -> ()`

Scale the stream by `gain_q16` (16.16 fixed point: 65536 is unity, 0
silent, at most 262144, four times unity); larger values fail with
`EINVAL`. Samples are scaled as they are copied out of the ring and
saturate instead of wrapping. A driver that cannot scale the granted
format fails with `ENOTSUP`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `stream` | `U32` | int |
| `gain_q16` | `U32` | int |

Returns `()`.

```rhai
let result = sys::audio::set_volume(stream, gain_q16);
```

### `set_mute(stream, mute)`

`SetMute(stream: U32, mute: Bool) -> ()`

Silence the stream (`mute`) or restore its volume, without losing it.
A muted stream keeps consuming and its position keeps moving.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `stream` | `U32` | int |
| `mute` | `Bool` | bool |

Returns `()`.

```rhai
let result = sys::audio::set_mute(stream, mute);
```

Not callable from a script (the request carries a kernel object): `AttachRing`.

## Types

### `AudioInfo`

Card capabilities.

| Name | IDL type | Script value |
|---|---|---|
| `streams` | `U32` | int |
| `formats` | `U32` | int |
| `rates` | `U32` | int |
| `channels` | `U32` | int |
| `name` | `String` | string |

```rhai
let value = sys::audio::new_audio_info();
```

### `StreamGrant`

Number of concurrent playback streams.
Supported `Format` bitmap: bit *n* is set when the `Format` with
ordinal *n* is supported (bit 0 `S16Le`, 1 `S24Le`, 2 `S32Le`,
3 `Float32`). Reserved bits are zero; a client ignores them.
Supported sample-rate bitmap: bit 0 8000 Hz, 1 11025, 2 16000,
3 22050, 4 32000, 5 44100, 6 48000, 7 88200, 8 96000, 9 176400,
10 192000. Reserved bits are zero.
Maximum channels per stream.
What to show a person for this card (`Built-in audio`,
`USB: <product>`); the mixer reports its current output's.
The parameters an opened stream actually runs at.

| Name | IDL type | Script value |
|---|---|---|
| `stream` | `U32` | int |
| `dir` | `U32` | int |
| `format` | `U32` | int |
| `rate` | `U32` | int |
| `channels` | `U32` | int |
| `period_bytes` | `U32` | int |
| `periods` | `U32` | int |

```rhai
let value = sys::audio::new_stream_grant();
```

### `AudioEvent`

One stream event, the payload of `system/audio/{card}/event`.
`frames` is the stream position when it happened.

| Name | IDL type | Script value |
|---|---|---|
| `stream` | `U32` | int |
| `kind` | `U32` | int |
| `frames` | `U64` | int |

```rhai
let value = sys::audio::new_audio_event();
```

### `EventKind`

A string; the constants of this module are:

- `EVENT_KIND_UNDERRUN` = `"Underrun"`
- `EVENT_KIND_OVERRUN` = `"Overrun"`
- `EVENT_KIND_DRAINED` = `"Drained"`
- `EVENT_KIND_DEVICE_ERROR` = `"DeviceError"`
- `EVENT_KIND_PERIOD` = `"Period"`

`EVENT_KIND` is the array of all of them.

### `Direction`

A string; the constants of this module are:

- `DIRECTION_PLAYBACK` = `"Playback"`
- `DIRECTION_CAPTURE` = `"Capture"`

`DIRECTION` is the array of all of them.

### `Format`

A string; the constants of this module are:

- `FORMAT_S16_LE` = `"S16Le"`
- `FORMAT_S24_LE` = `"S24Le"`
- `FORMAT_S32_LE` = `"S32Le"`
- `FORMAT_FLOAT32` = `"Float32"`

`FORMAT` is the array of all of them.

## Topics

### `system/audio/{card}/event`

An `EventKind` ordinal.
Xruns, drain completion and progress, for clients that would rather
not poll `Position` (issue #453), on the central broker. `{card}` is
`mixer` for the streams `audiod` serves applications (the stream ids
its `OpenStream` granted) and the driver's card name (`virtio-snd0`)
for the card's own stream, which belongs to the mixer.

Payload `AudioEvent`, QoS `buffered`.

Helpers: `EVENT_PATTERN`, `event_topic(card)`, `on_event(card, handler)`, `subscribe_event(card)`, `publish_event(card, payload)`.

```rhai
sys::audio::on_event(card, |event| print(event.payload));
```

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.audio.v1 on the wire](../../idl/os.lazy.audio.v1.md)
