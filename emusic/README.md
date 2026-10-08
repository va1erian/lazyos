# emusic on LazyOS

[emusic](https://github.com/va1erian/emusic), the music player and library,
as the installable package `org.lazy.emusic` (docs/media-plan.md). This
crate is only the LazyOS half: emusic's own crates do the rest.

| Piece | Where |
|---|---|
| Window, views, library, player | emusic's `emusic-frontend-portable`, `emusic-ui`, `emusic-player` (fetched at the revision `Cargo.toml` pins) |
| MP3 decoding and the player channel | emusic's `emusic-lazyaudio` (symphonia; no BASS) |
| The window backend | `xui-app`'s `LazyOSBackend`, through emusic's `run_on(Host)` |
| The sound | `src/audiod.rs`: emusic-lazyaudio's `Output` over `audioclient::PlaybackStream` to `audiod` |
| Serial evidence | `src/markers.rs`: `EMUSIC:PLAY`, `EMUSIC:POS`, `EMUSIC:END`, ... |
| Headless sound check | `src/soundcheck.rs`: `emusic.elf --sound-check <file>` |
| Image switch | `LAZYOS_EMUSIC=1` -> `build_support/emusic_embed.rs` -> `/system/share/samples/emusic.lzp` |

```bash
python tools/emusic/build.py                    # target/emusic/emusic.elf + target/pkg/emusic.lzp
python tools/emusic/build.py --emusic-src ../emusic   # a local emusic clone holding the pinned commit
python tools/run_demo.py --emusic               # desktop + the package + a sound card
python tools/emusic/run.py --app                # build, boot, record, judge (P4) and the app session (P3)
cargo test --manifest-path emusic/Cargo.toml --lib
```

On LazyOS, install it from the Terminal (`cp /system/share/samples/emusic.lzp
~/ && pkgctl install ~/emusic.lzp`) or by opening the package in Files, then
open an MP3 from Files or start emusic from the menu. Its config, library
database and thumbnails live in `~/.apps/org.lazy.emusic`; the package ships a
sample track, `resources/tones.mp3` (A4 for 2 s, then C5; `tools/emusic/make_tones.py`).

Not there yet: formats other than MP3, file dialogs (emusic's `file_picker`
seam has no LazyOS picker, so Settings -> Add folder answers "cancelled"),
library change watching (no inotify), gapless playback.
