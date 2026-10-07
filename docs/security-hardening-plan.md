# Security hardening plan: from model to reality

Status: **planned** (2026-10-01). Phase 0 is tracked by #446 (kernel) and
#447 (userspace and storage), which can be worked in parallel. Phase 3 and
the account parts of phases 0 and 1 have since landed through
[`accounts-plan.md`](accounts-plan.md) U0-U2 (PRs #645, #659, #660, #661):
graphical login, the session as the user with no capability, the account
database `/accounts/db`, and `elevd` instead of `uid == 0` checks. Section 1
is the survey as of 2026-10-01; [security-model.md](security-model.md)
section 0 has today's state.

Scope: make the [security model](security-model.md) true at runtime with the
smallest set of changes. Concretely: no program runs as root without a reason,
users are real accounts with a home directory, and Messenger calls are actually
authorized. This is a hobby OS; the plan deliberately skips the heavier parts
of the model (section 8).

Related: [security-model.md](security-model.md) (the target),
[architecture/ipc-security.md](architecture/ipc-security.md) (the kernel
mechanisms), [architecture/filesystem.md](architecture/filesystem.md),
[platform-plan.md](platform-plan.md).

## 1. Where it stands

The kernel mechanisms are real: kernel-stamped credentials, the audited
`CAP_SETUID` transition gate, VFS `rwx`/sticky checks, per-uid quotas and the
label ACL engine. What makes the result pretend is how they are wired:

1. **Almost everything is root, including the desktop.** `xuid` is started by
   the kernel with every capability. init's autostart launches desktop apps
   with a hardcoded uid 0 caller (`user/src/bin/init/autostart.rs`), so the
   Terminal's `sh` is uid 0 without any login. Every manifest service is uid 0
   with all capabilities except `CAP_INPUT_RAW`; only the drivers and their
   stacks (`sndd`, `audiod`, `usbd`, `netdrv`, `netd`) have their own uids.
2. **Services authorize with `uid == 0`.** keyd `Provision`, accountsd
   `Create`, confd `sys/`, messengerd `system/` publish and the xuid shell role
   all accept any uid 0 caller, so a capability-less uid 0 desktop app can plant
   a password in keyd and log in as anyone.
3. **The kernel ACL never closes.** No uid policy is ever loaded, so every call
   is `BOOTSTRAP_ALLOW` forever (`kernel/src/ipc/acl.rs`). *(Since the package
   system, #445 and #509, installed apps are the exception: init spawns each
   one labelled `app:<system_name>` and `pkgd` loads that label's rules from
   its manifest, so a packaged app is default-deny on Messenger. Every
   unlabelled task, the services, `xuid`, LazyShell and the Terminal among
   them, is still under bootstrap-allow.)* Any unlabelled process may
   register an unused `os.lazy.*` name (`kernel/src/ipc/policy.rs`; labelled
   apps may not), so a restarting keyd can be impersonated.
4. **Open doors in services.** keyd `Verify` and accountsd `Authenticate` are
   open to everyone without rate limiting; healthd `Report`, mimed `Register`,
   timed `SetZone`, netd `Renew`/`Reattach` change global state for any caller;
   logd `Tail`, logind `Sessions` and the init/healthd topic brokers leak or
   accept forged data; anyone can subscribe to `#`.
5. **No persistent home.** `/` is read-only FAT (every file `0555 root`);
   writable storage is the optional ext2 `/data` disk, which `cargo run` does
   not attach. Accounts named a home under `/home`, the directory was under
   `/data/home`, and logind ignored the field. Passwords were plaintext in
   the boot volume's `PASSWD` file. *(Since filesystem F4, #508: `/` is the
   ext2 OS volume, the accounts are `admin` (uid 0) and `user` (uid 1000) in
   `/system/etc/passwd`, the only account source (accountsd fails closed
   without it), and each home is the account's own `/home/<name>`, 0700, on
   the home volume or the OS volume. A login starts in it with `HOME`, `USER`
   and `PATH` set. The passwords are still plaintext.)* *(Since accounts U0
   and U1, #623 and #624: `admin` is uid 1001 in the `admin` group, nobody
   logs in as uid 0, and accounts, groups and Argon2id verifiers live in the
   account database `/accounts/db`, with `/system/etc/passwd` and `group` as
   generated views.)*
