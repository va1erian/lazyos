# `sys::usb_audio`

The Rhai module for the Messenger interface `os.lazy.usb.audio.v1`. Wire-level reference: [os.lazy.usb.audio.v1](../../idl/os.lazy.usb.audio.v1.md).

A USB audio playback function, as `usbd` drives it
(docs/usb-audio-plan.md).

`usbd` owns every xHCI controller, so the isochronous endpoint of a USB
speaker, headset or DAC can only be fed by it. `sndd usb=1` serves the
device as an ordinary sound card (`os.lazy.audio.v1` under
`os.lazy.audio.card/usb-audio0`) and drives it through this interface,
which `usbd` serves under `os.lazy.usb.audio` while an audio device is
bound (one at a time) and withdraws when it goes away.

The model is `sndd`'s card model: periods are *submitted* in order from
one shared staging buffer and later *completed* (`Reap` counts them).
Samples are always interleaved stereo `S16Le`; `usbd` converts them to the
device's subslot and channel count as it copies them into its own DMA,
so the staging buffer is read only by copy and a rewrite of it changes
what plays, never what `usbd` touches. `Stop` completes every period
still queued, as a stopped virtio-sound stream hands back its buffers.

Only `_snd` (uid 901) and root may call it: anyone else gets `EACCES`.
No device bound (it was unplugged) is `ENODEV`; a call out of order
(`Submit` before `Configure`, a range outside the staging buffer) is
`EINVAL`. Failures are returned as the shared structured error field.

## Functions

### `describe()`

`Describe() -> (device: Device)`

Periods played (or dropped by a stop) since `Configure`.
Times the isochronous ring ran dry (`usbd` was starved).
Service intervals the controller missed.

Returns `device`: `Device` (map (`Device`)).

```rhai
let result = sys::usb_audio::describe();
```

### `configure(rate)`

`Configure(rate: U32) -> (granted: U32)`

Pick the alternate setting and rate for a stereo stream at `rate` Hz
(the closest the device has), switch the device to it and reset the
counters. Returns the rate it runs at.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `rate` | `U32` | int |

Returns `granted`: `U32` (int).

```rhai
let result = sys::usb_audio::configure(rate);
```

### `start()`

`Start() -> ()`

Start sending; until periods arrive the device hears silence.

Returns `()`.

```rhai
let result = sys::usb_audio::start();
```

### `stop()`

`Stop() -> ()`

Stop sending; every queued period counts as completed.

Returns `()`.

```rhai
let result = sys::usb_audio::stop();
```

### `release()`

`Release() -> ()`

Stop and switch the device to its zero-bandwidth alternate.

Returns `()`.

```rhai
let result = sys::usb_audio::release();
```

### `submit(slot, offset, len)`

`Submit(slot: U32, offset: U32, len: U32) -> ()`

Queue one period: `len` bytes at `offset` of the staging buffer.
`slot` is the caller's tag, echoed nowhere; periods complete in order.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `slot` | `U32` | int |
| `offset` | `U32` | int |
| `len` | `U32` | int |

Returns `()`.

```rhai
let result = sys::usb_audio::submit(slot, offset, len);
```

### `reap()`

`Reap() -> (progress: Progress)`

Returns `progress`: `Progress` (map (`Progress`)).

```rhai
let result = sys::usb_audio::reap();
```

Not callable from a script (the request carries a kernel object): `Attach`.

## Types

### `Device`

What the bound device is and can play.

| Name | IDL type | Script value |
|---|---|---|
| `vendor` | `U32` | int |
| `product` | `U32` | int |
| `name` | `String` | string |
| `version` | `U32` | int |
| `rates` | `U32` | int |
| `channels` | `U32` | int |
| `bits` | `U32` | int |
| `pacing` | `U32` | int |
| `high_speed` | `Bool` | bool |
| `port` | `String` | string |

```rhai
let value = sys::usb_audio::new_device();
```

### `Progress`

The product string the device reports, empty when it has none.
The class revision: 1 (UAC 1.0) or 2 (UAC 2.0).
Rate bitmap, the `AudioInfo.rates` numbering of `os.lazy.audio.v1`.
Channels and significant bits of the alternate a 48 kHz stereo
stream would use.
How the device paces itself: a `Sync` ordinal (`sync` is a
reserved word in Rhai, which every interface is generated for).
The device runs at high speed (microframes) rather than full.
The `usbd` port name (`<hc>-<port>[.<hub port>...]`).
How far the device got.

| Name | IDL type | Script value |
|---|---|---|
| `completed` | `U64` | int |
| `underruns` | `U32` | int |
| `missed` | `U32` | int |

```rhai
let value = sys::usb_audio::new_progress();
```

### `Sync`

A string; the constants of this module are:

- `SYNC_NONE` = `"None"`
- `SYNC_ASYNC` = `"Async"`
- `SYNC_ADAPTIVE` = `"Adaptive"`
- `SYNC_SYNCHRONOUS` = `"Synchronous"`

`SYNC` is the array of all of them.

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.usb.audio.v1 on the wire](../../idl/os.lazy.usb.audio.v1.md)
