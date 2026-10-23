# LazyWeb+ harness (`tools/webplus`)

[`docs/lazywebplus.md`](../../docs/lazywebplus.md): the browser on WebKit, verified like the rest of
the apps: serial markers say when the guest is done, the verdict is what crossed the wire, what is on
the OS volume and what is on the screen.

LazyWeb+ is a plain xui app (`xui-lazywebplus.elf`) and a client of the resident daemon **webd**
(`os.lazy.webd`, `xui-webd.elf`), which owns WebKit and its two processes. The harnesses therefore read two
sets of markers from one serial log: the window's `WEBPLUS:*` and the engine's, which webd relays as
`WEBD:NOTE:<line>` (`judge.engine_lines` is the one reader; `WEBD:VIEW:CREATE|DESTROY` and `WEBD:LOST` are
webd's own). webd outlives the window: its network process stays after LazyWeb+ quits, the page's process
ends with the view.

```bash
python tools/webkit/deps.py && python tools/webkit/port.py --only WebProcess NetworkProcess webd-app   # Docker
python tools/webkit/webd.py                                # webd's program (target/xui/xui-webd.elf)
python tools/xui/build.py                                  # the client (xui-lazywebplus.elf), the other core apps, the packages
python tools/webplus/run.py --accel auto                   # build the image, boot it twice, judge
python tools/webplus/run.py --no-build                     # the image and certificates of the last run
python tools/webplus/run.py --label-trace                  # build with LAZYOS_LABEL_TRACE=1; a LABEL:DENY fails
python tools/webplus/run.py --judge-only shots/webplus     # judge a saved run again
python tools/webplus/test_judge.py                         # the judge fails when it should; sessions current
python tools/webplus/session.py --write                    # regenerate tools/screenshot/examples/lazywebplus*.json
python tools/webplus/notify_run.py                         # web notifications: build, boot twice, judge (shots/webplus-notify)
python tools/webplus/test_notify_judge.py                  # its judge fails when it should; its sessions are current
python tools/webplus/notify_session.py --write             # regenerate tools/screenshot/examples/lazywebplus_notify*.json
python tools/webplus/hang_run.py                           # a page that hangs: the bar, Wait, Reload, Stop; build, boot, judge (shots/webplus-hang)
python tools/webplus/test_hang_judge.py                    # its judge fails when it should; its session is current
python tools/webplus/hang_session.py --write               # regenerate tools/screenshot/examples/lazywebplus_hang.json
python tools/webplus/live_run.py --build --home-disk target/home.img --url https://www.theverge.com/ --url https://www.cnn.com/ --url https://www.nasa.gov/   # real sites, a /home disk: does the browser hang or abort? (verdict from PERF:CPU, LOCK:WAIT, WEBPROCESS:UNRESPONSIVE and a browser program ending by a crash signal in PROC:EXIT); `ENOSYS 285` is a finding (the HTTP cache then writes every body inline, issue #864) and any other `ENOSYS` a note; `FS:SLOW op=link|unlink device=<dev> us=<n>` lines in the serial log name a name operation that held the volume 20 ms or more
python tools/webplus/test_live.py                           # its verdict, on the lines of a real hang
python tools/webplus/relaunch_run.py --build                 # close and start the browser from the menu six times (real sites first): every start page loads, no refused sendmsg, no NetworkProcess killed (issue #865)
python tools/webplus/test_relaunch.py                        # its verdict, on the lines of the start-up hang
python tools/webplus/frame_run.py [--perf]                    # where a scroll frame goes at 2560x1440: CPU by process, paint stages, the kernel's present_mpx
python tools/webplus/frame_run.py --perf --sample-port 4499 --sample-stacks   # also sample the CPU with user stacks (shots/webplus-frame/samples.tsv)
python tools/webplus/cpu_profile.py samples.tsv --program webd-app --callers memcpy   # who calls a hot leaf in webd's UI process (the build volume names it)
python tools/webplus/scroll_bench.py --build                 # live ars technica and webkit.org in a maximized window at 2560x1440: frames a second while scrolling, longest gap, checkerboard (INFO:SMOOTH), the page's answer; --url to pick pages
python tools/webplus/scroll_stats.py shots/webplus-live/serial.log [more]   # one comparable block per scrolling run: page answer (w2d), paints/s, web CPU and guard demotions, the main thread's runnable wait (LAZYOS_PERF=1)
python tools/webplus/test_frame.py                           # its reader and session; the scroll page is current
python tools/webplus/scroll_page.py --write                  # regenerate fixtures/theoldnet.com/webplus/scroll.html
python tools/webplus/sample.py record --port 5555 --out s.tsv   # where the guest's CPU goes: CR3 + RIP at 30 Hz over a second QMP port
python tools/webplus/sample.py report s.tsv [--cr3 HEX --elf PROGRAM]   # by process, then by function (needs binutils' addr2line)
python tools/webplus/test_sample.py                        # its parser and summary
python tools/webplus/pointer_bench.py [--perf]             # a heavy live page under a 200 Hz pointer sweep: webd's paint meter, CPU of the web process, webd and the client (the pointer is always merged now)
python tools/webplus/test_pointer.py && python tools/webplus/test_procs.py   # their readers
```

| File | What |
|---|---|
| `run.py` | builds the image (`LAZYOS_LAZYWEBPLUS=1`, which brings webd; the UI probe, the big-app limits, a throwaway test CA), serves the host's stand-in sites (`tools/webtest/sites.py`), runs both boots, reads the OS volume with `osread`, judges |
| `session.py` | the two session scripts: the first boot browses, answers dialogs, clicks, downloads, closes; the second finds the history and the cookie and clears the history |
| `judge.py`, `pixels.py` | the verdict as pure functions: serial markers (the client's and, through `engine_lines`, the engine's), the host's request log, the profile (the history on LazyWeb+'s folder, the cookie jar in webd's `browser` profile `~/.apps/os.lazy.webd/profiles/browser`), the HTTP cache's `Blobs/` after the first boot and after the last (the cache keeps bodies larger than a page as hard-linked blobs, issue #864: `judge_cache` also fails on an `ENOSYS 285` in any boot), the page area of the screenshots |
| `procs.py` | the browser's four processes by kernel `comm=` (`lazywebplus.elf`, `webd.elf`, `webprocess`, `networkprocess`) and by the engine's own `WEBPROCESS:UP:<pid>`, for the benches |
| `reaper_judge.py` | no longer used (the launcher's reaper threads are webd's now); stays until CI stops running its test |
| `test_judge.py` | the judge's own tests |
| `sample.py`, `test_sample.py` | a sampling profiler for a running guest, from the host: `info registers` over a second QMP port (`--extra-arg=-qmp --extra-arg=tcp:127.0.0.1:5555,server=on,wait=off` on any session script), kept as CPL, CR3 (the process) and RIP; `report` shows time by address space and the busiest addresses, and names functions against an unstripped program |
| `hang_run.py`, `hang_session.py`, `hang_judge.py`, `test_hang_judge.py` | the hung-page harness (issue #862): `fixtures/theoldnet.com/webplus/hang.html` runs a loop that never ends; one boot checks that WebKit's unanswered ping becomes `WEBPROCESS:UNRESPONSIVE` and the window's bar (`HANG:SHOW`), that Wait puts it away and ends nothing, that the toolbar's Reload ends the stuck process and loads the page in a new one, that the bar's Stop ends a second one (webd reports the view lost, `WEBD:LOST view=<id> reason=other`) and the next load makes a new view and a new page process, and that every view webd made and every page process it launched is accounted for |
| `notify_run.py`, `notify_session.py`, `notify_judge.py`, `test_notify_judge.py` | the web notifications harness: the page `fixtures/theoldnet.com/webplus/notify.html` over two origins in two boots (no bar without a gesture, Allow kept, the toast titled with the site and updated in place, a burst cut, a click on a live and on a gone page, Block, Forget), judged from the markers, the shell's `SHELL:NOTIFY:POST` lines, the page's titles and `notify.tsv` on the OS volume |

The sites are the shared stand-ins of `tools/webtest` (`example.com`, `theoldnet.com` with its pictures and a download), WebKit's test pages
(`fixtures/theoldnet.com/webkit/`: script, form, a tall page for the wheel) and LazyWeb+'s
(`fixtures/theoldnet.com/webplus/`: a page whose script opens three dialogs and then tries to start another
program, run a command and open a window with no click; a page of links; two attachments, one slow). The
server also answers `/webplus/cookie` with a `Set-Cookie` and a title that says which cookie came.

Needs ports 80, 443 and 81 on 127.0.0.1 (any user on Windows; root, or on Linux `sudo sysctl
net.ipv4.ip_unprivileged_port_start=80`) and Docker for the build. Every run recreates `target/lazyos.img`'s OS
volume. Output is in `shots/webplus` (`first/` and `second/`: `serial.log`, `shot_*.png`, `requests.json`,
`net.pcap`; `profile.json`).

Markers (`WEBPLUS:` is a channel, `libs/tracefmt`, `docs/trace/channels.md`): the client prints `UP:PASS` (its
first frame), `NAV:<url>`, `LOAD:<url>`, `TITLE:<title>`, `LAUNCH:<url>:OK|FAIL|BLOCKED`, `POPUP:<url>`,
`DIALOG:<kind>`, `DOWNLOAD:START|DONE|FAIL`, `FAIL`, `STOPPED`, `HANG:*`, `NOTIFY:*`, `BLOCK:*` (the list's life),
`INFO:RESIZE:<w>x<h>` (a whole picture of a new size arrived), `INFO:WINDOW|SPEC|SMOOTH`, and `QUIT:PASS` (after
its window closed, exit status 0 and its `curl` child gone; `CHILD:curl:ALIVE` otherwise). The ones that name a
page print only in a `LAZYOS_UI_PROBE=1` image. The engine's markers are webd's `WEBD:NOTE:<line>`:
`WEBPROCESS:UP:<pid>`, `NETWORKPROCESS:UP:<pid>`, `PROCESS:LAUNCH|EXIT|TERMINATE|KEPT:...`,
`WEBPROCESS:UNRESPONSIVE|RESPONSIVE|RECOVER|GONE:...`, `FAIL:...`, `INFO:PAINT|TIME|QUOTA|MEM:...`,
`BLOCK:COUNT:<n>` (the `INFO:*` and `WEBPROCESS:*` ones under webd's budget of 120 per 10 s); webd adds
`WEBD:UP:PASS`, `WEBD:VIEW:CREATE client=<label> view=<id>`, `WEBD:VIEW:DESTROY view=<id>` and
`WEBD:LOST view=<id> reason=<refused|crashed|memory|other|unknown>`. Gone with the in-process engine: the bare
`UP`, `HEARTBEAT:UP`, `INFO:MAINLAG`, `STALL:main`, `INFO:CPU`, `INFO:REAPER` and `INFO:POINTER`.

The sandbox of the two engine processes is attacked by `python tools/webkit/attack_run.py --browser --app
lazywebplus` (and `--kernel`), see [`docs/lazywebplus.md`](../../docs/lazywebplus.md). `--label-trace` here fails
on any `LABEL:DENY` beyond the nine optional ones of #836 (`denies.py`), which are now webd's children's
(`app:os.lazy.webd/webprocess` and `/networkprocess`); no denial is allowed for `app:os.lazy.lazywebplus`, its
`listupdate` helper or `app:os.lazy.webd`.

## How to capture a freeze

A freeze report is a file and a recording, not a terminal copy (issue #861; the analysis of the first one is
issue #865).

1. Start the desktop with the serial port in a file and a second QMP socket for the sampler (the GUI launcher's
   Advanced tab has a "Sample port" field for the second):

   ```bash
   python tools/run_demo.py --lazywebplus --sample-port 5555
   ```

   The serial output goes to `target/serial-<image>-<date>-<time>-<pid>.log` (the newest ten are kept; `--serial-log FILE`
   names one yourself) and the launcher tails it to its console from a thread, so a console that stops reading (a
   Windows selection, the GUI's reader) cannot stall the guest. The monitor stays on the console (`Ctrl-A x` quits;
   Ctrl-C is not a signal to QEMU, as with `mon:stdio`). `--serial-stdio` is the old single muxed device.
2. While the page is stuck, record where the guest's CPU goes:

   ```bash
   python tools/webplus/sample.py record --port 5555 --out s.tsv     # Ctrl-C after ten seconds or so
   python tools/webplus/sample.py report s.tsv                        # by process (CR3), then by address
   ```

   A CR3 names the process; a kernel RIP (`0xffff8000...`) says it is inside a syscall (symbolize with
   `addr2line -e <kernel> <rip - 0xffff800000000000>`).
3. Read the log for what the kernel itself says (all channels in `docs/trace/channels.md`):

   | Line | Meaning |
   |---|---|
   | `PROC:EXIT tgid= thread= status= sig= by= parent= cpu_ms= comm=` | a process (thread group) ended: its exit status or the signal that killed it (`status=-` then), the process that sent it (`by=kernel` for a fault), the CPU of all its threads. Clean exits are budgeted apart from the rest |
   | `SCHED:STUCK tgid= thread= abi= nr= ms= cpu_ms= krip= urip= comm=` | a task runnable inside the kernel (a syscall, or a kernel frame) for 2 s or more, `ms` counted from the syscall's entry; `cpu_ms` near `ms` is a spin, near 0 is starvation; `urip` is where it came from in user code |
   | `SCHED:GUARD:DEMOTED` / `SCHED:GUARD:SUMMARY` | the first demotion of a task, then a summary per process every 10 s |
   | `PERF:CPU tgid= pct= user= kernel= class= demotions= comm=` | every 5 s while a process is above 20% (threads that ended still count) |
   | `LOCK:WAIT lock= holder= thread= nr= held_in= waiters= ms=` | a task waited over 500 ms for a contended lock (`vfs`, `ext2`, `ata`, `ahci`, `nvme`), who held it and the syscall the holder was in (`held_in=linux:<nr>`, `native:<nr>` or `-`) |
   | `SERIAL:BACKLOG bytes= dropped= max_io_us=` | the serial ring holds over 4 KiB (`max_io_us`: the longest port access in the last 5 s; large means the host is slow to take bytes) |
   | `SERIAL:DROPPED bytes= total=` | output the ring had no room for (whole lines; the ring never makes a writer wait). The harness judges fail a log that has one |
   | `PROC:SCM:REFUSE why= tgid= count= comm=` | the kernel refused an `SCM_RIGHTS` send with `ETOOMANYREFS` (`why=quota`: the in-flight budget, `cycle`: it would close a loop of sockets, `epoll`: an epoll instance never travels, `limit`: the walk met too many sockets; 20 lines per 10 s, then `PROC:SCM:SUPPRESSED n=`). WebKit then logs `Error sending IPC message: No error information` and drops that one message: a reply lost this way leaves a page waiting for good |
   | `ENOSYS <nr> <name>` then `ENOSYS ... count=` | an unimplemented syscall, once per process, then a count every 10 s |

   `IRQOFF:MAX` lines name interrupts-off stretches; none should end in `kernel/src/serial/tx.rs` any more.
