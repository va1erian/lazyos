# NTP in `timed`

Status: N0 to N3 are built (see "As built" at the end); the Settings page is
rendered offscreen and has no QEMU session of its own.
Implements the SNTP half of
[`tls-plan.md`](tls-plan.md) section 5.3 and stage T5 (the `keyd` password
and Rhai `http` items of T5 are unrelated and stay there).

## 1. Goal

When a network address exists, `timed` asks an NTP server for the time and
steps the wall clock, so a machine with a dead or wrong RTC gets correct
certificate validity (TLS, Mail, LazyWeb+), SMB/NTLM timestamps and file
times without the user touching Settings. It must cost nothing on an image
with no network stack, never make the clock worse than the RTC would, and
stay a correctness aid, not a security control (plain SNTP is
unauthenticated).

Non-goals for v1: clock slewing, NTS, running as an NTP *server*, leap-second
smearing, hardware timestamping, sub-10 ms accuracy.

## 2. What exists today (as read from the tree)

| Piece | Where | Relevance |
|---|---|---|
| The service loop | `user/src/bin/timed.rs` | One thread. `wait_any([server, confd-bell], deadline)`, then `housekeeping`. The deadline is `State::next_wake()` (next minute tick or confd retry). New work hooks in here. |
| State and `confd`/broker links | `user/src/bin/timed/state.rs` | Lazy links dropped on first failure and re-resolved; the confd watch + doorbell pattern is what the new config and network watches reuse. |
| `SetTime` | `user/src/bin/timed/handler.rs`, `idl/timed.midl` | Whole seconds, `EPERM` unless `CAP_SYS_TIME` or `elevd`; calls `sys::wall_set`, then `next_tick = 0` so the retained `time/tick` is republished. The NTP step goes through the same internal function. |
| Clock syscall | `kernel/src/process/wallsys.rs`, `kernel/src/wallclock.rs`, `libs/lazyos-sys/src/time.rs` | Syscall 24: `get` in centiseconds, `set` in **whole seconds** (`wallclock::set(secs, 0)`, which also writes the RTC). The clock is `PIT ticks + OFFSET_CS`, 10 ms resolution. Sleepers on absolute realtime deadlines keep their monotonic deadline across a step. |
| Credentials | `user/src/bin/init/supervise.rs` (`manifest_cred`) | A manifest service inherits `init`'s identity minus the input/USB caps, so `timed` already holds `CAP_SYS_TIME` (that is how `SetTime` works). `tls-plan.md` 5.3 saying it does not is stale. **Verify with `cred_get` at implementation and fix that doc.** |
| Network up signal | `idl/net.midl` | `system/events/network/up` (`AddressEvent`, **not retained**), retained `system/net/<if>/addr`, retained `system/net/interfaces`. A service that starts after `netd` has an address misses the event, so it also needs the retained topic or a poll. |
| Sockets and DNS | `user/src/messenger/netstd.rs` (`UdpSocket`), `netsock.rs`, `netstack.rs` (`resolve`) | `os.lazy.net.socket.v1` is allowed for any actor (`libs/netpolicy`). `recv_from` takes a timeout, 0 never parks. `Resolve` **parks** up to `timeout_ms` (10..60000). |
| Config | `confd` `sys/time/zone`, `sys/time/clock24`, `sys/time/show_seconds` | `sys/**` is written only through `elevd` (`libs/elevpolicy`, `conf.set`). The Time page already works this way (`xui-app/crates/settings/src/time_ops.rs`). |

## 3. Design

### 3.1 `libs/sntp` (new, `no_std`, host-tested)

Everything that can be pure is pure, so most of the risk is tested on the
host without QEMU:

* **Codec.** The 48-byte RFC 4330 packet: encode a client request, decode and
  validate a reply (length >= 48, trailing extension/MAC ignored; version 3
  or 4; mode 4; leap indicator 3 refused; stratum 1..=15, stratum 0 is a
  kiss-o'-death code, `RATE`/`DENY`/`RSTR` parsed; transmit timestamp
  non-zero; `t3 >= t2`).
