# Tracker module player (`modplay`)

A small ProTracker `.mod` player whose job is to **demonstrate the LazyOS audio
stack end to end**: file read from the VFS, in-process mixing, a paced stream to
`sndd` over `os.lazy.audio.v1`, and a verdict from the sound harness. It is a
demo, not a music app: 4-channel ProTracker only at first.

## Why a tracker

- Mixing is cheap and integer-only, which suits userspace being soft-float
  (`libs/pcm`: "everything here is fixed point").
- Timing is derived from frames produced, not from sleeping, so the 10 ms clock
  granularity and lost ticks (#344) do not matter.
- It needs exactly the things a real client needs: a stream, a ring, pacing,
  clean shutdown, a volume, and a data file. Each missing piece is a filed gap.

## Current state (verified)

| Piece | Status |
|---|---|
| `sndd` + `os.lazy.audio.v1` (`idl/audio.midl`): Open/AttachRing/Commit/Start/Stop/Drain/Position | works; `beep` is the only client |
| Format | S16Le, 48 kHz stereo is what `beep` and its soak use; driver snaps params |
| Mixing, volume, >1 stream per client | none; mix in-process |
| Client library | only in the `user` crate (`user/src/messenger/audio.rs`), blocking, raw ring math left to the caller |
| Underrun/Drained events | declared, not published |
| Data files on the boot volume | only `*.md` (`docs_embed`) |
| Verification | `tools/sound/run.py` records QEMU's output; `analyze_wav.py` judges it |

## Gaps (filed)

| Issue | Gap | Needed for |
|---|---|---|
| #451 | `libs/audioclient`: shared `PlaybackStream` (open, blocking `write`, `position`, `drain`, RAII close), usable from native and musl | clean API; the GUI front end (M4) |
| #452 | per-stream volume/mute in `os.lazy.audio.v1` | master volume without scaling samples in the client |
| #453 | publish `system/audio/{card}/event` | underrun accounting; sleeping instead of polling |
| #454 | embed binary assets (`/assets`) with a provenance manifest | shipping a `.mod` without `include_bytes!` |

None of these block M0-M2; M3 is gated on #451 and #454, M4 on #451.

## Design

### `libs/modplay` (`no_std` + `alloc`, no I/O, host-testable)

The library never touches the OS. It turns bytes into samples:

```rust
let module = Module::parse(&bytes)?;            // validated, owns pattern + sample data
let mut p = Player::new(&module, 48_000);       // fixed-point, deterministic
loop {
    let n = p.render(&mut buf);                 // interleaved stereo i16, returns frames written
    if n == 0 { break }                         // song ended (or loops, see Options)
}
p.position();                                   // (order, row) for a UI
```

Source split (each file under 500 lines): `module.rs` (header, sample table,
patterns), `parse.rs` (validation), `effects.rs`, `mixer.rs` (voices,
resampling, panning), `tables.rs` (period table, sine, finetune), `player.rs`
(tick/row sequencing).

Decisions:

- **Format scope:** `M.K.` / `M!K!` / `4CHN`, 31 samples. 6/8-channel and
  `FLT4` are rejected with a distinct error, not misplayed. Later extension.
- **Effects (M0-M1):** arpeggio, slide up/down, tone portamento, vibrato, volume
  slide, set volume, position jump, pattern break, set speed/tempo, `E6` loop,
  `E9` retrigger, `EC` cut, `ED` delay, fine slides. Unsupported effects are
  ignored and counted, never an error.
- **Timing:** samples per tick = `rate * 5 / (2 * bpm)` with the remainder
  carried, so no float and no drift.
- **Mixing:** per-voice 16.16 fixed-point position and increment
  (`PAL_CLOCK / (period * 2) / rate`), linear interpolation optional, Amiga
  hard panning (L R R L) with a 0-100% stereo-separation option, 32-bit
  accumulator, saturating narrow to i16.
- **Untrusted input:** the file is attacker-controlled. Bound the pattern
  count (<= 128), clip sample lengths to the bytes actually present, validate
  loop start/length, reject truncation with a typed `ModError`, and never index
  by file-supplied values unchecked. No panics on any input (fuzzed, below).
- **Songs that never end:** `Options { loops: Option<u32> }`; the end is
  detected by revisiting an `(order, row)` after a jump, otherwise the demo
  would play forever on looping modules.

### `user/src/bin/modplay.rs` (native, like `beep`)

```
modplay /assets/demo.mod [--loops N] [--volume 0-100] [--rate 48000]
```

Reads the file (`user/src/files.rs::read_all`), parses, opens a 48 kHz stereo
S16Le stream, renders one period at a time into the ring and commits it.
Pacing is on `Position` (never exceeds `consumed`, so it is safe). Prints
serial markers: `MODPLAY:LOAD:<title>`, `MODPLAY:PLAY`, `MODPLAY:ROW:<order>:<row>`
(rate-limited), `MODPLAY:DONE:underruns=<n>`. Keep the stream busy: `sndd`
reclaims a stream idle for 10 s, so the player must never stall mid-song.

Until #451 lands the binary talks to `user::messenger::audio::Client` behind a
tiny local `Sink` trait (`write(&[i16])`), so swapping to `PlaybackStream` is a
one-file change.

### Demo content

A real module file is not needed to prove the pipeline, and licensing is
cleaner without one: `tools/sound/gen_mod.py` writes a tiny synthetic
`demo.mod` (square/saw single-cycle samples, a bass line, arpeggio, slide and
tempo change). A freely licensed (CC0) tune can be added later through the
asset manifest from #454; its licence goes in the manifest, not in the
commit message.

## Verification

Per `AGENTS.md`, audio claims are judged by the recording, not serial markers.

1. **Library unit tests** (`cargo test -p modplay`): parse good/bad headers;
   period-to-frequency for known notes (A-3 = 440 Hz at period 428 with the
   PAL clock); volume and panning math; speed/tempo; `B`/`D` flow; loop
   detection; every effect against a hand-computed trace.
2. **Golden render:** the synthetic module rendered on the host; assert the
   dominant frequency per row window and the sample count, plus a checked-in
   hash to catch accidental behaviour change.
3. **Known-bad inputs:** empty file, truncated mid-pattern, 0 and 255 song
   length, order table pointing past the pattern count, sample length larger
   than the file, loop beyond sample end.
4. **Fuzz** (`fuzz/` crate, `fuzz::run(&[u8])` shared with seeded in-tree
   tests, seeds in `fuzz/seeds/modplay`): parse then render N frames; must
   never panic or allocate unboundedly.
5. **Soak:** render several hours of audio from looping modules in release;
   assert no panic, bounded memory and constant per-call work.
6. **Guest, audible:** a `tools/sound/run.py --modplay` variant boots with
   `LAZYOS_SOUND=1`, runs `modplay /assets/demo.mod`, records the WAV, and
   `analyze_wav.py` is extended to check the note sequence (windowed dominant
   frequency matches the song's notes in order), no long silence, and
   `underruns=0`. `cargo test -p pcm -p virtio-snd` keeps passing.
7. **Screenshot session** for M4 only: `qemu_session.py` script capturing the
   pattern view mid-song.

## Phases

| Phase | Deliverable | Depends on |
|---|---|---|
| M0 | `libs/modplay`: parser, mixer, core effects, unit + golden + fuzz tests, host-side `render-to-wav` example | none |
| M1 | Remaining effects, loop detection, soak, stereo separation option | M0 |
| M2 | `modplay` native bin on the raw audio client behind `Sink`; `tools/sound/run.py --modplay` with the synthetic module (ELF embedded for the test only) | M0 |
| M3 | Switch to `libs/audioclient` (#451); master volume via #452; module shipped from `/assets` (#454); count real underruns via #453 | #451 #452 #453 #454 |
| M4 (optional) | `xui-app` front end: pattern view, VU meters, open dialog, play/pause. Musl app, uses `audioclient` | #451, M3 |

## Out of scope

Multiple simultaneous streams or a system mixer (`audiod`), `.xm/.it/.s3m`,
sample-accurate Amiga filter emulation, capture, and any kernel change.
