# emusic

emusic is a music player and library ([va1erian/emusic](https://github.com/va1erian/emusic)),
here on LazyOS: its portable interface on the desktop compositor and its
`emusic-lazyaudio` backend, which decodes MP3s (symphonia) and plays them
through the system mixer.

- **Play a file:** open an `.mp3` from Files, or start emusic with a path.
- **Build a library:** Settings -> Library -> Add folder scans a folder
  (`~/Music` is the one the package may read).
- **Sample:** `resources/tones.mp3` in the install folder
  (`/apps/org.lazy.emusic/<version>/resources/`): A4 for two seconds, then C5.

Not on LazyOS yet: formats other than MP3 (tracker modules, MIDI and SID need
BASS or GCC), file dialogs (the folder picker answers "cancelled"), gapless
playback and the projectM visualizer. Your library, settings and thumbnails
live in `~/.apps/org.lazy.emusic`.
