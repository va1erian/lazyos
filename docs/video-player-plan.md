# Video Player plan

Status: V0 built (this document's first PR); V1-V6 are the plan.

The goal is a basic YouTube-style player on LazyOS at **720p**: HTTPS in,
H.264 and AAC decoded in software on the single CPU, drawn in an xui window.
This document records why that is feasible, what it needs and in what order
it is built. V0 is the part that does not depend on any decision still open:
the player, its window, its packaging and its tests, with Motion JPEG in AVI
as the first codec so the whole path (file, decode, pace, paint, seek) is
real and verified before a bigger decoder is added.

## 1. Feasibility

LazyOS draws to a framebuffer with no GPU, runs on one CPU (no SMP) and
enables SSE2 only (no XSAVE, so no AVX). Figures below were measured on a
modern desktop core; user code runs at near native speed under WHPX/KVM, so
they bound what the guest can do, but **no number here has been taken inside
the guest yet** (V1 does that).

### The pixel path is not the problem

Per full frame, for a window that plays video the way Doom does
(`doom/src/window.rs`: two buffer slots, a one-way `Present`):

| Stage | 720p | 1080p |
|---|---|---|
| app: YUV420 to RGBA (scalar) | 1.5 ms | 3.3 ms |
| app: image to slot (copy) | <0.1 ms | 0.25 ms |
| `xuid`: compose (copy, R/B swap, alpha) | 0.4 ms | 1.0 ms |
| kernel `present` (`rep movs` to the framebuffer) | 0.8 ms (`docs/perf/report.md`, WHPX) | ~2 ms |

About 3 ms per 720p frame: 10% of a core at 30 fps. Notes: the host
micro-benchmark is cache-hot; `user` is built at `opt-level = "s"`, so
`xuid`'s swap loop may be several times slower than measured and is still
only a few ms. A windowed video presents only its own damage rectangle.

### Decode is

H.264 (High profile, CABAC, 3 B-frames, 4 refs), ffmpeg, one thread, **all
SIMD disabled** (`-cpuflags 0`), synthetic content at YouTube-like bitrates:

| Stream | Decode per frame | Of one core at its frame rate |
|---|---|---|
| 720p30, ~2.5 Mbit | 2.3 ms | ~7% |
| 720p60, ~4.5 Mbit | 2.1 ms | ~13% |
| 1080p30, ~5 Mbit | 5.4 ms | ~16% |

Real footage may cost 1.5-2x more; 720p30 keeps a 5x margin. This is the
configuration LazyOS can run (a C decoder built by zig, no assembly).
Motion JPEG is far heavier per bit: a noisy 720p30 file at ~32 Mbit decodes
at 17.6 ms per frame even with `zune-jpeg`'s AVX paths, so MJPEG proves the
pipeline and is not the end goal (`cargo run --release -p lazyvideo
--example decode_bench -- file.avi` measures a file).

### What YouTube adds

* Streams are DASH: video-only fMP4 (`avc1`, itag 136 is 720p30, 298 is
  720p60) and audio-only (`mp4a`, itag 140). Muxed 720p is largely gone, so
  the player needs two range-request streams and an A/V clock.
* A playable URL needs the innertube API, the player JavaScript for the
  signature and `n` parameter, and increasingly PO tokens or SABR. It is a
  moving target and is against YouTube's terms: it is built last, as a
  component of its own, and the player works from a URL or file without it.
* The network stack is not a constraint: `nettls` measures 70-190 MB/s on
  sockets; a stream needs 0.3-0.6 MB/s. TLS crypto is software
  (`aes_force_soft`) and still far above that.

### Limits that remain

* One CPU: demux, decode, audio mixing and the compositor share it; threads
  add no parallelism. 1080p, or heavier than High-profile 720p, needs AVX2
  (XSAVE in `kernel/src/task/fpu.rs`) and then SMP.
* No vsync or scanout control, so tearing is possible; QEMU's own window
  refresh (about 30 Hz) caps what a person sees. Judge by `FrameDone`
  counts in the guest, not by eye.
* QEMU's BIOS mode caps the desktop at 1280x720, so a native 720p video plus
  window chrome does not fit: scale down, go fullscreen, or run `--hidpi`.

## 2. Design

```
 file / HTTPS ──► Demuxer ──► packets ──► Decoder ──► RGBA frame
                     (lazyvideo, one player thread, media clock)
                                                │ newest frame, latest-wins slot
                                                ▼
                       xui window (xui-videoview): picture, toolbar, seek bar
```

* `lazyvideo` (`xui-app/crates/video`) is portable, free of xui and
  host-tested. `Demuxer` and `Decoder` are the extension points; `open`
  picks them from the container and the codec tag.
* The **player thread** owns the file and the decoder, decodes a frame when
  the media clock says it is due, and leaves the newest in a one-slot
  mailbox. A window that is late takes a newer frame and the older one is
  counted as dropped. The window never blocks on I/O or decode.
* **Pacing** is `clock::schedule(pts, position, frame_duration)`: *show*,
  *wait* or *drop* (more than two frame durations late). When every frame
  is a key frame the player jumps to the frame the clock is at instead of
  decoding the ones it would drop. When audio plays its position replaces
  the clock; `schedule` does not change.
* **xui integration** (`xui-app/crates/videoview`, modelled on the PDF
  Viewer): a `Control` node paints the letterboxed frame; `Toolbar`,
  `Slider` (seek, thousandths of the duration), `StatusBar` and a
  `FileDialog` are xui widgets; keys (`Ctrl+O`, `Space`, `S`, arrows, `Home`)
  go through `ui.on_key`; files dropped on the window arrive as
  `text/uri-list`; the theme follows the desktop. The frame timer runs only
  while something moves, so an idle player costs no wakeups.
* **Evidence** (serial): `VIDEO:UP:PASS`, `VIDEO:OPEN:PASS:<path>:<w>x<h>:
  <fps>:<frames>:<codec>`, `VIDEO:OPEN:FAIL`, `VIDEO:PLAY|PAUSE|STOP`,
  `VIDEO:SEEK:<ms>`, `VIDEO:POS:<ms>:shown=:dropped=:fps=` once a second,
  `VIDEO:END:PASS`, `VIDEO:ERROR`, `VIDEO:QUIT:PASS`.

## 3. Phases

| | Phase | Exit criterion |
|---|---|---|
| **V0** | *Built.* AVI demux, Motion JPEG, media clock, player thread, xui window, package `os.lazy.video` (mime `video/x-msvideo`), sample clip, headless window tests, QEMU session `xui_video.json`. | `cargo test -p lazyvideo -p xui-videoview`; the session shows the clip playing, pausing, seeking and ending |
| **V1** | Present path and a guest benchmark. Write decoded pixels straight into the window's buffer slot (as Doom does) instead of building an `Image` per frame; `VIDEO:FPS` markers; a 720p MJPEG clip as the load. | Guest `shown+dropped` accounting at 720p30; per-stage ms in the log; a number replaces every "measured on the host" above |
| **V2** | Containers. MP4 and fragmented MP4 demux (`moov`/`sidx`/`moof`), keyframe flags, seek to the previous key frame and decode forward (`Demuxer::all_key() == false` path in the worker). | A host-generated H.264-less MP4 seeks accurately; fuzzed like the AVI scan |
| **V3** | H.264. libavcodec's H.264 decoder only, `--disable-asm`, built by zig (`tools/video/build.py`, pinned source, hash checked, like Doom). LGPL is GPLv2-compatible; `tools/nettls/licenses.py`'s rule extended to it. | 720p30 High profile at most 8 ms per frame in the guest; no `LABEL:DENY`; golden-frame CRC test |
| **V4** | Audio. AAC through `symphonia` (already in emusic), played by `audioclient::PlaybackStream`; audio position is the master clock; underrun and drift handling. | The sound harness's recording judged against a host render; A/V offset under 80 ms over 60 s |
| **V5** | Network. A `Read + Seek` over HTTPS range requests (`nettls`), read-ahead on a thread, a buffering state in the window. | A host-served 720p file plays from `http`/`https` URLs; stalls show "Buffering" and recover |
| **V6** | YouTube. A separate resolver (innertube request, signature and `n` through an embedded JS engine, or a host-side helper), choosing `avc1` + `mp4a` at 720p. | Opening a watch URL plays it; the resolver can break without touching the player |

Later, if more is wanted: XSAVE and AVX2 in the kernel (roughly 1.5-2.5x on
decode), then SMP. Zero-copy scanout (performance-plan P3.3) saves one copy
and does not matter here.

## 4. Risks and decisions

| Risk | Decision |
|---|---|
| H.264 decode slower in the guest than on the host | V1 measures before V3 commits; V3 has a per-frame gate |
| MJPEG AVIs without a DHT segment (`AVI1` style) fail to decode | Reported as an error; injecting the standard tables is a small follow-up if real files need it |
| Frame upload through xui's `Image` cache costs a copy and a cache entry per frame | Fine for V0; V1 bypasses it |
| YouTube changes break the resolver | It is isolated in V6 and never in the player's critical path |
| Patents on H.264 | A distribution question, not a technical one; the plan only builds the decoder when asked to |
| Audio and video drift on one CPU | The audio clock is master; video frames drop, audio never skips |

## 5. Verification

```bash
cargo test --manifest-path xui-app/Cargo.toml -p lazyvideo -p xui-videoview
python tools/video/make_sample.py --check
cargo run --release --manifest-path xui-app/Cargo.toml -p lazyvideo --example decode_bench -- clip.avi
python tools/xui/build.py
LAZYOS_DESKTOP=1 LAZYOS_UI_PROBE=1 LAZYOS_XUI_AUTOSTART=video LAZYOS_RESET_OS=1 cargo build
python tools/screenshot/qemu_session.py --image target/lazyos.img \
    --out shots/video --script tools/screenshot/examples/xui_video.json
```

The window test writes snapshots to `xui-app/target/snapshots/video-*.png`.
