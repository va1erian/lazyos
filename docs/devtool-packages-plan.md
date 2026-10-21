# Plan: developer-tool packages

**Goal:** one definition, one label and one look for the apps whose job is to act
with the *user's own authority* on a system they chose at run time: a scripting
window, a debugger, an inspector. Today such an app has two bad homes. It is an
ordinary package, confined to the calls its manifest lists, so it cannot do its
job (the Messenger Playground's scripts could call only what a hand-kept
manifest names, and could not serve the interfaces they want to). Or it is
built into `init` beside the Terminal and Devices, with no label at all: no
attribution, no install-time consent, no revocation, and every such tool
invents its own warning.

A **developer tool** is a package with `kind = "developer-tool"`. It runs under
its own label `tool:<system_name>`, whose Messenger rules are *default allow
with a short, reviewed deny list*, and it presents itself to the user the same
way every time. This is the "devtool role" of
[`permission-classes-plan.md`](permission-classes-plan.md) (shelved) done the
small way: it needs no IDL tags and no class tables.

Status: **implemented in this worktree, uncommitted** (T1 to T4 except the Settings group and revoke button; the Installer path and the first member, the Playground, included; T-later, the Terminal, in section 10). See the "As built" notes below and "Review findings" at the end. First member: the Messenger Playground
([`messenger-playground-plan.md`](messenger-playground-plan.md), D1 and D7).

---

## 1. What a developer tool is (and is not)

A tool qualifies when **the calls it makes are chosen by the user or by a
script the user runs**, so they cannot be listed in a manifest. It does not
qualify because it is "advanced" or lives in the `development` menu: an app
with a fixed set of calls keeps an exact `app:` manifest. Fabric Monitor, Tray
Demo and Net Tools stay as they are. The LazyRAD IDE stays an `app:` with
`develop = true`: its runs already get their own `dev:` label, and widening the
IDE itself would buy nothing.

