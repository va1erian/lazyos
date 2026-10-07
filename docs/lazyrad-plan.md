# Plan: LazyRAD on LazyOS (run the IDE, produce LazyOS apps)

**Goal.** Two deliverables, in this order:

1. **Produce:** a LazyRAD project (forms + Rhai) can be packaged as a *LazyOS app*
   that appears in the Start menu, launches through `init`/`mimed` like Editor or
   Paint, and runs sandboxed as an `xuid` client.
2. **Run:** the LazyRAD IDE itself (designer, editor, run/debug) runs as a LazyOS
   desktop app, so a user can build apps on LazyOS without a host machine.

[LazyRAD](https://github.com/va1erian/lazyrad) is a VB6-style RAD IDE in Rust on xui
+ Rhai (its own `PLAN.md` §11-§12 defers "M7: LazyOS" to exactly this). This plan
is the LazyOS half; the LazyRAD half is the platform seam in P0.

> Status: proposal. Everything marked **(verify)** is an assumption read from the
> docs that a spike must confirm before the phase that depends on it.

> **Update ([`lazyrad-package-plan.md`](lazyrad-package-plan.md), phase A).** LazyRAD
> is no longer an unlabelled system program. The IDE and the player are the core
> package `os.lazy.lazyrad` (`xui-app/packages/lazyrad`, `LAZYOS_LAZYRAD=1` images):
> `pkgd` installs `bin/lazyrad.elf` and `bin/lrplay.elf` into `/apps` at boot,
> the player is found beside the IDE, and the IDE's data is in
> `$HOME/.apps/os.lazy.lazyrad`. Make LazyOS App no longer calls `pkgd` (it
> refuses labelled callers): it hands the package to the Installer (section 9).
> The `/system/bin/lrplay` and `/system/bin/lazyrad` paths in the P1-P4 text
> below describe those phases as they were built.

---

## 1. Why this is mostly packaging, not porting

LazyOS already has the hard parts, which is the reason the plan is short:

| LazyRAD needs | LazyOS today | Source |
|---|---|---|
| static `x86_64-unknown-linux-musl` `std` binaries | Linux ABI shim L0-L4 done: mmap/brk, threads + futex, `fork`/`execve`/pipes, signals, `/proc/self/mounts` | `linux-abi-plan.md`, `architecture/processes.md` |
| xui on the tiny-skia painter, no winit | `xui-app` `LazyOSBackend` (owner mode and `xuid` client mode), upstream `xui-canvas` with `default-features = false`, bundled fonts | `xui-plan.md` |
| Rhai | `libs/rhai-lazy` + `rhai-host` (`rhai` command, `/system/bin/rhai`), Rhai pinned `=1.26.1`, `sync` off, no `std`-only assumptions beyond musl | issue #319 |
| file dialogs (LazyRAD gap G4) | portable xui file dialog with `LazyFileSystem` mount-point wrapper (Editor/Paint/Files use it) | `xui-apps-integration.md` |
| clipboard (G2) | `clipboardd` client in `xui-app/src/platform/clipboard.rs` | same |
| window title, resize, maximize | `os.lazy.display.v1` `SetTitle`, resizable/maximizable windows (#412) | `window-resize-plan.md` |
| launching apps | `init` app registry + `os.lazy.init.Launch`, `mimed` open-with | `shell-plan.md` §9 |
| spawning child processes with pipes (debugger) | `xui-term` hosts BusyBox `sh` over a pipe pair; `fork`/`execve`/`pipe2` exist | `xui-plan.md` desktop session |
| persistent storage | the user's home (`$HOME`, `/home/<user>` on the ext2 OS volume or the home volume), VFS-backed descriptors, survives reboot (`persist` ABI fixture) | `linux-abi-plan.md` L2, `filesystem-plan.md` F4 |

What is **missing** and is the real work:

- **App registry (done in F5).** `init`'s compile-time table
  (`user/src/bin/init/apps.rs` `APPS`) now holds only the built-ins; every
  desktop app, LazyRAD-made ones included, is a package `pkgd` installed and
  `init` reads from confd's `sys/apps/*` (issue #509), so a user-produced app
  registers by being installed (P2).
- **No per-app sandbox profile for user apps** beyond what the security model
  describes (manifest -> profile compiled by `messengerd`/`init`, not built yet).
- **Version skew.** LazyRAD pins xui `1487ae1`; `xui-app` pins `58c1a6e`. Rhai
  features differ (`debugging`/`metadata`/`internals` vs the exact-pinned minimal
  set in `rhai-lazy`).
- **Only the home is the user's.** Since filesystem F4 everything LazyRAD
  writes lives under `$HOME` (`init` passes it to every session app);
  `/transient` is a ramfs and `/apps` belongs to `pkgd`.

---

## 2. Key decisions

| # | Decision | Choice | Why |
|---|---|---|---|
| D1 | What a produced app *is* | A standard **LazyOS `.lzp` package** ([`packages.md`](packages.md), `libs/lazypkg`, PR #431): `manifest.toml`, `bin/lrplay.elf` (the player, a copy of the stub), `icons/app-{16,32,128}.png`, and the project under `resources/project/**` (`.lrp`/`.lfm`/`.rhai`/assets). `entry.args = ["--project", "resources/project"]` **(verify how the installer resolves paths relative to the install dir `/apps/<system_name>/<version>-<hash>`)** | LazyRAD does not invent a format; the installer, registry and Start menu treat a LazyRAD app like any other package. The player copy is deflated and bounded by the format's per-entry and total caps. |
| D2 | Player size / sharing | First version ships the player **inside each package** (the format requires a `bin/*.elf`). Dedup of one shared player across packages is a later optimisation and needs a package-dependency concept that does not exist yet | Keeps P2 inside the format as specified. Size is controlled by an `opt-level = "z"`, fat-LTO, stripped player without `metadata`/`internals`; budget and measure it in P1. |
| D3 | Where LazyOS-specific code lives | In the **LazyOS repo**, in the existing standalone-workspace pattern (`xui-app`, `rhai-host`): `lazyrad-os/` with two bins that depend on LazyRAD crates by git `rev` and on `xui-app` (lib) for the backend | LazyRAD stays platform-neutral (its PLAN §12 `platform` module); LazyOS keeps ownership of init/registry/IDL. Same build style as `tools/xui/build.py`. Alternative (vendor into `xui-app/crates/`, as Editor/Paint were) is the fallback if git deps across repos are painful. |
| D4 | IDE <-> player debug transport | Keep LazyRAD's JSON-lines protocol; run it over **pipes** with `Command::spawn` first, Messenger later | Pipes and `fork/exec` exist; no protocol change needed. Fallback: `init.Launch` has no pipes, so then move the protocol onto a Messenger channel (defined in MIDL, see §5). |
| D5 | Script file access for produced apps | **Sandboxed by default on LazyOS**: read/write only under `$HOME/.apps/<id>/` (never inside the install tree `/apps`, which `pkgd` owns; filesystem F4) plus paths the user picks through the file dialog | Deliberate departure from LazyRAD's desktop default ("fs allowed"). LazyOS's model is default-deny with manifest permissions (`security-model.md` §4-§6); an unbounded Rhai `file` module would defeat it. |
| D6 | App identity | `manifest.toml` `[app].system_name` (reverse-DNS, `user.<author>.<project>`), name, version, declared `[permissions]`; `regd`/`pkgd` own registration | Matches the manifest-driven model; permissions shown at install/update. |

---

## 3. Phases

Each phase ends in something demonstrable with real pixels/serial markers, per
`AGENTS.md` (no graphics claim from source alone).

### P0 - Alignment and the platform seam (both repos, ~1 week)

**LazyRAD side** (PR against `va1erian/lazyrad`):
- Gate everything desktop-only behind features/`cfg(target_os)`: `rfd`,
  `directories`, `dark-light`, `time`'s `local-offset`, and any `xui-canvas`
  `winit-backend` use. `lazyrad-runtime`, `lazyrad-player` (lib), `xui-form`,
  `xui-code-editor`, `lazyrad-designer` must build for
  `x86_64-unknown-linux-musl` with `xui-canvas` `default-features = false`.
- Introduce `lazyrad-runtime::platform` (PLAN §12): trait(s) for `dialogs`,
  `config_dir`, `clipboard`, `spawn_player`, `fs_policy`. Desktop impl keeps
  today's behaviour; LazyOS provides its own.
- Make the player's entry point a library function taking a **backend factory**
  (`run_with_backend(args, |w,h| Box<dyn Backend>)`) so the LazyOS bin can pass
  `LazyOSBackend::connect()`. Today `run_cli` assumes the desktop backend.
- Add `lazyrad-runtime` feature `fs-sandbox` (root dir + allowlist) used by the
  `file`/`dir`/`path` stdlib modules; default on for the LazyOS player.

**LazyOS side:**
- Bump `xui-app` and LazyRAD to **one shared xui rev** (the rule in `xui-plan.md`:
  every `rev` moves together, then confirm `cargo tree` shows no
  `winit|softbuffer|glutin|glow|arboard|xui-gpu`).
- Reconcile Rhai: LazyRAD needs `debugging` (+ `metadata`, `internals` for the
  IDE only). Keep `rhai-lazy`'s exact pin; add the features in LazyRAD with the
  **same exact version** and confirm `sync` stays off and no `getrandom` is
  needed at startup. `lazyrad-runtime` (player path) must not enable
  `metadata`/`internals` so the image size stays down.
- CI job (new, in `.github/workflows/`): `cargo check --target
  x86_64-unknown-linux-musl` for the LazyRAD crates, so regressions surface in
  LazyRAD's own CI instead of at integration time.

Exit: `cargo build --target x86_64-unknown-linux-musl -p lazyrad-player` succeeds
with no windowing crates in the graph.

### P1 - The player runs on LazyOS (~1-2 weeks)

- New `lazyrad-os/` workspace (Cargo.toml, `src/bin/lrplay.rs`) and
  `tools/lazyrad/build.py` modelled on `tools/xui/build.py`/`tools/rhai/build.py`;
  `build.rs` embeds `/system/bin/lrplay` the way `rhai_embed` does, and a
  `LAZYRAD_SAMPLES` set of `.lrp` projects (hello, calculator, todo) onto the
  FAT volume under `/lazyrad/`.
- `lrplay --project <dir>`: connect as an `xuid` client
  (`LazyOSBackend::connect`), register fonts (`xui_app::font`), run the form,
  set the window title from `form.title` (`SetTitle`), close on `WINDOW_CLOSE`.
- LazyOS `msg_box`: G14 (blocking modals on canvas) is unresolved on LazyOS as
  well, so keep LazyRAD's non-blocking in-window `Dialog` + callback. No new work.
- Extra Linux-ABI/musl smoke: run the player under `tools/abi` style fixtures if
  any syscall returns `ENOSYS` (`python tools/abi/coverage.py` lists them).

Verification:
```bash
python tools/xui/build.py && python tools/lazyrad/build.py
LAZYOS_DESKTOP=1 python tools/run_demo.py --headless   # image builds, /system/bin/lrplay present
python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/lrplay \
    --script tools/screenshot/examples/lazyrad_hello.json   # new: click Say hello
python tools/screenshot/pngstats.py shots/lrplay/*.png --min-nonblack 0.01 --min-colors 50
```
Serial markers to add and assert: `LRPLAY:UP:PASS`, `LRPLAY:EVENT:PASS` (handler
ran), `LRPLAY:EXIT:PASS`. Read the PNGs: the label must actually read
"Hello, ...". CI: extend `.github/workflows/xui.yml`.

Exit: the Hello and Calculator samples run on a LazyOS desktop with correct
pixels, mouse and keyboard input, and title bar.

### P2 - Produce LazyOS apps as `.lzp` packages (~2 weeks; installer part depends on the package-system session)

The package format (`libs/lazypkg` reader, `tools/pkg/build.py`, `docs/packages.md`)
and the installer/registry (`pkgd`, `regd`) belong to the *LazyOS application
package system* work (PR #431 is its reader; the installer is a later phase).
LazyRAD **consumes** them and must not fork the format. Until `pkgd` lands, P2
produces and validates packages and installs them through a trait with a
dev-install fallback.

1. **`lazyrad-packager`** (LazyRAD repo): Rust **zip writer** (stored + deflate,
   no zip64/data descriptors, same name rules as the reader; `.png` stored) and a
   `build_package(project, options) -> Vec<u8>` that lays out §1 of
   `packages.md`: manifest from the project's `.lrp` (name, version, author,
   `system_name = user.<author-slug>.<project>` unless set), the player stub,
   icons (generated default icons when the project has none), and the project
   under `resources/project/`. Runs the project's compile check first (LazyRAD
   PLAN §8).
   **Every test builds a package and re-opens it with `lazypkg::Package::open`**
   (dev-dependency on `libs/lazypkg`, which is `no_std` + `alloc` and usable on
   host), plus a cross-check that `tools/pkg/build.py` accepts the same tree.
2. **Permissions.** The manifest's `[permissions]` is derived from what the
   project uses: `files = ["read:$HOME/.apps/<id>", "write:$HOME/.apps/<id>"]`
   (private storage, D5; since F5 an absolute `/home/...` rule is refused, see
   va1erian/lazyrad#87), `interfaces = ["os.lazy.clipboard.v1"]` only if the stdlib
   `clipboard` module is used, `network = []`. The player enforces the `files`
   list itself (fs sandbox from P0) until `messengerd` compiles profiles.
3. **Install.** `trait Installer { fn install(&self, lzp: &[u8]) -> Result<InstalledApp, _> }`:
   the LazyOS implementation calls `pkgd` over Messenger using its **MIDL
   client** (generated; never hand-written, per `AGENTS.md`). Until `pkgd`
   exists, a dev fallback saves the `.lzp` for a later install and reports
   "saved, not installed" (unused on LazyOS, see P4). Do not add a second registry.
4. **Start menu / `mimed`:** nothing to build here; it comes from `regd` when
   `pkgd` installs the package. Register `.lrp` -> LazyRAD IDE via the IDE's own
   package manifest `[[mime]]`.

Tests (mandatory, `AGENTS.md`): writer round-trip and malformed-option cases
(bad `system_name`, oversize player/project, name collisions, case-folding
collisions, traversal in project file names, symlinks in the project tree,
1000+ file project at the `MAX_ENTRIES` edge), plus a soak loop building and
re-opening hundreds of packages. Any `init`/kernel change goes through
`python tools/test/run.py --accel none`.

Exit: the IDE-independent CLI `lazyrad-pack <project> --out dist` produces a
`.lzp` that `tools/pkg/build.py`'s validator and `lazypkg` both accept, and (when
`pkgd` is available) installs, appears in the Start menu, launches, and survives
a reboot.

### P3 - The IDE runs on LazyOS (~3-4 weeks)

- `lazyrad-os/src/bin/lazyrad.rs`: the IDE as an `xuid` client. The IDE is a
  multi-pane single window (designer, editor, explorer, output) so the
  single-window constraint (gap G16) is fine.
- LazyOS `platform` implementation, reusing `xui-app/src/platform/`:
  - **dialogs:** the portable explorer file dialog over `LazyFileSystem`
    (open project, save as, Make App). Default start dir `$HOME/projects`
    (created by the IDE), then `$HOME`, then `/transient`.
  - **clipboard:** `clipboardd` client (already used by the Editor). Replaces
    LazyRAD's in-process clipboard for the code editor and designer.
  - **config/settings:** persisted in `$HOME/.apps/lazyrad/config/` or through `confd`
    (**decision to make:** `confd` is the platform standard; the IDE's TOML
    settings file is simpler. Start with the file, move to `confd` if Settings
    wants to show them).
  - **theme:** follow the desktop theme (`libs/uitheme`) instead of
    `dark-light`.
  - **run/debug:** `spawn_player` = `Command::new("/system/bin/lrplay")` with stdin/
    stdout pipes, keeping the JSON-lines debug protocol. **Spike first (verify):**
    a process spawned by a `xuid` client must itself be able to open a
    window as an `xuid` client (session/display grant inheritance). If it
    cannot, fall back to D4's Messenger transport plus `init.Launch`.
- Editor performance check: the custom `xui-code-editor` paints per visible
  line with tiny-skia on a software display under TCG/WHPX; measure typing
  latency with a 2000-line file (`tools/bench`), because the LazyOS present path
  is not zero-copy yet.
- Fonts: embed JetBrains Mono (editor) and Droid Sans (UI) via `include_bytes!`
  as `xui-app` does; no font scanning (anonymous `mmap` only).

Verification: new sessions `lazyrad_ide.json` (open sample, drag a button, edit
a property, double-click to create a handler, type code, F5) and
`lazyrad_debug.json` (breakpoint, step, locals). Assert `LRIDE:UP:PASS`,
`LRIDE:RUN:PASS`, `LRIDE:BREAK:PASS`, and read the screenshots.

Exit: on a LazyOS desktop, open the Hello sample, change the label text, press
F5, see it run in a second window, hit a breakpoint and inspect locals.

### P4 - Make App inside the IDE (~1 week)

- *File -> Make LazyOS App*: validate, pack (P2 `.lzp`), install through
  `pkgd`, offer "Run". *Manage Apps* lists and
  removes what the IDE produced.
- App icon: the three required PNG sizes come from the project icon (resampled) or
  a generated default.
- Version bump on update re-shows the permission diff.

Exit (the headline demo): build "todo" in the IDE on LazyOS, *Make App*, close
the IDE, launch the app from the Start menu, reboot, launch it again.

### P5 - Hardening and docs (~1 week, overlaps)

- Resource limits in the LazyOS player: Rhai op budget, memory, call depth
  (`rhai-lazy`'s `Limits` are a starting point), relaxed only for trusted IDE
  runs. A runaway script must not wedge a desktop session: prove with a
  `while true {}` sample that Ctrl+Break/close still works.
- Crash handling: a player crash must surface as an `init` supervision event,
  not a silent vanish.
- Docs: `docs/architecture/userland.md` (new rows), `docs/lazyrad.md`
  (user-facing), update `docs/xui-plan.md` status, `AGENTS.md` "Commands".
  Keep every new source file under 500 lines.

### P6 - Stretch (unscheduled)

- LazyOS stdlib modules in the Rhai runtime. **Done for Messenger**
  ([`lazyrad-messenger-plan.md`](lazyrad-messenger-plan.md)): the player
  installs `msg` and the generated `sys::*` modules (calls to any IDL
  interface, topics, services written in Rhai; [`rhai/msg.md`](rhai/msg.md)),
  the form's window delivers events, and Make LazyOS App declares the
  interfaces and topics the scripts use. **Done for sound**
  ([`lazyrad-modplay.md`](lazyrad-modplay.md)): the `modplay` module plays
  ProTracker songs through the system mixer, and the MOD player sample is
  shipped as `/system/share/samples/modplayer.lzp`. Still open: `notify`, and plain PCM playback
  (`audio.beep`) for scripts.
- One shared player across packages (needs a package-dependency concept).
- Multi-window forms (`form.show()`) once `xuid` clients can own several
  surfaces; until then show secondary forms as in-window dialogs (G16).
- Networking (`http.get`) after `docs/networking-plan.md` lands.

---

## 4. Risks

| Risk | Mitigation |
|---|---|
| A process spawned by the IDE cannot obtain its own `xuid` window | P3 spike before committing to pipes; D4 fallback (Messenger channel + `init.Launch`) |
| Cross-repo builds (git rev pins in two repos drift again) | One documented bump procedure (as in `xui-plan.md`); a CI check that both lockfiles resolve a single `xui-core` rev |
| IDE too slow on the software present path | Measure in P3 before polishing; dirty-rect presents already exist; the editor only paints visible lines |
| Rhai features inflate `/system/bin/lrplay` | Player builds without `metadata`/`internals`; `opt-level = "z"`, fat LTO, `strip` as `rhai-host` does; track image size in CI (the FAT image is already ~29 MiB) |
| `init` registry grows past the small task table / file limits | New `appd` service rather than more code in `init` if needed; soak test in P2 |
| User scripts escape the sandbox through `file`/`dir` | D5 allowlist implemented in `lazyrad-runtime`, plus tests for traversal, symlinks and absolute paths |
| A malformed package crashes the installer | `lazypkg` already bounds and fuzzes the reader; the packager only writes, and its output is re-validated by `lazypkg` in tests |
| Scope creep into a BASIC dialect or a compiler | Out of scope; LazyRAD's decision (§1.1 there) holds: VB6 workflow, Rhai language |

## 5. Conventions this plan must respect (from `AGENTS.md`)

- Every Messenger interface is defined in a `.midl` under `idl/` and generated with
  `midlc`; no hand-written TLV encoders for the registry/install surface.
- Kernel/service changes ship correctness **and** stress tests and pass
  `python tools/test/run.py --accel none`.
- Graphics claims are verified with `qemu_shot.py`/`qemu_session.py` screenshots
  that are actually read, plus `pngstats.py` assertions; sessions and serial
  markers are CI-checked (`.github/workflows/xui.yml`).
- `unsafe` stays minimal with `// SAFETY:` comments (the LazyRAD crates
  `forbid(unsafe_code)`; the LazyOS glue should follow).
- Source files under 500 lines.

## 6. Order of work and rough size

| Phase | Deliverable | Size |
|---|---|---|
| P0 | Shared xui/Rhai rev, platform seam, musl-clean crates | 1 wk |
| P1 | `/system/bin/lrplay` runs Hello/Calculator | 1-2 wk |
| P2 | `.lzp` writer + packager, `pkgd` install client | 2 wk |
| P3 | IDE on LazyOS with F5 and debugger | 3-4 wk |
| P4 | Make App in the IDE | 1 wk |
| P5 | Hardening, docs | 1 wk |

**First deliverable that answers "capable of producing LazyOS apps" is P2** (a
valid `.lzp`, installed once `pkgd` exists). **P3 + P4 answer "runnable in LazyOS"
end to end.** P1 alone already makes LazyRAD-authored apps run on LazyOS if they
are built on a host, which is a useful interim state.

## 7. Open questions for the owner

0. **Package system dependency:** P2's install step needs `pkgd`/`regd` from the package-system session (PR #431 is only the reader). Confirm the `entry.args` path convention and that a `bin/` ELF may be a Linux-ABI xui client.
1. **Repo layout:** `lazyrad-os/` in this repo with a git dependency on LazyRAD
   (recommended), or vendor LazyRAD crates under `xui-app/crates/` as was done
   for the Editor/Paint?
2. **App storage root:** resolved by filesystem F4: per user, in
   `$HOME/.apps/<id>/`; a project run from the IDE (not installed) uses
   `$HOME/.apps/lazyrad/data/`. Without `$HOME` LazyRAD falls back to
   `/transient/lazyrad` and warns (`LRPLAY:HOME:WARN`, `LRIDE:HOME:WARN`).
3. **Shared player:** is one player copy per package acceptable (D2), or should the format grow a dependency mechanism?
4. **Permissions enforcement depth:** is player-level enforcement (P2) acceptable
   until `messengerd` compiles sandbox profiles from manifests, or should the
   kernel-level profile work gate shipping user apps?

---

## 8. Implementation status and findings (P0-P3)

Status: P0 (LazyRAD seam), P1 (`lrplay`), P2 (`.lzp` packager) and the P3 IDE
client are implemented; P4 onward is not. The LazyRAD half lives on LazyRAD's
`lazyos-p0` branch; `lazyrad-os/README.md` explains the rev pin.

**Resolved `(verify)` items**

- **Installer path resolution (D1): the installer does not exist yet, so the
  player does not depend on it.** `lrplay` resolves a relative `--project` (and
  the default `resources/project`) against its *install directory*, found from
  `argv[0]`. `std::env::current_exe()` is **false on LazyOS**: the kernel hard-codes
  `/proc/self/exe` to `/busybox` (`kernel/src/process/linux/pathops.rs`).
  Requirement for `pkgd`/`init`: start an installed app with its absolute path as
  `argv[0]`, or with the install dir as cwd, or pass an absolute `--project`.
  Verified: `LRPLAY:PROJECT:PASS:/tmp/app/resources/project` for `bin/lrplay.elf
  --project resources/project` run from `/tmp/app`.
- **A `bin/` ELF may be a Linux-ABI xui client: true.** `/system/bin/lrplay` is a static
  musl `xuid` client and runs from the Terminal and from the IDE.
- **A process spawned by a `xuid` client can open its own window: true** (with
  `--client`; without it the player first tries to bind the display as owner and
  panicked, so the IDE launcher passes `--client`).
- **D4 pipes: spawning works, but the portable launcher does not.** LazyOS does
  not share a descriptor table between threads, so reader/exit-watcher threads
  lose the pipe and the wait status (the IDE saw an instant "exit" and killed the
  player). `lazyrad-os::launcher::PollingLauncher` polls non-blocking pipes on the
  window timer instead; no Messenger transport was needed.
- **Debugger: not available upstream.** LazyRAD's IDE and player have no debug
  protocol yet (its M4), so `lazyrad_debug.json` / `LRIDE:BREAK:PASS` are not done.
- **Clipboard:** needs no LazyRAD seam; xui routes it through the backend.
- **Rhai/getrandom:** with `default-features = false` only a build-time proc-macro
  pulls `getrandom`; nothing at runtime. One xui rev (`58c1a6e`) and one Rhai
  (`=1.26.1`) in both repos.

**Other findings**

- `/tmp` (kernel-heap ramfs) cannot hold a 5 MiB player copy (`cp: Out of memory`).
- Sizes (release, `opt-level = "z"`, fat LTO): `lrplay.elf` about 5.3 MiB (2.1 MiB
  deflated in a package), `lazyrad.elf` about 7.3 MiB.
- IDE responsiveness is low (input reflected after seconds when the window is
  large); typing into a 2000-line, 150 KB module updates the document within
  about 20 ms to 270 ms per key (`lazyrad_typing.json`), repaint not separately
  timed. Session scripts therefore pace steps 2-3 s apart.
- The in-window file dialog (painted `FileDialog` over `LazyFileSystem`) is
  implemented and renders, but **typing a path into it panicked on LazyOS**
  (`xui-core listview/api.rs:39` `RefCell already borrowed`, xui rev `58c1a6e`);
  not reproduced on the host. The IDE therefore also takes a project on its
  command line (`lazyrad [--client] <dir | .lrp>`), which the sessions use.
- Property-grid scrolling did not repaint rows in one probe (scrollbar moved,
  content did not); not investigated.

## 9. P4: Make LazyOS App and the `pkgd` client (verified)

> **Superseded by phase A.** The IDE is a labelled core package, and `pkgd`
> refuses labelled callers, so `lazyrad-os/src/pkgd.rs` is gone. Today: the IDE
> pre-checks the built `.lzp` in its own process (`pkgstore::inspect`, the
> function `Inspect` runs), stages it in `/transient`, calls
> `mimed.Open(path, "install")` (the Installer runs unlabelled, shows the
> consent screen and calls `pkgd.Install`), follows `system/events/pkg/install|
> denied` by `system_name` and digest, and starts the app with `init.Launch`
> (`lazyrad-os/src/handoff`, the `Installer` seam unchanged). The text below is
> the original `pkgd` client; the staging path, `argv[0]`, ABI and reboot
> findings still hold.

File → **Make LazyOS App…** (shown only when the platform gives the IDE an
installer): save, compile check, build the `.lzp` with the same player the IDE
runs programs with, `pkgd.Inspect` (the platform's own permission wording and every
`problems` entry), a consent dialog listing each permission with its risk, then
`pkgd.Install`, then an offer to run it through `init.Launch`. Code:
`lazyrad-os/src/pkgd.rs` (client, over the generated stubs, mock-tested),
LazyRAD `lazyrad-ide/src/make_app.rs` plus the `Installer::review/launch` seam.

**Staging path.** `pkgd` reads packages as root and, for an unprivileged caller,
only from `/transient`, `/system/share` or the caller's home. The installer stages
`/transient/lazyrad-<system_name>-<version>.lzp` (about 2 MiB deflated, within
the ramfs's limits and `Inspect`'s 8 MiB cap), calls `pkgd`, and deletes it.

**Resolved questions**
- `init` starts an installed app with `argv[0]` = the absolute
  `/apps/<system_name>/<version>-<digest8>/bin/lrplay.elf`
  (`LRPLAY:PROJECT:PASS:.../resources/project exe=/apps/.../bin/lrplay.elf`), so
  the player's `argv[0]`-based install-dir lookup works and the fixed
  `entry.args = ["--project","resources/project"]` resolves under it. Both a
  launch from the IDE (`init.Launch`) and from the desktop right-click menu work.
- The manifest must say `abi = "linux"` (`pkgd` records it, `init` picks the
  Linux personality); the packager now writes it.
- The app runs as the launching session's user under the label
  `app:<system_name>` (`PKGD:LAUNCH:LABEL`).
- It survives a reboot: a second boot on the same data disk replays it
  (`PKGD:RECONCILE:PASS n=1`), the menu lists it and it launches.

**Sessions** (data disk required: `python -m tools.mkdisk target/lr-data.img`):
`lazyrad_makeapp.json` (serial `LRIDE:PKG:REVIEW|INSTALL|LAUNCH:PASS`,
`PKGD:INSTALL:PASS`, `PKGD:LAUNCH:LABEL`, `LRPLAY:UP:PASS`) and, on the same disk,
`lazyrad_makeapp_reboot.json` (`PKGD:RECONCILE:PASS n=1`, the menu entry, launch).

**Caveats found**
- Intermittently the data volume is unusable at boot (`confd: /data/confd not
  usable: probe write failed (errno 22)`, then `PKGD:STORE:ABSENT`); retrying the
  boot works. The sessions wait for `PKGD:AUDIT:PASS` first so a bad boot fails
  early. Not investigated (kernel/ext2 side). An install on such a boot reports
  the friendly "no writable data disk" message (observed once).
- The install blocks the IDE's UI thread for its duration.
- "Manage Apps" (list/remove) is not done.
