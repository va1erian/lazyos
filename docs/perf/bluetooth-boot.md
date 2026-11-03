# Bluetooth boot latency (B6)

[bluetooth-plan.md](../bluetooth-plan.md) decision 5 and phase B6: a bonded
keyboard or mouse must work as early in the boot as possible. Measured with
`python tools/bt/run.py --boot-typing` (BR/EDR) and `--le-boot-typing` (LE):
pair once, power off, boot the same image with the peer pressing keys from the
moment the controller is up. Every step of the path prints its own
milliseconds since the kernel started (`INIT:SPAN`, `USBD:SPAN`, `BT:SPAN`,
`BT:READY`, `BT:RECONNECT link_at= report_at=`; `tools/bt/spans.py` puts them
in order). Host: Windows 11, QEMU with WHPX, other builds running; boot 2 of
the harness image (boot evidence profile, no desktop).

## Where the time went

| Step | Before (run 1) | Before (run 2) | After (typical) |
|---|---:|---:|---:|
| `init` spawns `usbd` | 396 | 1009 | 85 |
| `usbd` controller up | 415 | 1034 | 102 |
| Bluetooth function bound | 563 | 1183 | 263 |
| `os.lazy.usb.bt` published | 565 | 1185 | 265 |
| `init` spawns `btd` | 89 | 87 | 359 |
| `btd` attached to the transport | 1090 | 2080 | 362 |
| bonds loaded | 1091 | 2081 | 383 |
| `HCI_Reset` done | 1102 | 2087 | 400 |
| fast path armed (`BT:READY`) | 1195 | 2180 | 501 |
| first link up | 1257 | 2243 | 515 |
| first report published | 1324 | 2307 | 623 |
| **radio ready after `usbd` started** | 798 | 1167 | 416 |
| **first report after the link** | 67 | 64 | 108 |

Before, `usbd` waited for `inputd` (behind `confd` and `messengerd`) and `btd`
looked for the registry name once a second, so attaching cost 1 to 2 s on
whichever second the look missed (`btd` running at 89 ms, attached at 1090 and
2080). After: `usbd` starts right after `inputd` (B6 first dropped that
dependency; the review put it back so early wired keys have a reader), serves the name the pass after
the bind and tells `init`, which starts `btd` once `inputd` serves too; `btd`
attaches 2 ms after it starts. What is left on the path is the USB reset and
debounce (about 160 ms from controller to bound, the specification's
minimums), `init` getting to `btd` (the `inputd` wait: 94 ms here, longer when
`init` is busy with the boot self-tests) and the controller's own commands
(`Reset` to armed, about 100 ms under WHPX, 155 ms for LE).

Repeated runs after (radio ready after `usbd` started / first report after the
link, ms): BR/EDR 623/93, 482/77, 416/108, 420/73; LE 1706/82, 479/75, 550/98,
478/79. The 1706 is `init` taking 1.3 s to reach `btd` (it was busy with the
boot self-tests while other builds loaded the host); the budgets below are the
plan's, with the report budget tightened.

## Budgets (`tools/bt/spans.py`)

| | Budget | Measured |
|---|---:|---:|
| radio ready after `usbd` started | 1500 ms | 416 to 623 (one 1706 under load) |
| first report published after the link | 250 ms | 73 to 108 |

They apply to a hardware-accelerated guest (WHPX/KVM); under `--accel none`
the table is printed and only the order is checked.

## The rest of the boot

`python tools/perf/run.py` (default and `--usb`), before (`4807595cf`) and
after (`60d6e4c81`), rows `B6 ...` in [history.md](history.md): the scheduling
and input-latency columns agree within the run-to-run spread (irq_wake p50
81 to 103 us, input_present p99 144 to 168 us, `--usb` input_present p99
380 to 438 us). Of the four "after" runs, one default run showed an `irqoff`
max of 113 ms and one `--usb` run an `input_read` max of 161 ms; each did not
repeat on its rerun, the default image carries no `usbd`, and the two "before"
runs had none (two samples cannot rule host noise out: IRQOFF numbers inflate
on a loaded host, AGENTS.md).

## Keys before a reader exists

The raw bus keeps a ring per reader, so a key published before `inputd`
attaches goes nowhere, and `inputd` queues events for a focused session only:
a key typed at the greeter before its window has focus is dropped, exactly like
a PS/2 key (a screenshot with the peer typing `go` after the name field took
focus shows `go`; the `lazy` typed while the machine booted is not there).
That is why `btd` waits for `inputd`: nothing it publishes earlier could be
read. Sources registered before `inputd` attaches are not lost, `inputd` reads
the kernel's table of them. A boot where `init` is blocked (the desktop image
after an update spent 8 s in one `unlink` holding the VFS lock; `FS:SLOW
op=unlink`, which also parked `usbd`'s dump write for 5.5 s) delays all of
this and is not a Bluetooth problem.