| Question | `app:<id>` (today) | `tool:<id>` (new) | Terminal (unlabelled) |
|---|---|---|---|
| Messenger rules | Default deny; the manifest's exact allow list | Default allow, minus the deny list (section 3) | uid rules, no label |
| May register names / use topics | Only `app.<id>.*`, `app/<id>/` | Those, plus any name or topic that is not reserved (`os.lazy.*`, another app's, NIC and Wi-Fi names, other uids' `user/<uid>/`) | The same non-reserved names |
| Children (`rhai`, a shell) | Inherit the label (so are confined) | Inherit the label (so keep the tool's authority) | Unlabelled |
| Installs packages, approves `elevd` | Never (`pkgd` refuses labelled callers) | Never in v1 (same refusal), **unless the tool is the user's shell** (`shell = true`, section 10: the Terminal) | As the session owner |
| Install | Consent screen, or core | Core package, or an administrator through `elevd`, with the developer-tool warning | n/a (built in) |
| Attribution, audit, revoke | By label | By label | By uid only |

The last two rows of difference from the Terminal are deliberate for v1: a tool
cannot install software or raise administrator approval on its own. The
Terminal itself has since moved onto this class as a *shell* tool, after the
"labelled means refused" sites were audited (section 10); the last column of
the table is now the Terminal's row of the `tool:` column plus that flag.

## 2. Manifest

```toml
[app]
name = "Messenger Playground"
system_name = "os.lazy.playground"
category = "development"        # required for this kind; omitted means development
kind = "developer-tool"         # default "application"

[permissions]
network = []                    # tool-kind default: no sockets unless declared
develop = false                 # also honoured: spawning into dev: labels
# `interfaces` and `topics` must be empty or absent: a tool is default-allow,
# and a list that looks like a limit would be a lie.
```

`lazypkg` validates the kind, rejects non-empty `interfaces`/`topics` for it,
and keeps `network`, `develop`, `files`, `resident` and `[processes.*]` with
their current meaning. `network` still means something because the socket
interface is on the deny list unless declared (section 3).

## 3. Label and rules

* **Label.** `tool:<reverse.dns.name>`, the fifth form in
  `kernel/src/ipc/labels.rs` (`Kind::Tool`, same charset and length rules as
  `app:`). Children keep it. `dev:` already means "an app being run from an IDE";
  docs and code say *tool label* and *development run* and never overload them.
* **Rules.** `pkgstore::rules::compile_tool` produces, in order: an explicit
  deny for each interface in `DEVTOOL_DENY`, then `allow ANY_INTERFACE /
  ANY_METHOD`. About seven rules (six denies and the allow-all), so the 256 limit disappears as a concern. The
  kernel's rule engine already has first-match-wins, wildcards and explicit denies
  (`kernel/src/ipc/acl.rs`); **no new rule semantics are needed**.
* **The deny list** (`pkgstore::devtool::DEVTOOL_DENY`, a draft for review in
  T0):
  * credentials: `os.lazy.keyd.v1`, `os.lazy.keyd.appsecret.v1`,
    `os.lazy.accounts.v1`;
  * the dev-spawn scope `os.lazy.process.label.spawn.v1`, unless `develop = true`;
  * `os.lazy.net.socket.v1`, unless `network = ["outbound"]`;
  * the label policy loader, which is never a parcel a service receives anyway.
  `elevd` is *not* denied: its prompt is trusted and names the requester, which
  is what makes a script that asks for an administrator visible and honest.
* **Namespace policy** (`kernel/src/ipc/policy.rs`, about six sites: the
  `app_id_of`/`name_verdict` paths, topic namespace checks, `devspawn`, `handles`,
  `dev/syscall.rs`). A `tool:` task is treated as a user task that happens to
  carry a name: `Register` of a name outside the reserved namespaces is
  `ALLOWED_BY_NAMESPACE` (an unlabelled user task is `BOOTSTRAP_ALLOW` today),
  the interface-domain rule of issue #495 does not apply (so a tool *can* serve
  `os.lazy.echo.v1` under `demo.pirate`, which the Playground's examples need),
  and everything reserved stays reserved. That the tool may do this is a
  property of the label kind, not of a rule list.
* **Revoke.** Loading an empty rule list revokes the tool exactly as for an app
  (`pkgd` does this on Remove, upgrade and a failed load; already-running
  processes keep running but every Messenger call and registration they make is
  refused). A Settings group with a *Revoke* action is **not built**.

## 4. Install and consent

* **Core tools** ship in the image behind **one** switch, `LAZYOS_DEVTOOLS=1`
  (`run_demo.py --devtools`, the GUI's Simple tab *Developer tools*, a `devtools`
  entry in `tools/suites.toml` that `full` includes). It replaces a per-app
  switch such as `LAZYOS_PLAYGROUND`: tools are opted into as a class.
* **Third-party tools** are installable, but `pkgd` accepts a `developer-tool`
  package only from a core source or after an administrator approved it through
  `elevd` (a new operation, `pkg.install-devtool`, in the U2 table). A session
  owner who is not an administrator cannot install one.
* **Consent screen** (`pkgstore::explain`, drawn by the Installer) opens with the
  kind: "Developer tool. Through the system's services it can do anything you
  can do, except use passwords, keys and your accounts, and open network
  connections unless listed. Its programs and scripts run with your
  permissions." (`devtool::CONSENT_TEXT`; the limits bind the tool's Messenger
  calls, not a tool's own native code, see `security-model.md` 2.0.) The same
  text, and the risk table below, is the app's Help page.
* **Audit.** `pkg.log` records the kind; each tool's start and exit markers go
  through `xui_app::trace` as usual.

## 5. One look: `xui_app::devtool`

A small module so every tool behaves the same and the warning text exists once:

* `devtool::run(Channel, ...)`: the `launch::run` wrapper that adds the title
  suffix "(developer tool)", the strip ("Runs with your full
  authority, exactly like the Terminal. Read a script before you run it."), and
  a `<CH>:DEVTOOL:UP` evidence line.
* The strip is dismissible (this was "non-dismissible" in the first plan; the user
  asked for a way to close it). A small "x" at its right end (probe name
  `devtool:strip_dismiss`) hides the band and the layout reclaims the space
  (height 0). The choice is a `devtool::Notice`: a marker file
  `$HOME/.apps/<system_name>/devtool-notice-dismissed`, so it is remembered per
  user and per tool across launches, holds nothing secret, and one tool's choice
  never hides another's. A tool's windows share one `Notice` (an `Rc`) and tell
  each other to re-lay out. Dismissing hides **only the band**: the "(developer
  tool)" title suffix, the Help page's full warning and risk table, the review
  gate and the Installer's consent band (`bin/installer/notice.rs`, a different
  thing: install-time consent) are untouched. View > "Show developer-tool
  notice" (Ctrl+Shift+D in the Playground) brings the strip back and deletes the
  marker. A tool wires it with `strip(notice, Msg::Dismiss)` and
  `Notice::for_tool(SYSTEM_NAME)`, as the Playground does.
* `devtool::risks()`: the shared risk table (what a script can do, the
  consequence, what the app does about it), rendered on the Help page and on the
  consent screen, owned by this crate so the two never drift.
* `devtool::ReviewGate`: the "run something you did not write?" prompt for text
  that arrived by paste, drop, file or share, used by any tool that runs text.
* Menu: the `development` category; Settings -> Menu and Settings -> Apps show
  tools in a *Developer tools* group.

The Playground's own D7 (opt-in, never auto-run, explicit Run, hash-only run
markers, the app-specific risk rows) sits on top of this and stays in its plan.

## 6. Phases

