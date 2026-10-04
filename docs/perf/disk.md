# Storage history

One row per labelled `python tools/perf/disk.py --label ...` run (docs/performance-plan.md P5). MB/s, milliseconds, microseconds.

| Label | Commit | Accel | write MB/s | read cold MB/s | read again MB/s | busybox read MB/s | exec ms | 400 small files ms | irqoff p99/max µs | worst syscall |
|---|---|---|---:|---:|---:|---:|---:|---:|---|---|
| P5.0 baseline | `bd0c5679+dirty` | whpx | 11.4 | 347.0 | 411.5 | 499.1 | 4.5 | 1155.0 | 912/1646315 | 0x1 |
| P5.1 read path | `69633947+dirty` | whpx | 7.9 | 420.3 | 453.6 | 668.2 | 3.6 | 994.0 | 4180/2810338 | 0x1 |
| P5.2 sleeping I/O | `9a850f73+dirty` | whpx | 24.0 | 612.2 | 631.5 | 657.4 | 3.6 | 363.0 | 144/3677 | 0xe7 |
| P5.3 serial + 1 MiB read pieces | `9712f9c6+dirty` | whpx | 27.2 | 1462.7 | 1789.9 | 781.6 | 1.6 | 371.0 | 340/2488 | 0x1 |
| P5 final | `8c56b2a3` | whpx | 26.8 | 1654.7 | 1749.7 | 672.3 | 1.4 | 356.0 | 104/824 | 0x3d |
