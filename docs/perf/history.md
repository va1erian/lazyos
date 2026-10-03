# Latency history

One row per labelled `python tools/perf/run.py --label ...` run. Microseconds.

| Label | Commit | Accel | irq_wake p50/p99/max | input_read p50/p99/max | input_present p50/p99/max | irqoff p99/max | ipc_rt p50/p99 |
|---|---|---|---|---|---|---|---|
| P0 baseline | `219f15fa+dirty` | whpx | 4549/11539/20278 (n=497) | 9951/20600/25503 (n=200) | 29906/58698/66589 (n=200) | 19.5/32550 (n=93182) | 2.2/2.8 (n=2000) |
| P1.1 reschedule on wake | `9cf0db77+dirty` | whpx | 4665/10132/22950 (n=476) | 9861/19702/24894 (n=200) | 30404/57773/62893 (n=200) | 22.4/29483 (n=93244) | 2.3/2.7 (n=2000) |
