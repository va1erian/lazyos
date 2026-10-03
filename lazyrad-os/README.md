# lazyrad-os

LazyRAD on LazyOS (see [`docs/lazyrad-plan.md`](../docs/lazyrad-plan.md)): a
standalone Rust workspace, like `xui-app` and `rhai-host`, built for
`x86_64-unknown-linux-musl` by `python tools/lazyrad/build.py` and embedded in the
disk image as `/system/bin/lrplay` and `/system/bin/lazyrad` (`LAZYOS_LAZYRAD=1`).

| Bin | What |
|---|---|
| `lrplay` | The player: runs one LazyRAD project as a `xuid` client. It is also the stub copied into every `.lzp` package. |
| `lazyrad` | The IDE as a `xuid` client (plan P3). |

The library (`src/lib.rs`) holds the LazyOS glue shared by both: command-line
parsing (`args`), the `lazyrad_runtime::platform::Platform` LazyOS installs
(`platform`: script file sandbox, config dir, player path), the serial
evidence markers (`marker`: `LRPLAY:UP|EVENT|EXIT`, `LRIDE:*`) and Messenger
for form scripts (`messenger`). That module installs `msg` and the generated
`sys::*` modules into every form's engine (`lazyrad_runtime::extensions::
add_scoped`) and registers the event source the form's window polls, so
`sys::confd::get("sys/ui/theme")`, `sys::confd::on_changed(...)` and
`msg::serve(...)` work in any form script; see
[`docs/rhai/msg.md`](../docs/rhai/msg.md#in-lazyrad-form-scripts) and
[`docs/lazyrad-messenger-plan.md`](../docs/lazyrad-messenger-plan.md). The
platform also derives a packaged app's interfaces and topics from its scripts.
The player prints `LRPLAY:MSG:PASS` once Messenger is installed and
`LRPLAY:MSGEVENT:PASS` after the first Messenger handler ran.

The `tracker` module is the same kind of extension for sound: `modplay::*`
loads and plays ProTracker songs on *decks* through the system mixer
(`libs/modplay`, `libs/audioclient` over `xui_app::platform::audio`), polled by
the form's window; see [`docs/lazyrad-modplay.md`](../docs/lazyrad-modplay.md).
The player prints `LRPLAY:MODPLAY:PASS:title=.. audio=0|1 rate=..` when a song
starts and `LRPLAY:MODEND:PASS:elapsed_ms=..` when one has played out.

`samples/` holds LazyOS-only sample projects (`samples/messenger`: confd, a
change topic and a served method; `samples/modplayer`: the MOD player, also
packaged as `/system/share/samples/modplayer.lzp` by `tools/lazyrad/package.py` with
`examples/lzpack.rs`). `run_demo.py --lazyrad` and the GUI
launcher embed them under `/system/share/lazyrad/` next to any `LAZYRAD_SAMPLES` entries;
`python tools/rhai/run.py --lazyrad` boots the Messenger one and judges it.

## Where LazyRAD writes

Everything lives in the home of the user running it (`$HOME`, which `init`
passes to every session app; filesystem plan F4). Nothing is written under
`/data`, and installed apps never write inside `/apps` (`pkgd` owns it).

| What | Where |
|---|---|
| IDE settings | `$HOME/.apps/lazyrad/config/` |
| projects (the file dialog's first stop) | `$HOME/projects/`, created by the IDE |
| data of a project run from the IDE or a shell | `$HOME/.apps/lazyrad/data/` |
| data of an installed app | `$HOME/.apps/<system_name>/` (manifest `read:`/`write:$HOME/.apps/<system_name>`) |
| packages staged for `pkgd` | `/transient/lazyrad-<system_name>-<version>.lzp`, deleted afterwards |

Without `$HOME` (a program started outside a session) the home is
`/transient/lazyrad` on the ramfs, so nothing survives a reboot; both programs
say so with `LRPLAY:HOME:WARN` / `LRIDE:HOME:WARN` on serial and stderr.
`lrplay` reports the folder its scripts may write as `LRPLAY:DATA:PASS:<dir>`.

Three sessions, run in order on one image (built with `LAZYOS_RESET_OS=1`, so
nothing is installed yet), check it end to end: `lazyrad_home.json` makes and
installs a copy of `hello` whose `form_load` writes `proof.txt`
(`LRPLAY:DATA:PASS:/home/admin/.apps/user.admin.hello`), then after a reboot
`lazyrad_home_project.json` reads that file back and creates `MyApp` with
File -> New Project (the dialog opens in `/home/admin/projects`), and after
another reboot `lazyrad_home_reboot.json` finds
`/home/admin/projects/MyApp/MyApp.lrp` and `proof.txt`, and nothing under
`/apps/<id>/*/data`.

## Running

```bash
python tools/xui/build.py && python tools/lazyrad/build.py
LAZYOS_DESKTOP=1 LAZYOS_XUI_APPS=target/xui/xui-term.elf LAZYOS_XUI_AUTOSTART=term \
LAZYOS_LAZYRAD=1 LAZYRAD_SAMPLES="<lazyrad>/examples/hello;<lazyrad>/examples/calculator" cargo build
python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/lrplay \
    --script tools/screenshot/examples/lazyrad_hello.json --fail-on "LRPLAY:[A-Z]+:FAIL"
```

The `lazyrad_*.json` sessions need the samples: an image built without
`LAZYRAD_SAMPLES` has no `/system/share/lazyrad/hello`, and `lrplay` then stops
with `LRPLAY:ARGS:FAIL:no project at ...`. From the CLI front end:
`python tools/run_demo.py --desktop --lazyrad-samples "<lazyrad>/examples/hello;<lazyrad>/examples/calculator;lazyrad-os/samples/perf2000"`.

In the Terminal: `/system/bin/lrplay --client /system/share/lazyrad/hello &`. Command line:
`lrplay [--client] [--project <dir> | <dir>] [attempt=N]`; see `src/args.rs` for
how a relative `--project` and the default `resources/project` resolve (against
the install directory, located from `argv[0]`; `current_exe()` is `/busybox` on
LazyOS today).

## LazyRAD dependency and the rev pin

LazyRAD is a git dependency on `va1erian/lazyrad` with `default-features = false`
(no `winit`, `rfd`, `directories`, `dark-light`, local-time). The pinned `rev`
is the head of LazyRAD's `lazyos-msg` branch (event sources, scoped
extensions, host permissions; see the comment in `Cargo.toml`) until it
merges. To develop against a local checkout, patch every LazyRAD crate, either
on the command line (`cargo test --config 'patch."https://github.com/va1erian/lazyrad".lazyrad-runtime.path="<lazyrad>/crates/lazyrad-runtime"' ...`)
or in the LazyOS repo's `.cargo/config.toml` (cargo reads config from the
working directory, which `tools/lazyrad/build.py` sets to the repo root; do
not commit it), and restore `Cargo.lock` afterwards (a patch rewrites it):

```toml
[patch."https://github.com/va1erian/lazyrad"]
lazyrad-runtime  = { path = "<lazyrad>/crates/lazyrad-runtime" }
lazyrad-player   = { path = "<lazyrad>/crates/lazyrad-player" }
lazyrad-project  = { path = "<lazyrad>/crates/lazyrad-project" }
lazyrad-packager = { path = "<lazyrad>/crates/lazyrad-packager" }
xui-form         = { path = "<lazyrad>/crates/xui-form" }
xui-rhai         = { path = "<lazyrad>/crates/xui-rhai" }
```

**After the merge:** set `rev` on every `va1erian/lazyrad` line in `Cargo.toml` to
the merge commit, delete the patch, and commit `Cargo.lock`. Bumping xui is the
procedure in [`docs/xui-plan.md`](../docs/xui-plan.md): LazyRAD's workspace, this
`Cargo.toml` and `xui-app/Cargo.toml` must all name one xui revision (today
`4c2a4fb`).

## Tests

```bash
cd lazyrad-os && cargo test    # args, platform, markers, tracker, tests/lzp_conformance.rs, tests/samples.rs, tests/modplayer.rs
# LAZYRAD_SAMPLES=<lazyrad>/examples/hello;... adds the real samples to the conformance run
```
