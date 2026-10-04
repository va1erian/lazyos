# Latency harness

Stage P0 of [`docs/performance-plan.md`](../../docs/performance-plan.md):
one command builds an instrumented desktop, boots it headless, drives it and
writes [`docs/perf/report.md`](../../docs/perf/report.md).

```bash
python tools/perf/run.py                    # build, boot, measure, report
python tools/perf/run.py --no-build         # re-measure the current image
python tools/perf/run.py --label "P1.1"     # also append a row to docs/perf/history.md
python tools/perf/run.py --accel none       # TCG: informative only
```

The image is built with `LAZYOS_DESKTOP=1 LAZYOS_NET=1 LAZYOS_PERF=1`.
`LAZYOS_PERF=1` sets the kernel cfg `lazyos_perf`, which compiles the hooks in
`kernel/src/perf/`; without it they are empty inline functions. The kernel
prints a `PERF:<metric>:n=.. p50_us=.. p90_us=.. p99_us=.. max_us=.. mean_us=..`
line every 2 s for each metric that changed, and `PERF:irqoff_worst` with the
syscall number of the longest interrupts-off stretch.

| Metric | Start | End | Load |
|---|---|---|---|
| `irq_wake` | device or PS/2 IRQ top half | the task the interrupt woke is scheduled | the host knocks on a port forwarded to the guest's virtio-net card every 50 ms; `netdrv` is the claimant |
| `input_read` | raw input record published (PS/2 IRQ) | `inputd`'s raw-bus poll returns it | 200 single-packet PS/2 mouse moves over QMP, 60 ms apart |
| `input_present` | the same pointer record | the compositor's next `present` returns | the same moves |
| `irqoff` | a syscall enters, or resumes inside itself | it returns, parks or naps | everything the boot and the run do |
| `ipc_rt` | in-kernel `begin_call` | `await_reply` returns (no context switch) | 2000 echoes, once, 15 s after boot |
| `sleep_1ms` | the kernel task asks for a 1 ms sleep | the sleep returns | 200 sleeps, once, 17 s after boot |
| `present` | the display owner's `present` syscall starts | it returns, breaths between chunks included (P3.2) | every frame the compositor shows during the run |
| `sched` | a scheduler entry (tick, park, yield) | it returns the stack to resume | everything the boot and the run do |
| `wake_run` | any task wakes another (not only an interrupt) | the woken task is put on the CPU | everything the boot and the run do |
| `msg_rt` | `msgbench` enters a `Ping` call (user `rdtsc`) | the call returns with the server's reply | 20000 round trips between two user processes, once, 19 s after boot |

`msgbench` (`user/src/bin/msgbench.rs`) also prints `msg_tput`: one-way
messages per second (bursts of 32 sends closed by a call) and synchronous
calls per second. The kernel prints `PERF:ctxsw` (running totals of context
switches and scheduler entries) with every report; the run ends with an 8 s
quiet window (no input, no network traffic) over which the idle desktop's
rate is taken (`idle_ctxsw`, the P7 exit metric).

`input_present` takes the first `present` after `inputd` read the record as
the one that moved the cursor; an unrelated repaint in between would shorten
the sample, never lengthen it.

Not measured yet: USB input to present (the start stamp belongs at the xHCI
transfer event, which the kernel cannot see while `usbd` polls the
controller), disk read and exec time. TCP throughput has a harness of its
own, `python tools/net/bulk.py` (`tools/net/README.md`, results in
[`docs/perf/network.md`](../../docs/perf/network.md)), because its verdict is
what crossed the wire, not a kernel histogram.