| Phase | Delivers | Done when |
|---|---|---|
| T0 Spike | Read every site that treats a labelled task specially (`kernel/src/ipc/{labels,policy,devspawn,handles}.rs`, `dev/syscall.rs`, `pkgstore::access`, `elevpolicy`, `init`'s `app:` intern in `installed.rs`, `pkgd`'s `policy.rs`); decide the deny list; check `elevd` standing approvals for a `tool:` caller; decide how a tool's children are labelled when spawned by `init` vs `execve`. | A short "As built" note here; the deny list is a table with a reason per row. |
| T1 Manifest | `lazypkg` `kind`, validation, `pkgstore::rules::compile_tool`, `explain` text, `docs/packages.md` section. Host tests, including a check that every `DEVTOOL_DENY` name exists in `idl/manifest.json`. | `cargo test -p lazypkg -p pkgstore`; a CI check lists interfaces in `idl/` that no one has classified (below). |
| T2 Kernel | `Kind::Tool` and the `tool:` prefix, the namespace verdicts of section 3, `docs/architecture/ipc-security.md`. Kernel tests per AGENTS.md: correctness (every verdict, every reserved name still refused, children inherit, revoke) and stress (many tool-label loads and revokes, many registrations from tool tasks, label table at capacity). | `LAZYOS_TEST_FILTER=label python tools/test/run.py --accel none` and the full suite pass. |
| T3 Services | `pkgd` loads `tool:` labels and refuses a tool package from a non-admin; `init` interns `tool:` and starts the app under it; `elevd` operation `pkg.install-devtool`; audit lines. | A session installs a test tool as `admin`; a non-admin is refused; `pkg.log` shows both. |
| T4 UX and wiring | `xui_app::devtool`, Settings group and revoke, `LAZYOS_DEVTOOLS=1`, `run_demo.py --devtools`, GUI control, suite entry, `core_packages.py` knows the kind. | `tools/screenshot` session shows the strip, the title and the Settings group in both themes; `tools/lazygui/test_catalog.py`, `python tools/test_suites.py`. |
| T5 First member | The Playground ships as a developer-tool package (its plan, P0 onwards). | Its P1 passes on this class. |
| T-later | The Terminal as a developer tool, after auditing the "labelled means refused" sites (`pkgd` access, `powerctl`, `pkgctl`, `messengerctl`) and deciding whether a tool label should be treated like a session owner there. Then Devices if its kernel inspection can be named by a permission. | **Terminal done, section 10.** Devices is not: `os.kernel.dev` is still no permission. |

### As built (wiring, T4)

The build and launcher half of the class, in place before the kernel and
`pkgd` halves:

* **One switch.** `LAZYOS_DEVTOOLS=1` ships every developer-tool core package;
  `build_support/devtools_embed.rs` lists them (`TOOLS`: short id and program,
  today only `playground`) and `xui_embed::embed_xui_apps` adds them to the
  packages it embeds, exactly as it does for `LAZYOS_LAZYSKY`. The switch is
  refused (a build panic naming `run_demo.py --devtools`) without
  `LAZYOS_DESKTOP=1` or without the `rhai` command (`rhai_embed::find`, built by
  `tools/rhai/build.py`); a tool whose program or package was not built fails
  the build naming the command to run (`build_support/tests/devtools_tests.rs`).
  A default desktop image ships no tool.
* **A new tool** is added to `TOOLS`, `build_support/core_packages.rs`
  (`is_core_stem`), `tools/xui/core_packages.py` (`CORE_APPS`, `optional=True`),
  `tools/xui/build.py` (`BINS`), `tools/run_demo.py`'s `DEVTOOLS_APPS`
  (`tools/demo_builds.py`) and `xui-app/crates/app-icons`. The Playground's
  package is `xui-app/packages/playground/` (`kind = "developer-tool"`,
  `category = "development"`, no `interfaces` or `topics`, `network = []`).
* **Manifest validation.** `tools/pkg/pkgmanifest.py` knows `app.kind` and
  refuses `interfaces`, `topics` and `[processes.*]` for a developer tool, as
  `lazypkg`'s `tool.rs` does (a test in `tools/pkg/test_build.py`), because the
  core package build runs it.
* **Package layout.** A package may only have `bin/`, `icons/`, `idl/`, `docs/`
  and `resources/`, so `tools/xui/core_packages.py` moves the source tree's
  `examples/` to `resources/examples/` in the archive (`CoreApp.relocate`).
* **Front ends.** `python tools/run_demo.py --devtools` (implies `--desktop`,
  builds `rhai` and the xui apps through `build_devtools`, refuses `--no-rhai`;
  `--dry-run` prints the switch); the launcher's Simple tab *Developer tools*
  group and Advanced *Developer tools* checkbox (`tools/lazygui/appsteps.py`
  `wants_devtools`, `simplecfg.py`, `variables.py`, `ui.py`), whose build modes
  add a `rhai` step; the session script `xui_playground.json` (launcher entry in
  `scripts.py`, its `LAZYOS_DEVTOOLS` in `scriptenv.py`); the `devtools` flag in
  `tools/suites.toml`'s `full` suite and its row in `docs/build-switches.md`.
  Tests: `tools/lazygui/test_catalog_devtools.py`, `tools/test_run_demo_apps.py`,
  `tools/test_suites.py`, `tools/xui/test_core_packages.py`.

### As built (the Installer, section 4)

* **Detection** is the test `elevd` applies: the `kind` row of `pkgd`'s
  inspection with the value `developer-tool` (`xui_app::installer::tool`,
  `Package::is_developer_tool`). It only chooses what to show and which door to
  knock on; `pkgd` and `elevd` decide again.
* **Consent.** The Permissions step opens with a warning-tinted band carrying
  the `kind` row's words, then "an administrator must approve installing it",
  then the rows as `pkgd` sent them in sections: what every tool can do (`risk`
  rows), what none may use (`denied` rows), the package's own permissions by
  risk. Review shows the same class note. The button reads "Install...".
