# TCP throughput (performance plan P0, P4)

Measured with `python tools/net/bulk.py` (`tools/net/README.md`, "Bulk
throughput"): a console image with the stack, a host server on
127.0.0.1:47810 reached through QEMU's user network as 10.0.2.2, and two
guest clients that send (`PUT`) and receive (`GET`) a deterministic stream,
checked byte for byte at the far end.

- **native**: `netbulk`, over `os.lazy.net.socket.v1` (one Messenger call per
  16 KiB `Send`/`Recv`).
- **linux**: `netbulk-linux`, a static musl `std::net` program over the
  kernel's `AF_INET` shim and `netd`'s pump.

MB/s is 10^6 bytes per second, the host server's figure (first payload byte to
the end of the stream for `PUT`, first byte sent to the guest's close for
`GET`). `connect` is the Linux client's own `Instant` around `connect()`
(TSC-interpolated `CLOCK_MONOTONIC`). Every run: WHPX, dev profile (the
`user` programs at `opt-level = "s"`, the kernel at 2), 1 GiB, one vCPU, on
a host shared with other QEMU runs, so compare rows of the same hour only.

## Results

Each step was run against the image of the step before it, alternately, in
the same hour (`bulk.py --no-build --image <copy>`); the host had other QEMU
guests running, which is why the same image moves between rows. Figures are
the host's MB/s per transfer.

### P0 baseline, quiet host (`85911762`, 32 MiB x 3)

| native PUT | native GET | linux PUT | linux GET | linux `connect` |
|---|---|---|---|---|
| 44.0, 46.7, 46.3 | 58.2, 66.1, 64.1 | 48.9, 51.0, 52.6 | 57.9, 58.4, 59.9 | 9.6-13.4 ms |

The plan's derived figure of 1.6 MB/s for the Linux path (one 16 KiB chunk
per 10 ms tick) was wrong under load: while bytes flow, every arriving frame
wakes `netd` through the driver's `Notify`, so its pump runs far more often
than its 1-tick timer. What the timer did cost is latency: `connect`, `bind`
and `listen` waited for the next 1-tick or 5-tick look at the request
queue.

### Step by step (32 MiB x 2, A then B then A then B)

| Step | Image | native PUT | native GET | linux PUT | linux GET | linux `connect` |
|---|---|---|---|---|---|---|
| P4.1 doorbell | P0 | 28-39 | 35-38 | 31-35 | 37-39 | 9.2-41.8 ms |
| | P4.1 | 31-49 | 38-40 | 34-35 | 38-40 | 0.73-1.5 ms |
| P4.2 drain in place | P4.1 | 34-51 | 41-47 | 37-39 | 40-45 | 0.51-3.8 ms |
| | P4.2 | 36-37 | 41-44 | 38-40 | 41-44 | 0.45-2.1 ms |
| P4.3 256 KiB windows | P4.2 | 31-34 | 35-40 | 35-37 | 36-43 | 0.62-2.4 ms |
| | P4.3 | 45-63 | 95-126 | 66-87 | 132-191 | 0.60-7.7 ms |
| P4.4 256 KiB syscalls (Linux only) | P4.3 | | | 70-74 | 150-158 | 0.58-9.9 ms |
| | P4.4 | | | 70-72 | 143-153 | 0.60-6.2 ms |
| P4.5 netdrv timer | P4.4 | 46-47 | 83-97 | 69-71 | 132-148 | 0.73-2.6 ms |
| | P4.5 | 43-45 | 96-102 | 70-76 | 149-151 | 0.56-6.0 ms |

`connect` ranges include the first connection of each program, which is
usually the slowest (1-10 ms; a likely cause, not measured, is that it is the
first to allocate and touch fresh 256 KiB buffers in `netd` and the kernel);
most later ones are 0.45-1.1 ms, with outliers up to 8 ms on a loaded host. The P4.5 pair is the last of three: the first two were disturbed
by host load on both images alike (40-100 ms header times). `nicctl` after
the P4.5 runs: about 189 000 frames received with no drop and about 6 200
interrupts on either image.

### Branch head, after P4.5 (32 MiB x 3, one run)

| native PUT | native GET | linux PUT | linux GET | linux `connect` |
|---|---|---|---|---|
| 62.8, 44.2, 42.2 | 123.9, 93.1, 77.8 | 81.5, 79.8, 75.5 | 177.8, 156.7, 119.3 | 0.60-5.5 ms |

`nicctl` afterwards: 283 167 frames received, none dropped, 10 036
interrupts. With `--pcap` (16 MiB, QEMU writing every frame to the capture,
which slows it) every stream reassembled from the capture equalled the
expected one.

