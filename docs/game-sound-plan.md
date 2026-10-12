# Plan: sound for Doom and SRB2Kart

> **Status: S0 (the spike) done 2026-10-12; S1-S5 not started.** This plan
> covers the sound that [`doom-port-plan.md`](doom-port-plan.md) left in D6
> and that [`srb2kart-port-plan.md`](srb2kart-port-plan.md) calls K2. Both
> games ship silent today: Doom is built without `FEATURE_SOUND`
> (`doom/build.rs`), SRB2Kart's `srb2kart/c/lazyos/i_sound.c` is a set of
> stubs. The spike read both engines' sound code at the pinned revisions,
> took a census of the game data, and measured Vorbis decoding and two
> threads on one vCPU inside LazyOS; its findings are in
> [S0 findings](#s0-findings) and already folded into the decisions below.

**Goal:** sound effects and music in both games, through the system mixer
(`audiod`, [`audio-plan.md`](audio-plan.md)) like every other client: one
`os.lazy.audio.v1` stream per game, opened through `libs/audioclient`'s
`PlaybackStream` on the musl transport (`xui_app::platform::audio`, the
path emusic already uses). The tray's Volume applet and the `mixer` command
then work on the games with no extra code. The verdict is a recording, never
the serial markers (AGENTS.md, "Sound harness").

**Non-goals:** a new Messenger interface (none is needed), MIDI out, sound in
the headless checks' frame CRCs, SRB2Kart's GME formats (`NOGME` stays),
SRB2Kart's one XM track and pitch variation (both left out, see S3), CD
audio, Quake (its `Pcm` records can reuse the output half later).

## What each engine asks for (confirmed in S0)

| | Doom (doomgeneric `dcb7a8d`, Chocolate Doom 2.x sound layer) | SRB2Kart (Kart-Public v1.6, `i_sound.h`) |
|---|---|---|
| Effects | DMX lumps (`DS*`): 8-bit unsigned mono, a 8-byte header, 16 pad bytes each side. Freedoom: 69, at 22050 (44), 11025 (22), 16000, 17990, 44100 Hz | 1263 audio lumps found by name (the last WAD wins): **1057 Ogg Vorbis** (832 stereo; 931 at 44100 Hz, the rest at 11025-48019 Hz, some odd rates such as 47954), **199 DMX**, 7 WAV. 177 more `DS*` lumps are textures and flats, not sounds |
| Channel calls | `sound_module_t` named **`DG_sound_module`**: `Init(use_sfx_prefix)`, `Shutdown`, `GetSfxLumpNum`, `Update`, `UpdateSoundParams(channel, vol, sep)`, `StartSound(sfx, channel, vol, sep)`, `StopSound`, `SoundIsPlaying`, `CacheSounds`. **No pitch.** `snd_channels` = 8 | `I_GetSfx`/`I_FreeSfx`, `I_StartSound(id, vol, sep, pitch, priority, channel)` returning a handle, `I_StopSound`, `I_SoundIsPlaying`, `I_UpdateSoundParams`, `I_SetSfxVolume`. `snd_channels` = 64. Upstream's SDL build **ignores pitch** (`(void)pitch; // Mixer can't handle pitch`) |
| Ranges and pan | vol 0..127, sep clamped 0..254; Chocolate's SDL law `left = (254 - sep) * vol / 127`, `right = sep * vol / 127` | vol 0..255, sep 0..255, scaled by `sfx_volume` 0..31: `volume = (vol + 1) * sfx_volume / 62`, `left = min((255 - sep) * 2, 255)`, `right = min(sep * 2, 255)` (centre is full on both sides, unlike Doom) |
| Music | `D_*` lumps; **Freedoom's 41 are standard MIDI** (`MThd`), not MUS; `GENMIDI` and `DMXGUS` are in `freedoom1.wad`. doomgeneric has `mus2mid.c` but **no OPL code** | **124 Ogg Vorbis** `O_*` tracks (all 44100 Hz, 122 stereo; 102.7 MiB packed, 1.78 GiB decoded, longest 301 s), loop tags `LOOPPOINT` (67, samples at 44.1 kHz), `LOOPMS` (8), none (49, loop to start); 1 XM (`O_KMAP36`, an OpenMPT module); 3 MIDI in `srb2.srb` (SRB2's own) |
| Music calls | `music_module_t` named **`DG_music_module`**, used unconditionally (`InitMusicModule`): `Init`, `Shutdown`, `SetMusicVolume` (0..127), `PauseMusic`, `ResumeMusic`, `RegisterSong(data, len)`, `UnRegisterSong`, `PlaySong(handle, looping)`, `StopSong`, `MusicIsPlaying`, `Poll` | `I_LoadSong(data, len)`, `I_PlaySong(looping)`, `I_StopSong`, `I_UnloadSong`, `I_SetMusicVolume`, `I_SetInternalMusicVolume` (0..100), `I_SongType`, `I_Get/SetSongPosition` and `I_GetSongLength` (the jingle system resumes a track where it was), `I_SetSongLoopPoint`, the fade family whose **callbacks** (`S_StopMusic`, `S_ChangeMusicToQueue`) run when a fade ends. `I_SetSongSpeed` is called (`S_SpeedMusic(1.2f / 1.4f)` on the final lap and in boss code) but upstream honours it only for GME music, so Ogg never changes speed in the official build |
| Selection | The sound module is chosen when its device list holds `snd_sfxdevice`, default `SNDDEVICE_SB` | none: our `i_sound.c` is the only implementation; `sounds.kart` and `music.kart` are loaded already (`d_main.c` adds them when `HAVE_SDL` is undefined, which it is) |
| Gotchas | `i_sound.c` includes `<SDL_mixer.h>` under `FEATURE_SOUND` (unused) and `I_BindSoundVariables` binds `use_libsamplerate` and `libsamplerate_scale`, which only the SDL/Allegro modules define | none found |

Both engines call their sound code from the game thread only, and an update
hook once per frame (Doom: `S_UpdateSounds` -> `I_UpdateSound`, which also
calls `Poll`).

## Decisions

| Question | Choice | Why |
|---|---|---|
| Where the mixing happens | **In the game process**, in a shared Rust crate, then one stereo S16 stream to `audiod` | `audiod` takes four streams per client and Kart has 64 channels; per-voice volume and pan changes at frame rate would be a Messenger call each. One stream also gives the user one volume per game in the mixer. |
| The shared crate | **`libs/gamemix`**, `no_std` + `alloc`, fixed point (16.16 gains, 32.32 positions, 64-bit accumulators), host-tested in the OS workspace; `doom/` and `srb2kart/` depend on it by path | Both ports need the same voices, pan laws, resampling and decoders; fixed point makes a mix bit-identical on the host and in the guest, so a headless sound CRC means something. It has its own linear-interpolating voice: `audiomix::Resampler` is a streaming converter and would pull the wire layer (`virtio-snd`, `messenger-generated`) into the games. |
| Output rate | 48000 Hz stereo | The card's rate: `audiod` passes it through untouched, so each voice is resampled once, by us. |
| The output thread | **A pull thread** in `xui_app::platform::audio` (new `pull.rs`) that **opens the stream itself** and asks a fill callback for one period at a time; the game thread only changes voice state under a short lock | Writing from the game loop underruns whenever a frame is slow or a lump loads. The ring (`SharedRing`, a raw mapping) is not `Send`, so the stream lives and dies on the thread that opened it: exactly what emusic's `emusic-lazyaudio` worker already does, verified by its recordings. S0 measured the scheduling (below). |
| Queue depth | 10 ms fill periods, **four** queued on our side (40 ms) to start; the pull thread measures how late it wakes and deepens the queue (up to eight) when a wake misses a period, and never shrinks it again in that run; `audiod` adds its own 64 ms | S0: an output thread woke at worst 0.5 ms late under WHPX with the main thread spinning, but 60 ms late under TCG. ~105 ms total keeps a shot from sounding late; TCG runs are only judged, not played. |
| Vorbis decoder | **`lewton`** (pure Rust, MIT/Apache-2.0) in `gamemix` behind a `vorbis` feature, in place of the port plan's `stb_vorbis` | S0, in the guest under WHPX: a 82 s track decodes 419x faster than real time (0.24% of the core); all 673 Ogg effects of `sounds.kart` in 2.6 s. Host-testable and fuzzable with the rest of the crate, no second C fetch; the binary is GPL-3.0-or-later, which takes Apache-2.0. |
| Music decoding | **Streamed**: the music voice decodes packets as it plays, a second or so ahead | 1.78 GiB decoded for the whole set, 50 MiB for the longest track. Cheap enough (above) to do on the output thread. |
| SRB2Kart effect decoding | `I_GetSfx` returns a handle at once and **a worker thread decodes**; a sound whose decode is not done when it starts plays from where it would be, or is dropped if more than 100 ms late. No pre-decoding of everything | S0, under WHPX: median 2.2 ms, p99 29 ms, worst 90 ms per effect, which would hitch the game thread; under TCG the worst is 1.1 s. A 35 Hz game loop beside a busy decode worker stayed within 10 ms of its tics (46 ms under TCG). Decoding everything would cost 228 MiB. |
| Decoded memory | Effects kept as i16 at their own rate and channel count (never expanded to 48 kHz), in a cache with a 64 MiB ceiling that evicts the least recently played | All of Kart's effects decoded are 228 MiB (207 of it Ogg, 832 of those lumps stereo); a race uses a small part of them. |
| Pitch and speed (SRB2Kart) | **Ignored**, as upstream's SDL build does: `I_SetSongSpeed` returns `false`, `pitch` is unused | Parity with the official release; the voice can step at any rate, so pitch is a small follow-up if wanted. |
| SRB2Kart's other music | The XM track (`O_KMAP36`) and the 3 MIDI lumps: `I_LoadSong` returns `false` (the map plays silent), logged once as `SRB2KART:SOUND:SKIP <name> <format>` | One map out of the set; an XM player is out of scope (`libs/modplay` is ProTracker only). |
| Fade callbacks | The output thread runs the fade ramp; the game thread fires the callback from `I_UpdateSound` once the ramp reports done | The callbacks change engine state (`S_StopMusic`), which must stay on the game thread. |
| Doom music | **Decided: OPL FM synthesis from `GENMIDI`**: Chocolate Doom **3.1.0** (`chocolate-doom-3.1.0.tar.gz`, SHA-256 `f2c64843dcec312032b180c3b2f34b4cb26c4dcdaa7375a1601a3b1df11ef84d`), fetched like doomgeneric, never vendored. Compiled as-is: `src/i_oplmusic.c`, `src/midifile.c`, `opl/opl3.c` (Nuked OPL3), `opl/opl_queue.c`. **Not** `opl/opl.c` (it needs SDL's mutex and condition variable for `OPL_Delay`) | `i_oplmusic.c` plays both MUS (through `mus2mid`) and the MIDI Freedoom ships; `GENMIDI` is in the IWAD, so nothing new ships. A General MIDI synth would need a SoundFont (size, licence) for a sound Doom never had on DOS. |
| The OPL glue | `doom/c/opl_lazyos.c` implements the `opl.h` API that `opl.c` and `opl_sdl.c` provide upstream (`OPL_Init` returning OPL3, `OPL_WriteRegister`, the callback queue, `OPL_Lock`/`Unlock`, pause, `OPL_SetSampleRate`) over a pull function `lz_opl_render(frames)` the output thread calls; Nuked OPL3 renders at 48 kHz itself (`OPL3_GenerateResampled`). `DG_music_module` wraps `music_opl_module`. A force-included `doom/c/choco_compat.h` declares `opl_driver_ver_t` and `I_SetOPLDriverVer` (Chocolate 3.x's `i_sound.h` has them, doomgeneric's does not) | Keeps "no upstream file patched"; `opl_sdl.c` (521 lines) is the model. The `const snddevice_t *` field difference between the two `i_sound.h` is a qualifier warning only. |
| Doom's build | `FEATURE_SOUND` defined; `doom/c/stub/SDL_mixer.h` (empty) on the include path; our module defines `use_libsamplerate = 0` and `libsamplerate_scale` | The two gotchas above, without patching `i_sound.c`. |
| Missing card | `Audio::try_connect()` returning `None` (or the stream failing) makes the game **silent, never fatal**: `<GAME>:SOUND:OFF reason=...`; a retry every 5 s picks up a restarted `audiod` | A desktop with no sound card must play exactly as today. |
| Headless | Headless mixes through the same `gamemix` into a buffer on its virtual clock and prints a sound CRC beside the frame CRC; it never opens a stream. The frame CRCs (`crc=b3580eb0` for Kart's check) must not change | Sound must not touch game state; a changed frame CRC is the alarm. |
| Permissions | `os.lazy.audio.v1` added to both manifests (emusic's line), confirmed by a `LAZYOS_LABEL_TRACE=1` run with no `LABEL:DENY` | AGENTS.md: permissions come from a traced run. |
| Launchers | **Decided:** `--doom` and `--srb2kart` imply `--sound auto` (as `--emusic` and `--modplayer` do in `tools/demo_args.py`); the GUI's `session_sound` adds the card for them; tests in `tools/test_run_demo_apps.py` and `tools/lazygui/test_catalog.py` | Every app launchable from both front ends, with its sound. |

## The seam

Both ports keep "everything Messenger-shaped on the Rust side". The C
platform files translate the engine's calls into a few new `lz_*` hooks (in
each port's `hooks.rs`, forwarding to a `Sound` object next to its
`Backend`):

```c
/* effects: the engine's lump bytes, copied by Rust; a handle comes back */
int32_t  lz_sfx_load(const uint8_t *lump, size_t len);           /* <0: unusable */
void     lz_sfx_free(int32_t sfx);
int32_t  lz_sfx_start(int32_t sfx, int32_t left, int32_t right); /* 0..255 each: a voice */
void     lz_sfx_update(int32_t voice, int32_t left, int32_t right);
void     lz_sfx_stop(int32_t voice);
int      lz_sfx_playing(int32_t voice);
/* music, SRB2Kart (Doom's goes through lz_opl_render instead) */
int      lz_music_load(const uint8_t *data, size_t len);         /* Ogg: 0 ok */
void     lz_music_play(int looping), lz_music_stop(void), lz_music_pause(int on);
void     lz_music_volume(int32_t volume);                        /* 0..100 */
void     lz_music_fade(int32_t from, int32_t to, uint32_t ms);
int      lz_music_fade_done(void);                               /* 1 once: fire the callback */
uint32_t lz_music_position_ms(void), lz_music_length_ms(void);
int      lz_music_seek_ms(uint32_t ms);
/* Doom's OPL: the output thread asks the C side for 48 kHz stereo frames */
void     lz_opl_attach(void (*render)(int16_t *out, uint32_t frames));
```

Each engine's own pan law stays in its C file and reaches Rust as left and
right gains, so `gamemix` knows no engine. Voices carry a generation in
their handle, so a stale handle the engine keeps after a voice was stolen
addresses nothing. Every length, rate and header field of a lump is
validated in `gamemix` before use (a PWAD or a Kart addon is untrusted
input), and the parsers get a seeded fuzz entry point shared with a `fuzz/`
target, as `libs/ext2fs` and the network crates do.

`gamemix` holds, in files under 500 lines each: `voice.rs` (a sample ref,
32.32 position and step, 16.16 gains, loop range, generation), `mixer.rs`
(N voices, steal-oldest when full, `fill(&mut [i16])`: voices, then the
music source, then saturation), `dmx.rs`, `wav.rs`, `vorbis.rs` (feature:
whole-lump for effects, streaming for music, `LOOPPOINT`/`LOOPMS` with
LOOPPOINT counted at 44.1 kHz as upstream does, seek by sample), `music.rs`
(a `MusicSource` trait over Vorbis or an external render callback; fades
as a gain ramp, pause, position), `cache.rs` (the decoded-effect cache and
its ceiling), `fuzz.rs` and the tests.

## Stages

**S0 — spike and census. Done.** See [S0 findings](#s0-findings);
`tools/srb2kart/sound_census.py` stays as the census tool.

**S1 — `libs/gamemix` and the pull output.** The crate with host tests
(both pan laws against the upstream formulas, an 11025 Hz voice resampled
to 48 kHz measured for pitch, odd rates such as 47954 Hz, voice stealing,
stale handles, fades reaching their target in the asked number of frames,
loop points sample-exact, saturation instead of wrap, the cache ceiling)
and a seeded fuzz of the DMX, WAV and Vorbis paths
(`FUZZ_CASES=20000 cargo test -p gamemix --release seeded`).
`xui_app::platform::audio::pull` with a host test over `audioclient`'s test
transport (a steady fill never starves the ring; a stalled fill produces
silence, not a stall). No game changes yet.

**S2 — Doom effects.** `doom/build.rs` defines `FEATURE_SOUND`, adds the
`SDL_mixer.h` stub and `doom/c/i_lazysound.c` (`DG_sound_module`, listing
`SNDDEVICE_SB`; `CacheSounds` and `StartSound` take lumps with
`W_CacheLumpNum` and pass the bytes to `lz_sfx_load`) and a silent
`DG_music_module` until S4. `lazydoom --sound-check` plays a fixed sequence
(pistol, door, a sweep of `sep` from left to right, eight effects at once)
with markers `DOOM:SOUND:OPEN rate= period= periods=`,
`DOOM:SOUND:CHECK:<step>`, `DOOM:SOUND:CHECK:DONE`. Headless adds
`sound_crc=` to `DOOM:HEADLESS:PASS`. Manifest permission, launcher flags
and GUI wiring. Session `tools/screenshot/examples/doom_sound.json`
(install, `--sound-check` in the Terminal, then the game from Games through
the attract demo) and `tools/doom/sound_run.py`: build with
`LAZYOS_SOUND=1 LAZYOS_DOOM=1 LAZYOS_UI_PROBE=1 LAZYOS_RESET_OS=1`, record,
judge with `tools/doom/sound_judge.py` (and `test_sound_judge.py`): the
check's steps found in order by onset, the pan sweep moving energy from the
left channel to the right, no clipping run, underruns from `audiod`'s
`system/audio/{card}/event` below a limit, the attract demo not silent.
Done when the recording passes and the frame CRC is unchanged.

**S3 — SRB2Kart effects and music (the port plan's K2).** `i_sound.c`
forwards every `i_sound.h` call to the seam, with upstream's volume and
pan formulas; effects through the decode worker and cache; music with loop
points, fades and their callbacks, position and length, pause; XM and MIDI
skipped with the marker. `srb2kart.elf --sound-check` (markers on the
`SRB2KART` channel) plays a few effects and the title music across its loop
point; session `srb2kart_sound.json` (title, menus, a Time Attack race) and
`tools/srb2kart/sound_run.py` / `sound_judge.py`: the check's segments, the
loop seam without a gap, music and effects both present during the race,
underruns bounded, the headless frame CRC still `b3580eb0`. Data rule: the
recording is Kart Krew's music and effects, so **the WAV is judged where it
is made and never uploaded**; CI publishes the verdict and level plots only.

**S4 — Doom music.** `tools/doom/fetch.py` gains Chocolate Doom 3.1.0
(pinned above); `doom/build.rs` compiles the four files with
`choco_compat.h` force-included, plus `opl_lazyos.c`; `DG_music_module`
wraps `music_opl_module`. `--sound-check` gains a music step (`D_E1M1`
for five seconds); the judge looks for sustained output across several
bands in that window, and headless gets a music CRC. Measured: the OPL
render's share of the core under WHPX. `doom/NOTICE` adds the Chocolate
Doom files (GPL-2.0-or-later) and Nuked OPL3 (LGPL-2.1-or-later).

**S5 — follow-ups (each independent).** Quake's `Pcm` records into the same
pull output (`quake-port-plan.md` D6); pitch variation in Kart; an XM
player for `O_KMAP36`; a host `--render-sound` that writes the headless mix
to a WAV.

## S0 findings

Read at the pins: doomgeneric `dcb7a8d` (`i_sound.c`, `i_sound.h`,
`i_sdlsound.c`, `s_sound.c`, `d_main.c`), Kart-Public v1.6 (`s_sound.c`,
`sdl/mixer_sound.c`, `d_main.c`), Chocolate Doom 3.1.0 (`src/i_oplmusic.c`,
`opl/`). Their facts are the table at the top.

**Data census** (`python tools/srb2kart/sound_census.py`; Freedoom by the
same WAD reader):

| Data | Kind | Format | Lumps | Packed | Decoded (i16) |
|---|---|---|---|---|---|
| SRB2Kart | effects | Ogg Vorbis | 1057 | 21.8 MiB | 206.8 MiB |
| SRB2Kart | effects | DMX | 199 | 10.4 MiB | 20.7 MiB |
| SRB2Kart | effects | WAV | 7 | 0.7 MiB | 0.7 MiB |
| SRB2Kart | music | Ogg Vorbis | 124 | 102.7 MiB | 1780 MiB |
| SRB2Kart | music | XM / MIDI | 1 / 3 | 0.4 MiB | n/a |
| Freedoom 1 | effects | DMX | 69 | | |
| Freedoom 1 | music | MIDI | 41 | | |

**Decoding and threads, in the guest.** A throwaway static-musl program
(`lewton` 0.10, release, the lumps embedded) booted as `abi-init` on a
128 MiB console image, one vCPU (`cpus=1`):

| Measure | Host (Windows, native) | LazyOS, WHPX | LazyOS, TCG |
|---|---|---|---|
| 82 s stereo track, decode | 85 ms (958x real time) | 195 ms (419x, 0.24% of the core) | 2.0 s (41x, 2.5%) |
| 673 Ogg effects, total | 1.5 s | 2.6 s | 28 s |
| per effect: median / p90 / p99 / max | 1.4 / 3.7 / 20 / 40 ms | 2.2 / 6.9 / 29 / 90 ms | 22 / 70 / 425 / 1095 ms |
| a 10 ms output thread while the main thread spins: wakes, worst late | 300, 0.6 ms | 299, 0.5 ms | 300, 60 ms |
| a 35 Hz loop (15 ms of work per tic) beside a decode worker: worst tic late | 1.0 ms | 10.3 ms | 46 ms |

So: Vorbis is cheap enough to stream music on the output thread, effects
must not decode on the game thread, the scheduler gives a sleeping output
thread its turn promptly under WHPX, and TCG needs the deeper queue.

## Verification

| Layer | Run |
|---|---|
| Mixer, decoders, fades, loops, pan, cache, fuzz | `cargo test -p gamemix` (`FUZZ_CASES=20000 ... --release seeded`) |
| Pull output against the client library's test transport | `cargo test --manifest-path xui-app/Cargo.toml` (the `platform::audio::pull` tests; Linux-only, WSL on Windows) |
| The ports' pure modules (argv, seam validation) | `cargo test --manifest-path doom/Cargo.toml --lib`, the same for `srb2kart/` |
| The judges fail when they should | `python tools/doom/test_sound_judge.py`, `python tools/srb2kart/test_sound_judge.py` |
| End to end, by recording | `python tools/doom/sound_run.py`, `python tools/srb2kart/sound_run.py` |
| No permission gap | the S2/S3 sessions on an image with `LAZYOS_LABEL_TRACE=1`: no `LABEL:DENY` |
| Sound does not touch game state | headless frame CRCs unchanged in both ports |

## Risks

- **One core, two or three threads** (game, output, Kart's decode worker).
  S0 shows WHPX schedules them well; the judges count `audiod` underruns so a
  regression shows.
- **TCG.** Decoding is ten times slower and wakes can be 60 ms late; CI
  sessions under TCG use the deeper queue and may judge only the
  `--sound-check` part.
- **Memory.** The 64 MiB effect cache plus the streamed music stay well
  inside the 1 GiB default; Kart's data is otherwise read on demand.
- **OPL glue.** `opl_lazyos.c` reimplements the part of `opl.c` that needs
  SDL; if `i_oplmusic.c` leans on more of it than the API listed above, the
  glue grows, not the upstream files.
- **Data rule (SRB2Kart).** Recordings carry Kart Krew's audio: same rule
  as the data itself, never committed, uploaded or attached. The census
  tool prints only counts and sizes.

## Owner decisions (2026-10-12)

1. **Doom's music is OPL** (the retro FM sound): S4 stays as planned, not
   deferred.
2. **`--doom` and `--srb2kart` add a sound card by default**, as `--emusic`
   does (the Launchers row above); the GUI's session runs record it.
