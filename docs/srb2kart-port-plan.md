# Plan: SRB2Kart on LazyOS

> **Status: K0 and K1 implemented (2026-10-11); K2-K4 not started.** The
> session `tools/screenshot/examples/srb2kart.json` installs the package, runs
> the headless check in the Terminal (`crc=b3580eb0`), launches the game from
> Games, reaches the title, drives a Time Attack race (55-60 fps at 640x400
> under WHPX) and quits from the window's close button. Decisions taken while
> implementing, which supersede the text below where they differ:
>
> - **Not `NONET`:** upstream's `NONET` build does not compile, so the network
>   code is built in and never started (`I_InitNetwork` says no) until K3.
>   Two force-included shims cover names that only exist under `HAVE_CURL`
>   (`srb2kart/README.md`). No upstream file is patched.
> - **The engine's home is `<app folder>/.srb2kart`:** upstream always appends
>   its `DEFAULTDIR`.
> - **The window opens at 640x400** (the engine's 1280x800 default does not fit
>   a 720p desktop) and follows the engine's mode changes; headless is 320x200.
> - **SRB2Kart is in the launcher's Games group** (and has its own Advanced
>   checkbox).
> - **Open problem: start-up reads ~1.3 GiB** through the ext2 volume before the
>   title (minutes under WHPX); suspected the start-up MD5 checks of the data
>   files plus lump reads; to profile next.

