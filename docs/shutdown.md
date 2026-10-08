# Orderly shutdown and reboot

**Status: stages S-a to S-e built (2026-10-02).** This page started as the wiki
"Shutdown Plan" and now describes what exists; section 8 lists what the plan
asked for and was deferred or done differently. See also
[`platform-plan.md`](platform-plan.md) §4.3,
[`architecture/processes.md`](architecture/processes.md) (syscall 21) and
[`messenger.md`](messenger.md).

`init` is the single orchestrator: it stops the session apps, then the
services in reverse dependency order, and only then calls the kernel's
`power()` syscall, which syncs the filesystems and stops or resets the machine.
Reboot and power-off are the same sequence until that last call.

## 1. Before this work

- **Kernel power path.** Syscall 21 (`power(op)`, `kernel/src/process/power.rs`)
  required `CAP_SYS_ADMIN`, ran `fs::sync_all()` (which marks ext2 clean),
  disabled interrupts and reset through the 8042 or wrote QEMU's ACPI ports,
  falling back to `halt()`.
- **Nothing above it.** No userspace program called `power()`; the shell's
  `reboot`/`poweroff` were BusyBox applets that signal a pid 1 LazyOS does not
  have, and Linux `reboot(2)` was accepted and ignored. `init` had no stop
  phase: an exiting service was restarted, `confd` could be cut mid-write and
  apps got no notice.

## 2. Design principles

1. **`init` is the single orchestrator.** The kernel only stops the machine.
2. **Reverse dependency order.** Dependents stop before their dependencies:
   apps, then services, with `confd`/`logd` after every service that could
   still write to them and `messengerd` last.
3. **Every phase has a deadline.** A hung program is killed, logged and
   skipped; nothing blocks the stop.
4. **Idempotent and one-way.** After the first request nothing restarts,
   autostart stops and `Launch` returns `EBUSY`; a second request reports the
   current phase.
5. **Authority.** Root or any caller in a login session may ask `init`; a
   labelled (installed) app may not. The kernel's `CAP_SYS_ADMIN` gate stays:
   `init` is the only caller of `power()`.
6. **Observable.** Each phase is published on a retained topic and printed on
   serial, so the compositor, `logd` and the tests can follow it.

## 3. The sequence

| Phase | Owner | Action | Deadline |
|---|---|---|---|
| 0. Request | caller -> `init` | `init.Shutdown(mode, reason, force)` replies at once with the phase. | - |
| 1. Freeze | `init` | `stopping()` is set: no restart, no autostart, `Launch` is `EBUSY`. Rows waiting to start or restart are retired. The kernel watchdog is armed (`power(ARM_WATCHDOG, op)`). Phase `stopping` is published. | - |
| 2. Apps | `init` | Every launched app is stopped by the one rule `Stop` and logout share (phase `apps`, issue #651, `svcpolicy::stop_mode`): `Quit(grace)` on its lifecycle channel when it watches one or is resident (the Volume applet prints `VOLUME:QUIT:PASS`), `SIGTERM` otherwise; LazyShell included: its `Restart::Always` row is retired like any other, never restarted. `INIT:SHUTDOWN:APPS asked=<n> quit=<n> term=<n>`. `xuid` paints the shutting-down overlay as soon as it sees the topic. | 3 s from the stage's start, then `SIGKILL` |
| 3. Services | `init` | The manifest services in [`stop_order`](../user/src/bin/init/stop_order.rs) order (phase `services`): a row is stopped once no live row depends on it and no lower-tier row is live. A service that serves `os.lazy.lifecycle.v1` (`confd`, `logd`, `pkgd`) gets its `Shutdown` message; any other gets `SIGTERM`. `pkgd` (an ordinary tier, depending on `confd` and `mimed`) fsyncs `/logs/pkg.log`, the tail of its audit chain, and prints `PKGD:STOP sync=<ok|none|errno>` before `confd` is asked to stop. Independent rows stop together. | 3 s each, then `SIGKILL` |
| 4. Persist | `confd`, `logd` | `confd` flushes its store's volume (`CONFD:STOP dir=/conf sync=ok`; the harness requires `/conf` on a desktop boot); its writes are synchronous, so none is in flight. `logd` drains its feeds, flushes and fsyncs its journals in `/logs`, and verifies its chain (`LOGD:STOP records=<n> verified=<bool> persisted=<n>`; the harness requires `persisted>0` on a desktop boot). Both are in the persist tier, so they stop after every ordinary service. | (phase 3's) |
| 5. Quiesced | `init` | Every row is reaped; `init: userspace quiesced (killed=N)`. Phase `power` is published. | - |
| 6. Kernel | `init` -> `power()` | `sync_all` (ext2 clean bit), then ACPI power-off or the 8042 reset, a triple fault if the 8042 ignored it, `halt()` as the last resort. | - |

