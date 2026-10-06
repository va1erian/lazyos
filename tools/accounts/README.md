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
`shots/accounts/` (session scripts, serial logs, `audit_*.txt`).

## What it does

All boots use a **copy** of `target/lazyos.img` (`shots/accounts/work/`).

1. `warm`: first boot (core packages install), clean power-off. The volume is
   read from the host (`osread tree /`) as the audit baseline.
2. `attack`: runs every scenario once in the desktop Terminal, then powers
   off. The stop is judged (`INIT:SHUTDOWN:*`, `power: filesystems synced`).
3. `verify`: boots again and must run a command (`TERM:OUT:ACCT:BOOT:OK`),
   then the session ends with the machine running: a hard kill.
4. `verify-kill`: boots after the hard kill and must answer again.

The volume is audited after step 2 and after step 4 against the baseline
(`libs/ext2fs/examples/osread.rs tree`: mode, owner, size, mtime, FNV-1a
content hash). Nothing may change outside `audit.ALLOWED` (`/home/user`, the
journals in `/logs`, `/lost+found`, scratch), except the paths an *open*
attack is declared to touch.

## Scenarios

`assets/accounts/attack.sh <name>` (and two rhai scripts) ship in the image
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
| `confd_sys` | rhai `sys::confd::set("sys/...")` |
| `keyd_provision` | rhai `sys::keyd::provision(...)` |
| `read_home_admin` | `ls /home/admin` |
| `signal_service` | `kill -CONT` the `logd` service |
| `fork_bomb` | up to 300 background tasks (SUCCEEDED above 150) |
| `disk_fill` | write 32 MiB into the home |

Not yet scripted (needs a build switch or a test package; the issue lists
them): claim xuid's shell role, install a package with `autostart` and check
the next boot, install a higher-versioned core app, plus the U1/U2 sets and
`LAZYOS_AUTOLOGIN`, which does not exist before U0.

## Expectations: how it passes today and gates later

`attack_judge.EXPECTATIONS` gives each scenario a state:

- `blocked`: must print `BLOCKED`; `SUCCEEDED` fails the run.
- `xfail`: known open, tracked by an issue; `SUCCEEDED` is expected and only
  noted. If it prints `BLOCKED` the judge notes `XPASS ... flip the expectation`
  (not a failure): change the entry to `blocked` and it is a gate.

The desktop runs as root until U0 (#623) lands, so all entries are `xfail` and
the harness passes while every attack succeeds. When U0 lands, flip the U0
rows; when quotas land (U3) flip `fork_bomb` and `disk_fill`. Each row's
`touches` lists the image paths the attack changes when it succeeds; the audit
excuses only those, and only while the row is `xfail`.

Always a failure, whatever the state: no marker, `ERROR`, `BLOCKED:ENOENT` (the
target was missing, so nothing was attacked), a scenario missing from the table.

## Files

- `run.py`: build, boots, verdict. `attack_judge.py`, `boot_judge.py`,
  `audit.py`: the judges. `test_judge.py`: their self-test.
- `assets/`: the guest scripts and their asset manifest.
