# LazyAgent harnesses

`python tools/agent/run.py` builds a desktop image with LazyAgent
(`LAZYOS_AGENT=1`), a throwaway test CA and `api.fireworks.ai` /
`api.github.com` mapped to the host, serves a mock Fireworks over TLS on
127.0.0.1:443, drives the app in headless QEMU and judges the result
(docs/agent.md, docs/ai-agent-plan.md §10.1).

| File | What |
|------|------|
| `run.py` | build, serve, boot, drive, judge; exit status non-zero on any failure; `--live` uses the real service |
| `mockserver.py` | the mock: a streamed Markdown answer, a stall, a long reply, a title, a 401 for a wrong key; records every request |
| `certs.py` | the harness CA and the leaf for the two names (nothing is committed) |
| `session.py` | the guest session; `--write` regenerates `tools/screenshot/examples/agent.json` |
| `judge.py` | the verdicts: markers, no key in the log, the mock's record, screenshots |
| `test_judge.py` | proves each verdict fails when it should; `python tools/agent/test_judge.py` |
| `p0_guest.py` | the P0 spike: `agent_cli` streaming a real reply and a mid-record timeout probe from a console image |

```bash
python tools/agent/run.py                 # build, boot, judge
python tools/agent/run.py --no-build      # reuse target/lazyos.img and shots/agent/certs
python tools/agent/run.py --label-trace   # any LABEL:DENY fails (a package must have none)
FIREWORKS_API_KEY=... python tools/agent/run.py --live   # the real Fireworks (manual)
python tools/agent/test_judge.py
python tools/agent/session.py --check     # the checked-in example is current
```

Host port 443 must be free (the web harnesses bind it too). The key the
session types is random per run and reaches the guest only through
`{"type_secret": "AGENT_TEST_KEY"}`; the judge fails if it (or the wrong key
the session also types) appears in the serial log, a URL or a request body.
`--live` reads `FIREWORKS_API_KEY` from the environment, starts no mock and
never touches a repository. Logs, the capture and the screenshots go to
`shots/agent`.
