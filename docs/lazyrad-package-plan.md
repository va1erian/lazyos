# LazyRAD as a package

Status: planned. Today the LazyRAD IDE and its player are unlabelled system
programs (`/system/bin/lazyrad`, `/system/bin/lrplay`, embedded by
`build_support/lazyrad_embed.rs` when `LAZYOS_LAZYRAD=1`, registered by hand in
`user/src/bin/init/apps.rs`). Every other desktop app except LazyShell, the
Installer, the Terminal and Devices is a core package
([`packages.md`](packages.md), "Core packages"). This plan makes LazyRAD one too.

## 1. Why it is not a package today

Two things a package label would break:

1. **Make LazyOS App.** `lazyrad-os/src/pkgd.rs` calls `pkgd`'s `Inspect` and
   `Install`; `pkgstore::access::may_inspect` and `may_manage`
   (`libs/pkgstore/src/access.rs`) refuse every labelled caller, whatever its
   manifest declares. This is the Installer's reason.
2. **Play.** `lazyrad-os/src/launcher.rs` forks `lrplay` on the project being
   edited and reads its pipes (the IDE's run console). A child inherits its
   parent's label, so under `app:os.lazy.lazyrad` the project's `sys::*` calls
   would be judged against the IDE's permissions, and `msg::serve` names would
   land in the IDE's namespace instead of the app's. A run would not behave
   like the installed app. This is the Terminal's reason.

`files` permissions are consent-only today (no kernel file sandbox), so file
access is not a blocker.

The plan keeps `pkgd`'s refusal of labelled callers as it is.

## 2. Phase A: the package and the Installer handoff (issue: LazyRAD A)

No kernel or policy change. It can land alone, but Play then runs under the
IDE's label (section 1, point 2) until phase B lands: projects that use
`sys::*` beyond what the IDE declares get `LABEL:DENY` in Play. Either land B
first or say so in the release notes and the IDE's run console.

### 2.1 The package

* `xui-app/packages/lazyrad/` (or a tree under `lazyrad-os/package/`):
  `manifest.toml` with `system_name = "os.lazy.lazyrad"`, `abi = "linux"`,
  `args = ["--client"]`, `category = "development"`, a `[[mime]]` for LazyRAD
  projects if one is registered, `bin/lazyrad.elf` and `bin/lrplay.elf`,
  icons (`tools/pkg/make_icons.py`) and `docs/*.md`.
* `tools/xui/core_packages.py`: `CORE_APPS` takes ELFs from `target/xui` only;
  add a source directory per app (`target/lazyrad/`) and the second binary.
  The package is optional: built and listed only when `LAZYOS_LAZYRAD=1`
  (`build_support/core_packages.rs` and `lazyrad_embed.rs`). Check the archive
  against `pkgd`'s 8 MiB limit.
* `user/src/bin/init/apps.rs`: remove the hand-written `lazyrad` row.
* The player path: `PLAYER_PATH = fhs::bin::LRPLAY`
  (`lazyrad-os/src/platform.rs`) becomes the `lrplay` beside the running
  `lazyrad` in its install directory. The packager copies the player into
  every `.lzp` it builds from that path.
* App data: `fhs::state::LAZYRAD_APP` (`$HOME/.apps/lazyrad`) becomes
  `$HOME/.apps/os.lazy.lazyrad`, with a one-time move of the old directory.
* Samples: inside the package, or kept in `/system/share/lazyrad`.
* Permissions derived from a run under `LAZYOS_LABEL_TRACE=1` (AGENTS.md,
  "Packages and the label-policy trace").
* `run_demo.py --lazyrad`, the GUI launcher (`tools/lazygui/catalog.py`,
  `test_catalog.py`) and the `lazyrad_*.json` sessions keep working.

### 2.2 Installing the apps it builds

`PkgdInstaller` is replaced by a handoff that never calls `pkgd`:

1. Validate the built package in-process with `lazypkg` (today a
   dev-dependency of `lazyrad-os`) for the IDE's own pre-check, replacing
   `Inspect`.
2. Write it to `/transient`.
3. `mimed.Open(path, "install")`. `mimed` asks `init` to launch the Installer
   as root into the caller's session, so it runs unlabelled and shows its
   trusted consent screen, then calls `pkgd.Install`.
4. Learn the outcome from `system/events/pkg/install` and
   `system/events/pkg/denied`, matched by `system_name` and digest. If the
   event payload lacks either, extend it in `idl/pkgd.midl` (generated code
   only).
5. Start the installed app with `init.Launch(system_name)`; `init` stamps an
   installed app with its own label, not the caller's.

New IDE permissions: `os.lazy.mimed.v1`, `os.lazy.init.v1`,
`subscribe:system/events/pkg/+`.

Known gap, optional: an `interfaces` entry grants every method, so
`os.lazy.init.v1` also grants `Stop` and `ListApps` (`Shutdown` refuses
labelled callers already). Method-level entries (`os.lazy.init.v1#Launch`)
would be tighter.

### 2.3 Documentation to change when phase A lands

`packages.md` "Core packages", `AGENTS.md` ("Packages and the label-policy
trace" and "Rhai scripting"), `docs/architecture/display.md`,
`tools/xui/core_packages.py`'s `CORE_APPS` comment, `lazyrad-os/README.md`,
`build_support/lazyrad_embed.rs`'s module doc, `tools/lazyrad/build.py`'s
docstring and `docs/lazyrad-plan.md`: LazyRAD is no longer an exception.

## 3. Phase B: development labels for Play (issue #529)

**Status: implemented** (branch `claude/lazyrad-package-phase-b`). Done: the
`dev:` label kind and its namespace; the spawn rule
(`kernel/src/ipc/devspawn.rs`, `LabelStamp::Develop`) with correctness and soak
tests in `kernel/src/tests/spawn_suite/dev*.rs`; `spawnv`'s
`personality::STDIO` (the child's 0/1/2 from three of the caller's
descriptors, which the table below did not foresee: a `spawnv` child starts on
the terminal, so "keeping the pipes" needed it); `os.lazy.process.label.spawn.v1`
in `idl/policy.midl`; `develop = true`; `pkgd`'s `Develop` (with a `confirm`
argument: `false` loads only an already approved set, `true` after the
Installer's consent) and its in-memory approvals revoked at logout; the
Installer's `develop` verb (`installer-develop` row, `--develop`);
`pkgctl develop`; `lazyrad_os::devplay` (Play under the label when the IDE runs
labelled, the plain fork otherwise) and `lazyrad --play-dev`. Waiting on phase
A: the IDE's own manifest (`develop = true`, `os.lazy.mimed.v1`,
`subscribe:system/events/pkg/+`). Until then `tools/lazyrad/devtest.py` packages
the IDE as a test app and `tools/screenshot/examples/lazyrad_devplay.json`
exercises the whole path.

Play must run the project under the permissions the installed app would get,
while the IDE keeps the child's pipes (fork + exec, not `init.Launch`).

| Layer | Change |
|---|---|
| Kernel labels (`kernel/src/ipc/labels.rs`) | A third kind, `dev:<system_name>`. `app_id_of` in `kernel/src/ipc/policy.rs` treats it as owning `app.<sn>.*` names and `app/<sn>/` topics, so served names and topics match the installed app. |
| Kernel spawn (`approve_labelled`, `kernel/src/process/creds.rs`; `spawnv` `AS_LABELLED`) | Today only an unlabelled privileged task may assign a label. New: a labelled caller may spawn a child into a `dev:` label when its own rules allow it on a new contractual interface `os.lazy.process.label.spawn.v1` with method id `fnv1a32(target label)` (the pattern of `os.lazy.messenger.names.resolve.v1`). uid, gid and session are the caller's; caps never widen; `app:` and `system:` targets stay refused. |
| `idl/policy.midl` | Declare `os.lazy.process.label.spawn.v1` next to `names.resolve.v1` (interface id only; nothing serves it). `LoadLabel` already accepts any well-formed label. |
| Manifest grammar (`libs/lazypkg`, `tools/pkg/pkgmanifest.py`, `libs/lazypkg/tests/cases/manifest.toml`) | `develop = true` under `[permissions]`; `pkgstore::rules::compile` turns it into the spawn rule for `dev:*`; `pkgd`'s explanation table: "Can run apps you are developing, with permissions you approve" (high risk). |
| `idl/pkgd.midl` | `Develop(path) -> (label: String, approved: Bool)`: validate the `.lzp`, compile its rules with `pkgstore::rules::compile`, load them for `dev:<system_name>`. Only the unlabelled Installer calls it (`may_manage` unchanged). `pkgd` keeps the approved rule set per `dev:` label in memory; an unchanged or narrower set is approved without asking again. Rules are dropped at logout and never persisted. Audited in `pkg.log`. |
| Installer | A `develop` verb (`mimed` registration for `application/x-lazyos-package`): "Run <app> from LazyRAD with these permissions", then `Develop`. |
| `lazyrad-os` | Play derives the manifest as Make LazyOS App does, ensures the dev label is approved (Installer via `mimed.Open(path, "develop")` when not), then spawns `lrplay` with `AS_LABELLED "dev:<sn>"` keeping the pipes. Needs the IDE's manifest to declare `develop = true` (phase A). |

### 3.1 Tests

Per AGENTS.md "Testing requirement for kernel components":

* kernel (`kernel/src/tests/`): spawn into `dev:` refused without the rule,
  allowed with it, refused into `app:`/`system:` labels, caps never widen,
  the child's names and topics resolve to the `dev:` app's namespace; a
  spawn/exit soak under a `dev:` label; `python tools/test/run.py --accel none`
  passes;
* host: `cargo test -p lazypkg -p pkgstore` (grammar, compile, `Develop`
  approval cache), `python tools/pkg/test_build.py`;
* integration: a session script (`tools/screenshot/examples/`) that plays a
  `sys::*` sample from the packaged IDE and shows no `LABEL:DENY` for the
  sample's derived permissions.

### 3.2 Alternatives rejected

* A broad IDE manifest so Play runs under the IDE's label: misleading consent,
  topics and served names still differ from the installed app, and it grows
  toward the 256-rule limit.
* Play as a real install plus `init.Launch`: no kernel change, but it loses
  the run console's pipes, prompts on every permission change and fills
  `/apps` and the menu with development builds.

### 3.3 Documentation to change when phase B lands

`packages.md` (manifest grammar, `develop`, `Develop`, the Installer's verbs),
`docs/architecture/ipc-security.md` (label kinds, spawning into `dev:`),
`docs/security-model.md`, `docs/lazyrad-messenger-plan.md` (Play runs under
the derived permissions), `docs/idl/` references regenerated by `midlc`.
