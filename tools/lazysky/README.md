# LazySky QEMU harness

`python tools/lazysky/run.py` builds a desktop image with LazySky
(`LAZYOS_LAZYSKY=1`), a throwaway test CA and `bsky.test` /
`cdn.bsky.test` mapped to the host, serves a mock Bluesky over TLS on
127.0.0.1:8443, drives the app in headless QEMU and judges the result
(docs/lazysky.md).

| File | What |
|------|------|
| `run.py` | build, serve, boot, drive, judge; exit status non-zero on any failure |
| `mockserver.py` | the mock: sign-in, 400 generated posts in pages of 30, pictures, threads; records every request (Host, SNI, headers, body) |
| `certs.py` | the harness CA and the leaf for the two names (nothing is committed) |
| `session.py` | the guest session; `--write` regenerates `tools/screenshot/examples/lazysky.json` |
| `judge.py` | the verdicts: markers, secrets, R7 frame times, the mock's record, screenshots |
| `test_judge.py` | proves each verdict fails when it should; `python tools/lazysky/test_judge.py` |

```bash
python tools/lazysky/run.py                 # build, boot, judge
python tools/lazysky/run.py --no-build      # reuse target/lazyos.img and shots/lazysky/certs
python tools/lazysky/run.py --label-trace   # also list LABEL:DENY (a package must have none)
python tools/lazysky/test_judge.py
python tools/lazysky/session.py --check     # the checked-in example is current
```

The app password is random per run and reaches the guest only through
`{"type_secret": "LAZYSKY_PASSWORD"}`; the judge fails if it (or either
token) appears in the serial log. `init` can hand an app one absolute path
only, so the session writes `settings.toml` (`service = "https://bsky.test:8443"`)
from the Terminal before launching the app.

Logs, the capture and the screenshots go to `shots/lazysky`.
