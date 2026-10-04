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

`input_present` takes the first `present` after `inputd` read the record as
the one that moved the cursor; an unrelated repaint in between would shorten
the sample, never lengthen it.

Not measured yet: USB input to present (the start stamp belongs at the xHCI
transfer event, which the kernel cannot see while `usbd` polls the
controller), TCP throughput, disk read and exec time.
