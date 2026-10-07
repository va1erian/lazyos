# User accounts and app permissions plan

Status: U0 done (#623, PR #645); U1 (#624) and U2 (#625) implemented
2026-10-07 (section 3.1); U3-U5 planned. Builds on [`security-hardening-plan.md`](security-hardening-plan.md)
(phases 0-3, #446, #447) and [`security-model.md`](security-model.md); where
the two overlap, this plan says *what the user gets*, the hardening plan says
*how the plumbing is closed*.

## 1. Goals

User accounts stop being a placeholder. Three invariants, each with a test:

1. **No bricking.** Nothing a user session does or launches, short of an
   admin approving it through `elevd`, can stop the next boot from reaching a
   working login screen.
2. **Isolation.** User A cannot read user B's files, settings, clipboard,
   windows or processes, or signal B's tasks.
3. **Informed control.** An app can never exceed the permissions shown before
   it was installed (the manifest is the ceiling), each user can see and change
   every grant of every app at any time, and the kernel enforces it, files and
   network included.

Outside those, users are lightly restricted: installing apps for yourself,
running anything from the Terminal and changing your own settings need no
password.

Decisions (2026-10-06):

- **Admin model:** admins are ordinary uids in an `admin` group. Privileged
  operations are performed by `elevd` after an admin password prompt drawn by
  `xuid`; nobody receives root.
- **Installs:** per-user by default; system-wide (`/apps`) installs and core
  app updates are admin-only through `elevd`.
- **Sessions:** one graphical session at a time; fast user switching later.

## 2. Where we start (survey, 2026-10-06)

- The desktop runs as **uid 0**: `init` autostarts LazyShell with a hardcoded
  root caller (`user/src/bin/init/autostart.rs`), `target_cred` keeps that uid
  for the Terminal's `sh` and every app (`user/src/bin/init/launch.rs`), and
  root skips mode bits (`kernel/src/fs/vfs/meta.rs`). `xuid` is spawned by the
  kernel with every capability; most services inherit init's root identity.
- Two accounts are fixed in `build_support/passwd`, with plaintext secrets in
  a 0644 `/system/etc/passwd`; logind prints them. `accounts.Create` is
  `ENOSYS`; no password change, groups or logout. Graphical login exists only
  behind an unshipped `sys/session/mode` key.
- Services authorize by `uid == 0` (keyd `Provision`, confd `sys/**`, xuid's
  shell role), so any root app can plant a verifier and log in as anyone.
- App labels confine **Messenger only**. A manifest's `files` compiles to no
  rule; Linux `AF_INET` ignores `network`; the Terminal, Installer, Devices and
  LazyShell are unlabelled, so everything typed in a shell is unconfined.
- Consent is all-or-nothing, at install, for every user, and not recorded; the
  only revocation is Remove (impossible for core apps). No first-use prompts.
  The file dialog is an in-process widget that grants nothing.
- Brick paths for a logged-in non-root user: install a package with
  `autostart` (init runs it **as uid 0 at every boot**), install a broken
  higher-versioned core app, exhaust the global 256-task table. Recovery is
  host-side only (rebuild the image).

## 3. Phases

| Phase | What the user gets | Issue |
|---|---|---|
| U0 | The desktop is not root: graphical login, logout, apps as the user | #623 |
| U1 | Real accounts: create, delete, passwords, admin group, setup wizard | #624 |
| U2 | `elevd`: admin-approved privileged operations on a trusted prompt | #625 |
| U3 | Brick-proofing: per-user installs, protected core apps, quotas, safe mode | later |
| U4 | Isolation beyond files: clipboard, windows, topics, process lists | later |
| U5 | Enforced, revocable, per-user app permissions and a trusted file picker | later |
| UT | Tooling: account monkey, attack harness, fuzzing | #626 |

Order: U0, then U1 and U2 together, then U3, U4, U5. UT starts with U0 and
grows with each phase: every phase lands with its attack scenarios.

### U0: stop running as root

Absorbs the open parts of #447 and phase 3 of the hardening plan.

- Graphical login is the default on desktop images (`sys/session/mode`
  shipped as `graphical`), with **logout** ending every task in the session.
  `LAZYOS_AUTOLOGIN=<name>` (a `run_demo.py --autologin` flag and a GUI
  control) logs straight in for development and the screenshot sessions,
  through the same session path as a typed password.
- LazyShell, the Terminal and every app run as the session's uid with caps 0.
  `autostart.rs` loses its root caller: autostart runs at login, in the
  user's session, as the user.
- `xuid` starts from init's single credential table under its own `_xui`
  account (with #447); services authorize the shell role by label or
  capability, never `uid == 0`. Same for keyd `Provision` and confd `sys/**`.
- Hashed passwords in a 0600 `/system/etc/shadow`, no plaintext fallback, no
  printed passwords outside test images (#447).
- Done when: Terminal `id` prints uid 1000, `rm /system/bin/init` is
  `EACCES`, every existing screenshot session passes under autologin, and the
  kernel suite passes.

### U1: real accounts

- accountsd implements `Create`, `Delete`, `SetPassword`, `SetAdmin` over a
  persistent account database owned by `_accounts` in `/accounts`;
  `/system/etc/passwd` becomes a generated view. Groups exist: `admin`.
- Creating an account creates its 0700 home (on the home volume when mounted)
  from a skeleton; deleting one archives or removes it on request.
- First boot of a fresh image runs a setup wizard: the owner account, as an
  admin. `LAZYOS_AUTOLOGIN` images skip it with the build's accounts.
- Settings gets an **Accounts** page: add/remove users, change your password,
  make admin (the last two privileged ones go through U2).
- `Authenticate` is rate-limited per name and per caller.

### U2: elevation (`elevd`)

- `elevd` runs a short list of privileged *operations* itself after an admin
  authenticates; it never hands out root or capabilities. First operations:
  system-wide install/remove and core app update (pkgd), `sys/**` settings
  (confd), set clock, create/delete/promote accounts, power policy.
- The prompt is drawn by `xuid` on a dimmed overlay no client can draw over,
  read or capture, and names the real caller from its kernel label and uid.
  Non-admins can ask an admin to type their password.
- Every request (granted, refused, cancelled) is logged to `/logs`.
- Protocol in `idl/elevd.midl`; services accept the privileged path only from
  `elevd` (by label), which replaces their `uid == 0` checks.

### 3.1 Where U1 and U2 landed (2026-10-07)

What exists, and the decisions taken on the way:

- **The account database** is `/accounts/db` (`libs/accountdb`: parser,
  views, operations, who may, the brake; host-tested, seeded fuzz entry
  `accountdb::fuzz::run`, cargo-fuzz target `fuzz/fuzz_targets/accountdb.rs`).
  One file holds accounts, groups and verifiers; `accountsd` runs as
  `_accounts` (uid 908, no capability), owns `/accounts` (0700, a
  top-level directory so `/conf` stays root's alone: see "Review fixes"
  below) and writes the database atomically
  (`db.new`, fsync, rename). `/system/etc/passwd` and `/system/etc/group`
  are generated views owned by `_accounts`. **Decision:** there is no
  `/system/etc/shadow` view any more: `keyd` reads the verifiers from the
  database itself, and a second copy of them had no reader. The image build
  seeds the database once (`build_support/accounts_seed.rs`, a seed an
  update never replaces) from `passwd`, `groups` and `passwords`.
- **uids:** accounts get the next never-used uid from 1000 (a `next:` record
  keeps a deleted account's uid from being handed out again); `admin` is now
  uid 1001 in the `admin` group (gid 10); nobody logs in as uid 0 (the
  parser refuses a uid 0 account). `elevd` is `_elev` (909).
- **Homes:** only root can give a directory to another uid, so `accountsd`
  asks `init` (`init.Home`, accepted from `_accounts` alone), which copies
  `/system/etc/skel` into a 0700 home with a BusyBox helper, or archives
  (`/home/.archived/<name>-<uid>`) or removes it, and answers when done.
- **First-boot setup:** an image built with `LAZYOS_SETUP=1`
  (`run_demo.py --setup`, the GUI's "First-boot setup") has no account; the
  login screen asks for the owner, the one `Create` `accountsd` takes from
  the `_greeter` identity while the database is empty, an administrator.
  **Decision:** default images keep the development accounts (`admin`,
  `user`), which every session script and CI job uses; an autologin image
  skips the setup.
- **The brake:** three free failures, then 1 s doubling to 60 s, refused
  at once with `EAGAIN`. An attempt is refused while its account name or
  (for an ordinary caller) its uid is locked, but a failure counts against
  the name only when `logind` or `elevd` asked (they mediate for a person
  at the keyboard and slow their own askers), and against the caller's uid
  only otherwise: a session flooding `Authenticate("admin", ...)` locks
  itself, never `admin` out of logging in or approving. A success clears
  the authenticated name alone, never a caller's lock, and a full table
  never evicts a locked slot (it refuses the newcomer instead);
  `accountdb::ratelimit::Attempt` (review of #659, H5).
- **`elevd`** (`idl/elevd.midl`, `libs/elevpolicy`, `user/src/bin/elevd`): the
  operation table, the prompt (`xuid`, `os.lazy.display.prompt.v1`, opened
  by `elevd` alone), admin check through `accountsd` (`Lookup.admin`,
  `Authenticate`), up to three tries per request, lockout per asker and
  per name, the audit topic `system/events/elevd/request`
  (`/logs/elevd.log`). **Decision (2026-10-07):** every change prompts:
  each `conf.set`/`conf.delete`, account, package, clock, power or service
  operation opens the prompt every time. Only the elevated Config editor's
  *view* stands: once `conf.elevate` is approved, the same uid, label and
  session may list and read every key (`conf.list`, `conf.get`) for five
  minutes without a prompt per row; a read changes nothing. The view ends
  early when the session ends (`elevd` follows `logind`'s session records,
  and a session id reused after a `logind` restart starts empty) or when
  Config closes (`Release`). **Decision (review of #659):** an unlabelled
  caller (label 0: everything started from a shell or a script) gets no
  standing view, since they all share that label; each read prompts. Binding
  the view to the requesting endpoint was rejected: clients share the one
  endpoint name resolution gives them, so it would not tell them apart.
- **Prompt floods (review of #659, H4):** a prompt takes every key and
  click, so it must cost the asker something. After a prompt was cancelled
  or timed out, `elevd` refuses that caller (uid, label, session) without a
  prompt (`EAGAIN`, audited `held`) for 5 s, doubling up to 2 minutes; an
  approval or ten quiet minutes reset it. Every caller waits a 3 s pause
  after any unanswered prompt, so two programs taking turns still leave the
  desktop (and Log out) reachable. `elevd` keeps reading while a prompt is
  up: a caller has one request in hand at a time and at most eight wait;
  the rest are refused at once (`EBUSY`, audited `busy`). Policy in
  `libs/elevpolicy` (`backoff`, `queue`, `sessions`, host-tested), service
  side in `user/src/bin/elevd/intake.rs`. **Decision:** no "deny this app"
  button on the prompt yet.
- **Services that trust `elevd`** (by kernel-stamped identity): `accountsd`
  (create, delete, promote, any password), `confd` (`sys/**`, any user's
  keys), `pkgd` (any source path; `InstallApproved`, the only install that
  replaces a core app for `elevd`, and only for `pkg.update-core`;
  replacing a core app is refused from a session: `core_replace` is
  blocked), `timed` (`SetTime`), `init` (`RestartService`, the services in
  `elevpolicy::RESTARTABLE` only). `keyd` takes `Provision`/`Forget` from `accountsd`
  alone.
- **Apps:** Settings has an Accounts page (list, add, remove, make admin,
  change your password) and writes `sys/**` and the clock through `elevd`;
  Config shows only `user/<uid>/**` until **Elevate**; the Installer replaces
  a core app through `pkg.update-core`.
- **Kernel:** a native `write_file` replaces a file its caller owns in a
  directory it cannot write (the views); native writes now drop the Linux
  ABI table's cached metadata of the same path (a Linux `stat` saw the old
  size); `/etc/group` lists the group view.

The prompt's keys come from `inputd`, like any window's: `xuid` gives the
focus to a surface of its own while the prompt is up and reads that
session (`user/src/bin/xuid/prompt_keys.rs`), so every keyboard (PS/2 or
USB) types into it under the active layout (`sys/input/layout`, e.g. `fr`).
No client can read those keys: only a surface's owner opens its session,
and this surface is the compositor's.

**The prompt fails closed (review of #659, H3).** It opens only once
`inputd` has confirmed, with a two-way `SetFocus` (retried for up to 1 s),
that its surface has the keyboard, so no client window does; otherwise
`xuid` refuses it (`EAGAIN`, `XUID:PROMPT:REFUSED`) and `elevd` refuses the
request (audited `nokeys`). The focus used to travel as a one-way note on
`inputd`'s shared endpoint, which any client can fill: a full queue only
postponed it, and a failed registration fell back to the kernel stream while
`inputd` still fed the previous window, so an app could have read the
password. Now the compositor makes every call after `Attach` on the channel
it handed `inputd`, which no client holds and which `inputd` serves first
(`user/src/bin/inputd/shellchan.rs`). Only when `inputd` is not running at
all does the prompt read the kernel's PS/2 key stream (US layout): then no
client gets keys from `inputd` either. The attack harness floods `inputd`
during a prompt (`input_flood`) and raises prompts back to back
(`prompt_flood`).

### 3.2 Review fixes (PR #659, services and apps)

- **The database left `/conf` (H1).** `/conf` had become 0711 so
  `_accounts` could reach `/conf/accounts`, which let any user read confd's
  0644 store by name. **Decision:** the database lives in its own top-level
  directory, `/accounts` (`fhs::state::ACCOUNTS_DIR`, 0700 `_accounts`), and
  `/conf` is 0700 root again. The next in-place update moves a database
  from `/conf/accounts` (`build_support/os_state.rs`) and makes every file
  in `/conf` 0600; confd creates its files 0600 and repairs its directory
  and files at every start. Gate: `read_conf_store` in the attack harness.
- **`keyd.Verify` is `accountsd`'s alone (H2)**, like `Provision`,
  `Forget` and the new `Restore`: a direct `Verify` went around the brake.
  Gate: `keyd_verify`.
- **No session can lock an account out (H5).** See "The brake" above
  (`ratelimit::Attempt`). **Decision:** `Authenticate` stays open (a user's
  own password change needs it); only the counting changed. Gate:
  `admin_lockout` (a session floods `Authenticate("admin")` while admin's
  right password approves an `elevd` request).
- **Homes belong to their accounts (H6).** Homes are seed directories of
  the image: made with the database, never re-created or re-chowned by an
  update. `init.Home` `create` keeps a home its account owns, hands over a
  root-owned one (`chown -hR`), and archives one another uid owns
  (`/home/.archived/<name>-<uid>`) before making it afresh. **Decision:**
  the migration of pre-U1 volumes (admin moved from uid 0 to 1001) runs at
  boot: `accountsd` asks `create` for every account at start, which covers
  the OS volume and the home volume alike (the build never sees the home
  volume, and `tools/mkdisk` only formats new ones). `run_demo.py --setup`
  (and the GUI's setup) formats the home volume with no home on it.
- **The setup screen never traps (H7):** after the owner is created the
  greeter switches to the login form; it waits for `accountsd` (asking
  every second) instead of guessing the login form.
- Also: the database parser requires an account's uid and primary gid in
  1000..=59999 and a name `accountdb::valid_account_name` accepts (no
  leading `_`, not `root`), the rule `Create`, `init.Home` and the greeter
  share; a `SetPassword` whose database write fails puts `keyd`'s old
  verifier back (`keyd.Restore`).

### 3.3 Review fixes (PR #659, what the prompt shows and the audit says)

- **A package prompt shows a package, and that package is what gets
  installed.** For `pkg.install` and `pkg.update-core`, `elevd` first has
  `pkgd` inspect the file (`Inspect`, the validation an install runs) and
  looks up the app it would replace (`Installed`, origin `core`); the prompt
  names the app, its system name and version, the core app replaced and
  from which version, the author marked *unverified* (packages are not
  signed), and the permissions grouped by risk: up to three high-risk ones
  by name, medium and low ones counted (`elevpolicy::package`,
  `user/src/bin/elevd/package.rs`). **Decision: a content hash, not a
  copy.** `pkgd` reported the archive's SHA-256 with the inspection; after
  the approval `elevd` calls the new `pkgd.InstallApproved(path, digest,
  core)` (accepted from `elevd` alone), and `pkgd` installs only bytes that
  hash to that digest: it reads the whole file into one buffer, hashes it
  and installs from that buffer, so the check and the use are the same
  bytes and a file swapped after the approval is refused ("the package
  changed after an administrator approved it"). A private copy would have
  needed storage `elevd` does not have and proved nothing more. `elevd`
  reads as a system service, so it first applies the asker's own source
  rule (`pkgstore::access::source_allowed`: `/transient`, `/system/share`
  or the asker's home): a request cannot make it inspect, and report on, a
  file the asker could not read.
- **Only `pkg.update-core` replaces a core app.** `InstallApproved` carries
  the intent: `core = false` (`pkg.install`) refuses a package whose system
  name is a core app's, `core = true` refuses one that is not; plain
  `Install` never replaces a core app for a session or for `elevd` (a
  `CAP_SETUID` service still may). `elevd` refuses both mismatches before
  any prompt (`EPERM` / `EINVAL`), and `pkgd` checks again
  (`user/src/bin/pkgd/approval.rs`). Gate: `core_claim`.
- **Text an asker chose is never misleading or cut out of sight**
  (`elevpolicy::text`, `summary`). **Decision, per field:** values that are
  stored as typed (a `conf.set` `str` value, a power policy value, a
  `conf.list` prefix, a package path) **refuse** control characters,
  Unicode format characters (bidi embeddings and overrides U+202A-202E,
  isolates U+2066-2069, marks U+200E/F, zero-width characters, U+FEFF, line
  and paragraph separators) and every space but U+0020 (`EINVAL`); text
  from elsewhere (a package's name, author, version, permissions) is shown
  **escaped** (`\u{202e}`, `\n`), as is anything the prompt's font cannot
  draw (it has ASCII and Latin-1), and `\` and `"`, so a quoted value ends
  where it seems to. Long paths lose their middle (`sys/ui/.../demo`), a
  long text value keeps its head and tail and names its length, a run of
  more than three spaces shows as `\[N spaces]`, and every summary is at
  most `MAX_SUMMARY` (300) characters. The prompt has room for six summary
  lines (the panel grew to 480x324), breaks a word wider than a line, and
  if text still does not fit it ends the last line with a visible `...`.
- **The audit trail cannot be forged.** The serial `ELEVD:REQUEST` line and
  `logd`'s `/logs/elevd.log` line are both rendered by
  `elevpolicy::audit::Line`: every field but the summary is one token
  (`[A-Za-z0-9._()-]`, anything else `_`), and the summary is the last
  field, quoted, with `\`, `"` and everything else escaped:
  `... outcome=granted summary="Set the setting sys/ui/demo to \"light\""`.
  Gate: `audit_forge` (a `conf.set` value carrying a line break and a whole
  forged `ELEVD:REQUEST ... outcome=granted` line is refused, and the judge
  fails the run if such a line ever starts a log line).
- **The `admin` field names an account or nothing.** On a refused prompt the
  audit keeps the name typed only when it is an existing account's, so a
  password typed into the name field never reaches `/logs/elevd.log`.
- **`service.restart` has an allowlist** (`elevpolicy::RESTARTABLE`):
  `inputd`, `audiod`, `sndd`, `netd`, `netdrv`, `usbd`, `devd`, `mountd`,
  `printd`, `clipboardd`, `mimed`, `healthd`, `sysmond`: drivers and
  services that hold no security state and come back as they were.
  **Never:** `elevd`, `logind`, `accountsd`, `keyd` (identity and
  approvals), `xuid` (the prompt), `init`, `messengerd`, `confd`, `logd`
  (the audit trail), `pkgd` and `timed`. `elevd` refuses any other name
  before a prompt (`EPERM`, `Operation::permitted`), and `init`'s
  `RestartService` checks the same list. Gates: `restart_elevd`,
  `restart_xuid`.
- Visual: `tools/screenshot/examples/pkg_elevate.json` (an image built with
  `LAZYOS_DESKTOP=1 LAZYOS_XUI_AUTOSTART=term LAZYOS_AUTOLOGIN=user
  LAZYOS_UI_PROBE=1 LAZYOS_RESET_OS=1`) installs a package through the
  prompt, shows the refusals before it, the core Counter's
  `pkg.update-core` prompt and a long setting elided in the middle.

### U3-U5 (outline, issues later)

- **U3 brick-proofing:** per-user installs labelled `app:<sn>@<uid>` that
  autostart only in that user's session; `/apps` and core apps only via
  `elevd`; per-uid task limit; ext2 reserved blocks; a safe-mode boot entry
  (no autostart, no user packages, default settings, admin repair console).
- **U4 isolation:** a real uid ACL policy instead of `BOOTSTRAP_ALLOW`;
  clipboard, session topics and window capture scoped to the session;
  process lists hide other users' command lines; `/tmp` 1777 (#446).
- **U5 permissions you control:** `files` compiled to VFS path rules and
  `network` enforced on Linux sockets; a trusted out-of-process file picker
  that hands the app a descriptor (choosing a file *is* the grant);
  `[permissions.optional]` with first-use prompts on the trusted overlay;
  per-user grants in `user/<uid>/apps/<sn>/grants`, a label loaded as manifest
  ∩ grants, reloaded live on change (labels unloaded at logout); updates that
  add permissions wait for re-consent; **Settings → Apps → Permissions** with
  toggles, last-used times and Reset; read/write method levels in MIDL. The
  Terminal stays unlabelled and runs as the user: what you type has your
  authority, as on Unix.

## 4. Verification

Every phase follows `AGENTS.md`: kernel correctness and soak suites for new
kernel surface, host tests for libraries, harnesses whose judges have
`test_judge.py` self-tests.

- **`tools/accounts/run.py`** (UT): boots as `user`, runs an attack list
  (delete `/system`, write `sys/**`, install an autostart or core-replacing
  package, signal services, read `/home/admin`, spawn tasks without limit,
  fill the disk, impersonate the shell role, flood `Authenticate`), reboots,
  and judges that the login screen returns and each attack failed with the
  expected error.
- **Account monkey** (UT): `tools/screenshot/monkey.py` driving login,
  logout, account creation and the elevation prompt with seeded random input
  as a non-admin, judged by the same invariants plus a host-side ext2 audit
  that no file outside the user's home changed owner or content.
- **Fuzzing** (UT): seeded `fuzz::run(&[u8])` entry points (shared with
  `fuzz/` cargo-fuzz targets) for the account database parser, the passwd and
  shadow parsers, accountsd and elevd requests, and the pkgd manifest/grant
  compiler.
