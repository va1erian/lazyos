# Web Attack Probe

The malicious-client half of webd's attack harness (`tools/webd/attack_run.py`,
docs/webd-plan.md P5). It is a real client of webd whose manifest lists **only**
`os.lazy.webd.view.v1` — not the `net` or `browser` scope — so a `CreateView` it sends through a
scope it was not granted is refused by the kernel before webd sees it. That gate cannot be
reproduced by the host fabric (which delivers any interface id), so it is proven only in a guest
run.

A short time after its window is up the probe runs `xui_webd_client::attack` against the running
webd and prints one `WEBDATTACK:PROBE:<name>:<outcome>` line per probe, then quits
(`WEBDATTACK:QUIT:PASS`). The judge (`tools/webd/attackjudge.py`) holds the expected outcome of
each probe:

- `scope_net`, `scope_browser`: `CreateView` through a scope the manifest lacks — refused by the
  kernel (`EACCES`/`EPERM`, with a `LABEL:DENY` of this label for that interface).
- `view_ok`: a view through the granted `view` scope — made.
- `flood_alive`: 1000 oneway mouse moves, then a call that needs a reply — webd still answers.
- `view_cap`: one more than `VIEWS_PER_CLIENT` live views — the extra is refused (`ENOSPC`).
- `alive_after`: webd still serves a fresh view after the flood and the cap.

Shipped only by `LAZYOS_WEBDDEMO=1` images, like the other webd test clients.
