# Storage history

One row per labelled `python tools/perf/disk.py --label ...` run (docs/performance-plan.md P5). MB/s, milliseconds, microseconds.

| Label | Commit | Accel | write MB/s | read cold MB/s | read again MB/s | busybox read MB/s | exec ms | 400 small files ms | irqoff p99/max µs | worst syscall |
|---|---|---|---:|---:|---:|---:|---:|---:|---|---|
| P5.0 baseline | `bd0c5679+dirty` | whpx | 11.4 | 347.0 | 411.5 | 499.1 | 4.5 | 1155.0 | 912/1646315 | 0x1 |
