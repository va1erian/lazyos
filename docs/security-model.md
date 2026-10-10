# LazyOS security model

LazyOS aims to be **friendly, transparent, and secure**. This document defines
how identity, permissions, sandboxing, secrets, and audit work. It complements
the [platform plan](platform-plan.md) and the [Messenger spec](messenger.md).

The one-sentence model: **no ambient authority; every privileged action is a
capability or a policy-checked Messenger call, performed by an authenticated
identity, and explained if denied.**

## 0. Implementation status

This document is the target model. As of 2026-10-07 the kernel and services
have the mechanisms in the left column; everything in the right column, and
everything below not listed here, is specification
([`architecture/ipc-security.md`](architecture/ipc-security.md) has the detail;
[`security-hardening-plan.md`](security-hardening-plan.md) is the plan to close
the gap). A mechanism that exists is not necessarily switched on; the list
after the table says what is actually enforced today.

| Mechanism implemented | Specified only |
|---|---|
| Kernel-stamped credentials (`uid/gid/caps/label/session`) on every task and message; the kernel gives the programs it starts root credentials and their descendants inherit them (which is why `init`-started services run as uid 0 unless they drop privilege); audited `CAP_SETUID` transitions that can never widen privilege (section 2); service accounts with no capability for the drivers, `accountsd` (`_accounts`) and `elevd` (`_elev`) | Service accounts for the other core services (section 4.1 "system services do not run as root" is the goal, not the state) |
| Console login and the desktop's graphical login through `logind` + `accountsd` (a login screen run as the `_greeter` uid, or the build's `LAZYOS_AUTOLOGIN`), logout ending every task of the session; the account database `/accounts/db` (accounts, the `admin` group, Argon2id verifiers; `_accounts`, 0600) with `/system/etc/passwd` and `/system/etc/group` generated views; account management (create, delete, passwords, admins) and the first-boot setup; `Authenticate` slowed per name and per caller; no plaintext anywhere (section 3, issues #447, #623, #624) | Key/2FA, per-user sealing of secrets, TLS in `keyd` |
| VFS `rwx`/`umask`/sticky checks against kernel credentials, root bypass (4.1) | POSIX ACLs, mount namespaces / filesystem jails (5.3) |
| Capability bits `CAP_NET_*`, `CAP_SYS_ADMIN`, `CAP_SYS_TIME`, `CAP_AUDIT_READ`, `CAP_IPC_CONTROL`, `CAP_SETUID`, `CAP_KILL` (cross-uid signals), `CAP_DEV_CLAIM` (device claims through syscall 23, 4.2); `CAP_SYS_ADMIN` gates the display grant (4.2); per-driver device-class rules installed at boot (#481) | Dropping capabilities on `execve` |
| Handles with rights as the primary Messenger right; default-deny ordered ACL at the kernel call boundary; per-segment topic policy; app labels with label-keyed rules compiled from a package manifest and loaded by `pkgd`, reserved `os.lazy.*`/`app.<id>.*` namespaces (4.3, 6) | A uid policy loader, the policy language/compiler, hot reload, revocation of live handles (5.2, 6) |
| Per-uid quotas on kernel memory, user memory, handles, queue bytes/depth, device claims and DMA memory (5.5); friendly `ERR_QUOTA` | Syscall allowlists (5.1), network policy beyond the label rules (5.4), fd/CPU quota enforcement |
| Validated user pointers on every native and Linux syscall, NX on user pages, length-checked parcels fuzzed in CI, every `unsafe` documented and gated by clippy (7) | W^X enforcement, SMEP/SMAP, stack canaries, signed kernel, crash dumps, watchdog |
| 128-entry hash-chained kernel audit ring, denials always recorded (9) | `auditd`, on-disk audit log, `CAP_AUDIT_READ` query interface, "why was this denied" UI |
| Install consent in the Installer: requested permissions grouped by risk with `pkgd`'s explanations (6, 12); the elevation service `elevd` and the trusted prompt in `xuid` (10, issue #625); the account attack harness (`tools/accounts/run.py`, CI's `accounts` shard), whose U0-U2 scenarios must all be refused (13) | Signed bundles and updates (11), first-use prompts, the rest of the red-team CI suite (jail escape, clipboard exfiltration, sockets; 13) |

What is enforced today, honestly:

- **The uid ACL never closes.** No uid policy is ever loaded, so every
  Messenger call from an unlabelled task is `BOOTSTRAP_ALLOW`
  (`kernel/src/ipc/acl.rs`), apart from the reserved name namespaces. Unlabelled covers every system service, `xuid`,
  LazyShell and the Terminal.
- **Installed apps are confined.** A packaged app runs labelled
  `app:<system_name>` and is default-deny on Messenger except for what its
  manifest grants (`pkgstore::rules`, [`packages.md`](packages.md)). Its
  `files` permissions are consent-only (there is no file sandbox), and its
  `network` permission gates only the Messenger socket interface: the Linux
  `AF_INET` path in the kernel does not check labels, and the per-uid call
  rules in `libs/netpolicy` wait for the uid policy loader.
- **The desktop session is not root** (U0, issue #623). A desktop image
  boots to a login screen (or the build's autologin account); LazyShell, the
  Terminal and every app, installed autostart apps included, run as the
  logged-in user with no capability, so the VFS mode bits apply to them
  (`rm /system/bin/init` is `EACCES`) and they cannot signal services. The
  services still run as uid 0 except the drivers and their stacks (`sndd`,
  `audiod`, `usbd`, `netdrv`, `netd`), `accountsd` and `elevd`, and `xuid`
  is still started by the kernel. Nobody logs in as root: `admin` is uid
  1001, an ordinary account in the `admin` group (U1, issue #624).
- **Privileged changes go through `elevd`** (U2, issue #625). A session asks
  `elevd` for one operation of a fixed table (system installs and core app
  updates, `sys/**` settings, the clock and the time zone, accounts, the
  power policy, a service restart); `xuid` shows the trusted prompt over a dimmed screen,
  above every client, naming the asker from its kernel label and uid; an
  administrator types their name and password (a non-admin can hand the
  machine to one); `elevd` then performs the operation itself. Nobody is
  handed root or a capability. `accountsd`, `confd`, `pkgd`, `timed` and
  `init` accept these privileged paths from `elevd`'s kernel-stamped
  identity (its uid, unlabelled, outside any session), never from a session;
  `timed` also takes `SetTime` and `SetZone` from `CAP_SYS_TIME`. Their
  refusals name the policy (`EPERM` with its text), so the attack harness
  can tell them from any other `EPERM`.
  Every request is audited (`system/events/elevd/request`, journalled to
  `/logs/elevd.log`), and wrong passwords lock the asker out for a growing
  delay.
- **Privileged calls need a capability, not a uid.** `pkgd`'s
  unrestricted install source, `mimed` `Unregister`, `xuid`'s privileged
  subscriptions and shell-only calls, and `init`'s launch-anywhere, `Stop`
  and `Shutdown` rules ask for `CAP_SETUID`, which `init` keeps for the
  services and never stamps on a login session, whatever its uid. `xuid` gives the shell role to an unlabelled
  task of the session that owns the display, never displacing a live shell.
- **No plaintext passwords.** `/system/etc/passwd` carries `x`; the
  verifiers are Argon2id hashes in the account database
  (`/accounts/db`, `_accounts`, 0600 in a 0700 directory) that the
  image build seeds and `keyd` derives for new passwords; only `keyd`
  (which loads them) and `accountsd` (which stores what `keyd` returns) read
  them. Without `keyd` (or the database) every login fails closed.

---

## 1. Goals and threat model

**In scope**

- Malicious or buggy *apps* run by a user (the common case).
- A compromised *service* (daemon) must not gain more than its profile allows.
- Cross-user isolation on a multiuser machine.
- Network-facing daemons (later).
- Physical/local attackers (console).

**Out of scope (initially)**

- Nation-state hardware attacks; DMA attacks (mitigated later by IOMMU).
- Side channels (Spectre-class); we still apply standard mitigations.
- Supply chain of upstream toolchains (we sign our own artifacts).

**Adversary capabilities we assume:** the attacker can run arbitrary code as an
unprivileged user, craft arbitrary syscalls and Messenger parcels, exhaust
memory/fds/handles within their quota, and race multi-threaded code.

---

## 2. Identity

| Principal | Representation |
|---|---|
| User | `uid` (u32), unique; `0` reserved for the system |
| Group | `gid`; a user has a primary group + supplementary groups |
| Service account | a user with no login, owning service state |
| Session | `session_id`, created by `logind`; scopes topic/service namespaces and default ACLs |
| Process label | short string/profile id used in policy: `app:<id>` for an installed app (e.g. `app:com.example.editor`), `system:<name>` for a platform service, `dev:<id>` for an app run from an IDE under the permissions the user approved for it (issue #529) |
| Kernel | the only component that stamps credentials; userspace can never set them |

Credentials travel with every Messenger message and syscall and are copied into
audit records. The kernel keeps `uid/gid/caps/label/session` in the task struct;
`setuid`-like transitions are only allowed toward *less* privilege or through the
elevation service. A label is assigned once, when a child is created, by an
unlabelled `CAP_SETUID` holder (`init`); the one exception is an IDE package
with `develop = true` spawning a child into a `dev:` label that `pkgd` holds an
approved rule set for: same uid, gid and session, never more capabilities
(`docs/architecture/ipc-security.md`, "Spawning into `dev:`").

---

## 3. Authentication and login

- **Accounts** are managed by the accounts service (users, groups, home dirs,
  password verifiers). A home is `/home/<name>`, 0700 and owned by its
  account: a directory of the optional home volume, or of the OS volume
  without one (the image build makes one per passwd account, filesystem F4).
  Password hashes are **Argon2id**, and only `keyd` can
  verify them. `keyd` derives a verifier and returns it to `accountsd`, which
  stores it in the account database (`/accounts/db`, 0600 `_accounts`); `keyd`
  loads them at boot and checks passwords in its own memory, and no other
  service or client ever receives one.
- **The account database** (issue #624, docs/accounts-plan.md U1) is
  `/accounts/db` (`libs/accountdb`): every account, the groups (`admin`
  makes administrators) and the Argon2id verifiers, owned by the `_accounts`
  service account (uid 908) that `accountsd` runs as, 0600 in a 0700
  directory. `/system/etc/passwd` (`name:uid:gid:x:home:shell`) and
  `/system/etc/group` are views of it, regenerated by `accountsd`; the Linux
  `/etc/passwd` and `/etc/group` come from them. The image build seeds the
  database once from `build_support/passwd`, `groups` and `passwords` (an
  update never replaces it, so accounts made at runtime survive). It is
  parsed strictly and **fails closed**: a missing, unreadable, oversize or
  malformed file, a duplicate name, uid or gid, an undeclared group or a uid
  0 account gives `ACCOUNTS:LOAD:FAIL reason=<...>`, health `failed`, and an
  error for every request; `logind` then refuses every login ("Login
  unavailable: the account database did not load", denial reason
  `no-accounts`). That is a recovery situation, never a machine with a default
  password. A good load prints `ACCOUNTS:LOAD:PASS rows=<n>`.
- **Account management** (`idl/accounts.midl`): `Create`, `Delete`,
  `SetAdmin` and setting another user's password are accepted from `elevd`
  alone (an administrator approved them on the trusted prompt), never from
  a uid or a capability; a user changes their own password with the old one.
  `Create` gives the next never-used uid (from 1000) and a 0700 home copied
  from `/system/etc/skel`, which `init` makes as root on `accountsd`'s request
  alone (`init.Home`); `Delete` archives or removes it. The last administrator
  can be neither deleted nor demoted. A machine with no account (an image
  built with `LAZYOS_SETUP=1`, which never logs anyone in) runs the
  first-boot setup: the login screen asks for the owner, the one `Create` it
  may make, an administrator. Every new password follows one rule
  (`accountdb::secret`: 4 to 64 characters, no control character), which
  `accountsd` enforces and the login screen and Settings check ahead of
  time.
- **Guessing is slowed.** `Authenticate` (and `SetPassword`'s old password)
  may fail three times in a row per key; each further failure locks that
  key for a delay doubling from 1 s to 60 s, during which attempts are
  refused at once (`EAGAIN`) without reaching `keyd` (`accountdb::ratelimit`).
  `logind` and `elevd`, which check passwords for a person at the keyboard,
  count failures against the account name and slow their own askers; any
  other caller counts against its own uid only, so it cannot lock another
  account out. A success clears only the name that authenticated, and a
  locked key is never evicted from the bounded table.
- **Default accounts.** The image ships two:

  | Name | uid:gid | Home | Password |
  |---|---|---|---|
  | `admin` | `1001:1001`, group `admin` | `/home/admin` | `nimda` |
  | `user` | `1000:1000` | `/home/user` | `lazy` |

  These are development passwords (`build_support/passwords`). The image
  build hashes them with Argon2id (keyd's own cost, a salt derived from name
  and password) into the account database and writes `x` in the passwd view:
  no plaintext reaches the volume, and the login prompt and screen never
  print them (issue #447). `keyd` loads the verifiers at boot, all or nothing
  (`KEYD:SHADOW:PASS rows=<n>` / `KEYD:SHADOW:FAIL`); `accountsd` only relays
  `Authenticate` to `keyd`'s `Verify` and refuses every login when `keyd`
  cannot answer. `keyd`'s account methods, `Verify` (a direct check would
  get around the brake), `Provision` (derive a new verifier, which it
  returns for the database) and `Forget`, are accepted from `accountsd`'s
  identity alone.
- **Session environment.** A console login starts the passwd shell in the
  account's home with `HOME`, `USER` and `PATH=/system/bin`. Every app `init`
  launches into a session, installed (labelled) apps included, gets the same
  three variables; the account comes from the login event `logind` publishes
  (`system/events/login/session/<id>` carries the home), so a launch never looks
  it up again.
- **Login** (`logind`) runs a small PAM-like pipeline: identify → authenticate
  (console password, later key/2FA) → create session → grant the session its
  default capabilities (own compositor, own clipboard, session topics, home dir).
- **Desktop sessions** (issue #623, `user/src/bin/logind/graphical.rs`): on a
  desktop image (`sys/session/mode`, default `graphical` there) `logind`
  shows the login screen (`greeter`, run by `init` as the `_greeter` system
  uid 907, the only identity `logind`'s `Login` accepts) or logs the build's
  `LAZYOS_AUTOLOGIN` account in through the same path. `init` launches
  LazyShell into the new session and then the session's autostart apps,
  all stamped with the user's uid and gid and no capability. `Logout` (the
  LazyOS menu's "Log out..." row) publishes `system/events/login/end`; `init` then
  kills every task stamped with the session id and the login screen returns.
- **Service accounts** never log in; they receive their profile at supervision
  time.
- Failed logins are rate-limited (above) and audited (source, user, attempt).

---

## 4. Authorization

Three layers, all checked, all auditable:

### 4.1 Filesystem (UNIX-style)

- `rwx` bits for owner/group/other, `umask`, sticky-bit on shared dirs, plus
  optional POSIX ACLs later.
- Enforcement in the **VFS core** against kernel credentials; no path bypass.
- Root (`uid 0`) bypass exists only inside the *kernel-init* profile; system
  services do not run as root.

### 4.2 Capabilities

Fine-grained kernel privileges, granted per profile, never ambient:

| Capability | Allows |
|---|---|
| `CAP_NET_BIND` | bind ports < 1024 |
| `CAP_NET_RAW` | raw sockets |
| `CAP_SYS_ADMIN` | mounts, driver grants |
| `CAP_SYS_TIME` | set the clock |
| `CAP_AUDIT_READ` | read the audit stream |
| `CAP_IPC_CONTROL` | manage other services' endpoints |
| `CAP_DEV_CLAIM` | attempt to claim a discovered device at all |

`CAP_DEV_CLAIM` is deliberately **coarse**: it is the single gate that lets an
actor call the device core's `claim` in the first place. It is *not* a
per-device or per-class grant — authority over a specific device is the
`Device` handle returned by `claim`, whose rights (`MMIO`, `PIO`, `IRQ`, `DMA`,
`CONFIG`) are fixed by that device's actual resources and by a class-specific
ACL rule (`os.kernel.dev.<class>`), and can only be narrowed afterwards.
`claim` fails with `EPERM` before any owner is recorded when either the
capability or the class rule is absent, so an app without `CAP_DEV_CLAIM`
cannot even attempt to probe the device table. Drivers run as their own system
uids (`_net`, `_snd`) holding `CAP_DEV_CLAIM` plus their one class rule, never
uid 0. The kernel enforces it in the `dev_*` syscall (syscall 23, driver-plan
stage D3, issue #240): claims are audited, port I/O below 0x100 and the PCI
config ports are never reachable, config writes are limited to a masked command
register (bus mastering needs the `DMA` right), and a dying driver's claims are
released, its device quiesced and its MMIO unmapped by `ipc::teardown_task`.

Capabilities are in the task struct, shown in `ps`, and dropped on `execve`
unless the profile is privileged.

### 4.3 Messenger (capability + policy)

- Holding a **handle** is the primary right: you can call an object only if you
  hold a handle for it with `CALL`.
- The **kernel ACL hook** additionally evaluates policy per call:
  `(sender label/uid/session, target owner, interface.method, rights)`.
- Names and topics are policy-controlled too: an app can `Resolve("os.lazy.fs")`
  only if its profile permits it, and can publish/subscribe only to granted topic
  segments (wildcards included).
- **Namespaces**: `os.lazy.*` service names are the platform's. An app labelled
  `app:<id>` registers only `app.<id>.<name>` (one dot-free segment) and owns the
  topics at or under `app/<id>/`; a `dev:<id>` label (an app run from the IDE
  before it is installed) owns the same names and topics. Anything else is
  refused and audited with a reason (`RESERVED_NAMESPACE`, `OUTSIDE_NAMESPACE`).
  The interfaces a service advertises are held to the app's own domain too,
  `<id>.<name>.v<N>`: the registration spells each interface id out by name and
  the kernel refuses a name outside the domain (`FOREIGN_INTERFACE`) or one
  that is missing or does not hash to its id (`UNNAMED_INTERFACE`), so an app
  cannot pose as an implementation of a platform or another app's interface.
  Details: `docs/architecture/ipc-security.md`.
- **Reserved service names beyond `os.lazy.*`**: a name in the NIC namespace
  (`os.lazy.net.nic`, `os.lazy.net.nic/<ifname>`) may be registered only by a
  NIC driver identity (`_net`, `_wifi`, `_wifisim`; unlabelled, no session),
  whatever capabilities the caller holds, because `netd` hands the holder its
  frame rings and believes the card it describes (`libs/netpolicy`,
  `docs/architecture/networking.md` "Who may be a card"). `List` reports every
  owner's kernel-stamped uid, label and session so a client that trusts what a
  name's owner says can check it without `CAP_SETUID`. Attack row:
  `tools/accounts` `nic_register`.
- **Label assignment**: `init` stamps a label when it spawns a task: `pkgd`
  asks for `app:<system_name>` after it has loaded the package's rules. Before a
  package is installed (development mode) an IDE with `develop = true` spawns
  the project under `dev:<system_name>`, whose rules `pkgd` loads only after
  the user approves them (`docs/packages.md`, "Development runs").
- **Default deny**: a new app can do nothing until its manifest permissions are
  approved.

---

## 5. Sandbox model

Every process runs under a compiled **profile** combining:

1. **Syscall allowlist.** The native syscall gate checks a per-process bitmap.
   Linux-ABI processes get the same treatment for their syscall table. Denials
   return the friendly error (never a bare `EPERM`) and are audited.
2. **Messenger policy.** Interface/method allowlists, topic filters, handle-right
   restrictions, and permitted peers. Enforced at the kernel call boundary.
3. **Filesystem view (jail).** A mount namespace exposes only the app's home,
   declared data scopes, and read-only system paths. Path traversal cannot escape
   because the VFS resolves against the namespace.
4. **Network policy.** Local/LAN/internet egress rules, bind permissions, DNS
   restrictions; enforced by `netd` + kernel socket checks.
5. **Resource quotas.** Memory, CPU share, fds, handles, IPC queue depth,
   outstanding transactions, buffer allocations. Exceeding a quota is a friendly
   `ERR_QUOTA` with the current usage and limit.
6. **Device grants.** Framebuffer, input devices, audio later — only via handles
   issued by the session/device services.

The kernel side of item 5 is `kernel::quota` (issue #103): a per-uid table of
limits and live usage for kernel memory (shared-buffer frames), user memory
(`mmap`/`brk` growth), handles, fds, Messenger queue bytes/depth, CPU ticks,
device claims and DMA memory (contiguous pool bytes held through `dma_alloc`,
issue #241).
Charges and releases happen at the choke points (`handles::open`, channel
enqueue/dequeue, `shared::create`, `mmap`/`brk`, `dev::dma_alloc`), keyed by the task's stamped
uid, so two processes of one user share one limit. A refusal is a friendly
`ERR_QUOTA` carrying the resource name, current usage and limit. Defaults live
in `quota::DEFAULT_LIMITS` (uid 0 stays uncapped until login stamps a real
uid); policy sets limits with `quota::set_limit`, and native syscall 11 reads
the caller's usage/limits read-only.

Profiles are **compiled by `messengerd`/`init` from manifests + admin policy** and
hot-loaded into the kernel. Human-readable source of truth is checked into the
app bundle; the compiled form is hashed and audited.

---

## 6. Apps, manifests, and consent

An app bundle declares:

```toml
[app]
id = "com.example.editor"
version = "1.2.0"
publisher = "Example Inc"
[permissions]
files = ["read:$HOME/docs", "write:$HOME/.config/editor"]
interfaces = ["os.lazy.fs.reader.v1:call", "os.lazy.clipboard.v1:read"]
topics = ["publish:session/*/editor", "subscribe:system/events/*"]
network = []
capabilities = []
```

- **Install consent** shows a human-readable diff of the requested permissions
  with plain-language explanations, grouped by risk, plus the publisher
  signature.
- **First-use prompts** for sensitive resources (camera, network, clipboard
  paste) can be enabled by policy; prompts are themselves Messenger calls to the
  consent service and always logged.
- **Revocation** is immediate: policy is hot-reloaded and existing handles that
  violate the new policy are marked for revocation.

---

## 7. Kernel hardening

- Rust `no_std` with no `unsafe` outside audited modules; all `unsafe` blocks
  documented with invariants.
- **W^X** for user pages; NX by default; SMEP/SMAP; stack canaries.
- Strict syscall validation: every pointer/length checked against the caller's
  VMAs; no kernel dereference of user addresses without a validated copy; all
  user copies go through checked helpers (the COW/MM rework supports this).
- Parcel parsing is length-checked and fuzzed; no recursion without a depth cap.
- Handle and buffer allocations are metered; no unbounded kernel allocation from
  userspace.
- Panic policy: kernel panics are logged with a backtrace and, by default, halt;
  a watchdog can reboot into recovery. Crash dumps are opt-in and symbolised.
- **Kernel integrity:** signed kernel image, measured boot chain (UEFI secure
  boot later), and a read-only kernel text section on x86_64 huge pages.
- **Least privilege in the kernel:** policy decisions use compact tables; no
  user-controlled code (no eBPF-like JIT) initially — filters are interpreted
  with hard limits.

---

## 8. Secrets and cryptography

- **`keyd`** owns all long-term secrets: password hashes, TLS private keys, disk
  encryption keys, signing keys.
- Clients never receive raw keys. They ask for operations: `Sign(key, digest)`,
  `Decrypt(handle, ciphertext)`, `Wrap/Unwrap`, `TLS server session`. Key material
  lives in `keyd`'s own memory: no `keyd` method carries a `Buffer`, so no
  client ever maps a page that holds a key.
- **Unlock model:** secrets are sealed per user with a key derived at login
  (Argon2id from the password + machine secret); a locked account's keys are
  unavailable even to the kernel.
- **RNG:** a kernel entropy source (RDRAND/RDSEED + timing jitter, pooled) feeds
  a CSPRNG; userspace gets bytes only via `keyd`/`getrandom`.
- **Crypto policy:** algorithm allowlists, key sizes, no home-grown primitives
  (use vetted crates), constant-time where relevant.

---

## 9. Audit and forensics

- A kernel **audit ring** records: logins/logouts, elevation, service
  start/stop/crash, policy load/change, **all denials**, sensitive grants
  (clipboard, filesystem writes outside home, network listens), and optionally
  traces for tagged transactions.
- `auditd` drains the ring into a hash-chained log on disk (tamper-evident),
  enforces retention, and exposes a read-only query interface
  (`CAP_AUDIT_READ`).
- Every record has: timestamp (monotonic + realtime), actor (pid/uid/label/
  session), action, target, decision, and a correlation id (`txn_id` when
  applicable).
- The **Task Manager / `messengerctl`** can show "why was this denied" by
  following the correlation id.

---

## 10. Privilege elevation

Implemented by `elevd` (docs/accounts-plan.md U2, issue #625;
`idl/elevd.midl`, `libs/elevpolicy`):

- Elevation is a service call, not a setuid bit, and **nothing is granted**:
  no root, no capability, not even to a child. A program asks `elevd` for one
  **operation** of a fixed table (`pkg.install`, `pkg.update-core`,
  `pkg.remove`, `conf.*`, `time.set`, `account.*`, `power.policy`,
  `net.config`, `service.restart`) with checked arguments; `elevd` (its own `_elev` uid,
  no capability) performs it itself once an administrator approved, and the
  services accept that path from its kernel-stamped identity alone.
- Elevation always prompts: `xuid` draws the prompt over a dimmed screen,
  above every client and the shell; while it is up no client gets a key or a
  pointer event, no keyboard grab holds, and no window rises above it; the
  display protocol has no way to read the screen. It names the asker from its
  kernel label and uid, Cancel is the default, Escape cancels and it gives up
  after 90 s. Only `elevd` may open it. Every change asks each time; only the
  elevated Config editor's view (`conf.elevate`, then reads) stands, for five
  minutes, for the same uid, label and session, never for an unlabelled
  caller, and it ends with the session or when Config closes.
- The prompt fails closed: it opens only once `inputd` confirmed, on a
  channel no client shares, that no client window has the keyboard;
  otherwise the request is refused. Only without any `inputd` does it read
  the kernel's key stream.
- A prompt is not free to raise: after one was cancelled or timed out, the
  asker is refused without a prompt for a growing hold (5 s doubling to
  2 minutes), every asker waits a short pause, and a caller has one request
  in hand at a time, so no program can keep the person at the screen from
  the desktop and Log out.
- The prompt shows what is approved, all of it (review of #659): a
  package request shows the package `pkgd` inspected (name, system name,
  version, the core app replaced and from which version, the author marked
  unverified, the permissions by risk), and the install that follows takes
  only the bytes with the inspected SHA-256 (`pkgd.InstallApproved`); only
  `pkg.update-core` may replace a core app. Values stored as typed refuse
  control, bidi and other format characters; text from a package is shown
  escaped; long paths and values lose their middle, never their end, and a
  summary that still does not fit ends in a visible `...`.
- Some operations are refused before any prompt: `service.restart` of
  anything outside a short allowlist of stateless services and drivers
  (never `elevd`, `xuid`, `logind`, `accountsd`, `keyd`, `confd`, `logd`,
  `init`; `init` checks it too), a package with problems, and a package
  request whose kind does not match the package.
- Every request is audited (granted, refused, cancelled, timed out, locked,
  failed, held, busy, nokeys) on `system/events/elevd/request`, which `logd`
  journals to `/logs/elevd.log`, in lines no value can forge (single-token
  fields, the summary last, quoted and escaped); the `admin` field names an
  account or nothing, so a password typed as a name is never logged; wrong
  passwords lock the asker out for a growing delay.
- `CAP_SYS_ADMIN` is never granted to ordinary sessions; system administration
  happens through scoped operations rather than a superuser shell, with every
  call logged.

---

## 11. Updates and trust

- Apps and services are distributed as **signed bundles**; the installer verifies
  the publisher signature against system trust roots and records the hash.
- The manifest's permission diff is shown at install *and* update time
  (permission creep is visible).
- `init`/`messengerd` verify service signatures when required by policy (system
  services must be signed).
- Rollback: previous versions are retained; a failed update can be reverted with
  `lazyosctl rollback`.
- Revocation lists and trust-root updates are themselves signed and audited.

---

## 12. Friendly security (UX)

Security must not be a maze:

- **Explain every denial.** `ERR_DENIED` carries: what was attempted, which app,
  which permission was missing, how to grant it, and a docs link.
- **Diff-based consent.** "This app wants to read /home/user/docs (new)" rather
  than a wall of scopes.
- **Least-surprise defaults.** New apps get no network and no filesystem access
  beyond their own data dir until approved.
- **Visible state.** Settings > Privacy shows active grants; Task Manager shows
  per-process capability and IPC activity; tray shows a shield when a sensitive
  grant is active (camera/mic later).
- **Reversible.** Revoke a grant and it takes effect immediately, with a clear
  list of what will break.

---

## 13. Verification plan

| Check | Where |
|---|---|
| Policy unit tests (allow/deny matrices) | CI |
| Syscall-gate deny tests per profile | CI |
| Messenger parcel/handle-transfer fuzzing | CI (nightly) |
| Red-team samples (read other user, escape jail, call ungranted interface, exfiltrate via clipboard, open socket) — all must be denied + audited | CI |
| Audit log integrity (hash chain verifies) | CI |
| Crash-dump symbolisation and panic-policy test | CI |
| Key isolation test (`keyd` buffers unmappable to clients) | CI |
| Boot integrity / signature verification | release |

---

## 14. Threat matrix (summary)

| Threat | Mitigation |
|---|---|
| Malicious app reads another user's files | VFS permission checks against kernel creds; fs jail; audit |
| App calls an ungranted service | Default-deny Messenger policy; handles required; audit |
| App escapes syscall allowlist | Per-process syscall bitmap; friendly deny; audit |
| App exhausts kernel memory | Metered handles/buffers/queues; quotas; OOM policy |
| Forgery of sender identity | Kernel-stamped credentials; userspace cannot write them |
| Clipboard/data exfiltration | Clipboard policy + consent; transfer caps; audit; a paste of sensitive data can be marked |
| Compromised daemon | Least-privilege service account; signed bundle; sandbox profile; no root |
| Network daemon exploit | Sandboxed; TLS keys in `keyd`; per-profile firewall; minimal parser surface |
| Kernel bug via syscalls/parcels | Rust safety, checked copies, fuzzing, W^X, SMEP/SMAP, KASLR-lite |
| Secret theft | `keyd` isolation (keys never leave its memory); per-login sealing |
| Repudiation | Hash-chained audit log; correlation ids for calls |
| Privilege creep | Capabilities dropped on exec; timed elevation; diff-based consent |

---

*See also:* [Platform plan](platform-plan.md) · [Messenger spec](messenger.md)
