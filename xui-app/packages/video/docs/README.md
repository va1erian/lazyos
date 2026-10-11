# Video Player

Plays video files in a window: open one (Ctrl+O, or drop it on the window),
pause and resume (Space), stop (S), seek with the bar, the arrow keys (5 s)
or Home.

## What plays

Motion JPEG in AVI. The player is built around two traits (`Demuxer`,
`Decoder` in `lazyvideo`), so H.264 and MP4 are the next additions rather
than a rewrite: see `docs/video-player-plan.md`.

Playback is paced against a media clock; a frame the decoder cannot make in
time is dropped and counted in the status bar, never shown late.

A sample ships at `/system/share/samples/lazyos-sample.avi`.
