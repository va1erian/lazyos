# Account attack harness

The tooling track (UT, issue #626) of `docs/accounts-plan.md`. Its judges are
the plan's invariants: no bricking, isolation, no privileged change without an
admin. This directory is part 1: **attack the OS as the desktop session's user
and check the machine and its files survive**.

```bash
python tools/accounts/run.py              # build a desktop image, boot four times, judge
python tools/accounts/run.py --no-build   # reuse target/lazyos.img (built by this script: it ships the guest scripts)
python tools/accounts/run.py --quick      # skip the hard-kill boot
python tools/accounts/test_judge.py       # the judges' self-test (fixtures for each failure kind)
python tools/accounts/attack_judge.py shots/accounts/attack/serial.log   # re-judge a log
```

Needs BusyBox (`python tools/abi/busybox.py`), the xui apps and `rhai`
(`run.py` builds them). QEMU on `PATH` as for the other harnesses. Output:
`shots/accounts/` (session scripts, serial logs, `audit_*.txt`). The image logs
`user` straight in (`LAZYOS_AUTOLOGIN=user`, issue #623) through the same path
as the login screen, and carries the probe packages `probe_packages.py` builds
into `target/accounts-assets/`.

## What it does

All boots use a **copy** of `target/lazyos.img` (`shots/accounts/work/`).

1. `warm`: first boot (core packages install), clean power-off. The volume is
   read from the host (`osread tree /`) as the audit baseline.
2. `attack`: runs every scenario once in the desktop Terminal, then powers
   off. The stop is judged (`INIT:SHUTDOWN:*`, `power: filesystems synced`).
3. `verify`: boots again and must log in (`LOGIN:OK:PASS`) and run a command
   (`TERM:OUT:ACCT:BOOT:OK`), then the session ends with the machine running:
   a hard kill. This login is also where `autostart_root` is judged.
4. `verify-kill`: boots after the hard kill and must answer again.

The volume is audited after step 2 and after step 4 against the baseline
(`libs/ext2fs/examples/osread.rs tree`: mode, owner, size, mtime, FNV-1a
content hash). Nothing may change outside `audit.ALLOWED` (`/home/user`, the
journals in `/logs`, `/lost+found`, scratch), except the paths an *open*
attack is declared to touch.

## Scenarios

`assets/accounts/attack.sh <name>` (and the rhai scripts beside it) ship in the image
through `LAZYOS_ASSETS` (`assets/manifest.txt`) at `/system/share/accounts/`.
Each prints one `ACCT:ATTACK:<name>:BLOCKED|SUCCEEDED:<detail>` line, which the
Terminal reports as `TERM:OUT:...`. Attacks that would brick the image are
probes: they open `/system/bin/init` for writing without changing a byte,
delete a canary file, and remove whatever they created.

| name | attack |
|---|---|
| `uid` | the session is not uid 0 |
| `rm_system` | `rm` a file under `/system` |
| `overwrite_init` | open `/system/bin/init` for writing |
| `write_conf` | create a file in `/conf` |
| `read_conf_store` | `cat /conf/store`: confd's raw store (`sys/**`, every user's keys) is root's alone (review of #659, H1) |
| `confd_sys` | rhai `sys::confd::set("sys/...")` |
| `keyd_provision` | rhai `sys::keyd::provision(...)` |
| `read_home_admin` | `ls /home/admin` |
| `signal_service` | `kill -CONT` the `logd` service |
| `autostart_root` | install `org.acct.autoprobe` (a package with `autostart`, as `user`); at the next login it must open in the session as `user`, never as root (judged from the verify boot's `INIT:AUTOSTART:*` lines) |
| `core_replace` | install `os.lazy.counter` 99.0.0 over the core app (only `elevd`'s `pkg.update-core` may, U2) |
| `acct_create` | rhai: `accountsd` `Create` of an admin, as the user (U1) |
| `acct_delete` | rhai: `accountsd` `Delete("admin")` (U1) |
| `acct_promote` | rhai: `accountsd` `SetAdmin("user", true)` (U1) |
| `acct_password` | rhai: `accountsd` `SetPassword("admin", ...)`, someone else's (U1) |
| `keyd_forget` | rhai: `keyd` `Forget("admin")`, `accountsd`'s alone (U1) |
| `keyd_verify` | rhai: `keyd` `Verify("admin", ...)` directly, around `accountsd`'s brake; `accountsd`'s alone (review of #659, H2) |
| `admin_lockout` | `attack.sh admin_lockout`: the session floods `Authenticate("admin", ...)` in the background (`auth_hammer.rhai`) while `elevd` asks for an administrator (`conf.elevate`); the harness types admin's right password and the request must be granted: a session's failures count against its own uid, never lock a name for `logind`/`elevd` (review of #659, H5) |
| `auth_flood` | rhai: 40 wrong `Authenticate("admin", ...)` in a row; BLOCKED when at most 8 were checked and the rest slowed (`EAGAIN`) (U1) |
| `direct_time` | rhai: `timed` `SetTime` directly, not through `elevd` (U2) |
| `direct_restart` | rhai: `init` `RestartService("inputd")` directly (U2) |
| `prompt_spoof` | rhai: open the trusted prompt itself (`os.lazy.display.prompt.v1`, `elevd`'s alone) (U2) |
| `input_focus` | rhai: move the keyboard focus through `inputd`'s compositor link, to take a prompt's keys (U2) |
| `display_read` | rhai: `ListSurfaces`, the display protocol's only screen-wide read (no method returns pixels) (U2) |
| `prompt_over` | `attack.sh prompt_over`: `elevd` shows the prompt, a window (the Counter) opens over it 5 s later; judged from screenshots before and after (`prompt_judge.py`: the window's `UI:RECT` overlaps the panel, and the panel is unchanged) (U2) |
| `prompt_keys` | the Terminal has the focus when the prompt opens; the session types `inject`, Enter and Escape: the prompt took the keys from `inputd` (`XUID:PROMPT:DONE ... keys=8`), `elevd` recorded the cancel, and the Terminal never saw a line (`TERM:CMD:inject`) (U2) |
| `input_flood` | the same (typing `flooded`) while three programs keep `inputd`'s shared endpoint full (`input_flood.rhai`, refused sends counted: the flood must be real); BLOCKED when no client got the keys, or `xuid` refused a prompt it could not take the keyboard for (review of #659, H3) |
| `prompt_flood` | cancel a prompt, then ask five more times at once: every request must be refused without a prompt (`EAGAIN`, `elevd`'s hold) (review of #659, H4) |
| `audit_forge` | rhai: `elevd` `conf.set` of a `str` value holding a line break and a whole forged `ELEVD:REQUEST ... admin=forged outcome=granted` line; BLOCKED when refused before any prompt (`EINVAL`, "control or formatting characters"), and the judge fails the run if a log line ever starts with the forged fields (review of #659) |
| `core_claim` | rhai: `elevd` `pkg.install` of `corereplace.lzp` (claims the core `os.lazy.counter`); BLOCKED when refused before any prompt (`EPERM`, "core app": only `pkg.update-core` replaces a core app) (review of #659) |
| `restart_elevd`, `restart_xuid` | rhai (`restart_guarded.rhai <name>`): `elevd` `service.restart` of a service outside `elevpolicy::RESTARTABLE`; BLOCKED when refused before any prompt (`EPERM`, "may not be restarted") (review of #659) |
| `fork_bomb` | up to 300 background tasks (SUCCEEDED above 150) |
| `disk_fill` | write 32 MiB into the home |

Not yet scripted: claiming `xuid`'s shell role from the session (no shipped
program a session can run subscribes to `xuid`; the rule is boot-tested by
`xuid`'s `XUID:SHELLCALLS` self-test). A client cannot inject keys at all (a
raw input source needs `CAP_INPUT_SOURCE`), so `prompt_keys` judges the other
half: the keys the person types go to the prompt and nowhere else. The
prompt scenarios are judged from their own part of the log (from their
`attack.sh` command to the next), and each request first waits out the hold
the previous scenario's cancel left (`elev_wait.rhai`).

## Expectations: how it passes today and gates later

`attack_judge.EXPECTATIONS` gives each scenario a state:

- `blocked`: must print `BLOCKED`; `SUCCEEDED` fails the run.
- `xfail`: known open, tracked by an issue; `SUCCEEDED` is expected and only
  noted. If it prints `BLOCKED` the judge notes `XPASS ... flip the expectation`
  (not a failure): change the entry to `blocked` and it is a gate.

U0 (#623), U1 (#624) and U2 (#625) landed: their rows are `blocked`
(`core_replace` since U2: a core app is replaced only through `elevd`).
`fork_bomb` and `disk_fill` stay `xfail` until U3 (quotas). Each row's
`touches` lists the image paths the attack changes when it succeeds; the audit
excuses only those, and only while the row is `xfail`. `side_effects` are
paths a scenario changes by allowed means whatever its state (installing a
package writes `/apps`, `/docs/apps` and `/conf`).

Always a failure, whatever the state: no marker, `ERROR`, `BLOCKED:ENOENT` (the
target was missing, so nothing was attacked), a scenario missing from the table.

## Files

- `run.py`: build, boots, verdict. `attack_judge.py`, `boot_judge.py`,
  `audit.py`: the judges. `test_judge.py`: their self-test.
- `assets/`: the guest scripts and their asset manifest.
