# Audio plan: system-wide mixing and clean APIs

**Goal.** Any number of programs play sound at the same time, each at its own
rate and volume, through one small API, while the card driver stays a dumb,
hardened, single-client pipe. This is the `audiod` that
[`architecture/audio.md`](architecture/audio.md) and
[`driver-plan.md`](driver-plan.md) §3.8 deferred, together with issues #451
(client library), #452 (volume and mute) and #453 (events).

## The shape

```
 beep   modplay   any app          mixer (CLI)   Settings (A7)
   \       |       /                    |            |
    PlaybackStream (libs/audioclient)   MixerControl (libs/audioclient)
            \            os.lazy.audio.v1 | os.lazy.audio.mixer.v1
             \                            |
              +-----> audiod  "os.lazy.audio"   (_audio, uid 905, no caps)
                        | libs/audiomix: per-stream ring -> resample ->
                        | stream gain -> sum -> master gain -> saturate
                        v os.lazy.audio.v1, one stream, 48 kHz stereo S16
                      sndd  "os.lazy.audio.card"   (_snd, uid 901, CAP_DEV_CLAIM)
                        v virtqueues + DMA
                      virtio-sound
```

- **One protocol, two servers.** `audiod` serves `os.lazy.audio.v1`, the same
  stream protocol the card serves: open, attach a shared ring, commit, start,
  drain, close. A client cannot tell the mixer from a card except that the
  mixer accepts many streams, every rate and mono or stereo.
- **The card belongs to the mixer.** `sndd` registers as `os.lazy.audio.card`;
  `audiod` opens its one stream at startup and holds it while it runs (stopping
  the device when idle), so nothing else can take the card from it.
- **The client owns its ring**, exactly as with the driver: the mixer reads it
  with raw copies of ranges computed from its own counters, so a client
  scribbling over its ring only produces noise in its own stream.
- **Control is a separate interface.** `os.lazy.audio.mixer.v1` lists streams
  and sets any stream's volume and the master volume. It never exposes samples
  or the stream calls, so holding it changes *how loud* others are, never
  *what* they play. Who may hold it is the Messenger policy's decision.
- **Fixed point throughout.** User programs are soft-float: 16.16 gains,
  32.32 resampling positions, 64-bit accumulators, saturation at the end.

## Stages

| Stage | What | Status |
|---|---|---|
| A0 | `sndd`, `os.lazy.audio.v1`, `beep`, the recording harness, `modplay` | done (D6, #455) |
| A1 | `libs/audiomix`: the engine (stream table, contract checks, resampler, gain, mixing, positions by card marks) and its wire layer, host-tested with a seeded script fuzz | **done** |
| A2 | IDL: `SetVolume`/`SetMute` in `os.lazy.audio.v1` (#452); `os.lazy.audio.mixer.v1` (`idl/audio_mixer.midl`) | **done** |
| A3 | `audiod`: the service, card pacing, deferred `Drain` replies, reclaim, `_audio` identity; `sndd` moves to `os.lazy.audio.card` and applies volume to its own copy | **done** |
| A4 | `libs/audioclient` (#451): typed `Client`/`MixerControl` and the blocking `PlaybackStream` over a `Transport` trait; `user::audio` is the native transport; `beep` and `modplay` use it | **done** |
| A5 | `mixer` shell command; harness `--mix` (chord + half-volume, judged by `tools/sound/mixcheck.py`); `mixer probe` and the reworked `beep probe` as boot evidence | **done** |
| A6 | Events (#453): publish `system/audio/{card}/event` and per-stream underrun/drained; a client `write()` that sleeps on the topic instead of polling; tickless `audiod` and `sndd` loops | next |
| A7 | Persisted volumes (`confd` `user/<uid>/audio/...`, `driver-config-plan.md`), a Sound page in Settings on `MixerControl`, a std/musl `Transport` so xui apps get `PlaybackStream` | next |
| A8 | Capture streams, `Float32`/`S24Le` clients, a better resampler (polyphase), per-session stream policy, MSI-X | later |

## How a period is made (A1, A3)

Every tick `audiod` asks the card how far it has played (`Commit` with the
current total, then `Position`) and keeps **three periods** (3 x 1024 frames,
64 ms at 48 kHz) queued beyond that. For each period it:

1. takes, from every running stream that has committed at least one output
   period's worth of input (or anything at all, once it is draining), exactly
   the input frames the resampler will pop (`Resampler::input_needed`), split
   where the ring wraps;
2. converts them to the card's rate (passthrough when equal, bit for bit) and
   to stereo (mono is copied to both sides);
3. adds them, scaled by the stream's gain, into 64-bit accumulators (a muted
   stream still consumes, so its position moves);
4. scales the sum by the master gain, saturates to 16 bits and queues the
   period on the card;
5. leaves a *mark* per stream: the card frame at the end of the period and the
   stream's `consumed` after it. When the card's position passes a mark, that
   stream's `Position` becomes the marked value, so `Position <= consumed <=
   committed` always holds and `Drain` completes exactly when the last mixed
   frame has played.

A running stream with less than a period committed contributes silence (one
underrun counted per dry spell) instead of a fragment, as the driver does. The
card is started once the first periods are queued and stopped after 0.5 s with
nothing to play; while stopped the mixer touches it every 3 s so the driver
never reclaims the stream.

## Contract changes for clients

- The mixer grants the closest standard rate and up to two channels; the
  period may be raised so the ring holds two output periods of input (a
  192 kHz client with a tiny ring would otherwise play in bursts).
- A client may hold four streams; the mixer holds sixteen. `EBUSY` beyond.
- `Drain` replies when the stream's last frame has *played*, without blocking
  the mixer (the reply is deferred); a concurrent `Stop`/`CloseStream` answers
  it with `ECANCELED`.
- Without a card the stream calls fail with `ENODEV`; the control panel still
  answers (`GetMaster` reports `card: false`).

## Security

- `audiod` runs as `_audio` (uid 905) with **no capabilities**: no device, no
  DMA, no `CAP_SETUID`. Compromising it yields the right to make noise.
- Stream calls are bound to the kernel-stamped sender of `OpenStream`; every
  other task gets `EACCES` (`beep probe` spawns an intruder to prove it).
- Every counter, gain and size a client sends is validated in `libs/audiomix`
  before it is used; a transferred buffer is adopted only by a well-formed
  `AttachRing` and closed on every other path (300-request flood in the probe).
- Residual: the card's name is resolvable by anyone and `sndd` cannot learn a
  caller's uid without `CAP_SETUID`, so it relies on `audiod` holding the one
  stream. Once the kernel stamps sender uids on messages (or the uid policy
  is loaded), the card interface should be restricted to `_audio`.

## Verification

| Layer | Run |
|---|---|
| Engine + wire layer (stream contract, mixing, resampler, gain, service errnos, ring adoption, seeded scripts; `sndd`'s `SetVolume`/`SetMute` path, `tests_volume.rs`, including a half gain measuring -6 dB) | `cargo test -p audiomix` (`FUZZ_CASES=3000` for a soak) |
| Client library against the real engine (ring wrap, start/drain, volume, two streams, stalls, seeded write patterns) | `cargo test -p audioclient` |
| Generated codecs | `cargo test -p messenger-generated --test audio` |
| Mix detector must fail when it should | `python tools/sound/test_mixcheck.py` |
| End to end: driver tone, a client through the mixer, probes, soak | `python tools/sound/run.py` (`--services` checks `_audio`) |
| End to end: a chord of two clients at unity, then a half-volume tone | `python tools/sound/run.py --mix` |