* **Install** goes to `elevd` as `pkg.install-devtool` (never `pkgd.Install`,
  which refuses a tool from a session), on the UI thread like the core-app
  update, then asks `pkgd` for the recorded row. An update of an installed tool
  is the same call. Cancel, timeout, "no administrator approved", "not a
  developer tool" and an app asking are each a sentence of their own
  (`tool::refusal_text`). Markers: `INSTALLER:CONSENT:TOOL risks=<n> denied=<n>`,
  `INSTALLER:ELEVATE pkg.install-devtool`, then the usual `INSTALL:PASS|FAIL`.

## 7. Keeping "default allow" honest

A default-allow label is a new idea in a system whose point is that apps hold
nothing, so it needs two guards that cost little:

1. **The deny list has an owner and a test.** `DEVTOOL_DENY` lives next to
   `rules.rs`; a host test asserts every name is in `idl/manifest.json`.
2. **New interfaces are classified.** `tools/devtool/check_interfaces.py` (a CI
   step) compares the interfaces in `idl/manifest.json` with
   `tools/devtool/reviewed_interfaces.txt` and fails when one is missing, with
   the message "decide whether a developer tool may call this: add it to
   `DEVTOOL_DENY` or to the reviewed list". A new credential-class service cannot
   become callable by tools by accident. (This is the cheap stand-in for the
   shelved IDL access tags.)

Also stated in `AGENTS.md` under "Project conventions": a new interface that
carries secrets or authority is added to `DEVTOOL_DENY`.

## 8. Open questions

1. The deny list, especially `elevd` (left allowed above) and whether
   `os.lazy.clipboard.v1` should need a declaration like `network` does.
2. Whether a tool may declare `develop = true` (a Playground that can also run
   projects as `dev:` labels) or that stays an IDE-only power.
3. Whether the tool label should count as "session owner" for `pkgd` and `elevd`
   standing approvals (phase T-later). **Answered (section 10):** for `pkgd` and
   `init`, only for a tool whose recorded row says `shell` (the image's own
   Terminal); never for `elevd`'s standing approvals.
4. `Kind::Tool` in `labels.rs` is the fifth form against a 256-label table;
   that is room for about 30 core packages plus tools, but T0 counts.
5. Naming: `tool:` and `kind = "developer-tool"` are proposals; the real
   constraint is not reusing `dev:`.

## 9. Review findings (independent review, 2026-10-21)

An agent that did not write the change reviewed it. Confirmed findings and what
happened (fixes are listed as they landed; anything not fixed says so):

1. **Standing `elevd` approval granted to a shared tool label** would cover
   every script the tool runs. Fixed: a tool label never gets or honours one.
2. **"No sockets" is not enforced for a raw `AF_INET` socket** (an unconfined
   label). The consent text and docs now say the limits bind the tool's
   Messenger calls; a tool's native code is the Terminal's authority. Not
   fixed: an inet-only gate for tool labels would close it.
3. **A revoked tool could still register unreserved names.** Fixed in the
   kernel: the free-name grant needs rules to be loaded; tests added.
4. **The review gate could be bypassed** by pasting a script shorter than the
   selection it replaced. Fixed: arrivals are detected by the inserted span.
5. **A duplicate, unused review gate** in `xui-playground` made its tests cover
   the wrong code. Removed.
6. **The administrator prompt omitted "for all users".** Fixed.
7. **Doc drift:** the label policy loader is now on `devtool::DENY`; the kernel
   tests' stand-in list says what it is. The Settings group is documented as
   not built. `RISKS` still lists posting notifications, which a tool's script
   cannot do today (below).
8. **A core tool could not be updated by an administrator.** Addressed by the
   services fix (see its As built note) or documented where not possible.

Suspected and accepted or open: an orphaned `rhai` child if the Playground is
killed (a live-run cap and a coalesced wake were added; parent-death handling is
reported separately); the subscriber's at-least-once drain and the broker's
unfreed receive buffers (a long listen is unmeasured); default allow rests on
every server refusing an unprivileged or labelled caller (spot-checked for
`xuid`, `inputd`, `logind`, `netdrv`, `keyd`, not for all 43 reviewed
interfaces: run `tools/accounts/run.py`-style attacks as a `tool:` caller
before widening the class); by design a tool, like the Terminal, can subscribe
to `session/<other>/...`, publish its own session's `apps/resident`,
`shell/tray` and `clipboard/changed`, and `dbgd`'s debug install can place a tool
without a prompt on a box with remote control enabled.

**Known gap, not a review finding:** a tool's script cannot post a toast: the
shell does not recognise a `tool:` child as a sender (see the Playground plan's
"As built").

## 10. T-later: the Terminal (done)

The desktop Terminal is the core package `os.lazy.terminal`
(`xui-app/packages/terminal/`), a developer tool that is **the user's shell**.
It runs under `tool:os.lazy.terminal`; its shell and every command typed in it
keep that label, as any tool's children do. The unlabelled built-in row in
`init` (`xui_app("terminal", ...)`, `fhs::bin::TERMINAL`) is gone.

The reason it was unpackaged was that a label is *refused* by the services that
guard the user's machine, so a packaged Terminal would have run `pkgctl`,
`powerctl` and `rhai` as a sandboxed app. The class (default allow, section 3)
fixes the Messenger half. This section is the other half: an audit of every
place a labelled caller is refused or treated differently (T0), and one
narrow, recorded property, `shell`, that lets `pkgd` and `init` treat the
Terminal like the session's owner while every other tool, the Playground
included, stays refused.

