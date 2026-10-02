# Driver and device configuration through `confd`

Status: **proposal**, written with stage D3 (issue #240). Scope: what device and
driver configuration belongs in the `confd` registry, how it is named and read,
how it stays safe, and what each driver ticket picks up. It changes no code in
`confd`.

Related: [driver-plan.md](driver-plan.md), [confd-plan.md](confd-plan.md),
[security-model.md](security-model.md), [architecture/devices.md](architecture/devices.md).

## 1. What belongs in `confd` and what does not

`confd` is a **userspace** service reached over Messenger, storing typed values
under `sys/**` (world-readable, root-writable) and `user/<uid>/**`. That fixes
the split.

| Kind of setting | Where | Why |
|---|---|---|
| Driver tuning: `irq_mode` (`auto`/`poll`), poll interval, ring and period sizes, MTU, MAC override, default audio format/rate/channels | `confd` `sys/dev/<class>/<driver>/...` | Tunable by an administrator, harmless to get wrong, wanted at driver start |
| Device enable/disable policy (per class, per PCI function) | `confd` `sys/dev/policy/...` | A preference `devd` applies before launching a driver |
| Driver binding **overrides** (prefer driver A over B for id X) | `confd`, restricted to `devd`'s static candidates | Choice among candidates the static table already allows |
| The driver-binding **table** itself (vendor/device/class to program, uid, caps, restart) | **Static** in `devd` (same shape as `init`'s `MANIFEST`) | It decides which code runs with which authority; an editable registry key must never add a binding, change a uid or widen capabilities |
| Who may claim what (ACL rules, `CAP_DEV_CLAIM`, quotas) | Kernel ACL/credentials, loaded by `messengerd`/`init` | The authority. `confd` holds preferences only |
| Runtime **state** (link up, negotiated rate, claimed-by, health, generation) | Messenger topics (`system/devices/<id>`, `system/health/<driver>`) and the driver's class `Info()` call | Changes constantly, is written by unprivileged services (section 3) |
| Secrets (keys, Wi-Fi passphrases; see [wifi-plan.md](wifi-plan.md) §5.1) | `keyd` | `sys/**` is world-readable |
| Per-user audio preferences (default volume, preferred output) | `confd` `user/<uid>/audio/...`, read by a future `audiod`, not by the driver | Owned by the user; a driver serves the whole machine |

**What the kernel can never read.** `confd` is a userspace process behind
Messenger; the kernel has no client, it starts before `confd`, and reaching up
into a service would invert the layering. Kernel-tier drivers (block, PS/2,
serial, timer, the ATA and virtio-blk drivers in `dev/driver.rs`) are therefore
configured by **compile-time constants and `LAZYOS_*` build variables**, the
existing `kernel/build.rs` pattern (`LAZYOS_INIT`, `LAZYOS_SERVICES`, ...). There
is no runtime kernel command line today; if a bootloader hand-off ever adds one,
kernel drivers would read only that. Nothing a kernel driver needs may depend on
`confd` being up. Userspace drivers and `devd` read `confd` themselves.

**How a userspace service reads it.** At start it resolves `os.lazy.confd`
(`user::messenger::confd`), `Get`s **only its own known keys** (never `List`s a
subtree), and falls back to the documented default for any key that is absent,
mistyped or out of range. It then subscribes to
`system/confd/changed/sys/dev/<its subtree>/#`. `confd` announces only `sys/`
changes, best-effort, with the payload `(path, deleted)` and never the value, so
the handler re-`Get`s the named key. (`confd-plan.md` writes the topic
as `confd/changed/<path>`; the implementation, `user/src/messenger/confd.rs`, uses
the `system/` prefix.) Driver configuration therefore lives under `sys/`, not
`user/`, so that changes are announced.

## 2. Namespace

Segments are `[a-z0-9_.-]+`, so a PCI id is spelled `1af4-1000` (vendor-device,
lowercase hex) and a function `00-03-0` (bus-device-function). All keys below
are `sys/dev/...`; values use the `confd` types (`bool`, `i64`, `u64`,
`string`, `bytes`). "Apply": **live** takes effect when the change arrives;
**restart** takes effect at the next driver start.

| Key | Type | Default | Validation | Apply | Written by | Read by |
|---|---|---|---|---|---|---|
| `policy/default` | string | `allow-known` | `allow-known` or `deny`; else default | live | admin | `devd` |
| `policy/<class>/enabled` | bool | `true` | class is `net`, `audio`, ... | live | admin | `devd` |
| `pci/<bb>-<dd>-<f>/enabled` | bool | `true` | | live | admin | `devd` |
| `pci/<vvvv>-<dddd>/driver` | string | static binding | must name a candidate in `devd`'s table, else ignored | restart | admin | `devd` |
| `net/<drv>/irq_mode` | string | `auto` | `auto` follows the device's routable flag, `poll` forces polling | restart | admin | driver |
| `net/<drv>/poll_interval_ms` | u64 | 10 | clamp 1..1000 | live | admin | driver |
| `net/<drv>/rx_ring_entries`, `tx_ring_entries` | u64 | 256 | power of two, clamp 16..4096 | restart | admin | driver |
| `net/<drv>/mtu` | u64 | 1500 | clamp 576..9000 | live | admin | driver |
| `net/<drv>/mac_override` | string | empty (use the device MAC) | `xx:xx:xx:xx:xx:xx`, unicast, locally administered bit set, else ignored | restart | admin | driver |
| `audio/<drv>/irq_mode` | string | `auto` | as above | restart | admin | driver |
| `audio/<drv>/format` | string | `s16le` | `s16le`, `s32le` or `f32le` | restart | admin | driver |
| `audio/<drv>/rate_hz` | u64 | 48000 | one of 44100, 48000, 96000 | restart | admin | driver |
| `audio/<drv>/channels` | u64 | 2 | clamp 1..8 | restart | admin | driver |
| `audio/<drv>/period_bytes`, `periods` | u64 | 4096, 4 | period power of two 256..65536; count 2..16 | restart | admin | driver |
| `audio/<drv>/volume_pct` | u64 | 100 | clamp 0..100 | live | admin | driver |

**Networking (N0) narrows two ranges**, because the code behind them has fixed
sizes: `rx_ring_entries`/`tx_ring_entries` clamp to 16..=256 (the virtio queue
cap, also the default) and `net/<drv>/mtu` to 576..=1500 (a frame slot is 2048
bytes). The clamping is `libs/virtio-net/src/settings.rs`; see
[networking-plan.md](networking-plan.md) section 13.

Ownership is uniform: **administrators (uid 0 via `confctl`, installers) write,
services only read.** Drivers and `devd` write nothing to `confd`. `<drv>` is the
driver's program stem (`virtio-net`, `virtio-snd`, `e1000`).

## 3. The permission problem

v1 lets only uid 0 write `sys/**`, and drivers run as unprivileged uids
(`_net`, `_snd`). For *configuration* this is not a problem: every service can
read `sys/**`, and the administrator writing it is root. The problem is
**runtime state**: `devd` should publish "device 3 claimed by `_net`, link up",
and an unprivileged `devd` cannot write `sys/dev/state/**`. Options:

| Option | Result | Verdict |
|---|---|---|
| A. `devd` runs as uid 0 with no other capabilities | Works with today's `confd` | Rejected. uid 0 is ambient authority: `devd` could rewrite `sys/dev/policy/**` and every other `sys/` key, and root bypasses VFS checks. A device-matching service must not be the most privileged process on the system |
| B. `confd` grows a static per-subtree write grant, keyed by a dedicated uid (`_devd`) | Precise, default deny | Sound but needs a `confd` change and a real `_devd` account; every service still runs as root today, so it protects nothing yet. Keying it on `CAP_DEV_CLAIM` is worse: every driver holds that capability, so any driver could forge any device's state, and the capability's meaning ("may claim") would be stretched |
| C. Publish state on Messenger topics only | No `confd` change, no new writer; kernel topic policy and audit already apply; retained topics serve late subscribers | **Recommended** |
| D. Keep state out of `confd` entirely | Same as C for state, plus the driver's own `Info()` | Part of the recommendation |

**Recommendation: C and D. `confd` stays a store of preferences that only
administrators write and services only read; observable device state is
published by `devd` on retained topics (`system/devices/<id>`,
`system/health/<driver>`) and by each driver's class interface.**

Reasoning: (1) no ambient authority: no service gains a write path into `sys/**`,
so nothing but an administrator can change what other services read; (2) default
deny: the existing `confd` rules already deny every non-root write, and this adds
no exception to reason about; (3) audit: administrator writes are ordinary
`confd` changes (a `History` trail arrives with v2, section 6.2 of the registry
plan), while state publications already go through the kernel's topic policy and
audit ring; (4) state is high-churn and best-effort, which a persisted,
fsync-per-write store is the wrong tool for. If a queryable mirror is wanted
later (`confctl list sys/dev/state`), do it after v2 per-path ACLs land (registry
plan 6.4) with a dedicated `_devd` writer grant on that one subtree, and only
once services actually run under distinct uids.

## 4. Boot order, failure modes, and the kernel ACL

**Order** (`init` `MANIFEST`): `messengerd`, then `confd`, then `devd`, then the
drivers. Drivers list `deps: ["messengerd", "devd"]`, **not** `confd`: `deps` is a
hard start gate, and a device must not stay dark because the registry is slow.
`confd` is a *soft* dependency: a service tries `registry::resolve("os.lazy.confd")`
a few times with backoff (a second at most), then runs on defaults and retries
the resolve from its main loop, applying keys when `confd` appears.

| Failure | Behaviour |
|---|---|
| `confd` not up yet | Defaults; retry resolve; no device is ever blocked |
| `confd` restarts | Re-resolve, re-`Get` known keys (change topics are best-effort and stateless) |
| Store corrupt | `confd` starts empty and keeps `store.corrupt` (registry plan 5); every key reads as absent, so defaults apply, including `enabled = true` |
| Store not persistent | The shipped image's `confd` falls back to `/tmp/confd` (ramfs) and reports degraded; config written at runtime is lost at reboot until `/system` is writable, so defaults must be good values and the image should seed any non-default |
| Key absent, wrong type, out of range | Documented default (or clamp, section 2); log once per key; never panic |
| Hostile value (huge string, non-numeric MAC, ring size `2^60`) | Validated and clamped **by the driver** before use: parse, check the table's rule, clamp; nothing is allocated from an unclamped value |
| Change while running | **live** keys apply on the change topic. A **restart** key applies only when its *effective* (clamped) value differs: the driver logs it, drains, exits 0 and `init`'s restart policy respawns it. A value that clamps to the current setting never triggers an exit, so garbage cannot start a crash loop |

**The kernel ACL stays the authority.** No `confd` key can grant a right: the
claim decision is `CAP_DEV_CLAIM` plus the class rule `os.kernel.dev.<class>` in
the kernel ACL, and the handle's rights follow the device's real resources
(driver-plan D3). `confd` can only *narrow or tune*: disabling a class makes
`devd` not launch its driver, `irq_mode=poll` makes a driver skip `irq_enable`,
a MAC override renames a link. Losing or corrupting `confd` never widens access
(worst case, a disabled device comes back, and it is still bound by the ACL).
`devd`'s binding overrides can only choose among its own static candidates, so a
compromised administrator key cannot make `devd` spawn an arbitrary program as
`_net`.

## 5. Checklist per ticket

- **#239 (device core, done):** nothing to change. Stable ids in `DeviceInfo`
  (vendor, device, class, bus address) are what `devd` turns into
  `pci/<vvvv>-<dddd>` and `pci/<bb>-<dd>-<f>` keys.
- **#240 (this ticket):** no `confd` code and no kernel read of `confd`. Hooks
  added so drivers and `devd` need nothing from `confd` to start:
  `list` rows carry `owned` and `generation` (the kernel cannot publish topics,
  so `devd` derives claim state and respawn events from them), the Interrupt Line
  and an `IRQ_ROUTABLE` flag (`irq_mode=auto` needs no kernel round trip), and BAR
  sizes and kinds but never physical bases; `irq_enable` returns `ENOSYS` for an
  unroutable line, which is the polling signal; every claim, release, denial and
  ack timeout is audited with the device id as `txn_id` (`dev::report`); the
  boot log prints one `dev: irq route` line per PCI function; `user/src/dev.rs`
  decodes `list` rows.
- **#241 (DMA, virtio, virtio-net):** add a small shared config helper
  (`user/src/devcfg.rs`, over `user::messenger::confd`) with typed getters that
  take `(path, default, min, max)` or an allowed set and return the clamped
  value, plus the MAC validator; unit-test it on the host with garbage input.
  The `virtio-net` driver reads the `net/virtio-net/*` keys of section 2 and
  ignores anything else. Add `devd` with its static binding table and a
  `sys/dev/policy/**` filter; add the `_net` manifest row with
  `deps: ["messengerd", "devd"]` and a per-row identity (`init` already keeps a
  `spawn_as` credential per launched service; the manifest needs the field).
  Publish `system/devices/<id>` from `devd`.
- **#242 (virtio-snd and hardening):** read the `audio/<drv>/*` keys through the
  same helper; live-apply `volume_pct`, restart on period or format change with
  the clamped-value rule of section 4; fuzz the config parsers and the `dev_*`
  syscall together; revisit a `devd` state mirror once registry v2 per-path ACLs
  exist; keep the e1000 and HDA drivers on the same key shapes to prove the
  namespace is not virtio-shaped.
