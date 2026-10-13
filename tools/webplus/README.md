# LazyWeb+ harness (`tools/webplus`)

[`docs/lazywebplus.md`](../../docs/lazywebplus.md): LazyWeb's chrome on WebKit, verified like the rest of
the browsers: serial markers say when the guest is done, the verdict is what crossed the wire, what is on
the OS volume and what is on the screen.

```bash
python tools/webkit/deps.py && python tools/webkit/port.py && python tools/webkit/app.py --plus   # Docker
python tools/xui/build.py                                  # the other core apps, and the package
python tools/webplus/run.py --accel auto                   # build the image, boot it twice, judge
python tools/webplus/run.py --no-build                     # the image and certificates of the last run
python tools/webplus/run.py --label-trace                  # build with LAZYOS_LABEL_TRACE=1; a LABEL:DENY fails
python tools/webplus/run.py --judge-only shots/webplus     # judge a saved run again
python tools/webplus/test_judge.py                         # the judge fails when it should; sessions current
python tools/webplus/session.py --write                    # regenerate tools/screenshot/examples/lazywebplus*.json
```

| File | What |
|---|---|
| `run.py` | builds the image (`LAZYOS_LAZYWEBPLUS=1`, the UI probe, the big-app limits, a throwaway test CA), serves the host's stand-in sites (`tools/web/sites.py`), runs both boots, reads the OS volume with `osread`, judges |
| `session.py` | the two session scripts: the first boot browses, answers dialogs, clicks, downloads, closes; the second finds the history and the cookie and clears the history |
| `judge.py`, `pixels.py` | the verdict as pure functions: serial markers, the host's request log, the profile, the page area of the screenshots |
| `test_judge.py` | the judge's own tests |

The sites are LazyWeb's (`example.com`, `theoldnet.com` with its pictures and a download), WebKit's test pages
(`fixtures/theoldnet.com/webkit/`: script, form, a tall page for the wheel) and LazyWeb+'s
(`fixtures/theoldnet.com/webplus/`: a page whose script opens three dialogs and then tries to start another
program, run a command and open a window with no click; a page of links; two attachments, one slow). The
server also answers `/webplus/cookie` with a `Set-Cookie` and a title that says which cookie came.

Needs ports 80, 443 and 81 on 127.0.0.1 (any user on Windows; root, or on Linux `sudo sysctl
net.ipv4.ip_unprivileged_port_start=80`) and Docker for the build. Every run recreates `target/lazyos.img`'s OS
volume. Output is in `shots/webplus` (`first/` and `second/`: `serial.log`, `shot_*.png`, `requests.json`,
`net.pcap`; `profile.json`).

Markers (`WEBPLUS:` is a channel, `libs/tracefmt`, `docs/trace/channels.md`): the window prints `UP:PASS`,
`NAV:<url>`, `LOAD:<url>`, `TITLE:<title>`, `LAUNCH:<url>:OK|FAIL|BLOCKED`, `POPUP:<url>`, `DIALOG:<kind>`,
`DOWNLOAD:START|DONE|FAIL`, `FAIL`, `STOPPED`; the UI process `UP`, `WEBPROCESS:UP:<pid>`,
`NETWORKPROCESS:UP:<pid>`, `INFO:TIME:...`, `INFO:RESIZE:<w>x<h>`, `CHILD:<name>:<pid>:GONE`, `QUIT:PASS`. The
ones that name a page print only in a `LAZYOS_UI_PROBE=1` image.

The sandbox of the two engine processes is attacked by `python tools/webkit/attack_run.py --browser --app
lazywebplus` (and `--kernel`), see [`docs/lazywebplus.md`](../../docs/lazywebplus.md). `--label-trace` here fails
on any `LABEL:DENY` beyond the nine optional ones of #836 (`denies.py`).
