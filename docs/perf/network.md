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
