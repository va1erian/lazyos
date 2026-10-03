# The LazyRAD MOD player and the `modplay` script module

A ProTracker MOD player written in LazyRAD: forms and Rhai, shipped both as a
LazyRAD sample (`/system/share/lazyrad/modplayer`) and as an installable package
(`/system/share/samples/modplayer.lzp`, `org.lazy.modplayer`). Its window shows the song, the transport,
the mix (volume, stereo separation, interpolation), a meter and a mute button
per channel, a pattern view that follows the row being heard, the playlist and
the instruments.

```bash
python tools/run_demo.py --modplayer                # desktop + LazyRAD + the package, with a sound card
python tools/lazyrad/modplayer_run.py               # build, play both ways, record, judge
python tools/lazyrad/package.py                     # just target/pkg/modplayer.lzp
```

In the desktop Terminal: `/system/bin/lrplay --client /system/share/lazyrad/modplayer &`,
or `cp /system/share/samples/modplayer.lzp ~/ && pkgctl install ~/modplayer.lzp` and then **ModPlayer**
from the Start menu. The
GUI launcher has the same choice under Simple → Extras and Advanced (the
`LAZYOS_MODPLAYER` switch).

## What is Rhai and what is native

Everything you see and do is the script (`lazyrad-os/samples/modplayer`):

| File | What |
|---|---|
| `main_form.lfm` | the window's layout |
| `main_form.rhai` | the playlist, transport, mix and mute controls, meters, the pattern view, what happens when a song ends |
| `cells.rhai` | periods to note names, hex, a pattern row as tracker text (`C-2 01 A0F`) |
| `demo_song.rhai` | the built-in song as base64 (generated) |

The player provides the `modplay` module (`lazyrad-os/src/tracker`): parsing
and mixing are `libs/modplay` (integer-only, fuzzed, shared with the native
`modplay` command), and the sound goes to the system mixer through
`libs/audioclient` over a musl transport (`xui-app/src/platform/audio.rs`).
Mixing four channels at 22 kHz in an interpreter is not realistic, so the
split mirrors the Amiga: a replay routine and its UI in software, the mixing in
"hardware".

## The `modplay` module

```rhai
let song = modplay::load("music/tune.mod");       // a file the app may read
let song = modplay::decode(demo_song::data());    // or base64 a script carries
let deck = modplay::play(song, #{ volume: 80, separation: 50 });
deck.on_update(Fn("show_deck"));                  // about 12 times a second
deck.on_end(Fn("deck_ended"));                    // once the last frame is heard
```

| Function | Returns |
|---|---|
| `modplay::load(path)` | `Song`. The path goes through the player's file sandbox like `file_read_text` (an installed app reads its own data directory and project). |
| `modplay::decode(base64)` | `Song`; whitespace in the text is ignored. |
| `modplay::play(song)`, `modplay::play(song, options)` | `Deck`, playing. Options: `loops` (0 = forever, default 1), `separation` (0-100, default 50), `volume` (0-100), `interpolate` (bool, default true), `rate` (8000-48000, default 22050). An unknown option is an error. |
| `modplay::sound_available()` | whether a mixer runs. Without one a deck still plays, silently, at wall-clock speed. |

**`Song`**: `title`, `length` (orders), `patterns`, `restart`, `channels` (4),
`rows` (64), `orders` (array), `samples` (array of
`#{ number, name, length, volume, finetune, looped }`), `pattern_at(order)`,
`row(order, row)` (four cells), `cell(order, row, channel)`
(`#{ period, sample, effect, param }`; empty past the end).

**`Deck`**: what is *heard*, not what is being mixed (the deck renders up to
0.74 s ahead): `status` (`"playing"`, `"paused"`, `"ended"`, `"stopped"`),
`playing`, `order`, `row`, `pattern`, `speed`, `tempo`, `levels` (four
`0..=64`), `elapsed_ms`, `sound`, `song`; read/write `volume`, `separation`,
`interpolate`; `pause()`, `resume()`, `stop()`, `seek(order)` (also replays an
ended deck), `mute(channel, bool)`, `muted(channel)`, `on_update(fn)`,
`on_end(fn)`. Handlers get the deck. A handler that throws is reported once
and dropped, so a broken meter cannot flood the screen with message boxes.