### 10.1 The `shell` property

```toml
[app]
kind = "developer-tool"
shell = true               # only valid with kind = "developer-tool"
category = "system"        # a shell may also be listed under System (see 10.4)
```

* **Declared, not named.** No service compares a label or a `system_name` to
  `os.lazy.terminal`. `lazypkg` parses `[app] shell` (`Manifest::is_shell`) and
  refuses it on any package that is not a developer tool; `tools/pkg/pkgmanifest.py`
  checks the same rule.
* **Core only.** `pkgd` records it in the app's `confd` row (`Installed.shell`,
  field 18) only for a package the image ships (`pkgstore::shellrole::shell_of`:
  the manifest asks *and* the `system_name` is in `/system/packages`). Any
  other package that asks (`shell = true` from a user, or an administrator's
  `pkg.install-devtool`) is **refused at install** with `SHELL_CORE_ONLY`. An
  administrator may update the core Terminal (`pkg.install-devtool` keeps the
  class, `toolgate::core_class_change`); that is their call, as for any core app.
* **Read from the row, never the package.** `init` and `pkgd` decide with
  `shellrole::row_is_shell(kind, shell, core)` on the row `pkgd` wrote. It
  re-checks that the row is a developer tool and shipped by the image, so a
  manifest edited after install, a row written by any other route, and a core
  package that left the image's set are no shell. `pkgd` refreshes every row
  from its stored manifest at boot (`refresh_row`), which recomputes the flag.
* **Session owner, not service.** `shellrole::as_session_owner` drops the label
  from the *caller* a service judges and never yields a system caller.
  A shell is not a system caller, an administrator, a developer-tool installer
  or `elevd`; it is what an unlabelled session program was.
* **Rules: a shell differs from every other tool in exactly one way.** The
  label's Messenger rules are the class's own (`compile_tool`): default allow
  minus `devtool::DENY`, and a shell tool does **not** have the three credential
  entries on its list (`DenyWhen::UnlessShell`: `os.lazy.keyd.v1`,
  `os.lazy.keyd.appsecret.v1`, `os.lazy.accounts.v1`). It acts as the session's
  user, exactly as the unlabelled Terminal did, and the credential services stay
  the authority: they refuse what the user may not do (`keyd` answers `Verify`
  to `accountsd` alone, `accountsd` needs the old password, an administrator's
  action goes through `elevd`, whose prompt remains the only gate). A shell
  still has the label policy loader, the dev-spawn scope (unless `develop`)
  and the sockets (until declared) denied. The Terminal declares
  `network = ["outbound"]` (its `curl`, `wget`, `ping`, `nc` and `smb` use the
  socket service), so it compiles to **two denies and the allow-all**. The
  Playground (`shell = false`) keeps the full list. `pkgd` honours the shell
  rules only for a core package: install refuses `shell = true` elsewhere and
  boot reconciliation refuses a stored manifest that claims it
  (`shellrole::shell_install`). The credential services see a tool-label child of
  the Terminal as the session user, never as an administrator. `shell` changes
  no kernel rule engine behaviour: it only changes which rules `pkgd` loads.

### 10.2 T0: every site that treats a labelled caller differently

"Terminal" below is a caller under a *shell* tool's label; "other tool" is any
other `tool:` label (the Playground) and an `app:` label, which are never
changed.