What is not measured: where the remaining time goes (the guest has one vCPU;
`netd`, `netdrv`, the client and slirp share it with the host's other load),
KVM, and the release profile.

## Wi-Fi on real hardware (`wlan0`, `tools/net/lan_bulk.py`)

The figures above are QEMU's user network. A real box on the LAN is measured
with the same `netbulk` client and the same host server (`bulkpeers.py`, the
protocol unchanged), but a person types the command, because `dbgd` reads a
box and cannot start programs. `tools/net/lan_bulk.py` is everything around
that command:

1. It reads the box over `dbgd` (read-only; nothing is installed, restarted or
   reloaded): `system/net/interfaces` (link, metric, which interface is
   `primary`, the holder of the default route), `system/net/<if>/addr` and
   `system/net/<if>/wifi/state` (SSID, signal, frequency). It refuses to start
   unless the interface under test (`--iface`, `wlan0`) is up, has an address
   and holds the default route. A wired route (metric 100) beats Wi-Fi (600),
   so **for a Wi-Fi figure the Ethernet cable must be unplugged**; the tool
   says so and waits (`--wait-route`, 180 s) for the route to move.
2. It serves the bulk server on this host (`0.0.0.0:47810`; the address it
   prints is the one the OS routes towards the box, and a Wi-Fi host adapter
   or a Public network profile is called out). It changes no firewall rule or
   other setting: the first listen may show Windows' "allow Python" prompt,
   where *Private networks* must be allowed. If nothing connects within
   `--connect-timeout` the tool says so; a blocked port looks exactly like a
   command that was never typed.
3. It prints the line to type in the box's Terminal:
   `netbulk 192.168.1.100 47810 16777216 1 2 > /tmp/nb3fa9c1.out; cat /tmp/nb3fa9c1.out`
   (two rounds of a 16 MiB `PUT` and `GET`). The redirect is deliberate: a
   desktop Terminal reports only the **first** line of a command's output to
   the log `dbgd` reads (one `TERM:OUT:` per command), so the full output goes
   to a file under `/tmp` (named per run, so an old file is never mistaken for
   this one), which `dbgd`'s `fs.read` may open. Without the
   redirect only the first `PUT` would be cross-checked (`--no-out-file` types
   the bare command, for a console image whose log holds every line).
4. It collects both ends. The server's record gives bytes, timing and the
   **address the box connected from**, which must be the address of the
   interface under test: a run that left by the wrong card fails. The box's
   `NETBULK:native:<PUT|GET>:PASS bytes= ms= mbps=` lines, read back over
   `dbgd`, must agree on the verdict, the byte count and (within 25 % or
   50 ms) the time; a `GET` is only byte-checked by the box, so it counts only
   when the box reported it. The headline MB/s (10^6 bytes/s) is the host
   server's, the same figure as the tables above, and the median of the
   rounds goes in the row the tool prints.

```bash
python tools/net/lan_bulk.py --box 192.168.1.81 --label "AMD NUC" --link-note "legacy rates, 20 MHz"
python tools/net/lan_bulk.py --box 192.168.1.81 --iface eth0      # the cable plugged in, for the wired figure
python tools/net/lan_bulk.py --box 192.168.1.81 --bytes 33554432 --rounds 3 --json > nuc.json
python tools/net/lan_bulk.py --dry-run-qemu                       # no hardware: a QEMU desktop types the command
python tools/net/test_lan_bulk.py                                 # parsing and cross-check tests
```

`--key`/`--key-file` give the `dbgd` key (default `target/dbgd.key`). Exit
status: 0 verified, 1 the cross-check found a problem, 2 setup failed (box
unreachable, wrong route, cannot listen), 3 the box never connected.

`--dry-run-qemu` builds a desktop image with the stack and `dbgd` (a
throwaway key; `python tools/xui/build.py` first), boots it headless, and has
the session type the same command into the Terminal; everything else is the
code used on the LAN, with the guest reached through QEMU's user network
(`10.0.2.2`, `dbgd` forwarded to 127.0.0.1:19701). Its numbers say nothing
about a network: it proves the tool.

**Steps at the box**

1. On the host: `python tools/net/lan_bulk.py --box 192.168.1.81 --label "AMD NUC"`.
2. If the tool says `wlan0` does not hold the default route, unplug the
   Ethernet cable from the box; it continues by itself.
3. If Windows shows the Python firewall prompt, allow *Private networks*.
4. When the tool prints `ON THE BOX, in its Terminal, type`, type that line in
   the box's Terminal (open it from the LazyOS menu) and press Enter. The box
   shows the four result lines when it is done.
5. The tool prints the table, `LAN_BULK:PASS` or the problems, and a results
   row for the table below.

### Results

Link: from `system/net/wlan0/wifi/state` (band, channel from the frequency,
signal) plus a note on the rate, which the state does not carry. `PUT`/`GET`
are median MB/s of the rounds.

| Box | Interface | Link | PUT MB/s | GET MB/s | Date |
|---|---|---|---|---|---|
| | | | | | |
