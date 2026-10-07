# Media playback plan (downscoped): emusic on LazyOS

**Goal.** Run emusic on LazyOS and play MP3s. No new generic media library:
the work is **one alternate backend for emusic** (`LazyBackend`, next to
`BassBackend`) over the sound path LazyOS already has, written so the decoding
and output half can be lifted into a shared `libs/` crate later without
touching emusic's player.

## What exists

- **emusic** (`va1erian/emusic`): `crates/player` reaches audio only through
  `AudioBackend` and `BackendChannel` (`crates/player/src/backend.rs`). `BassBackend`
  is the single implementation; BASS is a DLL loaded with `libloading`, which
  cannot exist in a static musl binary, so `bass` goes behind a cargo feature
  (18 files name `bass::`: the scanner's module/MIDI tags, `ui/src/backend/*`,
  `player` SID/tracker code).
- **LazyOS**: `audiod` mixes client streams (S16, 8-192 kHz, mono/stereo,
  per-stream volume, resampling); `libs/audioclient` has the blocking
  `PlaybackStream`. Missing for a musl app: a `Transport` (audio-plan A7), as
  `xui-app/src/tray.rs` has one for `trayclient`.

## Design

```
emusic-player (unchanged)  --AudioBackend/BackendChannel-->  LazyBackend
                                                              |
        emusic-lazyaudio (new crate in the emusic repo, cfg feature `lazyos`)
          decode.rs   Mp3Decoder: Source -> interleaved f32 + StreamInfo   <- the seam
          output.rs   Output trait + PlaybackStream impl                   <- the seam
          channel.rs  BackendChannel: decode thread, ring, clock, on_end
                                                              |
                          audioclient::PlaybackStream (musl Transport)  -> audiod
```

The two `<- the seam` files are the extraction boundary. They depend on
nothing from emusic (no `emusic_*` types, no `PlayerError`; their own small
`Error`), take a `Read + Seek` source and a sink trait, and expose
`StreamInfo { rate, channels, duration: Option<_>, seek: Exact|Approximate }`
and `fn read(&mut self, out: &mut [f32]) -> Result<usize>` / `fn seek(Duration)`.
Only `channel.rs` knows emusic. Extraction later = move those two files to
`libs/mediaplay`, add a `Format` registry, and `channel.rs` becomes a thin
adapter. Until a second consumer or format exists, none of that is built.

Choices, each reversible:

- **Decoder: symphonia** (`symphonia-bundle-mp3`, `symphonia-core`), pure Rust,
  gives probing, ID3, Xing/LAME info and seeking. It is MPL-2.0, compatible
  with both emusic (MIT) and LazyOS (GPL). Fallback
  if speed under TCG or the licence blocks it: a small pure-Rust Layer III
  decoder behind the same `decode.rs` API.
- **Tags/duration for the library scanner stay with `lofty`**, which emusic
  already uses; the backend only needs duration for `duration()`.
- **Clock = what was played.** `position()` is `audiod`'s per-stream
  `Position` (frames played), not frames decoded, so pause, seek and underrun
  stay correct. Polling at first; A6's event topic later.
- **Volume** maps to `PlaybackStream::set_volume` (no software gain).
- **Visualizer**: `fft()`/`samples()` are computed in emusic from a small
  ring of the last played frames the channel keeps (the same f32 the decoder
  produced).
- **Non-MP3 files** return `PlayerError` from `open`, so the library scanner
  and queue skip them. FLAC/WAV/Ogg are a later addition inside `decode.rs`
  (symphonia already handles them); MIDI/SID/trackers stay BASS-only.

## Stages

| Stage | Where | What | Done when |
|---|---|---|---|
| **P0** | lazyos + emusic | Spike, **done**: results in "P0 findings" below. | see below |
| **P1a** | emusic | Bump `xui` from `f589eb6` to LazyOS's `112b411` and migrate `frontend-portable` to the builder/layout API (the only large item, see findings); `rfd` behind a `FilePicker` seam; `emusic-sid` and `bass` behind features. Mergeable on its own: Windows keeps working. | `cargo test` and the portable shot tool unchanged on Windows |
| **P1** | emusic | Feature-gate `bass` and Windows-only crates; `emusic-lazyaudio` with `decode.rs` + `output.rs`, host-tested (fixtures: CBR, VBR+Xing, ID3v2/v1, mono, MPEG-2 22.05 kHz, truncated, garbage prefix; seek accuracy; seeded fuzz of the decoder over arbitrary bytes). A memory `Output` makes the tests need no OS. | `cargo test -p emusic-lazyaudio` |
| **P2** | emusic | `LazyBackend`/`LazyChannel` (play, pause, stop, seek with `SeekSupport` honest, duration, volume, `on_end`, `fft`/`samples`, capabilities); run `emusic-player`'s existing backend-contract tests against it. | player tests pass with `LazyBackend` |
| **P3** | lazyos | `tools/emusic/build.py` (pinned emusic revision, zig, `target/pkg/emusic.lzp`, as `tools/doom/build.py`), `LAZYOS_EMUSIC=1`, `run_demo.py --emusic`, GUI control and `tools/lazygui/catalog.py` entries with `test_catalog.py` cases, manifest permissions from a `LAZYOS_LABEL_TRACE=1` run (`os.lazy.audio` resolve, file access), `mimed` for `audio/mpeg`. Session `tools/screenshot/examples/emusic.json`, markers `EMUSIC:UP`, `EMUSIC:PLAY:<track>`, `EMUSIC:POS`, `EMUSIC:END`. | Screenshots show emusic playing an MP3 with a moving position |
| **P4** | lazyos | `tools/emusic/run.py`: boot, play a fixture of known tones (A4 2 s then C5 2 s), record with `-audiodev wav`, judge frequency and timing, a seek lands on the second tone, pause records silence, volume 0.5 is -6 dB; `test_judge.py` proves the judge can fail. | Judge passes under WHPX/KVM |
| **Later** | | Extract the seam into `libs/mediaplay` (+ `Format` registry, `play` shell command, rhai module, gapless via LAME delay/padding, other formats), when a second consumer asks. | n/a |