| # | Site | Labelled today | Terminal (shell tool) | Why |
|---|---|---|---|---|
| 1 | `pkgd` `access::may_manage`: `Install`, `Remove`, and the install half of `Develop` | Refused: "applications may not install or remove other applications" | **Allowed as the session's owner** (`Pkgd::dispatch` maps the caller with `as_session_owner` when `is_shell_label`) | `pkgctl install` is the Terminal's job. Every file-source and ownership rule (`source_allowed`, `may_change`) then applies exactly as to the unlabelled user |
| 2 | `pkgd` `may_inspect`: `List`, `Installed`, `Inspect` | Refused | **Allowed** | `pkgctl list`/`inspect` |
| 3 | `pkgd` `may_install_developer_tool` and `toolgate::install` | Refused (labelled), and a session owner is refused too: system callers only | Refused (a shell is not a system caller; `as_session_owner` clears `system`) | A developer tool is installed by the image or by an administrator through `elevd`; the Terminal could not before either |
| 4 | `pkgd` `LabelName` | `elevd` only | Unchanged (the mapping skips the method) | It tells `elevd` whether a label is a tool's |
| 5 | `elevd` `Operation::asker_permitted` (`pkg.install-devtool`) | Refused for any label | **Stays refused** | The Installer (unlabelled) is the only door, `pkgctl` has no such command, and a script in the Terminal asking an administrator to approve a developer tool would be the new thing. It is an easy follow-up if wanted: `pkgd.LabelName` could carry the flag |
| 6 | `elevd` standing view approval (`LabelKind`) | `Tool` and `Unknown` never stand; only an installed app's label does | **Stays refused** | Review finding 1: a tool's label is shared by every script it runs. The unlabelled Terminal never had a standing approval either (`Unlabelled` does not stand), so nothing is lost. Each Terminal request prompts |
| 7 | `elevd` request itself | Allowed from a session task of any label, naming the asker from its kernel stamp | Allowed (the same as before) | The prompt is the consent. `xuid`'s text now reads "The developer tool os.lazy.terminal, run by user (uid 1000)" instead of "A program without an app label" (`prompt.rs`) |
| 8 | `init` `Shutdown` (`shutdown::authorize`: `label_id == 0 && (CAP_SETUID or session)`) | Refused | **Allowed as the session's owner** (`InstalledApps::owner_cred`) | `powerctl` and the `shutdown`/`reboot` aliases |
| 9 | `init` `Launch` of a *built-in* program (`launch::target_cred` hands the caller's label to the child; the kernel's `As` stamp refuses a label the supervisor does not hold) | Fails for any labelled caller | **Allowed** (`owner_cred` drops the label for a shell) | `rhai`/menu launches of Devices or the Installer from the Terminal worked unlabelled. Installed apps are unaffected (they are stamped by name) |
| 10 | `init` `Stop`, `ListApps`, `Services` | No label rule (session rule only) | Unchanged | |
| 11 | `init` `RestartService`/`ReloadService`/`RelaunchApp`/`StartDriver`/`Home`, `dbgd`, `elevd`-only, `mountd`, `logind` greeter and session calls | Identity checks that need `label_id == 0` *and* `session == 0` or `CAP_SETUID` | Unchanged: a session task never qualified, labelled or not | Not a "labelled means refused" site in effect |
| 12 | `xuid` shell role (`may_be_shell`: labelled never) | Refused | **Stays refused** | An unlabelled Terminal could take the role when LazyShell was dead; a tool must not |
| 13 | LazyShell notification identity (`notify::policy::identify`) | A labelled sender that `init` did not launch itself is `NotAnApp` | Identified **by its label**: a process whose stamped label is a launched app's is that app. The Terminal's own and its children's notices keep the owner key `<terminal>` (users' mutes, `notify.json`) | Needed so `rhai ... notify` from the Terminal keeps working. `init.Services` carries the stamped label (`ServiceStatus.label`). This also closes the Playground plan's "known gap" (a tool's script could not post a toast) |
| 14 | Tray (`tray::policy::app_of`) | By pid | Unchanged | The Terminal has no tray item |
| 15 | `keyd` app secrets (`app:` label only), named secrets, `accountsd`, `confd` system keys, `timed`, `mimed` register, NIC/Wi-Fi name checks, `wlanmd`, `netd`, `homes` | Identity checks on service uids/caps | Unchanged | A session task, labelled or not, never qualified |
| 16 | Kernel Messenger policy (`labels`, `policy`, `topics`, `tool.rs`) | Default allow minus the deny list | No change in code | See 10.5 for what the Terminal user loses |
| 17 | Kernel file, socket, spawn and signal gates (`confine`) | Only confined labels (those with the manifest marker) | A tool is **not confined**: files, `fork`/`exec`, signals and `AF_INET` are the user's | |

Two rows deserve a note. Row 1 is the only way the Terminal could keep
`pkgctl`, and it is why `shell` is recorded by `pkgd` and not guessed from a
name. Rows 5, 6 and 12 are where this design deliberately does **not** treat
the Terminal as the session owner; each is the stricter side of a review
finding or of the class's own rule.

### 10.3 What stays refused (the Playground, and every other tool)

`as_session_owner` is the single place a labelled caller becomes a session
owner, and it needs the row's `shell` flag, core origin and tool kind. For the
Playground (no `shell`), for an `app:` label, for a `dev:` run, and for a
revoked or unknown label, `pkgd` and `init` see exactly what they saw before:
`pkgctl`, `powerctl` and an install from a Playground script fail with the same
messages. `tools/playground/run.py` and the host tests
(`pkgstore::shellrole`) pin this.

### 10.4 Decisions

1. **The class is opt-in, the Terminal is not: `always`.** Each tool in
   `build_support/devtools_embed.rs` `TOOLS` has `always`. The Terminal is
   `always: true` and ships in **every** desktop image (it also has to: almost
   every session script and harness types into it, `LAZYOS_XUI_AUTOSTART=term`
   and `term_*.json` among them). The Playground stays `always: false` and
   opt-in (`LAZYOS_DEVTOOLS=1`). `LAZYOS_DEVTOOLS=1` still requires `rhai`; the
   Terminal needs only the desktop. A missing `xui-term.elf` fails a desktop
   build with the command to run, as every default app does.