* **Anti-spoof.** The request's transmit field carries 64 random bits
  (kernel CSPRNG); a reply is accepted only if its *originate* equals them,
  and only from the address and port queried.
* **Era handling.** NTP seconds count from 1900 and wrap in 2036: map a
  timestamp to the era that puts it closest to the current clock, never
  assume era 0.
* **Maths.** `offset = ((t2-t1)+(t3-t4))/2`, `delay = (t4-t1)-(t3-t2)`.
  `t1`/`t4` wall times come from `wall_centis`; the *delay* is measured with
  `monotonic_ns` so a 10 ms wall quantum does not corrupt it. Accuracy target
  is "tens of ms", plenty for certificates.
* **Policy** (a plain state machine, no I/O): when to query, back-off, KoD,
  sample selection, step or refuse (3.4).

Tests: RFC vectors, an era-rollover case, offset/delay properties, hostile
packets, and a seeded fuzz entry point shared with a `fuzz/` target
(`FUZZ_CASES`/`FUZZ_SEED` like `libs/ext2fs`; `fuzz/gen_corpus.py --check`
keeps seeds current).

### 3.2 Configuration (`confd`)

| Key | Type | Default | Notes |
|---|---|---|---|
| `sys/time/ntp/enabled` | bool | **true on images that ship `netd`** (see 7, decision 1) | Machine setting; written through `elevd`. |
| `sys/time/ntp/servers` | string | `pool.ntp.org` | Space-separated `host[:port]`. |

`timed` watches `sys/time/ntp/*` with the same watch + doorbell as the zone
key. Harness override: a `timed` service argument `ntp.server=HOST:PORT`
(`LAZYOS_TIMED_ARGS`, the pattern `LAZYOS_NETD_ARGS` already uses in
`service_creds.rs`) that takes precedence and forces `enabled`.

### 3.3 When it runs

1. **Trigger.** Subscribe to `system/net/interfaces` (retained, so a late
   start still sees the current set) and `system/events/network/up`, rung
   through the existing doorbell. A 30 s `Addresses()` poll while enabled and
   unsynced is the safety net for a restarted `netd` or a missed event. On an
   image without `netd` none of this is ever reached: the socket client
   fails to connect, silently, and nothing fires.
2. **Exchange**, as a state machine stepped from `housekeeping`, not a
   blocking helper:
   `Idle -> Resolving -> Waiting(deadline) -> Done`. Resolve with a short
   timeout (1.5 s), send to up to 4 addresses, take the lowest-delay valid
   reply within 2 s, poll `recv_from(.., 0)` once per tick. The loop's
   `deadline` includes the exchange's next wake.
3. **Schedule.** First attempt right after the address appears; failures back
   off 5 s, 10 s, 20 s ... capped at 5 min with jitter; after a success poll
   every ~1 h (+-10 %). Never closer than 15 s to the same server; a `RATE`
   kiss quadruples the interval and drops that server for the session.
   Re-sync early after `network/up` fires again (lease regained, new Wi-Fi) if
   the last success is older than 10 min.

**Known wart: `Resolve` parks the whole service.** While it waits, `Now`,
`GetZone` and the minute tick wait too, up to the 1.5 s timeout, only when
DNS is failing. Accepted for v1 and measured in the harness (`Now` issued
during a black-holed lookup). If it bites, resolve in a short-lived helper
(`timectl`-style child, which needs the kernel NATIVE execve list) or move
the exchange to a thread, behind the same state machine.

### 3.4 Applying a result

The kernel can only step, so the policy is deliberately narrow:

* **Floor.** `timed` has a build-epoch constant (see 7, open item B). At
  start, a wall clock earlier than it is stepped up to it before any network
  exists (this alone fixes a dead RTC for "not yet valid" without a server).
  An NTP result earlier than the floor is refused, as is one at or above
  `MAX_SET_SECS`.
