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

| Step | Bytes x rounds | native PUT | native GET | linux PUT | linux GET | linux `connect` |
|---|---|---|---|---|---|---|
| P0 baseline (`3c10730a`) | 32 MiB x 3 | 44.0 / 46.7 / 46.3 | 58.2 / 66.1 / 64.1 | 48.9 / 51.0 / 52.6 | 57.9 / 58.4 / 59.9 | 13.1 / 9.6 / 13.1 / 13.1 / 13.4 / 13.0 ms |

The plan's derived figure of 1.6 MB/s for the Linux path (one 16 KiB chunk
per 10 ms tick) was wrong under load: while bytes flow, every arriving frame
wakes `netd` through the driver's `Notify`, so its pump runs far more often
than its 1-tick timer. What the timer does cost is latency: `connect`, `bind`
and `listen` wait for the next 5-tick (50 ms) or 1-tick look at the request
queue, which is the 10-13 ms above.
