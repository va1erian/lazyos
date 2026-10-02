# `os.lazy.audio.mixer.v1`

Interface id: `0x39a0c9a99b23a265`

The system mixer's control interface (docs/audio-plan.md stage A2).

`audiod` serves it next to `os.lazy.audio.v1` under the same name,
`os.lazy.audio`. Where `os.lazy.audio.v1` lets a stream's owner drive its
own stream, this interface is the volume control panel of the machine: it
lists every stream and sets any stream's volume and the master volume.

It never exposes samples, rings or the stream calls themselves, so holding
it lets a task change *how loud* others are, never *what* they play or hear.
Who may hold it is the Messenger policy's decision (an installed app needs
the permission explicitly); the service itself does not ask.

Gains are 16.16 fixed point: 65536 is unity, 0 silent, at most 262144
(four times unity). Failures are returned as the shared
structured error field (see `services::error_field`): an unknown stream is
`ENOENT`, an out-of-range gain `EINVAL`.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| ListStreams | 711680798 | sync | `() -> (streams: Array<StreamStatus>)` |
| SetStreamVolume | 953940373 | sync | `(stream: U32, gain_q16: U32, mute: Bool) -> ()` |
| GetMaster | 1362695053 | sync | `() -> (master: Master)` |
| SetMaster | 301176513 | sync | `(gain_q16: U32, mute: Bool) -> ()` |

## struct `StreamStatus`

- `stream: U32`
- `owner: U64`
- `state: U32`
- `rate: U32`
- `channels: U32`
- `gain_q16: U32`
- `mute: Bool`
- `frames: U64`
- `underruns: U32`

## struct `Master`

- `gain_q16: U32`
- `mute: Bool`
- `card: Bool`
- `rate: U32`
- `channels: U32`
- `streams: U32`
- `max_streams: U32`

## enum `StreamState`

- Idle, Running, Stopped, Draining, Drained
