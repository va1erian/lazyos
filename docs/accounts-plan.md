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
  persistent account database owned by `_accounts` in `/conf/accounts`;
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

- **The account database** is `/conf/accounts/db` (`libs/accountdb`: parser,
  views, operations, who may, the brake; host-tested, seeded fuzz entry
  `accountdb::fuzz::run`, cargo-fuzz target `fuzz/fuzz_targets/accountdb.rs`).
  One file holds accounts, groups and verifiers; `accountsd` runs as
  `_accounts` (uid 908, no capability), owns `/conf/accounts` (0700; `/conf`
  became 0711 so it can be reached) and writes the database atomically
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
- **The brake:** three free failures per account name and per calling uid,
  then 1 s doubling to 60 s, refused at once with `EAGAIN`; `logind` and
  `elevd` are counted per name only (they slow their own askers).
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
  minutes without a prompt per row; a read changes nothing.
- **Services that trust `elevd`** (by kernel-stamped identity): `accountsd`
  (create, delete, promote, any password), `confd` (`sys/**`, any user's
  keys), `pkgd` (any source path; replacing a core app is now refused from a
  session: `core_replace` is blocked), `timed` (`SetTime`), `init`
  (`RestartService`). `keyd` takes `Provision`/`Forget` from `accountsd`
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
and this surface is the compositor's. Without `inputd` the prompt falls back
to the kernel's PS/2 key stream (US layout).

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