Closing the window stops its decks. A deck the script no longer holds plays
on until it ends or the window closes.

**Permissions.** A script that calls `modplay::play` needs the mixer, so the
LazyOS platform adds `os.lazy.audio.v1` to what Make LazyOS App (and
`tools/lazyrad/package.py`) declares (`LazyOsPlatform::script_permissions`).

## How a deck keeps playing

A deck is a `libs/modplay` `Player` rendering into a sink, pumped by the
form's window: `tracker::Tracker` is a LazyRAD `EventSource`, so while a deck
plays, the window polls every 40 ms. Each poll tops the sink up, maps the
mixer's consumed count back to the song position through a timeline of
rendered slices, and runs `on_update` (at most every 80 ms). It never blocks.

- **Buffer.** The mixer caps a stream's ring at 64 KiB, which is 0.74 s at
  22.05 kHz stereo. The UI thread may stall that long (a slow repaint) before
  the music skips.
- **Rings fill to the last frame.** A mixer stream starts only once its ring
  is full, so the deck renders a short final slice when needed.
- **The end.** The mixer reads whole periods, so after the last note the deck
  pads with silence until the song's last frame has been read, then reports
  `ended`.
- **Pause is lossless.** The mixer has no pause (`Stop` discards and restarts
  numbering), so `MixerSink` (`tracker/mixer.rs`) closes the stream and later
  re-queues whatever the mixer had not consumed. It resumes from a freshly
  refreshed *consumed* count. Resuming from `Position` repeated 64 ms in a
  recording, and a stale consumed count repeated 20 ms. Measured as it is now,
  the recording stays sample-aligned with the reference through a pause.

## The demo song

"LazyOS Groove" is an original four-channel module written for this sample by
`tools/lazyrad/gen_demo_song.py` and dedicated to the public domain (CC0). It
is about 54 s, in A minor over Am-F-C-G, with synthesized samples (pulse lead,
saw bass, triangle pad, swept-sine kick, noise snare and hat), arpeggios,
vibrato, a tone portamento and a fade. A packaged LazyRAD app ships only its
forms and scripts, so the song travels as `demo_song.rhai`.
`python tools/lazyrad/gen_demo_song.py --check` (CI) fails if it is stale. More
songs: put `.mod` files in the app's `music` folder (`~/.apps/lazyrad/data/music`
for the sample, `~/.apps/org.lazy.modplayer/music` installed) and
press **Rescan**.

## Verification

| What | How |
|---|---|
| engine controls (seek, mute, live separation, names, owning player) | `cargo test -p modplay` (`src/tests_controls.rs`) |
| musl audio transport | `cd xui-app && cargo test --lib platform::audio` |
| decks, sinks, the script surface | `cd lazyrad-os && cargo test --lib tracker`: a mixer-like sink with an odd ring and period-granular reads, lossless pause against a fake card, handler errors, a deck soak |
| the sample itself | `lazyrad-os/tests/modplayer.rs` runs the real form on xui's offscreen backend, clicks through it, plays the song to the end, and fails if any handler raised an error |
| on LazyOS, by ear | `python tools/lazyrad/modplayer_run.py`: the Terminal session (`lazyrad_modplayer.json`: play, mute, pause, resume, end) and the installed-app session (`lazyrad_modplayer_installed.json`: copy to the home, `pkgctl install`, Start menu, label `app:org.lazy.modplayer`). Each recording must be the whole song (`modjudge.py`, mono, against a host render; `test_modjudge.py` shows it fails on a 62 ms skip or repeat, silence, truncation or another song) |

Found while building it, fixed:

- In a form script, a helper function named like a method you call inside it
  (`fn mute(channel, on) { deck.mute(...) }`) recurses until Rhai reports
  "Stack overflow". The sample names it `set_channel`.
- Debug-build Rhai needs more than a 2 MiB test thread for a handler a few
  script calls deep. The offscreen tests run on a 64 MiB thread. Release
  builds (the player) are fine.