## Verification

Sound is judged from the recording, not serial markers (AGENTS.md). Host
tests carry correctness and fuzzing; the session carries pixels. No kernel
change is planned; if P0 needs one it ships with a `kernel/src/tests/` suite and
a soak.

## Risks and decisions

1. **Licence (settled):** symphonia is MPL-2.0, compatible with MIT apps and
   with GPLv2/v3 (LazyOS's README requires GPLv3-compatible dependencies;
   emusic has no dependency licence policy and already ships LGPL and BASS
   DLLs). Add a symphonia entry to emusic's `THIRD-PARTY-NOTICES.md`.
2. **Speed under TCG:** measured, not a risk (see findings).
3. **No gapless in v1:** emusic's queue will show a small gap between tracks
   until the extraction adds LAME delay/padding trimming and preload.
4. **Port size is mostly not audio:** it is the xui migration (P1a), see findings.

## P0 findings (spike, 2026-10-07)

Method: a scratch copy of emusic (`main` at `0ce7bf1`), `cargo check --target
x86_64-unknown-linux-musl -p emusic` with zig as the C compiler
(`tools/xui/zig.py`'s environment) on the pinned `nightly-2026-09-25`; nothing
in either repo was changed.

**(a) Audio transport: already exists.** `xui-app/src/platform/audio.rs`
(`xui_app::platform::audio::Audio`) is the musl `audioclient::Transport`
(Messenger over `int 0x80`, display shared buffers for the ring), used by the
LazyRAD player. Audio plan item A7's transport half is done; P0(a) is not work.

**(b) emusic builds for musl, almost as is.** With no source change, these
type-check for musl: SQLite (`rusqlite` bundled C, compiled by zig), `lofty`,
`notify`, `emusic-library`, `-platform`, `-client`, `-metadata`, `-search` and
the `xui` crates on emusic's own pin. Two blockers, both off the MP3 path:

| Blocker | Cause | Fix |
|---|---|---|
| `emusic-sid` | cRSID needs GCC nested functions; zig's clang rejects them | feature-gate SID (bass and SID are not on LazyOS anyway) |
| `rfd` | needs `gtk3` or `xdg-portal`; neither exists | a `FilePicker` seam over the ~10 `rfd::FileDialog` call sites (settings pickers, export, folder picker); on LazyOS use xui-app's Open dialog |

With those two stubbed, `emusic-player`, `emusic-ui`, `emusic-frontend-portable`
and the `emusic` binary all compile for musl. `bass` compiles too (it is only a
`libloading` wrapper) but must not be used: no `dlopen` in a static binary.

**The real cost is xui, not audio.** emusic pins `xui` `f589eb6`; LazyOS pins
`112b411`, 101 commits later (a straight line, no divergence). Both must be one
revision (a single `xui_core` links, as `xui-app/Cargo.toml` notes). Bumping
emusic breaks `frontend-portable` with 184 errors in 37 of its 64 files
(12.9k lines), all the same cause: xui's CHANGELOG "Widgets are built only
through builders in layouts" made every rect constructor private (`Button::new`,
`ListView::with_model`, `Split::row`, `Menu::bar`, ...) and replaced them with
`arrange` builders (`button()`, `list()`, `panel(layout)`, `menu_bar()`) mounted
through `Ui::root/mount`. Those are the first errors rustc reports; a layout
rewrite usually surfaces more behind them (`measure` replaces `natural_size`,
spacing/margins renamed). Expect this to be most of the port: the heaviest
files are the settings pages (`playback/tracker.rs` 24, `visualization/form.rs`
22, `appearance.rs` 15, `server.rs` 14), then the tag editor, properties and
database dialogs, the album grid toolbar and the top bar. The alternative,
keeping emusic on the old xui and teaching LazyOS to host it, is not viable:
`xui-app` and every LazyOS app are written against `112b411`.

**The window seam is small.** emusic names its backend in one place
(`run.rs`: `Rc::new(WinitBackend::new())` into `xui_core::run_app`) and uses
three window-level calls through `WindowChrome` (`set_drag_region`,
`minimize`, `toggle_maximize`, `is_maximized`). On LazyOS the entry becomes
`xui_core::app(title).backend(LazyOSBackend)` as in `xui_app::launch::run`;
the chrome calls go through the same `Backend` trait (the compositor draws the
frame, so the app's own caption band should be off: `Decorations::System`, as
the macOS path already does).

**(c) Decode speed is not a concern.** symphonia (`mp3` feature only), release
build, 120 s of 128 kbit/s stereo 44.1 kHz: 0.06 s, about 2000x realtime on the
host. Even a 50x emulation slowdown leaves 40x margin.

### Where the complexity is

| Piece | Size | Notes |
|---|---|---|
| Audio backend (`emusic-lazyaudio`, P1-P2) | small | transport exists; symphonia is fast; trait seam is clean |
| xui bump + `frontend-portable` migration (P1a) | **large** | 37 files, builder API; independent of LazyOS, benefits the Windows build too |
| `rfd`, SID, BASS gating | small | feature flags and one picker trait |
| LazyOS entry/packaging (P3) | medium | follows the Doom pattern; `LazyOSBackend`, permissions from a label trace |
| Not yet looked at | unknown | file-system layout (`dirs` crate on LazyOS), `notify` (inotify) at run time, SQLite on ext2, fonts/icons in the image, and anything else that only fails when run, not when type-checked |