2. **UX: no strip, no title suffix.** The Terminal does not call
   `xui_app::devtool::run` and shows neither the non-dismissible strip, nor the
   persisted dismissible notice another change adds, nor " (developer tool)" in
   its title. Reasons: (a) the strip's text is "exactly like the Terminal", which
   would be circular in the Terminal itself; (b) the user typed every command, so
   there is no "script you did not write" to warn about, and the review gate
   (`ReviewGate`) is for tools that run text that arrived by paste, which a
   terminal's own paste confirmation (`TERM:PASTE`) already covers; (c) a text
   row of vertical space and a renamed window would break every script that
   clicks "Terminal" and every `UI:RECT`/`TERM:*` marker, for no reader. The class
   is disclosed where a decision is made: the package README
   (`/docs/apps/os.lazy.terminal`), the elevd prompt ("The developer tool
   os.lazy.terminal"), `pkgctl list` (`developer-tool`) and this document.
   A new tool that is not the user's own shell uses the strip.
3. **Launch, autostart, `mimed`.** `os.lazy.terminal` is core, so `pkgd`
   installs it at boot and `init` lists it first among the installed apps
   (`InstalledApps::refresh` sorts a shell before the other core apps), which
   keeps "the Terminal opens before the session's other autostart apps" (its
   windows and tab order in every script). `LAZYOS_XUI_AUTOSTART=term` selects
   the autostart variant of the archive (`core_packages::short_of("term")`), as
   it did for the built-in row; `user/build.rs` no longer sets a built-in
   autostart for it. The app id is `os.lazy.terminal` and its bare short id
   `terminal` still launches it (`alias_of`), so `xui_app::shell::launch(
   "terminal", cwd)` (**New Window**, `TERM:NEWWIN`), saved desktop launchers
   and `mimed` need no change. The built-in row's `verbs = ["open"]` registered
   no MIME type with `mimed` and nothing reads `AppInfo.verbs` of the Terminal,
   so the manifest has **no `[[mime]]`**. Markers that name the app id changed
   spelling because the app is now an installed package: `INIT:AUTOSTART:PASS
   app=os.lazy.terminal` and `SHELL:LAUNCH:PASS app=os.lazy.terminal` (nine
   harness and session files updated); `PKGD:LAUNCH:LABEL tool:os.lazy.terminal`
   is new.
4. **Terminal markers do not change.** `TERM:*` lines and probe names come from
   `xui-term` untouched (`xui-app/src/bin/term*`: no source change). The menu
   group stays **System**: `shell = true` lets a developer tool name
   `category = "system"` as well as `development` (`devtool::SHELL_CATEGORY`),
   so the Start menu's System submenu is as before.
5. **Notification owner stays `<terminal>`.** See row 13.
6. **Standing approval to a shared tool label: kept refused** (row 6). Showing
   it safe for a shell would need `elevd` to know the *script*, not the label.

### 10.5 What the Terminal loses by being labelled

These are consequences of the tool class, not of `shell`, and are accepted:

* Its Messenger calls to `os.lazy.messenger.policy.v1` are refused by the
  kernel, as for every tool. (The credential interfaces are **not** denied to a
  shell, see 10.1; they are to the Playground.) An early version of this change
  denied them to the Terminal too; the account attack harness
  (`tools/accounts/run.py`) caught it, because nine of its scenarios then ended
  in the kernel's refusal instead of the service's own policy text.
* A name or topic under `app/` or `system/...` that is not its own, a wildcard
  at the start of a subscription (`messengerctl tail '#'`), and publishing under
  `system/` are refused, as for every tool (`kernel/src/ipc/tool.rs`). Names
  under `system.` are reserved for system labels. Free service names and the
  waived interface-domain rule are as an unlabelled user task's.
* `pkg.install-devtool` through `elevd` (row 5).
* A Terminal-run program cannot take the shell role from `xuid` (row 12).

### 10.6 Security properties kept

Tool labels still cannot be gained by `app:` tasks or non-administrators (the
`approve_labelled` and `Assign` rules are untouched), a revoked tool still
cannot register names (`holds_rules`), and there is still no `dev:`-style spawn
(`os.lazy.process.label.spawn.v1` is denied unless `develop`). The new
property cannot be claimed: a manifest alone never sets it (`SHELL_CORE_ONLY`),
`pkgd` re-derives it from the manifest and the shipped set at every boot, and
both readers re-check class and origin. A shell is never a system caller.
`pkgd` and `init` ask the kernel for the label's name (`label_name`, which
needs `CAP_SETUID`), so a task cannot lie about it.

### 10.7 As built

* **Manifest and rules.** `lazypkg` (`App.shell`, `Manifest::is_shell`,
  `tool.rs`), `tools/pkg/pkgmanifest.py`, `devtool::SHELL_CATEGORY` and
  `tool_system_name`; `pkgstore::shellrole` (new: `shell_install`,
  `row_is_shell`, `shell_of`, `as_session_owner`, with tests) and
  `toolgate::tool_system_name`.
* **IDL.** `Installed.shell` (`idl/pkgd.midl`, field 18) and
  `ServiceStatus.label` (`idl/init.midl`, field 7), regenerated
  (`libs/generated`, `docs/idl`, `libs/rhai-lazy`, `idl/manifest.json`).
* **`pkgd`.** `Pkgd::dispatch` maps a shell caller; `Pkgd::is_shell_label`;
  `install.rs` refuses a non-core shell and records the flag in `row_of`.
* **`init`.** The built-in row is deleted (`apps.rs`, with its docs and
  selftest), `user/build.rs` no longer maps `term`, `InstalledApp.shell`,
  `InstalledApps::owner_cred` used for `Shutdown` and `Launch`
  (`protocol.rs`), the shell sorts first, `Services` reports each running
  installed task's label.