The whole sequence is bounded by `init`'s global deadline (20 s), after which
everything left is killed and phase 5 runs; the kernel watchdog (30 s) covers
an `init` that dies or hangs.

### Stop tiers

`stop_order::tier` puts each manifest service in a tier: ordinary services
(1), the persistence services `confd` and `logd` (2), and `messengerd` (3).
Within the lowest live tier a row is ready when no live row names it as a
dependency. If nothing is ready while live rows remain and none is stopping (a
dependency across tiers, or a cycle), the tier rule is dropped, then the
dependency rule, and `init` logs that it relaxed the order, so the shutdown
always progresses. On today's manifest the rounds are: the leaves (`keyd`,
`timed`, `usbd`, `pkgd`, `flaky`, the drivers, ...), then what they depended
on (`inputd`, `healthd`, `mimed`), then `confd` and `logd`, then `messengerd`.

### Native services and `SIGTERM`

The first end-to-end run had to `SIGKILL` ten services: a native task's
pending `SIGTERM` was only acted on when a timer tick found it in user mode,
and a service blocked in a Messenger `recv` was woken, re-parked inside the
kernel, and never got there. The native syscall gate now ends a task with a
pending default-fatal signal on its way out (`task::signal::deliver_native`),
and `recv` returns instead of parking again when woken by one. Every service
now exits on `SIGTERM`; the whole stop takes about half a second under WHPX.

### What is not stopped by `init`

`xuid` (and the kernel's console programs) are spawned by the kernel, not
`init`, so they run until the machine stops: that is what keeps the overlay on
screen. Login shells are `logind`'s children and outlive it. The kernel logs
`power: warning: N other task slot(s) in use at sync` for whatever is still
there; the sync is safe regardless (each write is atomic under the VFS lock).

`usbd` is deliberately left running (`OUTLIVE` in `shutdown.rs`): it serves
the USB stick that may hold `/home`, and the kernel's `sync_all` in `power()`
writes back and flushes that volume through it (SYNCHRONIZE CACHE) before
the machine stops ([usb-storage.md](architecture/usb-storage.md)). Its
dependency `inputd` is still stopped; `usbd`'s input then goes nowhere.

## 4. Interfaces (MIDL first)

- **`idl/init.midl`**:
  `method Shutdown(mode: U32, reason: String, force: Bool) -> (accepted: Bool, phase: String)`,
  `enum PowerMode { PowerOff, Reboot }` (travels as `U32`, generated as
  `POWER_MODE_POWER_OFF`/`POWER_MODE_REBOOT`), `struct PowerState { phase,
  mode, reason, deadline }` and the retained topic `system/power/state`. The
  `Services()` table shows rows as `stopping` while they are asked to stop.
- **`idl/lifecycle.midl`**: `os.lazy.lifecycle.v1` with one method,
  `Shutdown(reason: String) -> () oneway`. A service registers the interface
  next to its own and checks each message with
  `services::lifecycle::stop_requested`, which only obeys a sender holding
  `CAP_SYS_ADMIN`. `init` waits for the exit, not a reply.
- **Kernel**: syscall 21 is `power(op, arg)`: `0` reboot, `1` power-off,
  `2` arm the watchdog (`arg` is the stop to force). All three need
  `CAP_SYS_ADMIN`, checked before the op is decoded.
- **Clients**: `powerctl` (`/system/bin/powerctl`), run by the shell as `shutdown`,
  `poweroff`, `halt` (`powerctl poweroff`) and `reboot` (`powerctl reboot`);
  `-f` sets `force`. LazyShell's start menu (`xui-app/src/shell/power.rs`,
  rows in `xui-app/crates/shell/src/menu/power.rs`) ends with "Restart..." and
  "Shut down..."; choosing one swaps the two rows in place for "Restart now" /
  "Shut down now" and "Cancel", and only the confirmation calls `init.Shutdown`
  (`force = false`, reason "requested from the start menu"; markers
  `SHELL:POWER:CONFIRM`, `SHELL:POWER:REQUEST mode=<m> phase=<p>`,
  `SHELL:POWER:REQUEST:FAIL mode=<m> errno=<e>`). LazyShell runs in the
  user's login session (issue #623) and is admitted by its session id, as is
  anything else in a login session; a system service is admitted by
  `CAP_SETUID`; an installed (labelled) app never is, nor a sessionless task
  without the capability (the login screen, a driver). The shell raises no
  overlay itself: `xuid` follows `init`'s retained `system/power/state`, which
  `init` publishes before it stops anything. Nothing but `init` calls
  `power()`.

