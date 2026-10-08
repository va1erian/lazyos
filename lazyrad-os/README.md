# lazyrad-os

LazyRAD on LazyOS (see [`docs/lazyrad-plan.md`](../docs/lazyrad-plan.md)): a
standalone Rust workspace, like `xui-app` and `rhai-host`, built for
`x86_64-unknown-linux-musl` by `python tools/lazyrad/build.py` and shipped as the
core package `os.lazy.lazyrad` (`xui-app/packages/lazyrad`, built by
`tools/xui/core_packages.py` from `target/lazyrad/`): `bin/lazyrad.elf` and
`bin/lrplay.elf`, installed by `pkgd` at boot under its own label like every
other desktop app (`LAZYOS_LAZYRAD=1`, `docs/packages.md`, "Core packages"). It
is not an unlabelled exception any more; nothing of it is in `/system/bin`.

| Bin | What |
|---|---|
| `lrplay` | The player: runs one LazyRAD project as a `xuid` client. It is also the stub copied into every `.lzp` package. The IDE finds it beside itself in the install directory (`platform::player_beside`). |
| `lazyrad` | The IDE as a `xuid` client (plan P3). |

The library (`src/lib.rs`) holds the LazyOS glue shared by both: command-line
parsing (`args`), the `lazyrad_runtime::platform::Platform` LazyOS installs
(`platform`: script file sandbox, config dir, the player beside the IDE), the
install handoff behind File -> Make LazyOS App (`handoff`, over `transport`), the
one-time data-folder move (`migrate`), the serial
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
`examples/lzpack.rs`; `samples/pictures`: the Picture Viewer, the core package
`os.lazy.pictures` of `LAZYOS_PICTURES=1` images, docs/lazyrad-pictures.md). `run_demo.py --lazyrad` and the GUI
launcher embed them under `/system/share/lazyrad/` next to any `LAZYRAD_SAMPLES` entries;
`python tools/rhai/run.py --lazyrad` boots the Messenger one and judges it.

## Where LazyRAD writes

Everything lives in the home of the user running it (`$HOME`, which `init`
passes to every session app; filesystem plan F4). Nothing is written under
`/data`, and installed apps never write inside `/apps` (`pkgd` owns it).

| What | Where |
|---|---|
| IDE settings | `$HOME/.apps/os.lazy.lazyrad/config/` (moved once from `.apps/lazyrad`, `migrate`) |
| projects (the file dialog's first stop) | `$HOME/projects/`, created by the IDE |
| data of a project run from the IDE or a shell | `$HOME/.apps/os.lazy.lazyrad/data/` |
| data of an installed app | `$HOME/.apps/<system_name>/` (manifest `read:`/`write:$HOME/.apps/<system_name>`) |
| packages staged for the Installer | `/transient/lazyrad-<system_name>-<version>.lzp`, deleted afterwards |

## Make LazyOS App

The IDE is labelled, and `pkgd` refuses every labelled caller, so it never
calls `pkgd` (`src/handoff`). It pre-checks the built package in its own
process (`pkgstore::inspect`, the function `pkgd`'s `Inspect` runs, which fills
its consent step), stages it in `/transient`, calls `mimed.Open(path,
"install")` (`init` starts the Installer unlabelled; it shows the trusted
consent screen and calls `pkgd.Install`), waits for the matching
`system/events/pkg/install` or `denied` event, and then `init.Launch`es the new
app. Only `midlc`-generated stubs are used. The wait blocks the IDE's window
until you finish in the Installer (cancelling there publishes no event, so a
cancel is reported after five minutes); `LRIDE:PKG:*` markers record each step.
The manifest names `os.lazy.mimed.v1`, `os.lazy.init.v1` and
`subscribe:system/events/pkg/+` for it.

Play (phase B of [`docs/lazyrad-package-plan.md`](../docs/lazyrad-package-plan.md)):
the manifest declares `develop = true`, so the packaged IDE runs the project
under `dev:<system_name>` with the permissions the installed app would get,
once the user approves them in the Installer (`src/devplay.rs`). An IDE started
unlabelled (from the Terminal) forks the player plainly.

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
python tools/xui/build.py && python tools/lazyrad/build.py   # the latter repackages os.lazy.lazyrad
LAZYOS_DESKTOP=1 LAZYOS_XUI_APPS=target/xui/xui-term.elf LAZYOS_XUI_AUTOSTART=term \
LAZYOS_LAZYRAD=1 LAZYRAD_SAMPLES="<lazyrad>/examples/hello;<lazyrad>/examples/calculator" cargo build
python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/lrplay \
    --script tools/screenshot/examples/lazyrad_hello.json --fail-on "LRPLAY:[A-Z]+:FAIL"
```

The `lazyrad_*.json` sessions need the samples: an image built without
`LAZYRAD_SAMPLES` has no `/system/share/lazyrad/hello`, and `lrplay` then stops
with `LRPLAY:ARGS:FAIL:no project at ...`. From the CLI front end:
`python tools/run_demo.py --desktop --lazyrad-samples "<lazyrad>/examples/hello;<lazyrad>/examples/calculator;lazyrad-os/samples/perf2000"`.

In the Terminal: `P=$(echo /apps/os.lazy.lazyrad/*/bin/lrplay.elf)`, then
`$P --client /system/share/lazyrad/hello &` (the player lives in the installed
package; the install directory name holds the version and a hash). The IDE
starts from the menu or with `init.Launch("os.lazy.lazyrad", <project>)`, which
the sessions do from a two-line `rhai` script. Command line:
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
`bb14ce9`).

## Tests

```bash
cd lazyrad-os && cargo test    # args, platform, markers, tracker, tests/lzp_conformance.rs, tests/samples.rs, tests/modplayer.rs, tests/pictures.rs
# LAZYRAD_SAMPLES=<lazyrad>/examples/hello;... adds the real samples to the conformance run
```
