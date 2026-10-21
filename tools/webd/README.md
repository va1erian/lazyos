# webd harness (`tools/webd`)

The end-to-end proof of webd's light views (docs/webd-plan.md, P2 slice 2): a QEMU boot of a desktop
image with `webd` and the demo client, judged from the serial log, a host listener and screenshots.

```bash
python tools/webd/run.py                    # build (run_demo.py --webd), boot once, judge
python tools/webd/run.py --no-build         # the image of the last run
python tools/webd/run.py --skip-loss        # leave out the kill of the web process
python tools/webd/run.py --no-label-trace   # build without LAZYOS_LABEL_TRACE=1
python tools/webd/run.py --judge-only DIR   # judge a saved run
python tools/webd/test_judge.py             # the judge, the session and the listener fail when they should
python tools/webd/session.py --check        # tools/screenshot/examples/webd_light.json is current
```

Shared options with the sibling harnesses (`tools/webplus`): `--out` (default `shots/webd`), `--accel`,
`--qemu`, `--memory` (default the big-app profile's 4G), `--step-timeout`. Exit status is non-zero on any
failure.

## What it builds and boots

`run.py` calls `python tools/run_demo.py --webd --desktop --net --big-app --reset-os --build-only` with
`LAZYOS_XUI_AUTOSTART=term,webddemo`, `LAZYOS_UI_PROBE=1` and `LAZYOS_LABEL_TRACE=1` in the environment (the
caller's other `LAZYOS_*` switches are dropped, so the image is the same on every machine). Networking is on
so that a light view that did load its picture could reach the host. `webd` is a resident app the session
starts at login; the demo (`os.lazy.webddemo`, `xui-app/src/bin/webddemo.rs`, logic in
`xui-app/crates/webd-client/src/demo.rs`) shows one light view of
`xui_webd_client::page::demo_page`: a blue page with a heading, a link, a form and an
`<img src="http://10.0.2.2:18080/blocked.png">`.

## The one boot (`session.py`)

1. gates: `PKGD:PROVISION:DONE`, `TERM:UP:PASS`, `WEBD:UP:PASS`, `WEBDDEMO:UP:PASS`, the network;
2. `WEBDDEMO:VIEW:PASS id=<n>` and the first frame that is not the blank white page,
   `WEBDDEMO:FRAME:PASS colour=blue ...`; shot `w01_page`;
3. a click on the link (`UI:WIDGET name=link window=Web View Demo`, printed by the demo): `WEBDDEMO:LINK:<url>`,
   shot `w02_link`; a click in the field, typing, Enter (the submit is a link too), shot `w03_form`;
4. the loss (unless `--skip-loss`): the web process id from the engine's `PROCESS:LAUNCH:web:<pid>` line,
   Alt+Tab to the Terminal, `kill -9 <pid>`; `WEBDDEMO:LOST:crashed`, then a new view and
   `WEBDDEMO:RECREATE:PASS`; Alt+Tab back, shot `w04_recreated`;
5. Ctrl+Q quits the demo (a plain `q` is for the form); webd must then hold no view of it.

## What it judges (`judge.py`)

| Section | Passes when |
|---|---|
| `SESSION` | the boot reached its end (every gate seen) |
| `SERVICE` | `WEBD:UP:PASS` and `WEBD:SERVE:app.os.lazy.webd.light` |
| `VIEWS` | every `WEBDDEMO:VIEW:PASS id=<n>` has a `WEBD:VIEW:CREATE client=<demo's label> view=<n>`; ids are never reused |
| `PAGE` | the first `WEBDDEMO:FRAME:PASS` is blue and not tiny, `WEBDDEMO:LATENCY:<ms>` agrees with its `ms=` and is under a minute, `WEBDDEMO:STATE:Web View Demo` |
| `LINK` | `WEBDDEMO:LINK:https://example.com/clicked` after the click, no other link |
| `NETWORK` | the listener on 127.0.0.1:18080 saw **no connection and no request** |
| `SHOT` | at least half of the demo window in `w01_page` is the page's blue (the window's rectangle is `UI:RECT name=window:Web View Demo`) |
| `LOSS`, `RECREATED` | `WEBD:LOST view=<first>`, `WEBDDEMO:LOST:crashed`, a second, different view, `WEBD:VIEW:DESTROY view=<first>`, `WEBDDEMO:RECREATE:PASS`, and `w04_recreated` is blue again. With `--skip-loss`, any loss fails |
| `QUIT` | `WEBDDEMO:QUIT:PASS` and every `WEBD:VIEW:CREATE` has its `WEBD:VIEW:DESTROY` |
| `FAILURES` | no `WEBD:...:FAIL` or `WEBDDEMO:...:FAIL` line |
| `LABEL` | no `LABEL:DENY` for `app:os.lazy.webddemo`, `app:os.lazy.webd` or its children beyond `judge.OPTIONAL_CHILD` (the optional lookups LazyWeb+'s WebKit also makes; prune it with a run) |

`qemu_session.py` also aborts the run at once on a kernel `PANIC`/`EXCEPTION` or a `FAIL` marker of the demo
or webd.

## Notes for whoever runs it first

* Not run end to end where it was written: the program, package and `--webd` flag came from a parallel
  branch. The judge and the session are tested on canned logs (`test_judge.py`, and `run.py --judge-only` on a
  canned directory); the first real run may need the markers' exact spelling checked (`WEBD:VIEW:CREATE`,
  `WEBD:VIEW:DESTROY`, `WEBD:LOST`, `PROCESS:LAUNCH:web:<pid>`).
* The loss test needs `kill` from the Terminal to reach webd's web process and Alt+Tab to switch windows.
  If either does not work in the guest, the run stops at the `TERM:CMD:kill -9` or `WEBDDEMO:LOST:` gate; use
  `--skip-loss` and say so.
* The listener port is fixed by the demo page (`xui_webd_client::page::BLOCKED_IMAGE_URL`) and checked
  against it by `test_judge.py`. It takes none of the web harnesses' ports (80, 81, 443).