## 5. Failure handling

| Case | Behaviour |
|---|---|
| Program ignores its stop | Killed at its deadline (`init: <name> killed (stop deadline)`); a killed row not reaped within 0.5 s is written off. |
| Dependency cycle or cross-tier dependency | The order is relaxed (logged); the stop always progresses. |
| Whole sequence too slow | At 20 s everything left is killed and the machine stops. |
| `init` crashes or hangs mid-shutdown | The kernel watchdog (armed at phase 1) fires at 30 s from the kernel task: kills every user task, syncs, performs the armed stop. |
| A service crashes while stopping | Retired like a stop (`init: stopped <name> (status N)`), never restarted. |
| `sync_all` fails | Logged; the stop proceeds and the volume stays dirty, so the next mount sees it. |
| Second request while stopping | `accepted: true` with the current phase; `force: true` skips to killing what is left. |
| `power()` refused | `INIT:SHUTDOWN:FAIL power errno=N`, phase `failed`; `init` keeps serving. |
| Reboot when the 8042 does not reset | A triple fault (empty IDT, `int3`) resets the CPU. |

## 6. Tests and evidence

**Kernel** (`python tools/test/run.py --accel none`; the stops themselves are
stubbed under `lazyos_tests`):

- `power_suite`: the arm is `CAP_SYS_ADMIN`-gated before its argument is
  looked at; only a real stop may be armed; the first arm wins (a later arm
  never moves the deadline); expiry fires exactly once and never early; the
  deadline saturates instead of wrapping; repeated stop requests are safe; soaks
  of 20 000 arm/expire generations and 10 000 refused calls.
- `fsops_power_capability_gated`: an unprivileged reboot/power-off is `EPERM`.
- `task_signal_native_sigterm_fatal_at_syscall_return` and
  `task_signal_soak_native_sigterm` (1 500 generations): a native task's
  pending `SIGTERM` is fatal at its syscall return, ignored signals are
  consumed, a Linux task is left to its own path, and nothing leaks.
- `native_exec_lookup_maps_names_to_files`: `shutdown`, `poweroff`, `halt` and
  `reboot` reach `/system/bin/powerctl` with the right preset argument, and never
  shadow a real file.
- The shutdown's filesystem side is the existing ext2 coverage:
  `fs_ext2_sync_all_flushes_every_mount`, `fs_ext2_state_dirty_then_clean` and
  `fs_ext2_soak_state_generations` (200 write/stop/remount generations, clean
  and unclean), plus the configured layout's own power cycle in `mount_suite`:
  `mount_root_power_cycle_marks_clean` (writes through the native and the ABI
  table, the native `sync_all`, a clean next boot),
  `mount_root_unclean_stays_flagged_until_checked` and
  `mount_root_power_cycle_soak` (200 boots, clean and unclean stops, heap
  bounded). A clean stop restores the state found at mount, so an OS volume
  that once stopped uncleanly is reported again at every boot until the image
  build checks it (`Ext2::recover`, see
  [`architecture/filesystem.md`](architecture/filesystem.md)); a harness
  failure "the data volume was not clean after the power-off" on a reused
  `target/lazyos.img` usually means that, and a rebuild clears it.

**`init` self-test** (debug boots): `INIT:SHUTDOWN:ORDER:PASS` runs the stop
order on the manifest's real dependency graph (every dependent before its
dependency, tiers in order) and on a dependency cycle.

**End to end** ([`tools/shutdown/`](../tools/shutdown/README.md)):
`python tools/shutdown/run.py` builds the desktop image and boots it twice on a
fresh data disk: the Terminal writes a file to `/data` and types `shutdown`;
the second boot reads the file back, finds `/data` clean, and reboots through
LazyShell's start menu. `judge.py` checks each serial log (below);
`test_judge.py` proves the judge fails when it should. The two standalone
session scripts drive the same paths without the judge:

