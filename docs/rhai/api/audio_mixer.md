# `sys::audio_mixer`

The Rhai module for the Messenger interface `os.lazy.audio.mixer.v1`. Wire-level reference: [os.lazy.audio.mixer.v1](../../idl/os.lazy.audio.mixer.v1.md).

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

## Functions

### `list_streams()`

`ListStreams() -> (streams: Array<StreamStatus>)`

Every open stream, in opening order.

Returns `streams`: `Array<StreamStatus>` (array of map (`StreamStatus`)).

```rhai
let result = sys::audio_mixer::list_streams();
```

### `set_stream_volume(stream, gain_q16, mute)`

`SetStreamVolume(stream: U32, gain_q16: U32, mute: Bool) -> ()`

Set one stream's gain and mute flag, whoever owns it.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `stream` | `U32` | int |
| `gain_q16` | `U32` | int |
| `mute` | `Bool` | bool |

Returns `()`.

```rhai
let result = sys::audio_mixer::set_stream_volume(stream, gain_q16, mute);
```

### `get_master()`

`GetMaster() -> (master: Master)`

The master gain, the mute flag and what the card runs at.

Returns `master`: `Master` (map (`Master`)).

```rhai
let result = sys::audio_mixer::get_master();
```

### `set_master(gain_q16, mute)`

`SetMaster(gain_q16: U32, mute: Bool) -> ()`

Set the master gain and mute flag, applied after mixing.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `gain_q16` | `U32` | int |
| `mute` | `Bool` | bool |

Returns `()`.

```rhai
let result = sys::audio_mixer::set_master(gain_q16, mute);
```

## Types

### `StreamStatus`

What one open stream is doing.

| Name | IDL type | Script value |
|---|---|---|
| `stream` | `U32` | int |
| `owner` | `U64` | int |
| `state` | `U32` | int |
| `rate` | `U32` | int |
| `channels` | `U32` | int |
| `gain_q16` | `U32` | int |
| `mute` | `Bool` | bool |
| `frames` | `U64` | int |
| `underruns` | `U32` | int |

```rhai
let value = sys::audio_mixer::new_stream_status();
```

### `Master`

The owner's task id (the kernel-stamped sender of `OpenStream`).
A `StreamState` ordinal.
Frames played so far (the owner's `Position`).
Times the stream ran dry while playing.
The output side of the mixer.

| Name | IDL type | Script value |
|---|---|---|
| `gain_q16` | `U32` | int |
| `mute` | `Bool` | bool |
| `card` | `Bool` | bool |
| `rate` | `U32` | int |
| `channels` | `U32` | int |
| `streams` | `U32` | int |
| `max_streams` | `U32` | int |

```rhai
let value = sys::audio_mixer::new_master();
```

### `StreamState`

A string; the constants of this module are:

- `STREAM_STATE_IDLE` = `"Idle"`
- `STREAM_STATE_RUNNING` = `"Running"`
- `STREAM_STATE_STOPPED` = `"Stopped"`
- `STREAM_STATE_DRAINING` = `"Draining"`
- `STREAM_STATE_DRAINED` = `"Drained"`

`STREAM_STATE` is the array of all of them.

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.audio.mixer.v1 on the wire](../../idl/os.lazy.audio.mixer.v1.md)