6. **Callers are identified by task slot.** Reading a sender's credentials
   needs `CAP_SETUID` (hence every service holds it), and slots are reused
   without a generation.

## 2. Principles

- **uid 0 is init**, plus a short-lived elevated child later (section 6.6).
  Everyone else is a named account, so the VFS, quota and signal root bypasses
  stop mattering.
- **Services check capabilities or a service account, never `uid == 0`.** Map
  admin actions onto the capabilities that already exist:

  | Action | Required |
  |---|---|
  | accountsd `Create`, confd `sys/` writes, netd `Renew`/`Reattach` | `CAP_SYS_ADMIN` |
  | logd `Tail`, other users' sessions in logind `Sessions` | `CAP_AUDIT_READ` |
  | timed `SetZone` | `CAP_SYS_TIME` or `elevd` (done, review of #659) |
  | keyd `Provision` | caller is `_accounts` |

- **Per-method rules live in MIDL**, next to the method they protect, and are
  enforced by generated code.

## 3. Phase 0: blockers

Split into two parallel issues over disjoint files.

**Kernel (#446)**

- The kernel snapshots the sender's `uid/gid/caps/session` into each message
  header at send time. Services authorize without `CAP_SETUID`, and slot reuse
  can no longer misattribute a message. Everything below depends on it.
- `spawnv` (`SpawnCred::As`) fails closed: a failed credential stamp
  (`kernel/src/process/spawn.rs`) kills the child instead of leaving it with
  the caller's credentials.
- `/tmp` is mounted `1777` (the ramfs root is `0755 root` today).
- Native spawn checks the execute bit (it only checks read today).

**Userspace and storage (#447)**

- `/data` is attached on every boot path (`cargo run`, `qemu_shot`,
  `qemu_session`), created with `tools/mkdisk` when missing (#332).
- Homes are `/home/<user>` everywhere: the image build makes one per passwd
  account whose home is `/home/<name>` (0700, the account's uid/gid), and
  `tools/mkdisk --home-volume` seeds the home volume the same way (done in
  filesystem F4, #508; the `/data/home` tree is no longer seeded).
- No plaintext passwords: Argon2id hashes at build time, no byte-compare
  fallback without keyd, no keyd demo account. (Landed: the verifiers live
  in the account database `/accounts/db`, `_accounts`, 0600, which `keyd`
  reads; accounts-plan U1.)
- One credential table in init (the two `manifest_cred` functions merge), and
  `xuid` moves from the kernel launch path into init's manifest.
- Kernel-started XAPP and console-shell profiles drop `CAP_INPUT_RAW`.
- The xuid `Subscribe` bug is fixed (a non-`shell` role replaces the shell
  subscriber without a privilege check).

## 4. Phase 1: capabilities instead of `uid == 0`

1. Replace every `uid == 0` check in services with the table in section 2,
   through small helpers on the stamped caller (`caller.has(CAP_X)`,
   `caller.is_service("_accounts")`).
2. midlc gains a per-method annotation, e.g.
   `@allow(any | session | cap SYS_TIME | service _accounts)`. The generated
   server dispatch checks it before the handler runs; a method without one is a
   compile error.
3. Close the open doors:

   | Service | Change |
   |---|---|
   | keyd `Verify`, accountsd `Authenticate` | only `_logind` (later `elevd`); per-user rate limit; same timing for unknown users |
   | healthd `Report` | only the service itself or init |
   | mimed `Register` | per user (stored in the caller's confd tree), not global |
   | logind `Sessions` | own sessions only, unless `CAP_AUDIT_READ` |
   | netd `Renew`/`Reattach` | `CAP_SYS_ADMIN` |
   | netdrv attach | `_netd` only |

## 5. Phase 2: service accounts

| Account | Caps | State dir |
|---|---|---|
| `_messenger` | `IPC_CONTROL` | none |
| `_keyd`, `_accounts`, `_confd`, `_logd`, `_clip`, `_mime`, `_health`, `_sysmon` | none | `/data/var/<svc>`, `0700` |
| `_logind` | `SETUID` (it can only grant what it holds) | none |
| `_time` | `SYS_TIME` | none |
| `_input` | `INPUT_RAW` | none |
| `_xui` | `SYS_ADMIN` (display grant) | none |

- init stays root, creates the state directories at boot and starts each
  service through `spawnv` (`SpawnCred::As`).
- **Names are bound to owners.** init hands the kernel a table
  (`os.lazy.keyd -> _keyd`, ...) and the registry refuses `os.lazy.*` to any
  other uid, which ends name squatting.
- Quotas now apply to services (they are no longer uid 0); tune the limits.
  Services also stop being able to signal each other, which is the point.

## 6. Phase 3: real login and a real desktop session

Landed as [`accounts-plan.md`](accounts-plan.md) U0 and U2 (#623, #625), with
two changes from the list below: autostart stays in `init`, which runs it in
the user's session as the user, and `elevd` never spawns an elevated child:
it performs a fixed table of operations itself after an administrator types
their name and password on `xuid`'s trusted prompt.

1. Boot brings up `xuid` (as `_xui`) and a login screen; a console prompt is
   fine at first.
2. logind authenticates and starts the session leader as the user's uid/gid,
   with a new session id and `HOME` and cwd at `/data/home/<user>`.
3. Per-session autostart moves from init to logind, which calls init `Launch`
   with that session.
4. The xuid shell role is granted to the leader of the active session instead
   of uid 0.
5. The Terminal's `sh` runs as the logged-in user.
6. **Minimal `sudo`:** a small `elevd` re-authenticates the user, checks an
   `admin` group listed in the accounts file, and spawns one audited child
   with the requested credentials. This needs no kernel supplementary groups.

## 7. Phase 4 and 5: close the kernel, then prove it

**Phase 4**

1. Once core services have registered, init loads a uid policy, which ends the
   bootstrap window. It stays small: who may register which names and publish
   which topic segments. Per-method rules stay in the services (phase 1).
2. The legacy unauthenticated topic brokers in init and healthd are removed;
   their topics move to messengerd.
3. Subscribing to `system/audit/#` and `system/security/#` needs
   `CAP_AUDIT_READ`.

**Phase 5.** A red-team session script logs in as `user` and tries, each of
which must be refused and audited: read `/home/admin`, register
`os.lazy.keyd`, call keyd `Provision`, subscribe to `#`, signal confd, call
timed `SetZone`, write `/data/var/confd`. It runs in CI, and the status table
in [security-model.md](security-model.md) section 0 is updated to match.

Kernel changes in every phase follow the AGENTS.md rule: correctness and stress
tests under `kernel/src/tests/`, and `python tools/test/run.py --accel none`
passes.

## 8. Deliberately skipped

A policy language and hot reload, mount namespaces, syscall allowlists,
setuid binaries, a real/effective/saved uid split, kernel supplementary groups
and signed bundles. Three items this list once skipped have since landed
through other work: app labels and manifests, wired for installed packages
(`pkgd` loads each app's label rules), the Installer's consent screen
([packages.md](packages.md)), and an ext2 root filesystem (filesystem F2, #506,
[filesystem-plan.md](filesystem-plan.md)).

Order: #446 and #447 in parallel, then phases 1, 2, 3, 4, 5, one PR each.
The stamped header (#446) and the login session (phase 3) are the large items;
the rest is mostly wiring.