```bash
python tools/screenshot/qemu_session.py --image target/lazyos.img \
    --out shots/shutdown --script tools/screenshot/examples/shutdown_shell.json \
    --extra-arg=-no-shutdown
python tools/screenshot/qemu_session.py --image target/lazyos.img \
    --out shots/reboot --script tools/screenshot/examples/shutdown_menu.json \
    --extra-arg=-no-shutdown
```

`shutdown_shell.json` types `shutdown` in the Terminal; `shutdown_menu.json`
opens LazyShell's start menu (the "LazyOS" button at (44, 704)), picks
"Restart..." at (134, 648) (the power rows are the menu's last two, centred at
`H - 72` and `H - 48`), confirms with "Restart now" in the same place, and
captures the "Restarting..." overlay. In the serial log the phases appear in order
(`INIT:SHUTDOWN:BEGIN`, `PHASE stopping`, `apps`, `services`, `CONFD:STOP`,
`LOGD:STOP`, `INIT:SHUTDOWN:QUIESCED`, `PHASE power`), then
`power: filesystems synced` and `power: shutdown requested` (or `reboot
requested`). `-no-shutdown` makes QEMU pause at the power-off (and, with the
session's `-no-reboot`, at the reset) instead of exiting, so the last frame
(the overlay) can still be captured; the kernel would print a fallback line
had the VM not stopped, which the judge rejects.

## 7. Delivery stages

| Stage | Content | Status |
|---|---|---|
| S-a lifecycle plumbing | `Stopping` phase and freeze in `init`, `Shutdown` MIDL method, ordered stop with deadlines, `powerctl` and the shell commands | built |
| S-b service cooperation | `os.lazy.lifecycle.v1`; `confd` flushes and `logd` drains on stop; `system/power/state` | built |
| S-c GUI | start menu "Restart..." / "Shut down..." with a confirmation (in `xuid`'s menu at first, LazyShell's since #157), the shutting-down overlay, input ignored under it | built |
| S-d kernel hardening | watchdog, triple-fault reboot fallback, live-task warning at sync, `SIGTERM` delivery to native tasks at syscall return | built; FADT ACPI deferred (section 8) |
| S-e docs | this page, `architecture/processes.md`, `architecture/userland.md`, `platform-plan.md` | built |

## 8. Decisions, deviations and deferred items

- **One topic, not two.** The plan proposed `system/power/stopping` and
  `system/power/phase` in `topics.midl`; a single retained
  `system/power/state` declared by its owner (`init.midl`) carries the phase,
  mode, reason and deadline.
- **The lifecycle contract is opt-in.** The plan wanted the
  `messenger_async` `shutdown { method }` clause wired into every daemon. The
  real daemons are hand-written loops, not `service!` users, and most hold no
  state worth saving, so only `confd`, `logd` and `pkgd` (since F4, issue
  #508) serve `os.lazy.lifecycle.v1`; everything else gets `SIGTERM`. Adding a service to
  `shutdown::GRACEFUL` and registering the interface is all it takes.
- **Exit, not reply.** `Shutdown` is one-way: the service's exit is the
  acknowledgement, which `init` already reaps, so `init` never blocks on a
  service that might itself be waiting on `init`'s broker.
- **`logind` keeps no "no more logins" state.** It is a tier-1 service, so it
  is stopped in phase 3; a login in the second before that is ended with it.
- **Open writers are warned about, not refused.** The kernel reports how many
  task slots are still in use at sync instead of tracking write handles.
- **ACPI from the FADT** (real hardware power-off and the FADT reset register)
  is [`real-pc-boot-plan.md`](real-pc-boot-plan.md) H4; until then power-off
  uses QEMU's ports and halts with "it is now safe to turn the machine off"
  elsewhere.
- **Linux `reboot(2)`** is still accepted and ignored: the kernel cannot run
  `init`'s sequence for a Linux caller, and the shell commands no longer reach
  BusyBox's applets.
- **Who may shut down:** a system service (`CAP_SETUID`) or any
  login-session caller, for now (above). A tighter
  policy (console session only, or a `confd` setting) can come later without
  changing the sequence.
- **App veto or delay** ("unsaved changes"): deferred. Apps get `Quit` (or
  `SIGTERM` without a lifecycle channel) and a fixed 3 s, the same rule as
  `Stop` and logout (issue #651); a veto would need its own hard timeout.
- **Reboot reason record** (a boot-record page so the next boot can log why it
  restarted): deferred.
