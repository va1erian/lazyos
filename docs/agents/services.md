# System services, packages and security

Companion to [`AGENTS.md`](../../AGENTS.md); Developer tools and the playground, rhai, packages and the label trace, app failures, notifications, tray, accounts attacks, keyd secrets, shutdown.

## Developer tools and the Messenger Playground (`LAZYOS_DEVTOOLS=1`)

A **developer tool** is a package with `[app] kind = "developer-tool"`: an app
whose calls are chosen at run time by the user or a script, so no manifest can
list them. It runs under the label `tool:<system_name>`, whose Messenger rules
are default allow minus `devtool::DENY` (credentials unless the tool is the
user's shell, sockets unless `network` is declared, the dev-spawn scope unless
`develop`), its children keep
the label, and `xui_app::devtool` gives every tool the same title suffix,
strip (dismissible, remembered per user and tool), risk table and review gate. It is installed only by the
image build or by an administrator through `elevd`'s `pkg.install-devtool`.
Design and as-built notes: [`docs/devtool-packages-plan.md`](../devtool-packages-plan.md).
One switch ships every optional tool: `python tools/run_demo.py --devtools` (the
launcher's *Developer tools*; `full` suite). A tool with `always` in
`build_support/devtools_embed.rs` ships in every desktop image without it: the
Terminal, which is also the only tool with `shell = true` (a recorded property
that makes `pkgd` and `init` treat what runs under its label as the session's
owner; only the image's own packages can have it,
[`docs/devtool-packages-plan.md`](../devtool-packages-plan.md) section 10).

The Messenger Playground (`os.lazy.playground`, `xui-app/src/bin/playground*`,
portable core `xui-app/crates/playground`, examples in
`xui-app/packages/playground/examples/`) is the first tool: browse every
interface and topic, listen with a doorbell subscription
(`xui_app::fabric::Subscriber`, never polled on a timer: the broker never frees
its receive buffers), and run Rhai through a child `rhai`; see
[`docs/messenger-playground-plan.md`](../messenger-playground-plan.md).

```bash
python tools/run_demo.py --devtools
python tools/playground/run.py          # build, boot, six sessions, judge (LABEL:DENY, markers, screenshots)
python tools/playground/test_judge.py   # the judge fails when it should
cargo test --manifest-path xui-app/Cargo.toml -p xui-playground        # the portable core
LAZYOS_TEST_FILTER=label_tool python tools/test/run.py --accel none    # the kernel's `tool:` label
python tools/devtool/check_interfaces.py                                # every interface classified for tools
```

## Rhai scripting (`rhai` command and `msg` module)

`rhai` (`rhai-host/`, bindings in `libs/rhai-lazy/`) is a static-musl command
embedded as `/system/bin/rhai`; the plan is [`docs/rhai-plan.md`](../rhai-plan.md).
Its `msg` module calls any Messenger service from a script, driven by a table
`midlc --schema` generates from `idl/` ([`docs/rhai/msg.md`](../rhai/msg.md)),
and `midlc --rhai-api` generates one documented module per interface on top of
it (`sys::confd::get(...)`, `libs/rhai-lazy/api/`). LazyRAD form scripts on
LazyOS get both, with events delivered by the form's window
([`docs/lazyrad-messenger-plan.md`](../lazyrad-messenger-plan.md)).
The LazyRAD IDE and its player ship as the core package `os.lazy.lazyrad` in
images built with `LAZYOS_LAZYRAD=1` (`--lazyrad` implies `--desktop`): `pkgd`
installs it at boot under its own label like every other desktop app, so the
`lazyrad_*.json` sessions start the IDE through `init.Launch` and run the player
from `/apps/os.lazy.lazyrad/*/bin/lrplay.elf`; Make LazyOS App hands the package
to the Installer instead of calling `pkgd`
([`docs/lazyrad-package-plan.md`](../lazyrad-package-plan.md)).
One command builds `rhai`, BusyBox and the image, boots it and judges it:

```bash
python tools/rhai/run.py              # console checks (rhai_demo.json)
python tools/rhai/run.py --desktop    # plus the desktop Terminal and the msg session
python tools/rhai/run.py --lazyrad    # the LazyRAD Messenger sample (lazyrad_msg.json)
cargo test --manifest-path libs/rhai-lazy/Cargo.toml   # bindings vs an in-memory fabric
python tools/midlc/midlc.py --schema libs/rhai-lazy/src/msg/idl.rs --rhai-api libs/rhai-lazy/api --rhai-docs docs/rhai/api idl/*.midl   # after an IDL change
```

`python tools/run_demo.py` rebuilds `rhai` before each image (`--no-rhai` skips it).

## Packages and the label-policy trace

Every desktop app except Devices, the Installer and LazyShell
(the opt-in LazyRAD IDE, `LAZYOS_LAZYRAD=1`, included, and the Terminal, a
developer tool that is the user's shell) is a core package (`xui-app/packages/<short>/`, [`docs/packages.md`](../packages.md)):
`pkgd` installs it into `/apps` at boot and the kernel confines it to the
permissions its manifest declares. `LAZYOS_LABEL_TRACE=1` is the supported
debug switch for that policy: an image built with it (`kernel/build.rs`, cfg
`lazyos_label_trace`; off by default, and nothing else changes) prints one
`LABEL:DENY label=<label> iface=<id> method=<id>` (or `resolve=<name>`,
`topic=<name>`) serial line per refused call. Use it to derive a package's
permissions from a run instead of by hand:

```bash
LAZYOS_DESKTOP=1 LAZYOS_XUI_AUTOSTART=term LAZYOS_LABEL_TRACE=1 LAZYOS_RESET_OS=1 cargo build
python tools/screenshot/qemu_session.py --image target/lazyos.img \
    --out shots/core_apps --script tools/screenshot/examples/core_apps.json
grep LABEL:DENY shots/core_apps/serial.log   # map iface ids with idl/manifest.json
```

An IDE package with `develop = true` runs the project it edits under
`dev:<system_name>` (issue #529; `docs/packages.md`, "Development runs";
`kernel/src/ipc/devspawn.rs`). The session `lazyrad_devplay.json` plays a sample
that way from a test package of the IDE (build it with
`python tools/lazyrad/build.py && python tools/lazyrad/devtest.py`, then an
image with `LAZYOS_DESKTOP=1 LAZYOS_LAZYRAD=1 LAZYOS_XUI_AUTOSTART=term
LAZYRAD_SAMPLES=lazyrad-os/samples/devplay LAZYOS_LABEL_TRACE=1
LAZYOS_RESET_OS=1`) and must show no `LABEL:DENY`.

A package may also declare helper programs that run under a narrower label of
their own, `[processes.<name>]` tables whose `program` an `execve` of the app's
task enters as `app:<system_name>/<name>` (issue #832; `docs/packages.md`,
"Process tables"; `kernel/src/ipc/children.rs`). Such a label holds at most the
app's permissions, owns no names, and may not start programs. A traced run
prints `LABEL:ENTER label=<child> pid=<n> program=<path>` when a task enters
one and `LABEL:DENY label=<child> start=<call>` when it tries to start a
program; the kernel tests are `LAZYOS_TEST_FILTER=label_child python
tools/test/run.py --accel none`, and a ring-3 `execve` of a bound program
runs end to end in `label_child_real_execve`.

## App failures (the "stopped unexpectedly" notice)

`init` restarts services and the desktop shell as before, but a launched app
that fails while starting (or keeps crashing) is not restarted: `init`
publishes `system/events/app/<id>` on the central broker and LazyShell shows
one toast (an Error with *Restart* and *Close*, until dismissed) with the
app's name, its exit status and the reason the app gave through
`init.ReportFailure` (`lrplay` sends the player's error). The rules are
`libs/svcpolicy` (host-tested), the text `lazyshell::notice`, the toast
`xui-app/src/shell/notify/` ([Notifications](#notifications-toasts)).

```bash
cargo test -p svcpolicy
python tools/crash/run.py            # build, install crashload.lzp, open it from the menu, judge
python tools/crash/test_judge.py     # the judge fails when it should
```

## Notifications (toasts)

`os.lazy.notify` (`idl/notify.midl`, [`docs/notify-plan.md`](../notify-plan.md),
issue #747) lets a service or app tell the user something: LazyShell serves it
and draws the toasts, animated (spring in, icon flourish, eased reflow, hover
pauses, flick dismisses; `sys/ui/motion` = `full`/`reduced`/`none`). Nothing is
kept (no history; Warn and Error stay until dismissed), identity is the
kernel-identified app (a script from the Terminal shows as "Terminal"),
`sys/ui/notify/dnd` and `sys/ui/notify/mute` are the user's. From a script:
`sys::notify::notify("Info", "Title", "Body", (), [], (), (), ())`. A toast never
takes keyboard focus, so **an action must never be the only way to do
something**. The model is `lazyshell::notify`, the curves `lazymotion`
(`xui-app/crates/motion`); both are host-tested, no QEMU needed:

```bash
cargo test --manifest-path xui-app/Cargo.toml -p lazymotion -p lazyshell
FUZZ_CASES=20000 cargo test --manifest-path xui-app/Cargo.toml -p lazyshell --release notify::fuzz
python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/notify     --script tools/screenshot/examples/notify.json   # LAZYOS_DESKTOP=1 LAZYOS_UI_PROBE=1 LAZYOS_XUI_AUTOSTART=term, with rhai
```

Markers: `SHELL:NOTIFY:POST|SHOW|CLOSE|ACTION|DENY|LIMIT|SETTINGS`; with the UI
probe `UI:RECT name=toast:<label>` (`toast:retry`, `toast:dismiss`, `toast:body`).

## Tray icons (`LAZYOS_TRAYDEMO=1`)

Background apps show an icon in the taskbar's tray
([`docs/tray-plan.md`](../tray-plan.md)): LazyShell serves
`os.lazy.shell.tray`, one item per app, keyed by the caller's label. Apps use
the client libraries `xui_app::tray` (Rust xui apps) or `libs/trayclient`. The
sample `os.lazy.traydemo` ships only in `LAZYOS_TRAYDEMO=1` desktop images
(`run_demo.py --traydemo`, the launcher's *Tray demo* checkbox). Markers:
`SHELL:TRAY:SET|CLEAR|RESTORED|DENY`, `TRAYDEMO:*`.

The **Removable Drives** applet (`os.lazy.removable`,
`xui-app/src/bin/removable.rs`, shipped with `LAZYOS_USB=1` desktops) shows USB
sticks: notices as the tooltip and `Attention`, the mounted count as a badge, a
menu with Open and Eject per drive (`eject` is its command-line twin;
[`docs/architecture/usb-storage.md`](../architecture/usb-storage.md)); its
session is `python tools/storage/removable_run.py`.

A package with `[entry] resident = true` is a *resident app*: it runs once
per session, may live with no window (`xui_app::resident`: a windowless loop,
windows opened on demand and closed to the tray), always has a tray icon (the
shell gives a running resident app a default item with Open and Quit from
`init`'s retained `session/<s>/apps/resident`), and gets its lifecycle from
`init` over `os.lazy.init.app.v1`: `Reopen` on a second launch (queued until
it watches), `Quit` on a `Stop` with a hard 3 s grace from the `Stop` (then a
kill, `INIT:APP:QUIT:TIMEOUT`); `Stop` answers once every target is reaped
(`INIT:STOP:DONE`). A resident app the session's autostart opens gets
`--background` (`INIT:LAUNCH:BACKGROUND`; issue #928) and must start with its
tray icon only: ask `xui_app::resident::started_in_background()` before the
first window (session `tray_background.json`, image `LAZYOS_DESKTOP=1
LAZYOS_TRAYDEMO=1 LAZYOS_UI_PROBE=1 LAZYOS_XUI_AUTOSTART=term,traydemo`, judged
by `python tools/tray/background_check.py shots/<out>/serial.log`). The tray applets Volume (`os.lazy.volume`, every desktop:
the wheel sets the mixer's master volume) and Network Status
(`os.lazy.netstatus`, `LAZYOS_NETD=1` images) open with every session. The
lifecycle session needs `rhai` (`python tools/rhai/build.py`); the applets
session records the sound card and is judged by ear:

```bash
python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/tray_resident --script tools/screenshot/examples/tray_resident.json
LAZYOS_DESKTOP=1 LAZYOS_UI_PROBE=1 LAZYOS_XUI_AUTOSTART=term LAZYOS_NETD=1 LAZYOS_NETD_ARGS=demo=0 LAZYOS_RESET_OS=1 cargo build
python tools/screenshot/qemu_session.py --image target/lazyos.img --net --out shots/tray_applets --script tools/screenshot/examples/tray_applets.json --extra-arg=-audiodev --extra-arg=wav,id=a0,path=shots/tray_applets/volume.wav --extra-arg=-device --extra-arg=virtio-sound-pci,audiodev=a0
python tools/tray/volume_check.py shots/tray_applets/volume.wav    # TRAY:VOLUME:PASS: the 660 Hz tone at half the 440 Hz one
python tools/tray/test_volume_check.py
cargo test -p lazypkg -p pkgstore -p svcpolicy -p netpolicy
```

```bash
LAZYOS_DESKTOP=1 LAZYOS_TRAYDEMO=1 LAZYOS_UI_PROBE=1 LAZYOS_XUI_AUTOSTART=term LAZYOS_RESET_OS=1 cargo build
python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/tray --script tools/screenshot/examples/tray.json
python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/tray_menu --script tools/screenshot/examples/tray_menu.json   # menus, the Quit row; rebuild with LAZYOS_RESET_OS=1 first (it switches to the light theme)
cargo test -p trayclient -p messenger-generated
cd xui-app && cargo test -p lazyshell
```

## Account attack harness

`python tools/accounts/run.py` (issue #626, `docs/accounts-plan.md`) boots a
copy of the desktop image, runs attacks as the session user from the Terminal
(`rm` under `/system`, writes to `/conf`, confd `sys/**`, keyd `Provision`,
signals, reading `/home/admin`, fork and disk fill), powers off, reboots (also
after a hard kill) and audits the OS volume from the host
(`osread ... tree /`). Each attack has an expectation (`blocked` or
`xfail` with its issue) in `tools/accounts/attack_judge.py`: the U0-U2
scenarios (accounts, `elevd`, the prompt, `sys/**`, core apps) are `blocked`,
and only `fork_bomb` and `disk_fill` stay `xfail` until U3 (quotas); flip a
row to `blocked` when its phase lands. A policy probe counts as blocked only
as `BLOCKED:EPERM-policy` (the refusal carries the service's policy text). A
phase is not done until its scenarios are here. CI runs it in `xui.yml`'s
`accounts` shard. See `tools/accounts/README.md`;
`python tools/accounts/test_judge.py` is the judges' self-test.

## Named secrets in `keyd` (Wi-Fi prerequisites, WP2)

`keyd` keeps user-chosen secrets (`StoreSecret`, `DeleteSecret`,
`ListSecrets`, `WifiPmk`; `libs/secretstore`, `docs/security-model.md`
section 8) sealed under a machine key in `/conf/svc/keyd`. A `system` secret
changes only through `elevd`'s `net.wifi.system`; the PMK goes to `wlanmd`
(uid 912) alone.

```bash
cargo test -p secretstore -p elevpolicy
python tools/keyd/run.py            # build, three boots, judge (KEYD:SECRETS: PASS)
python tools/keyd/test_judge.py     # the judge fails when it should
```

## App secrets in `keyd` (`os.lazy.keyd.appsecret.v1`, P2b)

An installed app keeps its own passwords and tokens across reboots with
`Put`/`Get`/`Delete`/`List`, scoped to the caller's kernel-stamped uid **and**
label (`app:<system_name>`): an unlabelled caller, a `dev:` run and a helper
(`app:<name>/<child>`) are refused. They are sealed under the machine key in
`/conf/svc/keyd/appsecrets` (own magic and version; the `secrets` file is
untouched), at most 16 per app and user, and `keyd` deletes a removed
package's secrets (`KEYD:SWEEP:PASS`). A package lists the interface in its
manifest (MEDIUM); xui apps call `xui_app::platform::appsecret`, scripts
`sys::keyd_appsecret`. Mail's *Remember this password* uses it. Never log or
print a secret or its name's source.

```bash
cargo test -p secretstore -p pkgstore
python tools/keyd/run.py            # four boots: a labelled probe app keeps a secret across a reboot, another app cannot read it, the Terminal is refused, a removed app's secrets are swept
python tools/keyd/test_judge.py     # the judge fails when it should
```

## Shutdown and reboot

Only `init` stops the machine ([`docs/shutdown.md`](../shutdown.md)): its
`Shutdown` method stops the apps, then the services in reverse dependency
order, then calls the kernel's `power` (syscall 21). Never call `power()` from
anything else; ask `init` (`powerctl`, or `services::shutdown`). A service
that holds durable state serves `os.lazy.lifecycle.v1` (`idl/lifecycle.midl`)
and is listed in `user/src/bin/init/shutdown.rs` (`GRACEFUL`). The harness
boots the desktop twice (power-off from the shell, reboot from the menu) and
judges the serial logs (`logd` must report `persisted>0`, `confd` must stop
with `dir=/conf`, `pkgd` must stop through the lifecycle before `confd`, and
the second boot finds the first one's records in `/logs/service.log`); the
session scripts
drive the same paths by hand:

```bash
python tools/shutdown/run.py             # build, boot twice, judge (tools/shutdown/README.md)
python tools/shutdown/test_judge.py      # the judge fails when it should
python tools/screenshot/qemu_session.py --image target/lazyos.img \
    --out shots/shutdown --script tools/screenshot/examples/shutdown_shell.json \
    --extra-arg=-no-shutdown
python tools/screenshot/qemu_session.py --image target/lazyos.img \
    --out shots/reboot --script tools/screenshot/examples/shutdown_menu.json \
    --extra-arg=-no-shutdown
```