* **First sync after boot:** step for any offset (a dead RTC is the case this
  exists for).
* **Later syncs:** step only when `250 ms <= |offset| <= 1000 s`; smaller is
  left alone (no churn), larger is logged and refused (an on-path attacker
  cannot wander the clock after boot: one lying answer cannot; see 8.2 for
  what an attacker who answers every query alike can still do).
* **A step** goes through the same function as `SetTime`, so the retained
  `time/tick` is republished, and prints one `TIMED:NTP:STEP` line with
  server, offset, delay, stratum.

### 3.5 Kernel and syscall: centisecond step

Today `set` has whole-second granularity, so the sub-second part of every
NTP offset would be dropped. Make syscall 24's `set` take **centiseconds**
(`wallclock::set(secs, centis)` already exists), change `lazyos_sys::wall_set`
to match and `SetTime` to multiply by 100. No parallel op and no alias, per
the repo's no-backward-compat preference. The RTC write stays once per step;
with the policy above that is a handful per day.

Per `AGENTS.md`, a kernel-facing change ships correctness tests (range,
`CAP_SYS_TIME` check still ahead of the argument, centisecond round trip,
`read_unix` after set) and a soak (many alternating forward/back steps with
readers, no wrap or negative remainder) in `kernel/src/tests/timed_suite.rs`
and `wallclock_suite.rs`.

### 3.6 Interface additions (`idl/timed.midl`, regenerated with `midlc`)

* `method NtpStatus() -> (enabled, state, server, stratum, offset_ms,
  delay_ms, last_sync_unix, next_sync_unix)`; `state` an ordinal
  (`Disabled`, `Waiting for network`, `Syncing`, `Synchronized`, `Failed`).
* `method SyncNow() -> ()`: unprivileged (it only triggers a query whose
  result is bounded by 3.4) but rate-limited like the schedule, so a local app
  cannot make the machine spam a server or its own clock.

Regenerate `libs/generated`, `docs/idl/os.lazy.timed.v1.md`, the Rhai API
and `libs/rhai-lazy/src/msg/idl.rs` (`python tools/midlc/midlc.py --schema
libs/rhai-lazy/src/msg/idl.rs --rhai-api libs/rhai-lazy/api idl/*.midl`).

### 3.7 CLI and Settings

* `timectl ntp status` and `timectl ntp sync` (unprivileged). Changing the
  config is a Settings or `elevd` action, as for the zone.
* Time & Date page (`xui-app/crates/settings/src/time_page.rs`): a "Set time
  automatically" checkbox, a server field, a "Sync now" button and a status
  line ("Synchronized 3 min ago, +12 ms, pool.ntp.org"). With automatic time
  on, the manual date/time fields and Apply are disabled. Writes use the
  existing `time_ops` + `elevd` route. New `System` trait methods get the
  offscreen test double so the page stays host-testable.

## 4. Security and failure notes

* Source address and port, originate echo, mode/version/stratum/LI and
  delay/root-distance bounds are all checked (3.1); a packet failing any is
  dropped and counted, never partially trusted.
* Every step is logged with enough to reconstruct it; steps are rare by
  policy. The audit stream is for Messenger decisions; the log line is the
  record here.
* Config writes need `elevd` (`sys/**`); add a row to the account attack
  harness (`tools/accounts/attack_judge.py`) that a session user cannot write
  `sys/time/ntp/*` or push `SyncNow` past its rate limit.
* A user-visible side effect to document: a backward step can put new files
  "before" existing ones (ext2 stamps) and shifts `time/tick`; both are
  inherent to stepping and bounded by 3.4.
* Privacy: the pool sees the machine's public IP, like every OS. Changing
  `servers` to a private host is the opt-out; so is `enabled=false`.
* `timed` keeps `init`'s identity for now. A dedicated uid with only
  `CAP_SYS_TIME` is the right end state but needs confd write policy for its
  own keys first; tracked as a follow-up, not part of this plan.