**Goal:** SRB2Kart (Kart Krew's kart racer on the Sonic Robo Blast 2 engine)
in a LazyOS desktop window, offline: Grand Prix, Match Race and Time Attack
against bots, keyboard-driven, on the software renderer. Like Doom
([`doom-port-plan.md`](doom-port-plan.md)), the engine is upstream C, fetched
at a pinned revision and compiled with `zig cc`, under a small Rust program
that owns `main` and implements the platform on the `xuid` display protocol.
It is a static musl program run by the Linux ABI personality; no kernel change
is the target.

**Non-goals (v1):** sound and music (K2), netplay (K3, K4), OpenGL and 3D
models, gamepads, addons that need zlib (compressed PK3), screenshots (PNG).

## Decisions

| Question | Choice | Why |
|---|---|---|
| Codebase | **Kart-Public v1.6** (`https://github.com/STJr/Kart-Public`, mirror of `git.do.srb2.org/KartKrew/Kart-Public`), the tag's source archive pinned by SHA-256 | The last release (Nov 2022); upstream plans no more releases but security fixes, so a pin stays current. The data files are v1.6's. |
| Not Ring Racers | rejected | Its successor needs C++17, vcpkg, SDL2 and eight libraries, and its `rhi` layer has only GL2/GLES2 backends: the 2D compositor goes through it even in software mode, so a framebuffer OS would first need a software RHI written in C++. Its data has no licence either. |
| Renderer | Software only (`NOHW`) | No GL in LazyOS. 8-bit palettised into `screens[0]`, which our backend expands through the palette into the window's RGBA slot. |
| Optional libraries | All compiled out: `NOHW NONX86 NOASM NOMIXER NOGME NOCURL NOPNG NOZLIB NOTHREADS NONET NOUPNP`, no discord-rpc | None is needed to boot and race offline. `NOZLIB` forces `NOPNG`; Lua (`blua`) and MD5 are bundled. Threads only serve the HTTP master server. |
| Platform backend | Our own `srb2kart/c/lazyos/` (`i_main.c`, `i_system.c`, `i_video.c`, `i_sound.c`, `i_net.c`, `i_cdmus.c`) compiled in place of `src/sdl/`, with `src/dummy/` as the checklist (it does not compile as is) | The engine's platform surface is `i_system.h` (49), `i_video.h` (16), `i_sound.h` (43, all stubs in v1) plus `i_net`; about 110 entry points, most stubs. The C files forward the real work to Rust through the seam below. |
| Engine sources | Fetched, never vendored; **no upstream file is patched** in v1. A needed fix to an upstream file is an overlay copied over the extracted tree by `tools/srb2kart/build.py` and listed in `srb2kart/README.md` | Quake's rule (two overlaid files, documented). |
| Build | `srb2kart/build.rs` compiles the upstream file list (the Makefile's core objects, `blua`, `md5`) + our backend with the `cc` crate, like `doom/build.rs`; we do not drive upstream's Makefile | Same toolchain as Doom (`tools/xui/zig.py`). Flags: `-O2 -fwrapv -fno-strict-aliasing -fno-sanitize=undefined -std=gnu99`, defines `UNIXCOMMON LINUX LINUX64 NOHW NONX86 NOASM NOMIXER NOGME NOCURL NOPNG NOZLIB NOTHREADS NONET` (MD5 stays: it verifies the data files), `COMPVERSION`'s generated header written by `build.rs`. No AVX (LazyOS saves FXSAVE state only). |
| Game data | **Fetched at build time** from the official v1.6 release, `AssetsLinuxOnly.zip` (220,991,971 bytes), pinned by SHA-256 (cross-checked against Flathub's `org.srb2.SRB2Kart` manifest), extracted to `target/srb2kart/data/`. Never committed, never in the `.lzp` | The data has no licence (and the characters are SEGA's), so we do what Flathub does with `extra-data`: the user's build downloads it from Kart Krew's own release. The engine package stays pure GPL. |
| Where the data lives | `/system/share/srb2kart/` on the OS volume (`fhs::share::SRB2KART_DATA`), written by `build_support/srb2kart_embed.rs` when `LAZYOS_SRB2KART=1`, beside a `CREDITS.txt` | One copy (a package copy would be stored three times: samples, home, `/apps`), read-only, outside `assets/` (whose manifest requires a free licence). |
| Packaging | `org.lazy.srb2kart` `.lzp`, engine only, at `/system/share/samples/srb2kart.lzp` (`fhs::share::SRB2KART_LZP`), category `games`, installed like Doom | Same path as Doom and Quake. |
| Disk size | `LAZYOS_SRB2KART=1` raises the default `LAZYOS_OS_SIZE` to `1G` (an explicit value still wins; an existing smaller volume needs `--reset-os`) | ~250 MB of data on a 512 MiB volume leaves too little. |
| Config and saves | `$HOME/.apps/org.lazy.srb2kart` (`kartconfig.cfg`, `kartdata.dat`, replays), `/tmp/srb2kart` without a home (`fhs::state::SRB2KART_TMP`) | Doom's rule. The engine's `srb2home` is set to it. |
| Licence | Engine GPL-2.0-or-later + our wrapper GPL-3.0-or-later = binary GPL-3.0-or-later, `srb2kart/NOTICE` with the source offer | As Doom. |

## Credits (explicit, everywhere the game appears)

The data is not ours and not free; every place that ships or shows it says
whose it is and where it came from:

- `srb2kart/NOTICE` (in the package): the engine's authors (Kart Krew, Sonic
  Team Junior, the Doom Legacy team, id Software), its GPL-2.0-or-later
  licence and the source offer; that the game data is **not** part of the
  package and is not covered by the GPL.
- `/system/share/srb2kart/CREDITS.txt` (written by the image build beside the
  data, from `srb2kart/CREDITS.txt`): "SRB2Kart v1.6 game data © Kart Krew;
  downloaded unmodified from <URL> (SHA-256 …) by this image's build; not
  redistributed by LazyOS. Sonic the Hedgehog and related characters are
  trademarks of SEGA. SRB2Kart is a fan game, not affiliated with or endorsed
  by SEGA or Sonic Team."
- `srb2kart/package/docs/README.md` (the app's docs page) and the manifest's
  `author`/`description`: the same credits, short.
- `srb2kart/README.md` and the AGENTS.md section: the data rule ("never
  commit, upload, attach or publish the data or an image containing it; CI
  artifacts carry no image") and the credits.
- The engine's own credits screen stays as upstream wrote it.

## The C ↔ Rust seam

The C backend calls a few Rust functions (`#[no_mangle] extern "C"`, in
`srb2kart/src/hooks.rs`); everything with Messenger stays in Rust. The header
is `srb2kart/c/lazyos/lazyos_host.h`:

```c
/* Rust -> C: called once by Rust main; never returns (I_Quit exits). */
int srb2kart_main(int argc, char **argv);            /* our i_main.c */

/* C -> Rust */
void     lz_log(const char *line);                    /* I_OutputMsg, one line */
uint64_t lz_ticks_ns(void);                           /* monotonic since start */
void     lz_sleep_us(uint32_t us);
int      lz_video_open(int w, int h, const char *title); /* 0 ok */
void     lz_present(const uint8_t *pixels, int w, int h, int pitch,
                    const uint8_t palette_rgb[256 * 3]); /* 8-bit frame */
int      lz_poll_event(struct lz_event *out);          /* 1 when *out was filled */
const char *lz_data_dir(void);                         /* /system/share/srb2kart */
const char *lz_home_dir(void);                         /* the writable config dir, created */
void     lz_quit(int status);                          /* QUIT marker, exit */

struct lz_event { int32_t kind; /* 1 key down, 2 key up, 3 close */
                  int32_t key;  /* SRB2 keys.h code (KEY_*, ASCII) */ };
```

Rust maps HID usages and keysyms to SRB2's `keys.h` codes (`keymap.rs`,
host-tested against the header's values); C posts them with `D_PostEvent`.
In headless mode `lz_present` CRCs the expanded frame instead of drawing and
`lz_poll_event` returns nothing.

## Architecture

```
srb2kart/                 standalone Cargo workspace (like doom/)
  Cargo.toml build.rs NOTICE README.md CREDITS.txt
  c/lazyos/*.c *.h        our platform backend (C)
  src/main.rs lib.rs      args (--client, -headless -frames N), calls srb2kart_main
      hooks.rs            the seam above
      launch.rs           data and home dirs (fhs), chdir
      window.rs window_input.rs session.rs keys.rs keymap.rs pixels.rs
                          from doom/, keymap to keys.h, pixels expand the palette
      headless.rs crc.rs  -headless -frames N: SRB2KART:HEADLESS:PASS frames=N crc=<hex>
  package/manifest.toml icons/ docs/README.md
tools/srb2kart/fetch.py   Kart-Public v1.6 source archive + AssetsLinuxOnly.zip, SHA-256 pinned
tools/srb2kart/build.py   fetch, cargo build (zig), target/srb2kart/srb2kart.elf,
                          target/srb2kart/data/, target/pkg/srb2kart.lzp; JSON map on stdout
build_support/srb2kart_embed.rs   LAZYOS_SRB2KART=1: the .lzp + /system/share/srb2kart/*
```

Paths the tools agree on (the contract between the pieces):

| Artifact | Path |
|---|---|
| Engine source tree | `target/srb2kart/Kart-Public-<ver>/` (env `SRB2KART_SRC` for `build.rs`) |
| Extracted data | `target/srb2kart/data/{srb2.srb,gfx.kart,textures.kart,chars.kart,maps.kart,sounds.kart,music.kart,bonuschars.kart,patch.kart}` (whatever the zip holds besides `mdls*`, which only GL uses and is left out) |
| Program | `target/srb2kart/srb2kart.elf` |
| Package | `target/pkg/srb2kart.lzp` |
| Image paths | `/system/share/samples/srb2kart.lzp`, `/system/share/srb2kart/<file>`, `/system/share/srb2kart/CREDITS.txt` |
| Install | `/apps/org.lazy.srb2kart/<ver>/bin/srb2kart.elf` |
| Markers (channel `SRB2KART`) | `SRB2KART:UP:PASS mode=window\|headless`, `SRB2KART:DATA:FAIL <reason>`, `SRB2KART:WINDOW:FAIL`, `SRB2KART:HEADLESS:PASS frames=N crc=<hex>` (also `/tmp/srb2kart-result.txt`, `fhs::state::SRB2KART_RESULT`), `SRB2KART:QUIT:PASS` |

## Phases

**K0 — headless engine.** `fetch.py`, `build.py`, `srb2kart/` with the C
backend and the seam; video and input stubs that CRC frames. Runs on the
Linux host too (WSL), which is the fast loop: `srb2kart.elf -headless -frames
300 -home /tmp/k -data target/srb2kart/data` reaches the title screen (or
`-warp` a map) and prints the PASS line. Then the same on LazyOS from the
Terminal. Done when it passes in QEMU.

**K1 — window and keyboard.** `window.rs`, `pixels.rs` (8-bit + palette to
RGBA, aspect-kept nearest scale and letterbox; host tests), `keymap.rs`
(host tests), the input session and grab from Doom. Video modes 320x200,
640x400 (default), 960x600, 1280x800. The image plumbing: `srb2kart_embed.rs`,
`fhs` constants, `LAZYOS_SRB2KART`, `run_demo.py --srb2kart` (implies
`--desktop`), the GUI's Simple and Advanced controls with `test_catalog`
tests, the `SRB2KART` trace channel, the OS size default. Session
`tools/screenshot/examples/srb2kart.json` (install, headless check, launch
from Games, menus, start a race, screenshots). Done when a race renders and
steers in QEMU under WHPX, and the screenshots show it.

**K2 — sound.** `i_sound.c` on vendored `stb_vorbis` (both music `O_*` lumps
and sound effects are Ogg Vorbis; DMX lumps as a fallback for addons), a
mixer of N channels with volume and separation, `LOOPPOINT=`/`LOOPMS=`
looping, pre-decoding the effects a race uses on a worker thread (the Vita
port's lesson: first-play decode stalls up to 2.4 s), feeding
`audioclient::PlaybackStream`. Judged by recording, like the other sound
work.

**K3 — direct-IP netplay.** Drop `NONET`; `i_tcp.c` over the Linux ABI's UDP
sockets (port 5029; datagrams ≤ 1472 bytes, no broadcast, so no LAN
discovery; `connect <ip>` works). Needs `LAZYOS_NETD=1` and
`network = ["outbound"]`. **K4 — master server** through curl over
`nettls`.

## Risks

- **Speed:** Kart maps (slopes, FOFs, many sprites) cost more than Doom's.
  Expected fine at 640x400 under WHPX/KVM on one core, marginal under TCG; CI
  sessions run 320x200. The logic runs at 35 tics/s whatever the frame rate.
- **Memory:** lumps are cached on demand; without audio, well under the 1 GiB
  default. `MAXVIDWIDTH/HEIGHT` lowered to 1280x800 shrinks the renderer's
  tables.
- **Missing libc pieces:** the engine uses `stdio`, `dirent`, `stat`,
  `mkdir`, `localtime`, `setjmp`; all exist on the Linux ABI. Any gap becomes
  a `tools/abi/` fixture first, as the Doom plan required.
- **Data rule:** a CI job may fetch the data to run the session, but uploads
  only screenshots of the session and logs, never the image or the data.