* **`xuid`.** The prompt names a tool (`prompt.rs`).
* **LazyShell.** `identify` by label (`crates/shell/src/notify/policy.rs`,
  `src/shell/notify/service.rs`, `services::launched_labelled`); the default
  desktop launcher is `os.lazy.terminal`.
* **Build.** `xui-app/packages/terminal/` (manifest, icons from
  `app-icons`, README), `tools/xui/core_packages.py` (`terminal` ->
  `xui-term.elf` as `bin/terminal.elf`), `build_support/core_packages.rs`
  (`is_core_stem`), `build_support/xui_embed.rs` (no `XUI_DESTINATIONS` entry,
  the Terminal leaves `DESKTOP_XUI_APPS`), `build_support/devtools_embed.rs`
  (`Tool.always`, `ships`), `libs/fhs` (`TERMINAL` removed).
* **Tests and harness.** Host tests in `lazypkg`, `pkgstore`, `devtool`,
  `lazyshell`, `build-support-tests`, `messenger-generated`,
  `tools/pkg/test_build.py`, `tools/xui/test_core_packages.py`; the session
  `tools/screenshot/examples/term_devtool.json` (below). No kernel code changed,
  so no new kernel test.

### 10.8 Verification

Guest runs (QEMU, WHPX, one at a time; each session boots a fresh copy of the
image because a session's installs persist in the OS volume, and the harnesses
that build an image need `LAZYOS_RESET_OS=1` after another harness's
developer-tools image, whose installed Playground would otherwise still autostart
and take the click):

| What | Result |
|---|---|
| `term_devtool.json` (image: `LAZYOS_DESKTOP=1 LAZYOS_UI_PROBE=1 LAZYOS_XUI_AUTOSTART=term LAZYOS_LABEL_TRACE=1 LAZYOS_RESET_OS=1`): `PKGD:LAUNCH:LABEL tool:os.lazy.terminal`; `pkgctl list/inspect/install/remove`; the elevd prompt names "The developer tool os.lazy.terminal" and `service.restart` is granted; `pkg.install-devtool` refused; `init.Launch` of a built-in; `keyd.Verify` refused by `keyd` itself (no `LABEL:DENY`); `shutdown` accepted | pass |
| `term_devtool_refused.json` (the Playground image of `tools/playground/run.py`): a script run by the Playground gets `PKGD:INSTALL:FAIL applications may not install or remove other applications` and `os.lazy.init.v1.Shutdown` refused | pass |
| `notify`, `core_apps`, `term_scrollback`, `term_select_copy`, `term_window_size`, `term_resize`, `term_font`, `term_menu`, `xui_context_menu`, `shutdown_shell`; `xui_desktop`, `xuid_pointer_restart`, `shutdown_menu` (image with `term,sysmon,fabricmon,counter`) | pass |
| `python tools/accounts/run.py` | `ACCOUNTS: PASS`: every scenario is `blocked` with the services' own policy text, as before |
| `python tools/shutdown/run.py` | `SHUTDOWN: PASS` |
| `python tools/rhai/run.py --desktop` | `RHAI:HARNESS:PASS` |
| `python tools/playground/run.py` | `PLAYGROUND:HARNESS:PASS` |

Findings along the way: (1) the account attack harness caught that denying
`keyd` and `accountsd` to the Terminal ended nine scenarios in the kernel's
refusal instead of the service's own policy text; a shell tool no longer has
those denies (10.1). (2) `term_redirect.json` fails on main too: it expects
`apps` as the first entry of `ls /`, and `/accounts` has sorted first since the
accounts database moved there. (3) `tools/shutdown/run.py` typed a command of
64 characters, which wraps in the Terminal's 68 columns, so its
`TERM:OUT:wrote-0` marker never fired (the Terminal source is unchanged; the
rewrite of the Terminal is newer than the harness); the command is shorter now.

Host: `cargo test -p devtool -p lazypkg -p pkgstore -p elevpolicy
-p messenger-generated -p build-support-tests`, `cargo test --manifest-path
xui-app/Cargo.toml -p lazyshell -p lazyterm`, the `xui-term` binary tests under
WSL Ubuntu (`cargo test --bin xui-term -p xui-app`), `tools/pkg/test_build.py`,
`tools/xui/test_core_packages.py`, `tools/test_run_demo.py`,
`tools/test_run_demo_apps.py`, `tools/accounts/test_judge.py`,
`tools/test_suites.py`, `tools/devtool/check_interfaces.py`,
`tools/check_file_length.py`, `tools/fhs/check_literals.py`,
`tools/trace/check_adhoc.py --ratchet`, and the `midlc --check` of every
generated file.

Not verified: the Terminal as the *only* shell tool against a pre-change
(main) image for the two harness findings above was argued from the unchanged
Terminal source and `git` history, not by building main; no kernel code
changed, so the kernel suite was not run.

### 10.9 Left

* The Playground's help says a refused credential call is to be made from the
  Terminal; that is true again (a shell tool is not denied those services).
  The "known gap" about a tool's toast is closed (10.2 row 13); the Playground
  plan's text still calls it open.
* `pkg.install-devtool` from the Terminal (row 5) is a small follow-up if wanted.
* Devices stays unlabelled: `os.kernel.dev` is not a permission.
