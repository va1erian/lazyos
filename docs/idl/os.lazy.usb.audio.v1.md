# `os.lazy.usb.audio.v1`

Interface id: `0xd48ce91568d0be08`

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

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Describe | 582948360 | sync | `() -> (device: Device)` |
| Attach | 145305188 | sync | `(staging: Buffer) -> ()` |
| Configure | 373171691 | sync | `(rate: U32) -> (granted: U32)` |
| Start | 182978943 | sync | `() -> ()` |
| Stop | 1266644741 | sync | `() -> ()` |
| Release | 1830722334 | sync | `() -> ()` |
| Submit | 1081664837 | sync | `(slot: U32, offset: U32, len: U32) -> ()` |
| Reap | 1527513801 | sync | `() -> (progress: Progress)` |

## Objects

Kernel objects a request carries, in the order of the parcel's
object list (the index each field must hold).

| Method | Field | Type | Object |
|---|---|---|---|
| Attach | `staging` | `Buffer` | `objects[0]`, a shared buffer |

## struct `Device`

- `vendor: U32`
- `product: U32`
- `name: String`
- `version: U32`
- `rates: U32`
- `channels: U32`
- `bits: U32`
- `sync: U32`
- `high_speed: Bool`
- `port: String`

## struct `Progress`

- `completed: U64`
- `underruns: U32`
- `missed: U32`

## enum `Sync`

- None, Async, Adaptive, Synchronous