## 5. Verification

Per `AGENTS.md`, "it builds" is not "it works": the clock is judged from the
guest and the wire.

1. **Host:** `cargo test -p sntp -p timezone`, the seeded fuzz,
   `python fuzz/gen_corpus.py --check`, the `libs/rhai-lazy` bindings test,
   `cd xui-app && cargo test -p xui-settings` (snapshots in
   `xui-app/target/snapshots`, Read them).
2. **Kernel suite:** `python tools/test/run.py --accel none`, plus
   `LAZYOS_TEST_FILTER=wallclock` while iterating.
3. **QEMU harness** `tools/ntp/run.py` with `tools/ntp/test_judge.py` (the
   judge fails when it should). A host UDP server `fake_ntpd.py` (unprivileged
   port, `LAZYOS_TIMED_ARGS=ntp.server=10.0.2.2:PORT`) plays: honest with the
   clock skewed by +10 s and +1 h, KoD `RATE`, wrong originate, wrong source
   port, stratum 0/LI 3, silent (timeout), large delay. The guest boots with a
   deliberately wrong RTC (`-rtc base=2020-01-01`). Judged: `timectl now`
   before/after, `TIMED:NTP:STEP`/`TIMED:NTP:REFUSE` markers, the server's own
   request log (request count within the rate limit, one per back-off step,
   originate echoed), and a late-link case (`set_link` down at boot, up
   later: no query before the link, one soon after).
4. **Settings screenshot session** `settings_time_ntp.json` (Read the PNGs;
   `pngstats.py` assertions), built with the launcher flags in 6.
5. Whole kernel suite and `python tools/services/` evidence unchanged on a
   no-network image (timed's NTP code must print nothing there).

## 6. Staging

| Stage | Content | Kernel tests | Done when |
|---|---|---|---|
| **N0** | Floor at start; centisecond `set` (3.5); verify and correct the stale `CAP_SYS_TIME` note; `timectl` already covers `set`. | **yes** | A boot with a pre-floor RTC lands on the floor; suite green. |
| **N1** | `libs/sntp`: codec, maths, policy, fuzz target and seeds. | none | Host tests and fuzz green. |
| **N2** | `timed` engine, config keys, triggers, `NtpStatus`/`SyncNow`, `timectl ntp`, `LAZYOS_TIMED_ARGS`, `fake_ntpd.py`, `tools/ntp/run.py` and judge, attack-harness row. | none | `tools/ntp/run.py` passes every scenario. |
| **N3** | Settings Time & Date controls and a screenshot session; docs (`tls-plan.md` 5.3/T5, `topics-catalog.md`, `build-switches.md`, `AGENTS.md` time section). No launcher flag: automatic time is a runtime setting on by default in every image that has a network stack, and the only build hook, `LAZYOS_TIMED_ARGS`, is a harness switch like `LAZYOS_NETD_ARGS` (no GUI field; `run_demo.py` and the launcher already pass the environment through). | none | Screenshot session reviewed; `check_file_length.py`, `check_literals.py` clean. |
| **N4** (later) | Kernel slew (`adjtime`-style) so small drifts are corrected without steps; DHCP option 42 as a server source; several servers with real selection; NTS. | yes | Separate plan. |

Every stage ends with the required pre-PR review by a separate agent
(`AGENTS.md`, "Pre-PR review").

## 7. Decisions and open items

**Decisions for the owner (recommendation first):**

1. **Default.** On for images that ship `netd`, off otherwise, because the
   request is "when network is available" and a dead RTC is exactly the
   unattended case. `tls-plan.md` 5.3 said "off by default"; this reverses
   it, so say so there.
2. **Sub-second `set`.** Change syscall 24's `set` to centiseconds (no
   alias), vs a new op beside it. Recommended: change it.
