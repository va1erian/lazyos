# Latency history

One row per labelled `python tools/perf/run.py --label ...` run. Microseconds.

| Label | Commit | Accel | irq_wake p50/p99/max | input_read p50/p99/max | input_present p50/p99/max | irqoff p99/max | ipc_rt p50/p99 | sleep_1ms p50/p99/max |
|---|---|---|---|---|---|---|---|---|
| P0 baseline | `219f15fa+dirty` | whpx | 4549/11539/20278 (n=497) | 9951/20600/25503 (n=200) | 29906/58698/66589 (n=200) | 19.5/32550 (n=93182) | 2.2/2.8 (n=2000) | - |
| P1.1 reschedule on wake | `9cf0db77+dirty` | whpx | 4665/10132/22950 (n=476) | 9861/19702/24894 (n=200) | 30404/57773/62893 (n=200) | 22.4/29483 (n=93244) | 2.3/2.7 (n=2000) | - |
| P1.2 prompt device IRQs | `fd8fa6df+dirty` | whpx | 81.3/707/25562 (n=437) | 9344/31669/31947 (n=200) | 40188/83419/87888 (n=200) | 13.5/29100 (n=66585) | 4.8/5.2 (n=2000) | - |
| P1.3 input doorbell | `6aa9a647+dirty` | whpx | 88.3/354/19314 (n=506) | 246/638/11140 (n=200) | 20244/58291/63436 (n=200) | 21.6/28676 (n=59576) | 4.4/5.0 (n=2000) | - |
| P1.4 compositor wait set | `c58c94ca+dirty` | whpx | 108/1718/12042 (n=515) | 334/1514/2191 (n=200) | 472/1893/2791 (n=200) | 31.0/29891 (n=51978) | 5.1/5.5 (n=2000) | - |
| P1.5 exit hands CPU on (all of P1) | `8fa51ec1+dirty` | whpx | 145/7086/19753 (n=432) | 134/393/516 (n=200) | 280/545/661 (n=200) | 9.3/30883 (n=3604365) | 2.3/5.2 (n=2000) | - |