3. **Step-only in v1** (3.4's bounds) vs wait for a kernel slew. Recommended:
   step-only; the bounds make it safe enough and N4 closes the gap.
4. **Server list.** `pool.ntp.org` unadorned (the pool asks vendors to
   register a vendor zone before shipping a product widely; fine for this
   project, worth knowing).

**Open items to verify while implementing:**

* A. `timed`'s effective capabilities and uid at runtime (3.4 and section 2).
* B. Where the build date comes from: `SOURCE_DATE_EPOCH`, the commit time,
  or a stamp `build_support` already writes. A reproducible-build-friendly
  constant is wanted; check before adding one.
* C. Whether the elevd `conf.set` allowlist already covers
  `sys/time/ntp/*` (the clock-format keys pass today under `sys/time/`).
* D. Whether `netd`'s ephemeral UDP source ports are unpredictable (the
  originate echo covers most of off-path spoofing; a predictable port makes
  it cheaper). If not, that is a `netd` fix, not an NTP one.
* E. Exact subscription syntax for the two network topics through
  `user::central` (confd's `name_system_confd_changed` is the model).

## 8. As built

What differs from the plan above, and what the open items turned out to be.

* **Open item A (credentials).** `timed` is started by `init` with its own
  identity minus the input and USB capabilities (`manifest_cred`), so it
  holds `CAP_SYS_TIME`; `SetTime` already depended on it. The stale sentence
  in `tls-plan.md` is corrected. A dedicated uid stays a follow-up.
* **Open item B (build date).** Nothing stamps one, and a build-time stamp
  would make every build differ, so the floor is the checked-in constant
  `sntp::CLOCK_FLOOR_UNIX`, 2026-01-01: the kernel's own fallback for a
  missing RTC, so a reset RTC is treated like an absent one. (A first choice
  of the build month made the accounts harness's `time.set 1767225600`
  fall under it.) `SetTime` refuses anything below it with `EINVAL`, so what
  is accepted is what survives the next start.
* **Open item C (`elevd`).** `conf.set` covers all of `sys/**`: the Settings
  page writes `sys/time/ntp/*` through `elevd` with no policy change.
* **Open item E (subscriptions).** `timed` subscribes `system/net/+/addr`
  (retained, an all-zero address when a lease is lost) and rings the
  service loop through a doorbell, as it does for `confd`; the network is
  confirmed with `Addresses`. The single `confd` watch is widened to
  `system/confd/changed/sys/time/#`.
* **Harness hook.** `LAZYOS_TIMED_ARGS` (default `demo=1`) is passed to
  `timed`; `ntp.server=HOST[:PORT]` forces the client on and at that server.
* **Syscall.** Native `wall_time` op 1 (`set`) now takes centiseconds, the
  unit `get` returns; `lazyos_sys::wall_set` became `wall_set_centis`.
* **Not as planned.** The `Resolve` call still parks the service for up to
  1.5 s when DNS is failing (section 3.3); the exchange itself is polled in
  one-tick slices. `SyncNow` also answers `EAGAIN` while a query is in
  flight.
* **`timectl` from a shell.** A program a shell may run is on the kernel's
  `NATIVE` list (`kernel/src/process/linux/native.rs`); `timectl` was not, and
  ran as a Linux program (it died at its first allocation). It is on it now.

### 8.1 The harness as built (`tools/ntp`)

A services console image (`LAZYOS_SERVICES=1 LAZYOS_NETD=1`, the login typed
as `user`/`lazy`), one boot per scenario against `fake_ntpd.py`, judged from
the serial log, the time `timectl now` prints and the server's request log:
`dead_rtc` (a 2020 RTC is floored at start and then stepped to the server's
time), `skew`, `resync` (a second exchange 20 s later), `refuse_large` (a
later answer 2 h away is refused and the clock stays), `rate` (one request,
then silence), `silent` (retries back off, never under 15 s apart) and
`hostile` (forged nonce, alarm, stratum 16, zero timestamps, garbage, a reply
from another port, and 1970: nothing is believed). All seven pass under WHPX
(`--accel auto`); the guest behaved the same in the earlier runs under TCG.

Not covered there, and why:

* **The configuration keys.** A session cannot write `sys/**`, and the
  console has no `elevd` prompt. The key handling is `libs/sntp` (server
  list), the Settings tests (`time_ops`, the window test) and the page's
  `elevd` route, which the existing Time & Date session already exercises.
* **A late link** and the account attack-harness row of section 4. Both are
  still to do; the schedule's network-edge rules are covered by the host
  tests of `libs/sntp/src/schedule.rs`.
* **A Settings session in QEMU.** The page is rendered offscreen in the
  settings tests (`settings-time-ntp-*.png`); the new widgets sit under the
  taskbar-clock toggles so the existing `xui_settings_time.json` clicks land
  where they did.

**A guest clock that loses time.** Whenever the kernel holds interrupts off
for long, QEMU's PIT drops ticks and the guest's PIT-driven wall clock falls
behind the host's: seconds in tens of seconds, under TCG and WHPX alike. That
is why the harness never asks for a particular offset from a second
exchange, and it is a fair argument for the scheduled sync (the clock is
corrected again each hour) and for the later-sync bound of 1000 s.

### 8.2 Changes after the pre-PR review

* **Switching the client off** drops the address subscription (an unread
  doorbell in the wait set made the service spin).
* **`last_sync_unix`** is on the clock after a step, not before it.
* **A `RATE` hold** (`Schedule::hold_until_ms`) survives "Sync now" and a
  network edge; **`DENY`/`RSTR`** addresses are not asked again this boot.
* **Resolution** is bounded: names are tried in order until one gives
  addresses, and no new name after 1.5 s of lookups (worst case about 4 s of
  stall, not removed: `Resolve` still parks the service).
* **A large offset that keeps agreeing** (`sntp::policy::Persistence`: three
  refusals within 5 s of each other over at least ten minutes) is accepted as
  a clock that really moved, so a paused or restored machine is corrected
  without a reboot.
* **The floor** is the kernel's own fallback base, 2026-01-01
  (`sntp::CLOCK_FLOOR_UNIX`); `SetTime` refuses anything below it, and
  Settings says so.
* **Settings** refuses a hand-set clock only while `timed` reports a network
  time client that is actually acting (not on an image with no network).
* **Address subscription retries** are spaced 30 s apart, and the network
  time settings are read whenever the zone was, watch or no watch.

### 8.3 Changes after the second review (a fresh-context Sonnet agent)

* **The reply window starts after the lookups**, not before: a slow
  resolver no longer eats the time the servers get to answer.
* **A clock stepped under an exchange** (`SetTime` or anything else) drops
  that exchange (`Ntp::clock_stepped`) and a sample whose wall-clock span
  disagrees with its monotonic round trip by more than 1.5 s is not used.
* **Switching automatic time on, or pointing it at another server**, asks
  at once (within the 15 s minimum and any RATE hold) instead of at the next
  hourly poll; a new server list also forgets denied addresses.
* **No entropy, no exchange**: the nonce is the only defence against an
  off-path sender, so there is no guessable fallback.
* **`elevd`'s `time.set`** refuses a time below the floor, as `timed` does,
  so an administrator is not asked for a password for a change that must
  fail (`libs/elevpolicy` now shares `sntp::CLOCK_FLOOR_UNIX`).
* **The bound is honestly weaker than first written.** One lying answer
  cannot move a running clock by more than 1000 s; an on-path attacker who
  answers every query the same way gets there after ten minutes through
  `Persistence`. Plain SNTP offers no defence against that; NTS would. The
  documents that said "cannot wander" are corrected.
* **Harness:** every scenario also asserts `timed` announced itself once
  (a crash and restart would otherwise be invisible), a `deny` scenario
  exists, and `silent` really requires a growing retry delay.
* **Open:** `SyncNow` from any local app can still trigger a query every
  15 s and does not wait out a failure back-off.
